//! Persisted state management (`state.json`), atomic file writes, ID pruning, and single-instance locking.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::paths::lock_path_for_state;

/// Current schema version for `state.json`.
pub const STATE_VERSION: u32 = 2;

/// Maximum number of announced chapter IDs retained per series.
pub const MAX_ANNOUNCED_IDS: usize = 5_000;

/// Number of rename retry attempts on Windows when antivirus/indexer briefly holds the target file.
const RENAME_MAX_RETRIES: u32 = 3;

/// Delay between atomic rename retries.
const RENAME_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Errors returned by state persistence and instance locking operations.
#[derive(Debug, Error)]
pub enum StateError {
    /// Another `besideme-cli` instance holds the exclusive lock on `state.lock`.
    #[error("another instance of besideme-cli is already running (lock held on `{}`)", .0.display())]
    Locked(PathBuf),

    /// The state file could not be read or parsed. Never silently re-baselines.
    #[error(
        "state file `{}` is corrupt or unreadable ({reason}); inspect the file or run `besideme-cli reset --all` to re-baseline",
        path.display()
    )]
    Corrupt {
        /// Path to the state file.
        path: PathBuf,
        /// Underlying failure details.
        reason: String,
    },

    /// The state file has an unknown `version` field.
    #[error(
        "state file `{}` uses unsupported version {version} (expected {STATE_VERSION}); run `besideme-cli reset --all` to re-baseline",
        path.display()
    )]
    UnsupportedVersion {
        /// Path to the state file.
        path: PathBuf,
        /// Found schema version.
        version: u32,
    },

    /// Filesystem I/O failure while reading, writing, or locking state.
    #[error("state I/O error at `{}`: {source}", path.display())]
    Io {
        /// Path involved in the I/O operation.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
}

/// Root structure serialized to `state.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateFile {
    /// Schema version (`2`).
    pub version: u32,
    /// Per-series state keyed by `"<site_id>:<key>"` (e.g. `"mangaup:261"`).
    pub series: BTreeMap<String, SeriesState>,
}

impl Default for StateFile {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            series: BTreeMap::new(),
        }
    }
}

/// Persisted state for a single series.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeriesState {
    /// Set of chapter IDs that have already been baselined or announced (insertion-ordered, capped at 5 000).
    pub announced_ids: Vec<String>,
    /// Label of the most recently baselined or announced head chapter (e.g. `"Chapter 67.3"`).
    pub last_announced_label: Option<String>,
    /// Total chapter count in the last valid snapshot (used by the shrinkage anomaly guard).
    pub last_snapshot_len: usize,
    /// Unix timestamp (seconds) of the last check attempt.
    pub last_checked_unix: Option<i64>,
    /// Unix timestamp (seconds) of the last successful fetch and parse.
    pub last_success_unix: Option<i64>,
    /// Number of consecutive fetch/parse/HTTP failures.
    pub consecutive_failures: u32,
    /// Whether a health alert has been sent for the current failure streak.
    pub alert_sent: bool,
    /// HTTP `ETag` from the last successful fetch, used for `If-None-Match`.
    pub etag: Option<String>,
    /// Number of consecutive anomaly detections (re-baselines on the 3rd consecutive anomaly).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub consecutive_anomalies: u32,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_zero(val: &u32) -> bool {
    *val == 0
}

impl SeriesState {
    /// Appends newly announced chapter IDs (avoiding duplicates) and prunes to [`MAX_ANNOUNCED_IDS`].
    ///
    /// When pruning is needed, IDs absent from `current_snapshot_ids` are removed first.
    pub fn record_announced_ids<I>(&mut self, new_ids: I, current_snapshot_ids: &[String])
    where
        I: IntoIterator<Item = String>,
    {
        let mut existing: HashSet<String> = self.announced_ids.iter().cloned().collect();
        for id in new_ids {
            if existing.insert(id.clone()) {
                self.announced_ids.push(id);
            }
        }
        self.prune_to_cap(current_snapshot_ids);
    }

    /// Enforces the [`MAX_ANNOUNCED_IDS`] cap, pruning IDs absent from `current_snapshot_ids` first.
    pub fn prune_to_cap(&mut self, current_snapshot_ids: &[String]) {
        self.prune_to_custom_cap(MAX_ANNOUNCED_IDS, current_snapshot_ids);
    }

