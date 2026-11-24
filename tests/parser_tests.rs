//! Parser tests for Manga UP! Global (`sites/manga_up.rs`) and custom `Site` trait extensibility.

use std::fmt::Write as _;

use reqwest::Url;

use manga_watcher::model::{ChapterRef, SeriesSnapshot};
use manga_watcher::sites::{MangaUp, ParseError, Registry, Site};

const FIXTURE_BASE: &str = include_str!("fixtures/manga261_base.html");
const FIXTURE_NEW: &str = include_str!("fixtures/manga261_new.html");
const FIXTURE_HEAD_REMOVED: &str = include_str!("fixtures/manga261_head_removed.html");
const FIXTURE_ATTR_REORDERED: &str = include_str!("fixtures/manga261_attr_reordered.html");
const FIXTURE_PAGE_ERROR: &str = include_str!("fixtures/manga261_page_error.html");

#[test]
fn parses_base_fixture_and_preserves_order_and_early_access_flags() {
    let site = MangaUp::default();
    let snap = site.parse("261", FIXTURE_BASE).expect("parse base fixture");

    assert_eq!(snap.title, "Smoking Behind the Supermarket with You");
    assert_eq!(snap.chapters_newest_first.len(), 5);

    let head = &snap.chapters_newest_first[0];
    assert_eq!(head.id, "130419");
    assert_eq!(head.label, "Chapter 67.2");
    assert_eq!(head.subtitle.as_deref(), Some("67th Smoke"));
    assert_eq!(head.url, "https://global.manga-up.com/manga/261/130419");
    assert_eq!(
        head.thumbnail.as_deref(),
        Some(
            "https://global-img.manga-up.com/asset/J0O/ja/chapter/130419.webp?h=abc2&e=1795618800"
        )
    );
    assert!(head.published.is_none());
    assert!(head.early_access);

    // Ensure firstPageImageUrl was never used
    for ch in &snap.chapters_newest_first {
        assert!(
            !ch.thumbnail
                .as_deref()
                .unwrap_or_default()
                .contains("ignore"),
            "firstPageImageUrl must never be referenced"
        );
    }

    assert!(snap.chapters_newest_first[1].early_access);
    assert!(!snap.chapters_newest_first[2].early_access);
    assert_eq!(
        snap.chapters_newest_first[2].published.as_deref(),
        Some("Nov 21, 2026")
    );
    assert_eq!(snap.chapters_newest_first[4].id, "28665");
    assert_eq!(snap.chapters_newest_first[4].label, "Chapter 1");
}

#[test]
fn parses_new_head_removed_and_attr_reordered_fixtures() {
    let site = MangaUp::default();

    let snap_new = site.parse("261", FIXTURE_NEW).expect("parse new fixture");
    assert_eq!(snap_new.chapters_newest_first.len(), 6);
    assert_eq!(snap_new.chapters_newest_first[0].id, "130422");
    assert_eq!(snap_new.chapters_newest_first[0].label, "Chapter 67.3");

    let snap_removed = site
        .parse("261", FIXTURE_HEAD_REMOVED)
        .expect("parse head_removed fixture");
    assert_eq!(snap_removed.chapters_newest_first.len(), 4);
    assert_eq!(snap_removed.chapters_newest_first[0].id, "130416");

    let snap_reordered = site
        .parse("261", FIXTURE_ATTR_REORDERED)
        .expect("parse attr_reordered fixture");
    assert_eq!(snap_reordered.chapters_newest_first.len(), 5);
    assert_eq!(snap_reordered.chapters_newest_first[0].id, "130419");
}

#[test]
fn parses_generated_160_chapter_fixture() {
    let mut chapters_json = String::new();
    for i in (1..=160).rev() {
        if !chapters_json.is_empty() {
            chapters_json.push(',');
        }
        let published_field = if i <= 157 {
            r#","published":"Jan 01, 2026""#
        } else {
            ""
        };
        let _ = write!(
            chapters_json,
            r#"{{"id":{},"mainName":"Chapter {i}","subName":"Smoke #{i}","imageUrl":"/asset/{i}.webp"{published_field}}}"#,
            100_000 + i
        );
    }

    let html = format!(
        r#"<html><body><script id="__NEXT_DATA__" type="application/json">{{"props":{{"pageProps":{{"titleId":261,"error":null,"data":{{"titleName":"Smoking Behind the Supermarket with You","chapters":[{chapters_json}]}}}}}}}}</script></body></html>"#
    );

    let site = MangaUp::default();
    let snap = site.parse("261", &html).expect("parse 160 chapters");
    assert_eq!(snap.chapters_newest_first.len(), 160);
    assert_eq!(snap.chapters_newest_first[0].id, "100160");
    assert!(snap.chapters_newest_first[0].early_access);
    assert_eq!(snap.chapters_newest_first[159].id, "100001");
    assert!(!snap.chapters_newest_first[159].early_access);
}

