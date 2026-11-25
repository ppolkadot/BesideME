//! Golden JSON tests for Discord payloads, wiremock tests for `DiscordClient`,
//! and wiremock tests for `SiteHttpClient` (`http.rs`).
#![allow(clippy::assert_is_empty)]

use std::time::Duration;

use reqwest::Url;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use besideme::config::WebhookUrl;
use besideme::detect::ChapterGroup;
use besideme::http::{FetchOutcome, HttpError, MAX_RESPONSE_BYTES, SiteHttpClient};
use besideme::model::ChapterRef;
use besideme::notify::discord::{
    AnnouncementContext, DiscordClient, DiscordError, build_announcement_payload,
    build_health_alert_payload, build_test_webhook_payload, escape_discord_markdown,
    truncate_chars,
};

fn sample_part(id: &str, label: &str, subtitle: Option<&str>, early_access: bool) -> ChapterRef {
    ChapterRef {
        id: id.to_string(),
        label: label.to_string(),
        subtitle: subtitle.map(ToString::to_string),
        url: format!("https://global.manga-up.com/manga/261/{id}"),
        thumbnail: Some(format!(
            "https://global-img.manga-up.com/asset/J0O/ja/chapter/{id}.webp?h=sig&e=1795618800"
        )),
        published: if early_access {
            None
        } else {
            Some("Nov 21, 2026".to_string())
        },
        early_access,
    }
}

#[test]
fn golden_json_for_multipart_early_access_folded_and_no_subtitle() {
    // 1. Multi-part early-access group (67.1 - 67.3) matching §5 spec example
    let group = ChapterGroup {
        number: Some(67),
        parts: vec![
            sample_part("130416", "Chapter 67.1", Some("67th Smoke"), true),
            sample_part("130419", "Chapter 67.2", Some("67th Smoke"), true),
            sample_part("130422", "Chapter 67.3", Some("67th Smoke"), true),
        ],
    };
    let ctx = AnnouncementContext {
        series_title: "Smoking Behind the Supermarket with You",
        site_display_name: "Manga UP!",
        mention: "@everyone",
        folded_count: 0,
        timestamp_rfc3339: "2026-11-23T12:00:00Z",
    };

    let payload = build_announcement_payload(&group, &ctx);
    let val: Value = serde_json::to_value(&payload).expect("serialize payload");

    assert_eq!(val["username"], "Yamada");
    assert_eq!(
        val["content"],
        "@everyone New chapter 67 (3 parts) of **Smoking Behind the Supermarket with You**"
    );
    assert_eq!(val["allowed_mentions"], json!({ "parse": ["everyone"] }));
    let embeds = val["embeds"].as_array().expect("embeds array");
    assert_eq!(embeds.len(), 3);
    assert_eq!(embeds[0]["title"], "Chapter 67.1 — 67th Smoke");
    assert_eq!(embeds[1]["title"], "Chapter 67.2 — 67th Smoke");
    assert_eq!(
        embeds[2],
        json!({
            "title": "Chapter 67.3 — 67th Smoke",
            "url": "https://global.manga-up.com/manga/261/130422",
            "description": "New chapter of **Smoking Behind the Supermarket with You**",
            "color": 3_447_003,
            "timestamp": "2026-11-23T12:00:00Z",
            "author": { "name": "Manga UP!" },
            "thumbnail": { "url": "https://global-img.manga-up.com/asset/J0O/ja/chapter/130422.webp?h=sig&e=1795618800" },
            "footer": { "text": "Early access — no general release date yet" }
        })
    );

    // 2. Normal dated chapter without subtitle, with folded count = 4 and mention = ""
    let single_no_sub = ChapterGroup {
        number: Some(68),
        parts: vec![ChapterRef {
            thumbnail: None,
            ..sample_part("130500", "Chapter 68", None, false)
        }],
    };
    let ctx_no_ping = AnnouncementContext {
        series_title: "Smoking Behind the Supermarket with You",
        site_display_name: "Manga UP!",
        mention: "",
        folded_count: 4,
        timestamp_rfc3339: "2026-11-23T12:00:00Z",
    };
    let payload_no_ping = build_announcement_payload(&single_no_sub, &ctx_no_ping);
    let val_no_ping: Value = serde_json::to_value(&payload_no_ping).unwrap();

    assert_eq!(
        val_no_ping["content"],
        "New chapter 68 of **Smoking Behind the Supermarket with You**"
    );
    assert_eq!(val_no_ping["allowed_mentions"], json!({ "parse": [] }));
    assert_eq!(val_no_ping["embeds"][0]["title"], "Chapter 68");
    assert!(val_no_ping["embeds"][0].get("thumbnail").is_none());
    assert_eq!(
        val_no_ping["embeds"][0]["footer"]["text"],
        "+4 earlier chapters since last check"
    );

    // 3. Both early-access AND folded -> joined with " · "
    let early_and_folded = ChapterGroup {
        number: Some(69),
        parts: vec![sample_part("130600", "Chapter 69", None, true)],
    };
    let payload_both = build_announcement_payload(&early_and_folded, &ctx_no_ping);
    let val_both: Value = serde_json::to_value(&payload_both).unwrap();
    assert_eq!(
        val_both["embeds"][0]["footer"]["text"],
        "Early access — no general release date yet · +4 earlier chapters since last check"
    );
}

