//! Pure HTML/`__NEXT_DATA__` parser for Manga UP! Global (`https://global.manga-up.com`).

use reqwest::Url;
use serde::Deserialize;
use serde_json::Value;

use crate::model::{ChapterRef, SeriesSnapshot};
use crate::sites::{CatalogEntry, ParseError, Site};

/// Default production web host for Manga UP! Global.
pub const DEFAULT_BASE_URL: &str = "https://global.manga-up.com";

/// Default production image CDN host for Manga UP! Global thumbnails.
pub const DEFAULT_IMG_BASE_URL: &str = "https://global-img.manga-up.com";

/// Manga UP! Global site parser.
#[derive(Debug, Clone)]
pub struct MangaUp {
    base_url: Url,
    img_base: Url,
}

impl MangaUp {
    /// Creates a new [`MangaUp`] parser with custom base and image hosts (useful for wiremock tests).
    #[must_use]
    pub fn new(base_url: Url, img_base: Url) -> Self {
        Self { base_url, img_base }
    }

    /// Returns the base URL configured for this site instance.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Returns the image base URL configured for this site instance.
    #[must_use]
    pub fn img_base(&self) -> &Url {
        &self.img_base
    }

    fn build_chapter_url(&self, series_key: &str, chapter_id: &str) -> String {
        let base = self.base_url.as_str().trim_end_matches('/');
        format!("{base}/manga/{series_key}/{chapter_id}")
    }

    fn build_thumbnail_url(&self, image_url: &str) -> Option<String> {
        let trimmed = image_url.trim();
        if trimmed.is_empty() {
            return None;
        }
        if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
            return Some(trimmed.to_string());
        }
        if !trimmed.starts_with('/') {
            tracing::warn!(
                image_url = trimmed,
                "Manga UP! imageUrl did not start with '/'; prepending image host anyway"
            );
        }
        let base = self.img_base.as_str().trim_end_matches('/');
        let rel = trimmed.trim_start_matches('/');
        Some(format!("{base}/{rel}"))
    }
}

impl Default for MangaUp {
    fn default() -> Self {
        #[allow(clippy::expect_used)]
        let base_url = Url::parse(DEFAULT_BASE_URL).expect("valid static DEFAULT_BASE_URL");
        #[allow(clippy::expect_used)]
        let img_base = Url::parse(DEFAULT_IMG_BASE_URL).expect("valid static DEFAULT_IMG_BASE_URL");
        Self::new(base_url, img_base)
    }
}

#[derive(Debug, Deserialize)]
struct RawChapter {
    id: u64,
    #[serde(rename = "mainName")]
    main_name: String,
    #[serde(rename = "subName")]
    sub_name: Option<String>,
    #[serde(rename = "imageUrl")]
    image_url: Option<String>,
    published: Option<String>,
    #[serde(rename = "consumptionType")]
    consumption_type: Option<i64>,
    price: Option<i64>,
    badge: Option<i64>,
}

