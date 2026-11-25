//! Format-preserving edits of `config.toml` (used by the GUI to add/remove manga).
//!
//! Edits go through `toml_edit`, so the user's comments and layout survive. Every edit is
//! validated with [`Config::from_toml_str`] before it is written, and the write is atomic.

use std::path::{Path, PathBuf};

use thiserror::Error;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use crate::config::{Config, ConfigError, LoadOptions};
use crate::model::SeriesKey;
use crate::paths::{APP_DIR_NAME, CONFIG_FILE_NAME};
use crate::sites::Registry;

/// Default `config.toml` written for a fresh install (no series yet).
pub const DEFAULT_CONFIG_TEXT: &str = include_str!("../installer/config.default.toml");

/// Errors produced while editing `config.toml`.
#[derive(Debug, Error)]
pub enum EditError {
    /// The current file is not valid TOML.
    #[error("config.toml is not valid TOML: {0}")]
    Toml(String),
    /// `series` exists but is not an array of `[[series]]` tables.
    #[error("`series` in config.toml must be written as [[series]] tables")]
    SeriesNotTables,
    /// The URL is not a series page of any supported site.
    #[error("not a supported series link: {0}")]
    UnsupportedUrl(String),
    /// The series is already configured.
    #[error("this manga is already being watched")]
    AlreadyPresent,
    /// The series to remove was not found.
    #[error("this manga is not in config.toml")]
    NotFound,
    /// The edited config failed validation (nothing was written).
    #[error("edited config is invalid: {0}")]
    Invalid(#[from] ConfigError),
    /// Filesystem error.
    #[error("could not write {path}: {source}")]
    Io {
        /// File involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// A series entry as listed in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedSeries {
    /// Resolved `<site>:<key>`.
    pub key: SeriesKey,
    /// URL exactly as written in the file.
    pub url: String,
    /// Optional `label` override.
    pub label: Option<String>,
    /// Title from the `# comment` line written above the entry by [`add_series`].
    pub comment_title: Option<String>,
}

/// The `config.toml` the GUI should edit: the file the CLI would load if one exists,
/// otherwise `%APPDATA%\BesideME\config.toml` (created on first save).
#[must_use]
pub fn gui_config_path() -> PathBuf {
    if let Ok(p) = crate::paths::resolve_config_path(None) {
        return p;
    }
    if let Some(profile) = crate::paths::profile_dir_override() {
        return profile.join(CONFIG_FILE_NAME);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_DIR_NAME)
        .join(CONFIG_FILE_NAME)
}

/// Reads `path`, or returns [`DEFAULT_CONFIG_TEXT`] when the file does not exist yet.
pub fn read_or_default(path: &Path) -> Result<String, EditError> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DEFAULT_CONFIG_TEXT.to_string()),
        Err(source) => Err(EditError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn parse(text: &str) -> Result<DocumentMut, EditError> {
    text.parse::<DocumentMut>()
        .map_err(|e| EditError::Toml(e.to_string()))
}

fn series_tables(doc: &DocumentMut) -> Result<Option<&ArrayOfTables>, EditError> {
    match doc.get("series") {
        None => Ok(None),
        Some(Item::ArrayOfTables(a)) => Ok(Some(a)),
        Some(_) => Err(EditError::SeriesNotTables),
    }
}

fn resolve_key(registry: &Registry, url: &str) -> Option<SeriesKey> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    registry.resolve_url(&parsed).map(|r| r.series_key)
}

/// Lists the `[[series]]` entries (entries with unsupported URLs are skipped).
pub fn list_series(text: &str, registry: &Registry) -> Result<Vec<ListedSeries>, EditError> {
    let doc = parse(text)?;
    let Some(aot) = series_tables(&doc)? else {
        return Ok(Vec::new());
    };
    Ok(aot
        .iter()
        .filter_map(|t| {
            let url = t.get("url")?.as_str()?.to_string();
            let key = resolve_key(registry, &url)?;
            let label = t
                .get("label")
                .and_then(Item::as_str)
                .map(ToString::to_string);
            let comment_title = t
                .decor()
                .prefix()
                .and_then(|p| p.as_str())
                .and_then(|p| {
                    p.lines()
                        .rev()
                        .map(str::trim)
                        .find_map(|l| l.strip_prefix('#'))
                        .map(|c| c.trim().to_string())
                })
                .filter(|c| !c.is_empty());
            Some(ListedSeries {
                key,
                url,
                label,
                comment_title,
            })
        })
        .collect())
}

/// Returns `text` with a new `[[series]]` entry for `url` appended. `title` is written as a
/// comment above the entry for readability (it is not a config value).
pub fn add_series(
    text: &str,
    registry: &Registry,
    url: &str,
    title: Option<&str>,
) -> Result<String, EditError> {
    let parsed =
        reqwest::Url::parse(url.trim()).map_err(|_| EditError::UnsupportedUrl(url.to_string()))?;
    let resolved = registry
        .resolve_url(&parsed)
        .ok_or_else(|| EditError::UnsupportedUrl(url.to_string()))?;

    if list_series(text, registry)?
        .iter()
        .any(|s| s.key == resolved.series_key)
    {
        return Err(EditError::AlreadyPresent);
    }

    let mut doc = parse(text)?;
    if doc.get("series").is_none() {
        doc.insert("series", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    let aot = doc
        .get_mut("series")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or(EditError::SeriesNotTables)?;

    let mut table = Table::new();
    table["url"] = value(resolved.canonical_url.as_str());
    if let Some(t) = title.map(sanitize_comment).filter(|t| !t.is_empty()) {
        table.decor_mut().set_prefix(format!("\n# {t}\n"));
    } else {
        table.decor_mut().set_prefix("\n");
    }
    aot.push(table);
    Ok(doc.to_string())
}

/// Returns `text` without the `[[series]]` entry for `key`.
pub fn remove_series(
    text: &str,
    registry: &Registry,
    key: &SeriesKey,
) -> Result<String, EditError> {
    let mut doc = parse(text)?;
    let aot = match doc.get_mut("series") {
        Some(Item::ArrayOfTables(a)) => a,
        Some(_) => return Err(EditError::SeriesNotTables),
        None => return Err(EditError::NotFound),
    };
    let idx = aot
        .iter()
        .position(|t| {
            t.get("url")
                .and_then(Item::as_str)
                .and_then(|u| resolve_key(registry, u))
                .is_some_and(|k| &k == key)
        })
        .ok_or(EditError::NotFound)?;
    aot.remove(idx);
    if aot.is_empty() {
        doc.remove("series");
    }
    Ok(doc.to_string())
}

/// Validates `text` as a config (webhooks may be missing) and atomically writes it to `path`.
pub fn validate_and_write(text: &str, path: &Path, registry: &Registry) -> Result<(), EditError> {
    let opts = LoadOptions {
        allow_missing_webhooks: true,
        ..LoadOptions::default()
    };
    Config::from_toml_str(text, &opts, registry, |_| None)?;

    let io = |source| EditError::Io {
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(io)?;
    let mut last = None;
    for _ in 0..3 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(io(
        last.unwrap_or_else(|| std::io::Error::other("rename failed"))
    ))
}

fn sanitize_comment(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WITH_COMMENTS: &str = "# my settings\ninterval_minutes = 30 # keep\n";

    #[test]
    fn add_list_remove_roundtrip_preserves_comments() {
        let reg = Registry::with_defaults();
        let t1 = add_series(
            WITH_COMMENTS,
            &reg,
            "https://global.manga-up.com/manga/261/?x=1",
            Some("Smoking\nBehind"),
        )
        .unwrap();
        assert!(t1.starts_with("# my settings\ninterval_minutes = 30 # keep\n"));
        assert!(t1.contains(
            "# Smoking Behind\n[[series]]\nurl = \"https://global.manga-up.com/manga/261\""
        ));

        let listed = list_series(&t1, &reg).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].key, SeriesKey::new("mangaup", "261"));
        assert_eq!(listed[0].comment_title.as_deref(), Some("Smoking Behind"));

        assert!(matches!(
            add_series(&t1, &reg, "https://global.manga-up.com/manga/261", None),
            Err(EditError::AlreadyPresent)
        ));
        let t2 = add_series(&t1, &reg, "https://global.manga-up.com/manga/1050", None).unwrap();
        assert_eq!(list_series(&t2, &reg).unwrap().len(), 2);

        let t3 = remove_series(&t2, &reg, &SeriesKey::new("mangaup", "261")).unwrap();
        let left = list_series(&t3, &reg).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].key.key, "1050");
        assert!(t3.contains("# keep"));

        let t4 = remove_series(&t3, &reg, &SeriesKey::new("mangaup", "1050")).unwrap();
        assert_eq!(list_series(&t4, &reg).unwrap().len(), 0);
        assert!(matches!(
            remove_series(&t4, &reg, &SeriesKey::new("mangaup", "1050")),
            Err(EditError::NotFound)
        ));
    }

    #[test]
    fn rejects_unsupported_urls_and_inline_series() {
        let reg = Registry::with_defaults();
        assert!(matches!(
            add_series("", &reg, "https://example.com/manga/1", None),
            Err(EditError::UnsupportedUrl(_))
        ));
        assert!(matches!(
            add_series(
                "series = []\n",
                &reg,
                "https://global.manga-up.com/manga/1",
                None
            ),
            Err(EditError::SeriesNotTables)
        ));
    }

    #[test]
    fn default_config_is_valid_and_empty_and_writes_atomically() {
        let reg = Registry::with_defaults();
        assert_eq!(list_series(DEFAULT_CONFIG_TEXT, &reg).unwrap().len(), 0);
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        let text = add_series(
            &read_or_default(&path).unwrap(),
            &reg,
            "https://global.manga-up.com/manga/261",
            None,
        )
        .unwrap();
        validate_and_write(&text, &path, &reg).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert!(!path.with_extension("toml.tmp").exists());
    }
}
