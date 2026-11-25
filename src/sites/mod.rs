//! Site abstraction (`Site` trait) and runtime registry of supported manga platforms.

pub mod manga_up;

use thiserror::Error;

#[cfg(test)]
mod tests {
    use super::{CatalogEntry, search_catalog};

    fn e(title: &str) -> CatalogEntry {
        CatalogEntry {
            key: title.len().to_string(),
            title: title.into(),
            url: String::new(),
            cover: None,
            last_updated: None,
        }
    }

    #[test]
    fn search_ranks_prefix_then_substring_and_requires_all_words() {
        let cat = vec![
            e("I Want to Smoke"),
            e("Smoking Behind the Supermarket with You"),
            e("Supermarket Smoking Club Extended Edition"),
            e("Unrelated Title"),
        ];
        let hits: Vec<_> = search_catalog(&cat, "SMOKING", 10)
            .into_iter()
            .map(|c| c.title.as_str())
            .collect();
        assert_eq!(
            hits,
            [
                "Smoking Behind the Supermarket with You",
                "Supermarket Smoking Club Extended Edition"
            ]
        );
        assert_eq!(search_catalog(&cat, "supermarket you", 10).len(), 1);
        assert_eq!(search_catalog(&cat, "   ", 10).len(), 0);
        assert_eq!(search_catalog(&cat, "s", 1).len(), 1);
    }
}

use crate::model::{SeriesKey, SeriesSnapshot};

pub use self::manga_up::MangaUp;

/// Errors produced when parsing a site's HTML/JSON payload.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseError {
    /// The expected `<script id="__NEXT_DATA__">` tag was not found in the response body.
    #[error("missing __NEXT_DATA__ script block in HTML")]
    NextDataMissing,

    /// The extracted script block is not valid JSON.
    #[error("invalid JSON in page data: {0}")]
    InvalidJson(String),

    /// Required JSON field or structure was missing or had an unexpected type.
    #[error("schema mismatch at `{0}`")]
    SchemaMismatch(String),

    /// The site returned an application-level error object in `pageProps.error`.
    #[error("site returned page error: {0}")]
    PageError(String),

    /// The series parsed structurally, but its chapter list was empty.
    #[error("chapter list is empty")]
    EmptyChapterList,
}

/// Pure parser and URL resolver for a manga publishing platform.
///
/// Implementations perform **no I/O**. All HTTP requests are executed by `http.rs`.
pub trait Site: Send + Sync {
    /// Stable identifier used in config and persisted state keys, e.g. `"mangaup"`.
    fn id(&self) -> &'static str;

    /// Human-friendly platform name for the Discord embed author line, e.g. `"Manga UP!"`.
    fn display_name(&self) -> &'static str;

    /// Returns `Some(key)` if this site owns the URL (e.g. `"261"`), otherwise `None`.
    fn series_key(&self, url: &reqwest::Url) -> Option<String>;

    /// Returns the canonical URL fetched once per polling cycle for the given series key.
    fn series_url(&self, key: &str) -> reqwest::Url;

    /// Purely parses the fetched HTML body into a [`SeriesSnapshot`].
    fn parse(&self, key: &str, body: &str) -> Result<SeriesSnapshot, ParseError>;

    /// Whether `url` is a page on this provider's own official site.
    ///
    /// Links recorded for the user to open (new releases) must satisfy this; anything else is
    /// replaced by the official series page. The default refuses every URL.
    fn is_official_url(&self, _url: &reqwest::Url) -> bool {
        false
    }

    /// Optional: the single public page listing every title on the site, used by the GUI's
    /// "search by name" (fetched once, then filtered locally). `None` = search unsupported.
    fn catalog_url(&self) -> Option<reqwest::Url> {
        None
    }

    /// Purely parses the page returned by [`Site::catalog_url`].
    fn parse_catalog(&self, _body: &str) -> Result<Vec<CatalogEntry>, ParseError> {
        Ok(Vec::new())
    }
}

/// One title from a site's public catalog (used for search-by-name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    /// Site-scoped series key (e.g. `"261"`).
    pub key: String,
    /// Display title.
    pub title: String,
    /// Canonical series page URL.
    pub url: String,
    /// Absolute cover image URL, if any.
    pub cover: Option<String>,
    /// Raw "last updated" string from the site, if any.
    pub last_updated: Option<String>,
}