impl Site for MangaUp {
    fn id(&self) -> &'static str {
        "mangaup"
    }

    fn display_name(&self) -> &'static str {
        "Manga UP!"
    }

    fn series_key(&self, url: &Url) -> Option<String> {
        if url.origin() != self.base_url.origin() {
            return None;
        }
        let mut segments: Vec<&str> = url.path_segments()?.collect();
        if segments.last() == Some(&"") {
            segments.pop();
        }
        match segments.as_slice() {
            ["manga", key] if !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit()) => {
                Some((*key).to_string())
            }
            _ => None,
        }
    }

    fn series_url(&self, key: &str) -> Url {
        let mut url = self.base_url.clone();
        url.set_path(&format!("manga/{key}"));
        url.set_query(None);
        url.set_fragment(None);
        url
    }

    fn parse(&self, key: &str, body: &str) -> Result<SeriesSnapshot, ParseError> {
        let json_slice = extract_next_data(body)?;
        let root: Value =
            serde_json::from_str(json_slice).map_err(|e| ParseError::InvalidJson(e.to_string()))?;

        let page_props = root
            .get("props")
            .and_then(|p| p.get("pageProps"))
            .and_then(Value::as_object)
            .ok_or_else(|| ParseError::SchemaMismatch("props.pageProps".to_string()))?;

        if let Some(err_val) = page_props.get("error")
            && is_truthy_error(err_val)
        {
            return Err(ParseError::PageError(err_val.to_string()));
        }

        if let Some(title_id) = page_props.get("titleId") {
            let matches_key = title_id.as_u64().is_some_and(|n| n.to_string() == key)
                || title_id.as_str().is_some_and(|s| s == key);
            if !matches_key {
                tracing::warn!(
                    expected_key = key,
                    actual_title_id = %title_id,
                    "Manga UP! pageProps.titleId does not match requested series key"
                );
            }
        }

        let data = page_props
            .get("data")
            .and_then(Value::as_object)
            .ok_or_else(|| ParseError::SchemaMismatch("props.pageProps.data".to_string()))?;

        let title = data
            .get("titleName")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ParseError::SchemaMismatch("props.pageProps.data.titleName".to_string())
            })?
            .to_string();

        let chapters_arr = data
            .get("chapters")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ParseError::SchemaMismatch("props.pageProps.data.chapters".to_string())
            })?;

        if chapters_arr.is_empty() {
            return Err(ParseError::EmptyChapterList);
        }

        let mut chapters_newest_first = Vec::with_capacity(chapters_arr.len());
        for (idx, item) in chapters_arr.iter().enumerate() {
            let raw: RawChapter = serde_json::from_value(item.clone()).map_err(|_| {
                ParseError::SchemaMismatch(format!("props.pageProps.data.chapters[{idx}]"))
            })?;

            let main_name = raw.main_name.trim().to_string();
            if main_name.is_empty() {
                return Err(ParseError::SchemaMismatch(format!(
                    "props.pageProps.data.chapters[{idx}].mainName"
                )));
            }

            let id_str = raw.id.to_string();
            let published = raw
                .published
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let early_access = published.is_none();

            tracing::debug!(
                series_key = key,
                chapter_id = %id_str,
                label = %main_name,
                early_access,
                consumption_type = ?raw.consumption_type,
                price = ?raw.price,
                badge = ?raw.badge,
                "parsed Manga UP! chapter entry"
            );

            let subtitle = raw
                .sub_name
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let thumbnail = raw
                .image_url
                .as_deref()
                .and_then(|u| self.build_thumbnail_url(u));
            let url = self.build_chapter_url(key, &id_str);

            chapters_newest_first.push(ChapterRef {
                id: id_str,
                label: main_name,
                subtitle,
                url,
                thumbnail,
                published,
                early_access,
            });
        }

        Ok(SeriesSnapshot {
            title,
            chapters_newest_first,
        })
    }

    fn catalog_url(&self) -> Option<Url> {
        // `/search` server-renders every title (`pageProps.data.titles`); the result pages
        // (`/search/result?word=`) are client-rendered and return HTTP 500 to plain GETs.
        let mut url = self.base_url.clone();
        url.set_path("search");
        url.set_query(None);
        Some(url)
    }

    fn parse_catalog(&self, body: &str) -> Result<Vec<CatalogEntry>, ParseError> {
        let root: Value = serde_json::from_str(extract_next_data(body)?)
            .map_err(|e| ParseError::InvalidJson(e.to_string()))?;
        let titles = root
            .pointer("/props/pageProps/data/titles")
            .and_then(Value::as_array)
            .ok_or_else(|| ParseError::SchemaMismatch("props.pageProps.data.titles".into()))?;

        let mut out = Vec::with_capacity(titles.len());
        for item in titles {
            let key = match item.get("titleId") {
                Some(Value::Number(n)) => n.to_string(),
                Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
                _ => continue,
            };
            let Some(title) = item
                .get("titleName")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            out.push(CatalogEntry {
                url: self.series_url(&key).to_string(),
                key,
                title: title.to_string(),
                cover: item
                    .get("imageUrl")
                    .and_then(Value::as_str)
                    .and_then(|u| self.build_thumbnail_url(u)),
                last_updated: item
                    .get("lastUpdated")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
        if out.is_empty() {
            return Err(ParseError::SchemaMismatch(
                "props.pageProps.data.titles (no usable entries)".into(),
            ));
        }
        Ok(out)
    }
}

/// Extracts the JSON content inside `<script ... id="__NEXT_DATA__" ...>{...}</script>`.
fn extract_next_data(html: &str) -> Result<&str, ParseError> {
    let marker = r#"id="__NEXT_DATA__""#;
    let id_pos = html.find(marker).ok_or(ParseError::NextDataMissing)?;
    let after_id = &html[id_pos + marker.len()..];
    let gt_rel = after_id.find('>').ok_or(ParseError::NextDataMissing)?;
    let content_start = &after_id[gt_rel + 1..];
    let end_rel = content_start
        .find("</script>")
        .ok_or(ParseError::NextDataMissing)?;
    Ok(content_start[..end_rel].trim())
}

fn is_truthy_error(val: &Value) -> bool {
    match val {
        Value::Null | Value::Bool(false) => false,
        Value::String(s) => !s.trim().is_empty(),
        _ => true,
    }
}