#[test]
fn site_text_mentions_are_neutralized_and_markdown_escaped() {
    let malicious_group = ChapterGroup {
        number: None,
        parts: vec![sample_part(
            "999",
            "Special @here <@123456> *bonus*",
            Some("Sub @everyone"),
            false,
        )],
    };
    let ctx = AnnouncementContext {
        series_title: "Title with @here and @everyone and <@!999> *stars* _underscores_ ~tilde~ `code` |pipe| >quote",
        site_display_name: "Site @here",
        mention: "@everyone",
        folded_count: 0,
        timestamp_rfc3339: "2026-11-23T12:00:00Z",
    };

    let payload = build_announcement_payload(&malicious_group, &ctx);
    let content = payload.content.as_deref().unwrap();

    // Only the leading configured @everyone may exist; @here and <@...> must be stripped
    assert!(content.starts_with("@everyone "));
    let after_prefix = &content["@everyone ".len()..];
    assert!(!after_prefix.contains("@everyone"));
    assert!(!after_prefix.contains("@here"));
    assert!(!after_prefix.contains("<@"));

    let desc = payload.embeds[0].description.as_deref().unwrap();
    assert!(!desc.contains("@here"));
    assert!(!desc.contains("@everyone"));
    assert!(!desc.contains("<@"));
    assert!(desc.contains(r"\*stars\*"));
    assert!(desc.contains(r"\_underscores\_"));

    // Health and test payloads never ping
    let health =
        build_health_alert_payload("Manga @here", "timeout", "1h ago", "2026-11-23T12:00:00Z");
    assert!(health.content.is_none());
    assert!(health.allowed_mentions.parse.is_empty());

    let test_pl = build_test_webhook_payload("Test @everyone", "2026-11-23T12:00:00Z", true);
    assert_eq!(test_pl.username, "Sasaki");
    assert!(test_pl.content.is_none());
    assert!(test_pl.allowed_mentions.parse.is_empty());

    let escaped = escape_discord_markdown("*_~`|>\\");
    assert_eq!(escaped, r"\*\_\~\`\|\>\\");
}

#[test]
fn multibyte_char_truncation_never_panics_and_respects_char_count() {
    let cjk = "煙草とスーパーマーケット🌸".repeat(30);
    let truncated = truncate_chars(&cjk, 256);
    assert_eq!(truncated.chars().count(), 256);
    assert!(truncated.ends_with('…'));
}

