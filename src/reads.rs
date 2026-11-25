//! Per-title read markers, stored apart from the checker's `state.json`.
//!
//! The desktop app marks releases read while a scheduled check may be writing the checker
//! state. Keeping the markers in their own file (`reads.json`, guarded by its own short-lived
//! `reads.lock`) lets both happen at the same time without either overwriting the other.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::state::{DiscoveredRelease, InstanceLock, SeriesState, StateError};

/// File name of the read-marker store, placed next to `state.json`.
pub const READS_FILE_NAME: &str = "reads.json";

/// Lock file guarding read-modify-write cycles of the read-marker store.
pub const READS_LOCK_NAME: &str = "reads.lock";

/// Current schema version of `reads.json`.
pub const READS_VERSION: u32 = 1;

/// Maximum read release identifiers remembered per series (oldest are forgotten first).
pub const MAX_READ_IDS_PER_SERIES: usize = 200;

const LOCK_ATTEMPTS: u32 = 20;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Returns the read-marker path that belongs to a given `state.json` path.
#[must_use]
pub fn reads_path_for_state(state_path: &Path) -> PathBuf {
    sibling(state_path, READS_FILE_NAME)
}

fn sibling(state_path: &Path, name: &str) -> PathBuf {
    match state_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

/// Release identifiers the user has marked read, per series.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadMarkers {
    /// Schema version (`1`).
    pub version: u32,
    /// Read release identifiers keyed by `"<site_id>:<key>"`, oldest first.
    #[serde(default)]
    pub series: BTreeMap<String, Vec<String>>,
}

impl Default for ReadMarkers {
    fn default() -> Self {
        Self {
            version: READS_VERSION,
            series: BTreeMap::new(),
        }
    }
}

impl ReadMarkers {
    /// Loads the markers. A missing file is empty; an unreadable one is moved aside and
    /// treated as empty (markers are a convenience, so they never block the app).
    #[must_use]
    pub fn load(path: &Path) -> Self {
        match fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<Self>(&bytes) {
                Ok(markers) if markers.version == READS_VERSION => markers,
                _ => {
                    tracing::warn!(path = %path.display(), "read markers are unreadable; starting empty");
                    let mut backup = path.as_os_str().to_os_string();
                    backup.push(".bad");
                    let _ = fs::rename(path, PathBuf::from(backup));
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    /// Whether `release_id` is marked read for `series_key`.
    #[must_use]
    pub fn is_read(&self, series_key: &str, release_id: &str) -> bool {
        self.series
            .get(series_key)
            .is_some_and(|ids| ids.iter().any(|id| id == release_id))
    }

    /// Marks identifiers read, keeping at most [`MAX_READ_IDS_PER_SERIES`] per series.
    pub fn mark_read<'a, I>(&mut self, series_key: &str, release_ids: I)
    where
        I: IntoIterator<Item = &'a str>,
    {
        let ids = self.series.entry(series_key.to_owned()).or_default();
        for id in release_ids {
            if !ids.iter().any(|known| known == id) {
                ids.push(id.to_owned());
            }
        }
        if ids.len() > MAX_READ_IDS_PER_SERIES {
            let excess = ids.len() - MAX_READ_IDS_PER_SERIES;
            ids.drain(0..excess);
        }
    }

    /// Forgets every marker of a series (used when the series is removed).
    pub fn forget(&mut self, series_key: &str) -> bool {
        self.series.remove(series_key).is_some()
    }

    fn save_atomic(&self, path: &Path) -> Result<(), StateError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|source| StateError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut tmp = path.as_os_str().to_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| StateError::Corrupt {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
        let io = |source| StateError::Io {
            path: tmp.clone(),
            source,
        };
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)
                .map_err(io)?;
            file.write_all(&bytes).map_err(io)?;
            file.sync_all().map_err(io)?;
        }
        let mut last = None;
        for _ in 0..4 {
            match fs::rename(&tmp, path) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last = Some(e);
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }
        Err(StateError::Io {
            path: path.to_path_buf(),
            source: last.unwrap_or_else(|| std::io::Error::other("rename failed")),
        })
    }

    /// Applies `edit` to the stored markers under the short-lived `reads.lock`.
    ///
    /// This never touches the checker's `state.lock`, so it can run while a check is in
    /// progress.
    pub fn update<F>(path: &Path, edit: F) -> Result<(), StateError>
    where
        F: FnOnce(&mut Self),
    {
        let lock_path = path.with_file_name(READS_LOCK_NAME);
        let mut attempt = 0;
        let _lock = loop {
            match InstanceLock::acquire(&lock_path) {
                Ok(lock) => break lock,
                Err(StateError::Locked(_)) if attempt < LOCK_ATTEMPTS => {
                    attempt += 1;
                    thread::sleep(LOCK_RETRY_DELAY);
                }
                Err(err) => return Err(err),
            }
        };
        let mut markers = Self::load(path);
        edit(&mut markers);
        markers.save_atomic(path)
    }
}

/// Releases of a series the user has not marked read, newest first.
#[must_use]
pub fn unread_releases<'a>(
    series_key: &str,
    state: &'a SeriesState,
    markers: &ReadMarkers,
) -> Vec<&'a DiscoveredRelease> {
    state
        .discovered
        .iter()
        .rev()
        .filter(|release| !markers.is_read(series_key, &release.id))
        .collect()
}