#[test]
fn parser_returns_typed_errors_and_never_maps_failures_to_empty_ok() {
    let site = MangaUp::default();

    // Missing __NEXT_DATA__
    let err = site
        .parse("261", "<html><body>No next data here</body></html>")
        .unwrap_err();
    assert_eq!(err, ParseError::NextDataMissing);

    // Malformed JSON
    let err = site
        .parse(
            "261",
            r#"<script id="__NEXT_DATA__">{ broken json </script>"#,
        )
        .unwrap_err();
    assert!(matches!(err, ParseError::InvalidJson(_)));

    // PageError when pageProps.error is set
    let err = site.parse("261", FIXTURE_PAGE_ERROR).unwrap_err();
    assert!(matches!(err, ParseError::PageError(_)));

    // Missing chapters field -> SchemaMismatch
    let no_chapters_html = r#"<script id="__NEXT_DATA__">{"props":{"pageProps":{"data":{"titleName":"Test"}}}}</script>"#;
    let err = site.parse("261", no_chapters_html).unwrap_err();
    assert_eq!(
        err,
        ParseError::SchemaMismatch("props.pageProps.data.chapters".to_string())
    );

    // Empty chapters list -> EmptyChapterList
    let empty_chapters_html = r#"<script id="__NEXT_DATA__">{"props":{"pageProps":{"data":{"titleName":"Test","chapters":[]}}}}</script>"#;
    let err = site.parse("261", empty_chapters_html).unwrap_err();
    assert_eq!(err, ParseError::EmptyChapterList);
}

#[test]
fn parser_accepts_minimal_chapter_without_optional_fields_and_ignores_unknown_fields() {
    let site = MangaUp::default();
    let html = r#"
        <script id="__NEXT_DATA__" type="application/json">
        {
            "props": {
                "pageProps": {
                    "unexpectedRootKey": [1, 2, 3],
                    "data": {
                        "titleName": "Minimal Manga",
                        "extraMeta": {"foo": "bar"},
                        "chapters": [
                            {
                                "id": 42,
                                "mainName": "Chapter 1",
                                "unknownFutureField": "ignored"
                            }
                        ]
                    }
                }
            }
        }
        </script>
    "#;

    let snap = site.parse("999", html).expect("minimal chapter parses");
    assert_eq!(snap.title, "Minimal Manga");
    assert_eq!(snap.chapters_newest_first.len(), 1);
    let ch = &snap.chapters_newest_first[0];
    assert_eq!(ch.id, "42");
    assert_eq!(ch.label, "Chapter 1");
    assert_eq!(ch.subtitle, None);
    assert_eq!(ch.thumbnail, None);
    assert_eq!(ch.published, None);
    assert!(ch.early_access);
    assert_eq!(ch.url, "https://global.manga-up.com/manga/999/42");
}

#[test]
fn series_key_parsing_handles_trailing_slash_query_and_rejects_invalid_paths() {
    let site = MangaUp::default();

    for valid in [
        "https://global.manga-up.com/manga/261",
        "https://global.manga-up.com/manga/261/",
        "https://global.manga-up.com/manga/261?x=1&y=2",
    ] {
        let url = Url::parse(valid).unwrap();
        assert_eq!(
            site.series_key(&url).as_deref(),
            Some("261"),
            "should accept {valid}"
        );
    }

    for invalid in [
        "https://global.manga-up.com/manga/abc",
        "https://global.manga-up.com/manga/261/130422",
        "https://global.manga-up.com/manga/",
        "https://global.manga-up.com/titles/261",
        "https://other-host.example.com/manga/261",
    ] {
        let url = Url::parse(invalid).unwrap();
        assert_eq!(site.series_key(&url), None, "should reject {invalid}");
    }
}

/// ~30-line stub `Site` implementation demonstrating that adding a new platform requires
/// only implementing `Site` in one struct (§0, §10.8).
struct ExampleStubSite;