#[tokio::test]
async fn discord_client_retries_short_429_and_aborts_long_429_and_classifies_4xx() {
    let server = MockServer::start().await;
    let client = DiscordClient::new("BesideME-test/0.1", Duration::from_secs(5)).unwrap();
    let payload = build_test_webhook_payload("unit test", "2026-11-23T12:00:00Z", false);
    assert_eq!(payload.username, "Yamada");

    // 1. 429 with retry_after: 0.1 -> retries once and succeeds
    let wh_retry = WebhookUrl::parse(
        "BESIDEME_WEBHOOK",
        &format!("{}/api/webhooks/111111/token_retry", server.uri()),
        true,
    )
    .unwrap();

    Mock::given(method("POST"))
        .and(path("/api/webhooks/111111/token_retry"))
        .and(query_param("wait", "true"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({ "retry_after": 0.05 })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/webhooks/111111/token_retry"))
        .and(query_param("wait", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "msg1" })))
        .expect(1)
        .mount(&server)
        .await;

    client
        .send(&wh_retry, None, &payload)
        .await
        .expect("429 short retry succeeds");

    // 2. 429 with retry_after: 120.0 -> gives up immediately without retry
    let wh_long_429 = WebhookUrl::parse(
        "BESIDEME_WEBHOOK",
        &format!("{}/api/webhooks/222222/token_long429", server.uri()),
        true,
    )
    .unwrap();

    Mock::given(method("POST"))
        .and(path("/api/webhooks/222222/token_long429"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({ "retry_after": 120.0 })))
        .expect(1)
        .mount(&server)
        .await;

    let err = client.send(&wh_long_429, None, &payload).await.unwrap_err();
    assert!(matches!(err, DiscordError::RateLimited { .. }));

    // 3. 404 -> WebhookGone
    let wh_404 = WebhookUrl::parse(
        "BESIDEME_WEBHOOK",
        &format!("{}/api/webhooks/333333/token_404", server.uri()),
        true,
    )
    .unwrap();

    Mock::given(method("POST"))
        .and(path("/api/webhooks/333333/token_404"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;

    let err = client.send(&wh_404, None, &payload).await.unwrap_err();
    assert!(matches!(err, DiscordError::WebhookGone { status: 404 }));

    // A rejected destination can echo its own credential in a response.
    let raw_400 = format!("{}/api/webhooks/444444/token_400", server.uri());
    let wh_400 = WebhookUrl::parse("BESIDEME_WEBHOOK", &raw_400, true).unwrap();

    Mock::given(method("POST"))
        .and(path("/api/webhooks/444444/token_400"))
        .respond_with(ResponseTemplate::new(400).set_body_string(format!("Rejected {raw_400}")))
        .expect(1)
        .mount(&server)
        .await;

    let err = client.send(&wh_400, None, &payload).await.unwrap_err();
    assert!(matches!(err, DiscordError::WebhookRejected { status: 400 }));
    assert!(!format!("{err} {err:?}").contains("token_400"));
}

#[tokio::test]
async fn site_http_client_enforces_ua_etag_timeout_redirects_and_5mb_size_cap() {
    let server = MockServer::start().await;
    let ua = "BesideME/0.1 (+personal use)";
    let client = SiteHttpClient::new(ua, Duration::from_millis(400)).unwrap();

    // 1. Sends User-Agent and returns ETag, then sends If-None-Match and handles 304
    Mock::given(method("GET"))
        .and(path("/manga/261"))
        .and(header("user-agent", ua))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"etag-v1\"")
                .set_body_string("<html>ok</html>"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/manga/261"))
        .and(header("if-none-match", "\"etag-v1\""))
        .respond_with(ResponseTemplate::new(304).insert_header("ETag", "\"etag-v1\""))
        .expect(1)
        .mount(&server)
        .await;

    let url = Url::parse(&format!("{}/manga/261", server.uri())).unwrap();
    let first = client.fetch(&url, None).await.expect("200 fetch");
    assert_eq!(
        first,
        FetchOutcome::Fetched {
            body: "<html>ok</html>".to_string(),
            etag: Some("\"etag-v1\"".to_string()),
        }
    );

    let second = client
        .fetch(&url, Some("\"etag-v1\""))
        .await
        .expect("304 fetch");
    assert_eq!(
        second,
        FetchOutcome::NotModified {
            etag: Some("\"etag-v1\"".to_string()),
        }
    );

    // 2. Timeout enforcement
    Mock::given(method("GET"))
        .and(path("/manga/hang"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
        .mount(&server)
        .await;

    let hang_url = Url::parse(&format!("{}/manga/hang", server.uri())).unwrap();
    let err = client.fetch(&hang_url, None).await.unwrap_err();
    assert_eq!(err, HttpError::Timeout);

    // 3. Max 2 redirects: 3 redirects -> TooManyRedirects
    Mock::given(method("GET"))
        .and(path("/r1"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "/r2"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/r2"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "/r3"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/r3"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "/r4"))
        .mount(&server)
        .await;

    let r1_url = Url::parse(&format!("{}/r1", server.uri())).unwrap();
    let err = client.fetch(&r1_url, None).await.unwrap_err();
    assert_eq!(err, HttpError::TooManyRedirects);

    // 4. 5 MiB response size cap
    let oversized_body = vec![b'a'; MAX_RESPONSE_BYTES + 1024];
    Mock::given(method("GET"))
        .and(path("/manga/huge"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(oversized_body))
        .mount(&server)
        .await;

    let huge_url = Url::parse(&format!("{}/manga/huge", server.uri())).unwrap();
    let err = client.fetch(&huge_url, None).await.unwrap_err();
    assert_eq!(
        err,
        HttpError::ResponseTooLarge {
            limit: MAX_RESPONSE_BYTES
        }
    );

    // 5. Non-loopback http:// rejected
    let insecure_url = Url::parse("http://global.manga-up.com/manga/261").unwrap();
    let err = client.fetch(&insecure_url, None).await.unwrap_err();
    assert!(matches!(err, HttpError::InsecureScheme(_)));
}
