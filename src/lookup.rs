//! One-off lookups used by the GUI: preview a series from its link, and load a site's
//! title catalog for search-by-name. Both go through [`SiteHttpClient`] (same UA, timeout
//! and size cap as the watcher) and make exactly one GET.

use std::time::Duration;

use thiserror::Error;

use crate::config::{DEFAULT_TIMEOUT_SECONDS, DEFAULT_USER_AGENT};
use crate::http::{FetchOutcome, HttpError, SiteHttpClient};
use crate::model::{SeriesKey, SeriesSnapshot};
use crate::sites::{CatalogEntry, ParseError, Registry};

/// Errors from a GUI lookup.
#[derive(Debug, Error)]
pub enum LookupError {
    /// Not a series link of any supported site.
    #[error(
        "that link isn't a supported manga page (expected e.g. https://global.manga-up.com/manga/261)"
    )]
    UnsupportedUrl,
    /// The site has no searchable catalog.
    #[error("search by name isn't supported for this site")]
    NoCatalog,
    /// Network failure.
    #[error("couldn't reach the site: {0}")]
    Http(#[from] HttpError),
    /// The page didn't have the expected shape.
    #[error("the page couldn't be read: {0}")]
    Parse(#[from] ParseError),
    /// Unexpected 304 without a cached copy.
    #[error("unexpected empty response")]
    Empty,
}

/// A previewed series: what the GUI shows before the user confirms "Add".
#[derive(Debug, Clone)]
pub struct SeriesPreview {
    /// Resolved `<site>:<key>`.
    pub key: SeriesKey,
    /// Canonical series URL (what gets written to config).
    pub url: String,
    /// Site display name.
    pub site_name: String,
    /// Parsed page.
    pub snapshot: SeriesSnapshot,
}

fn client() -> Result<SiteHttpClient, HttpError> {
    SiteHttpClient::new(
        DEFAULT_USER_AGENT,
        Duration::from_secs(DEFAULT_TIMEOUT_SECONDS),
    )
}

async fn get(url: &reqwest::Url) -> Result<String, LookupError> {
    match client()?.fetch(url, None).await? {
        FetchOutcome::Fetched { body, .. } => Ok(body),
        FetchOutcome::NotModified { .. } => Err(LookupError::Empty),
    }
}

/// Fetches and parses the series page behind `link`.
pub async fn preview_series(registry: &Registry, link: &str) -> Result<SeriesPreview, LookupError> {
    let parsed = reqwest::Url::parse(link.trim()).map_err(|_| LookupError::UnsupportedUrl)?;
    let resolved = registry
        .resolve_url(&parsed)
        .ok_or(LookupError::UnsupportedUrl)?;
    let body = get(&resolved.canonical_url).await?;
    let snapshot = resolved.site.parse(&resolved.series_key.key, &body)?;
    Ok(SeriesPreview {
        url: resolved.canonical_url.to_string(),
        site_name: resolved.site.display_name().to_string(),
        key: resolved.series_key,
        snapshot,
    })
}

/// Loads the searchable catalog of every registered site that has one.
pub async fn load_catalog(registry: &Registry) -> Result<Vec<CatalogEntry>, LookupError> {
    let mut all = Vec::new();
    let mut any = false;
    for site in registry.iter() {
        let Some(url) = site.catalog_url() else {
            continue;
        };
        any = true;
        let body = get(&url).await?;
        all.extend(site.parse_catalog(&body)?);
    }
    if any {
        Ok(all)
    } else {
        Err(LookupError::NoCatalog)
    }
}