/// Case-insensitive name search over a catalog. Every whitespace-separated word of `query`
/// must occur in the title; titles starting with the query rank first, then shorter titles.
#[must_use]
pub fn search_catalog<'a>(
    catalog: &'a [CatalogEntry],
    query: &str,
    limit: usize,
) -> Vec<&'a CatalogEntry> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let words: Vec<&str> = q.split_whitespace().collect();
    let mut hits: Vec<(u8, usize, &CatalogEntry)> = catalog
        .iter()
        .filter_map(|e| {
            let t = e.title.to_lowercase();
            if !words.iter().all(|w| t.contains(w)) {
                return None;
            }
            let rank = if t.starts_with(&q) {
                0
            } else if t.contains(&q) {
                1
            } else {
                2
            };
            Some((rank, e.title.len(), e))
        })
        .collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    hits.into_iter().take(limit).map(|(_, _, e)| e).collect()
}

/// Resolved site reference and series key for a configured URL.
#[derive(Clone)]
pub struct ResolvedSeries<'a> {
    /// Reference to the matched site implementation.
    pub site: &'a dyn Site,
    /// Composite series key (`"<site_id>:<key>"`).
    pub series_key: SeriesKey,
    /// Canonical fetch URL for this series.
    pub canonical_url: reqwest::Url,
}

/// Runtime collection of registered [`Site`] implementations.
pub struct Registry {
    sites: Vec<Box<dyn Site>>,
}

impl Registry {
    /// Creates an empty [`Registry`].
    #[must_use]
    pub fn empty() -> Self {
        Self { sites: Vec::new() }
    }

    /// Creates the default production [`Registry`] with Manga UP! Global registered.
    #[must_use]
    ///
    /// `BESIDEME_SITE_URL` (an `https` URL, or `http` on loopback) points Manga UP! at a local
    /// fixture server for smoke tests; any other value is ignored.
    pub fn with_defaults() -> Self {
        let mut reg = Self::empty();
        reg.register(Box::new(site_override().unwrap_or_default()));
        reg
    }

    /// Registers a new [`Site`] implementation.
    pub fn register(&mut self, site: Box<dyn Site>) {
        self.sites.push(site);
    }

    /// Looks up a registered site by its stable `id` (e.g. `"mangaup"`).
    #[must_use]
    pub fn get_by_id(&self, id: &str) -> Option<&dyn Site> {
        self.sites
            .iter()
            .find(|s| s.id() == id)
            .map(std::convert::AsRef::as_ref)
    }

    /// Resolves a series URL against all registered sites.
    #[must_use]
    pub fn resolve_url(&self, url: &reqwest::Url) -> Option<ResolvedSeries<'_>> {
        for site in &self.sites {
            if let Some(key) = site.series_key(url) {
                let canonical_url = site.series_url(&key);
                return Some(ResolvedSeries {
                    site: site.as_ref(),
                    series_key: SeriesKey::new(site.id(), key),
                    canonical_url,
                });
            }
        }
        None
    }

    /// Whether any registered site recognises `url` as one of its own official pages.
    #[must_use]
    pub fn is_official_url(&self, url: &reqwest::Url) -> bool {
        self.sites.iter().any(|site| site.is_official_url(url))
    }

    /// Returns an iterator over all registered sites.
    pub fn iter(&self) -> impl Iterator<Item = &dyn Site> {
        self.sites.iter().map(std::convert::AsRef::as_ref)
    }
}

/// Environment variable that redirects Manga UP! to a local fixture server.
pub const SITE_URL_ENV: &str = "BESIDEME_SITE_URL";

fn site_override() -> Option<MangaUp> {
    let raw = std::env::var(SITE_URL_ENV).ok()?;
    let url = reqwest::Url::parse(raw.trim()).ok()?;
    let loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    let acceptable = url.scheme() == "https" || (url.scheme() == "http" && loopback);
    if !acceptable || url.cannot_be_a_base() {
        tracing::warn!("ignoring {SITE_URL_ENV}: expected an https URL or an http loopback URL");
        return None;
    }
    Some(MangaUp::new(url.clone(), url))
}

impl Default for Registry {
    fn default() -> Self {
        Self::with_defaults()
    }
}