    /// Enforces a custom cap on `announced_ids`, pruning IDs absent from `current_snapshot_ids` first.
    pub fn prune_to_custom_cap(&mut self, cap: usize, current_snapshot_ids: &[String]) {
        if self.announced_ids.len() <= cap {
            return;
        }
        let snapshot_set: HashSet<&str> = current_snapshot_ids.iter().map(String::as_str).collect();
        let mut to_remove = self.announced_ids.len() - cap;

        self.announced_ids.retain(|id| {
            if to_remove > 0 && !snapshot_set.contains(id.as_str()) {
                to_remove -= 1;
                false
            } else {
                true
            }
        });

        if self.announced_ids.len() > cap {
            let excess = self.announced_ids.len() - cap;
            self.announced_ids.drain(0..excess);
        }
    }
}

impl StateFile {
    /// Loads `state.json` from `path`.
    ///
    /// If the file does not exist, returns a fresh empty [`StateFile`] (`version: 2`).
    /// If the file is corrupt, unreadable, or has an unknown version, returns [`StateError`].
    pub fn load(path: &Path) -> Result<Self, StateError> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(StateError::Corrupt {
                    path: path.to_path_buf(),
                    reason: e.to_string(),
                });
            }
        };

        let raw_value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| StateError::Corrupt {
                path: path.to_path_buf(),
                reason: e.to_string(),
            })?;

        if let Some(ver) = raw_value.get("version").and_then(serde_json::Value::as_u64)
            && ver != u64::from(STATE_VERSION)
        {
            #[allow(clippy::cast_possible_truncation)]
            return Err(StateError::UnsupportedVersion {
                path: path.to_path_buf(),
                version: ver as u32,
            });
        }

        let state: Self = serde_json::from_value(raw_value).map_err(|e| StateError::Corrupt {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;

        if state.version != STATE_VERSION {
            return Err(StateError::UnsupportedVersion {
                path: path.to_path_buf(),
                version: state.version,
            });
        }

        Ok(state)
    }

    /// Atomically persists the state to `path` via `<filename>.tmp`, `sync_all`, and retrying `fs::rename`.
    pub fn save_atomic(&self, path: &Path) -> Result<(), StateError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|source| StateError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let tmp_path = temp_path_for(path);
        let serialized = serde_json::to_vec_pretty(self).map_err(|e| StateError::Corrupt {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;

        {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)
                .map_err(|source| StateError::Io {
                    path: tmp_path.clone(),
                    source,
                })?;

            file.write_all(&serialized)
                .map_err(|source| StateError::Io {
                    path: tmp_path.clone(),
                    source,
                })?;
            file.write_all(b"\n").map_err(|source| StateError::Io {
                path: tmp_path.clone(),
                source,
            })?;
            file.sync_all().map_err(|source| StateError::Io {
                path: tmp_path.clone(),
                source,
            })?;
        }

        let mut last_err = None;
        for attempt in 0..=RENAME_MAX_RETRIES {
            match fs::rename(&tmp_path, path) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = Some(e);
                    if attempt < RENAME_MAX_RETRIES {
                        thread::sleep(RENAME_RETRY_DELAY);
                    }
                }
            }
        }

        Err(StateError::Io {
            path: path.to_path_buf(),
            source: last_err
                .unwrap_or_else(|| std::io::Error::other("rename failed after retries")),
        })
    }
}

fn temp_path_for(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".tmp");
    PathBuf::from(os)
}

/// Exclusive single-instance OS file lock on `state.lock`.
///
/// The lock is held for the lifetime of this guard and released when dropped.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
    path: PathBuf,
}

impl InstanceLock {
    /// Acquires an exclusive non-blocking lock on `state.lock` next to `state_path`.
    pub fn acquire_for_state(state_path: &Path) -> Result<Self, StateError> {
        let lock_path = lock_path_for_state(state_path);
        Self::acquire(&lock_path)
    }

    /// Acquires an exclusive non-blocking lock on the specified `lock_path`.
    pub fn acquire(lock_path: &Path) -> Result<Self, StateError> {
        if let Some(parent) = lock_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|source| StateError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|source| StateError::Io {
                path: lock_path.to_path_buf(),
                source,
            })?;

        match file.try_lock() {
            Ok(()) => Ok(Self {
                _file: file,
                path: lock_path.to_path_buf(),
            }),
            Err(TryLockError::WouldBlock) => Err(StateError::Locked(lock_path.to_path_buf())),
            Err(TryLockError::Error(source)) => Err(StateError::Io {
                path: lock_path.to_path_buf(),
                source,
            }),
        }
    }

    /// Returns the path of the held lock file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}