impl Site for ExampleStubSite {
    fn id(&self) -> &'static str {
        "stubsite"
    }
    fn display_name(&self) -> &'static str {
        "Example Stub Manga"
    }
    fn series_key(&self, url: &Url) -> Option<String> {
        if url.host_str()? != "stub.example.com" {
            return None;
        }
        match url.path_segments()?.collect::<Vec<_>>().as_slice() {
            ["series", slug] if !slug.is_empty() => Some((*slug).to_string()),
            _ => None,
        }
    }
    fn series_url(&self, key: &str) -> Url {
        Url::parse(&format!("https://stub.example.com/series/{key}")).unwrap()
    }
    fn parse(&self, key: &str, body: &str) -> Result<SeriesSnapshot, ParseError> {
        let v: serde_json::Value =
            serde_json::from_str(body).map_err(|e| ParseError::InvalidJson(e.to_string()))?;
        let title = v["title"]
            .as_str()
            .ok_or_else(|| ParseError::SchemaMismatch("title".into()))?
            .to_string();
        let arr = v["items"]
            .as_array()
            .ok_or_else(|| ParseError::SchemaMismatch("items".into()))?;
        if arr.is_empty() {
            return Err(ParseError::EmptyChapterList);
        }
        let chapters_newest_first = arr
            .iter()
            .map(|item| ChapterRef {
                id: item["id"].as_str().unwrap_or_default().to_string(),
                label: item["label"].as_str().unwrap_or_default().to_string(),
                subtitle: None,
                url: format!(
                    "https://stub.example.com/series/{key}/{}",
                    item["id"].as_str().unwrap_or_default()
                ),
                thumbnail: None,
                published: Some("2026-11-23".to_string()),
                early_access: false,
            })
            .collect();
        Ok(SeriesSnapshot {
            title,
            chapters_newest_first,
        })
    }
}

#[test]
fn custom_stub_site_integrates_with_registry_and_parses_fixture() {
    let mut registry = Registry::with_defaults();
    registry.register(Box::new(ExampleStubSite));

    let url = Url::parse("https://stub.example.com/series/my-hero").unwrap();
    let resolved = registry.resolve_url(&url).expect("resolved stub site");
    assert_eq!(resolved.series_key.as_state_key(), "stubsite:my-hero");

    let fixture = r#"{"title":"Stub Adventure","items":[{"id":"c2","label":"Chapter 2"},{"id":"c1","label":"Chapter 1"}]}"#;
    let snap = resolved.site.parse("my-hero", fixture).expect("parse stub");
    assert_eq!(snap.title, "Stub Adventure");
    assert_eq!(snap.chapters_newest_first.len(), 2);
    assert_eq!(snap.chapters_newest_first[0].id, "c2");
}

#[test]
fn parses_search_page_catalog_and_skips_bad_entries() {
    use manga_watcher::sites::search_catalog;
    let site = MangaUp::default();
    let html = r#"<html><script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{"data":{"genres":[],"titles":[
        {"titleId":261,"titleName":"Smoking Behind the Supermarket with You","imageUrl":"/asset/a/en/manga_list/261.webp?h=x","lastUpdated":"Nov 21, 2026"},
        {"titleId":"1050","titleName":"  100 Things the Sheltered Lady  "},
        {"titleName":"no id"},
        {"titleId":7,"titleName":""}
    ]}}}}</script></html>"#;
    let cat = site.parse_catalog(html).unwrap();
    assert_eq!(cat.len(), 2);
    assert_eq!(cat[0].key, "261");
    assert_eq!(cat[0].url, "https://global.manga-up.com/manga/261");
    assert_eq!(
        cat[0].cover.as_deref(),
        Some("https://global-img.manga-up.com/asset/a/en/manga_list/261.webp?h=x")
    );
    assert_eq!(cat[0].last_updated.as_deref(), Some("Nov 21, 2026"));
    assert_eq!(cat[1].title, "100 Things the Sheltered Lady");
    assert_eq!(search_catalog(&cat, "supermarket", 5)[0].key, "261");

    assert!(site.parse_catalog("<html></html>").is_err());
    let empty = r#"<script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{"data":{"titles":[]}}}}</script>"#;
    assert!(site.parse_catalog(empty).is_err());
    assert_eq!(
        site.catalog_url().map(|u| u.to_string()).as_deref(),
        Some("https://global.manga-up.com/search")
    );
}
