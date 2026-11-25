//! What the window shows, read from `config.toml`, `state.json` and `reads.json`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use besideme::config_edit::{list_series, read_or_default};
use besideme::model::SeriesKey;
use besideme::reads::{ReadMarkers, unread_releases};
use besideme::sites::Registry;
use besideme::state::{RunRecord, StateFile};
use besideme::watcher::retry_wait_secs;

/// A background result that is not ready yet, is ready, or failed.
pub enum Loadable<T> {
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

/// A release the user has not marked read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadRelease {
    pub id: String,
    pub label: String,
    pub subtitle: Option<String>,
    /// Official provider link (validated again when loaded).
    pub url: String,
    pub parts: usize,
}

/// One watched title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub key: SeriesKey,
    /// Official series page.
    pub url: String,
    pub title: String,
    pub latest: Option<String>,
    pub last_checked: Option<i64>,
    pub last_success: Option<i64>,
    pub failures: u32,
    pub last_error: Option<String>,
    /// Newest first.
    pub unread: Vec<UnreadRelease>,
    /// Unix time from which a single-title retry is allowed again (`None` = now).
    pub retry_ready_at: Option<i64>,
    /// The title has been checked at least once.
    pub baselined: bool,
}

impl Row {
    /// Whether the title has releases the user has not marked read.
    #[must_use]
    pub fn has_unread(&self) -> bool {
        !self.unread.is_empty()
    }

    /// Whether the last check of this title failed.
    #[must_use]
    pub fn has_issue(&self) -> bool {
        self.failures > 0 || self.last_error.is_some()
    }
}

/// The shelf filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    Unread,
    Issues,
}

impl Filter {
    pub const ALL: [Self; 3] = [Self::All, Self::Unread, Self::Issues];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Unread => "Unread",
            Self::Issues => "Issues",
        }
    }

    #[must_use]
    pub fn matches(self, row: &Row) -> bool {
        match self {
            Self::All => true,
            Self::Unread => row.has_unread(),
            Self::Issues => row.has_issue(),
        }
    }
}

/// Counts per filter, for the filter labels.
#[must_use]
pub fn counts(rows: &[Row]) -> [usize; 3] {
    Filter::ALL.map(|f| rows.iter().filter(|r| f.matches(r)).count())
}

/// Modification times used to notice that another process wrote a file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileStamps {
    config: Option<SystemTime>,
    state: Option<SystemTime>,
    reads: Option<SystemTime>,
}

fn stamp(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl FileStamps {
    #[must_use]
    pub fn read(config: &Path, state: &Path, reads: &Path) -> Self {
        Self {
            config: stamp(config),
            state: stamp(state),
            reads: stamp(reads),
        }
    }
}

/// Everything the window needs from disk.
pub struct Snapshot {
    pub rows: Vec<Row>,
    pub last_run: Option<RunRecord>,
    pub config_error: Option<String>,
}

/// Paths of the three files the window reads.
#[derive(Debug, Clone, Default)]
pub struct Paths {
    pub config: PathBuf,
    pub state: PathBuf,
    pub reads: PathBuf,
}

/// Reads the watchlist with its checker state and read markers.
///
/// An unreadable state file is treated as empty (the checker reports that problem itself),
/// and unreadable read markers mean everything discovered shows as unread.
#[must_use]
pub fn load(
    registry: &Registry,
    paths: &Paths,
    catalog_titles: &HashMap<String, String>,
    now_unix: i64,
) -> Snapshot {
    let text = read_or_default(&paths.config).unwrap_or_default();
    let (listed, config_error) = match list_series(&text, registry) {
        Ok(listed) => (listed, None),
        Err(e) => (Vec::new(), Some(format!("config.toml problem: {e}"))),
    };
    let state = StateFile::load(&paths.state).unwrap_or_default();
    let markers = ReadMarkers::load(&paths.reads);

    let rows = listed
        .into_iter()
        .map(|s| {
            let state_key = s.key.as_state_key();
            let st = state.series.get(&state_key);
            let title = s
                .label
                .clone()
                .or_else(|| catalog_titles.get(&s.key.key).cloned())
                .or(s.comment_title.clone())
                .unwrap_or_else(|| format!("Manga #{}", s.key.key));
            let unread = st
                .map(|st| {
                    unread_releases(&state_key, st, &markers)
                        .into_iter()
                        .map(|r| UnreadRelease {
                            id: r.id.clone(),
                            label: r.label.clone(),
                            subtitle: r.subtitle.clone(),
                            url: official_or(registry, &r.url, &s.url),
                            parts: r.part_ids.len(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            Row {
                title,
                latest: st.and_then(|x| x.last_announced_label.clone()),
                last_checked: st.and_then(|x| x.last_checked_unix),
                last_success: st.and_then(|x| x.last_success_unix),
                failures: st.map_or(0, |x| x.consecutive_failures),
                last_error: st.and_then(|x| x.last_error.clone()),
                unread,
                retry_ready_at: st
                    .and_then(|x| retry_wait_secs(x, now_unix))
                    .map(|wait| now_unix + wait),
                baselined: st.is_some_and(|x| !x.announced_ids.is_empty()),
                url: s.url,
                key: s.key,
            }
        })
        .collect();

    Snapshot {
        rows,
        last_run: state.last_run,
        config_error,
    }
}

/// `candidate` when a registered provider vouches for it, otherwise `fallback`.
#[must_use]
pub fn official_or(registry: &Registry, candidate: &str, fallback: &str) -> String {
    reqwest_url(candidate)
        .filter(|u| registry.is_official_url(u))
        .map_or_else(|| fallback.to_owned(), |u| u.to_string())
}

fn reqwest_url(text: &str) -> Option<reqwest::Url> {
    reqwest::Url::parse(text.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(unread: usize, failures: u32, error: Option<&str>) -> Row {
        Row {
            key: SeriesKey::new("mangaup", "1"),
            url: "https://global.manga-up.com/manga/1".into(),
            title: "T".into(),
            latest: None,
            last_checked: None,
            last_success: None,
            failures,
            last_error: error.map(str::to_owned),
            unread: (0..unread)
                .map(|i| UnreadRelease {
                    id: i.to_string(),
                    label: format!("Chapter {i}"),
                    subtitle: None,
                    url: String::new(),
                    parts: 1,
                })
                .collect(),
            retry_ready_at: None,
            baselined: true,
        }
    }

    #[test]
    fn filters_partition_rows() {
        let rows = vec![
            row(0, 0, None),
            row(2, 0, None),
            row(0, 1, Some("x")),
            row(1, 3, None),
        ];
        assert_eq!(counts(&rows), [4, 2, 2]);
        assert!(Filter::Unread.matches(&rows[3]));
        assert!(Filter::Issues.matches(&rows[3]));
        assert!(!Filter::Issues.matches(&rows[1]));
    }

    #[test]
    fn only_official_links_are_opened() {
        let registry = Registry::with_defaults();
        let fallback = "https://global.manga-up.com/manga/1";
        assert_eq!(
            official_or(&registry, "https://global.manga-up.com/manga/1/9", fallback),
            "https://global.manga-up.com/manga/1/9"
        );
        for bad in [
            "https://evil.example/manga/1/9",
            "https://global.manga-up.com.evil.example/x",
            "https://user:pw@global.manga-up.com/manga/1/9",
            "http://global.manga-up.com/manga/1/9",
            "javascript:alert(1)",
            "file:///C:/Windows/System32/calc.exe",
            "not a url",
        ] {
            assert_eq!(official_or(&registry, bad, fallback), fallback, "{bad}");
        }
    }
}
