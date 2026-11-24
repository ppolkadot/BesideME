//! Core domain data models shared across sites, detection, state, and notifications.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Reference to a single chapter entry scraped from a series page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChapterRef {
    /// Site-scoped, stable, opaque chapter identifier (stored as string).
    pub id: String,
    /// Primary display label, e.g. `"Chapter 67.3"`.
    pub label: String,
    /// Optional chapter subtitle, e.g. `"67th Smoke"`.
    pub subtitle: Option<String>,
    /// Absolute, clickable URL to the chapter.
    pub url: String,
    /// Optional absolute URL to the chapter cover thumbnail.
    pub thumbnail: Option<String>,
    /// Raw site publication date string, kept as-is when present.
    pub published: Option<String>,
    /// `true` when `published` is absent (early-access / advance release).
    pub early_access: bool,
}

/// A parsed point-in-time snapshot of a series page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeriesSnapshot {
    /// Canonical title of the manga series as reported by the site.
    pub title: String,
    /// Chapters ordered newest first (`chapters_newest_first[0]` is the latest head).
    pub chapters_newest_first: Vec<ChapterRef>,
}

/// Composite key identifying a series (`"<site_id>:<key>"`, e.g. `"mangaup:261"`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SeriesKey {
    /// Registered site identifier, e.g. `"mangaup"`.
    pub site_id: String,
    /// Site-scoped series identifier, e.g. `"261"`.
    pub key: String,
}

impl SeriesKey {
    /// Creates a new [`SeriesKey`].
    #[must_use]
    pub fn new(site_id: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            site_id: site_id.into(),
            key: key.into(),
        }
    }

    /// Formats the key in its persisted state form (`"<site_id>:<key>"`).
    #[must_use]
    pub fn as_state_key(&self) -> String {
        format!("{}:{}", self.site_id, self.key)
    }
}

impl fmt::Display for SeriesKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.site_id, self.key)
    }
}
