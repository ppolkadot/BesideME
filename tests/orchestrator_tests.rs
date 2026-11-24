//! End-to-end orchestrator tests (`run_check`, health alerts, partial commit recovery,
//! `--dry-run`, instance locking, backoff scheduling, and `#[ignore]` live contract test).

use std::time::Duration;

use reqwest::Url;
use serde_json::{Value, json};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use besideme::config::{Config, DEFAULT_WEBHOOK_ENV, HEALTH_WEBHOOK_ENV, LoadOptions};
use besideme::http::{FetchOutcome, SiteHttpClient};
use besideme::sites::{MangaUp, Registry, Site};
use besideme::state::{InstanceLock, StateFile};
use besideme::watcher::{
    CheckOptions, ExitCode, FailureKind, compute_next_delay, run_check, run_reset,
};

const FIXTURE_BASE: &str = include_str!("fixtures/manga261_base.html");
const FIXTURE_NEW: &str = include_str!("fixtures/manga261_new.html");

fn build_mock_registry(site_server: &MockServer) -> Registry {
    let base_url = Url::parse(&site_server.uri()).unwrap();
    let img_url = Url::parse("https://global-img.manga-up.com").unwrap();
    let mut reg = Registry::empty();
    reg.register(Box::new(MangaUp::new(base_url, img_url)));
    reg
}

fn build_test_config(
    site_server: &MockServer,
    announce_webhook_url: &str,
    health_webhook_url: Option<&str>,
    alert_after_failures: u32,
    registry: &Registry,
) -> Config {
    let toml = format!(
        r#"
        interval_minutes = 30
        timeout_seconds = 5
        max_messages_per_cycle = 3
        alert_after_failures = {alert_after_failures}

        [[series]]
        url = "{}/manga/261"
        "#,
        site_server.uri()
    );

    Config::from_toml_str(
        &toml,
        &LoadOptions {
            allow_any_webhook: true,
            ..LoadOptions::default()
        },
        registry,
        |key| match key {
            DEFAULT_WEBHOOK_ENV => Some(announce_webhook_url.to_string()),
            HEALTH_WEBHOOK_ENV => health_webhook_url.map(ToString::to_string),
            _ => None,
        },
    )
    .expect("build test config")
}

#[tokio::test]
async fn orchestrator_baseline_then_no_change_then_announce_new_chapter() {
    let site_server = MockServer::start().await;
    let webhook_server = MockServer::start().await;
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path().join("state.json");

    let registry = build_mock_registry(&site_server);
    let announce_wh = format!("{}/api/webhooks/100/announce_tok", webhook_server.uri());
    let config = build_test_config(&site_server, &announce_wh, None, 6, &registry);

    // Mount base fixture initially
    let base_mock = Mock::given(method("GET"))
        .and(path("/manga/261"))
        .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE_BASE))
        .mount_as_scoped(&site_server)
        .await;

    // 1. First run_check -> establishes baseline silently (0 webhook POSTs), writes state
    let summary1 = run_check(&config, &registry, &state_path, CheckOptions::default()).await;
    assert_eq!(summary1.exit_code, ExitCode::Ok);
    assert_eq!(summary1.reports[0].outcome, "baseline");
    assert!(state_path.is_file());
    assert_eq!(webhook_server.received_requests().await.unwrap().len(), 0);

    // 2. Second run_check -> NoChange (0 webhook POSTs)
    let summary2 = run_check(&config, &registry, &state_path, CheckOptions::default()).await;
    assert_eq!(summary2.exit_code, ExitCode::Ok);
    assert_eq!(summary2.reports[0].outcome, "no_change");
    assert_eq!(webhook_server.received_requests().await.unwrap().len(), 0);

    // 3. Swap to new fixture -> 1 webhook POST, exit code 10 (NewsAnnounced)
    drop(base_mock);
    Mock::given(method("GET"))
        .and(path("/manga/261"))
        .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE_NEW))
        .mount(&site_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/webhooks/100/announce_tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "1"})))
        .expect(1)
        .mount(&webhook_server)
        .await;

    let summary3 = run_check(&config, &registry, &state_path, CheckOptions::default()).await;
    assert_eq!(summary3.exit_code, ExitCode::NewsAnnounced);
    assert_eq!(summary3.reports[0].outcome, "announced");
    assert_eq!(summary3.reports[0].announced_groups, 1);

    let reqs = webhook_server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    let body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(body["embeds"][0]["title"], "Chapter 67.3 — 67th Smoke");

    // 4. Subsequent run_check -> NoChange (never notifies twice)
    let summary4 = run_check(&config, &registry, &state_path, CheckOptions::default()).await;
    assert_eq!(summary4.exit_code, ExitCode::Ok);
    assert_eq!(summary4.reports[0].outcome, "no_change");
}

#[tokio::test]
async fn orchestrator_webhook_500_leaves_state_uncommitted_and_partial_multi_group_failure_resends_only_unsent()
 {
    let site_server = MockServer::start().await;
    let webhook_server = MockServer::start().await;
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path().join("state.json");

    let registry = build_mock_registry(&site_server);
    let announce_wh = format!("{}/api/webhooks/100/announce_tok", webhook_server.uri());
    let config = build_test_config(&site_server, &announce_wh, None, 6, &registry);

    // Baseline first
    let base_mock = Mock::given(method("GET"))
        .and(path("/manga/261"))
        .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE_BASE))
        .mount_as_scoped(&site_server)
        .await;
    run_check(&config, &registry, &state_path, CheckOptions::default()).await;
    drop(base_mock);

    // Serve 3 new chapter groups (68, 69, 70) on top of base fixture
    let three_groups_html = r#"<script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{"titleId":261,"error":null,"data":{"titleName":"Smoking Behind the Supermarket with You","chapters":[{"id":2003,"mainName":"Chapter 70","published":"Dec 3"},{"id":2002,"mainName":"Chapter 69","published":"Dec 2"},{"id":2001,"mainName":"Chapter 68","published":"Dec 1"},{"id":130419,"mainName":"Chapter 67.2"},{"id":130416,"mainName":"Chapter 67.1"},{"id":130413,"mainName":"Chapter 66.2","published":"Nov 21, 2026"},{"id":130410,"mainName":"Chapter 66.1","published":"Oct 25, 2026"},{"id":28665,"mainName":"Chapter 1","published":"Apr 04, 2023"}]}}}}</script>"#;

    Mock::given(method("GET"))
        .and(path("/manga/261"))
        .respond_with(ResponseTemplate::new(200).set_body_string(three_groups_html))
        .mount(&site_server)
        .await;

    // 1st POST succeeds (Chapter 68), 2nd POST fails with 500 (Chapter 69)
    let first_ok = Mock::given(method("POST"))
        .and(path("/api/webhooks/100/announce_tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "1"})))
        .up_to_n_times(1)
        .mount_as_scoped(&webhook_server)
        .await;
    let second_500 = Mock::given(method("POST"))
        .and(path("/api/webhooks/100/announce_tok"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .mount_as_scoped(&webhook_server)
        .await;

    let summary_fail = run_check(&config, &registry, &state_path, CheckOptions::default()).await;
    assert_eq!(summary_fail.exit_code, ExitCode::WebhookError);
    assert_eq!(summary_fail.reports[0].announced_groups, 1);

    // Verify Chapter 68 ("2001") was immediately committed to state.json, while 69 ("2002") and 70 ("2003") were not!
    let saved_state = StateFile::load(&state_path).unwrap();
    let series_state = &saved_state.series["mangaup:261"];
    assert!(series_state.announced_ids.contains(&"2001".to_string()));
    assert!(!series_state.announced_ids.contains(&"2002".to_string()));
    assert!(!series_state.announced_ids.contains(&"2003".to_string()));

    drop(first_ok);
    drop(second_500);
    webhook_server.reset().await;

    // Next run: webhook recovers (200 OK) -> only 2nd and 3rd groups (Chapters 69 and 70) are sent!
    Mock::given(method("POST"))
        .and(path("/api/webhooks/100/announce_tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "ok"})))
        .expect(2)
        .mount(&webhook_server)
        .await;

    let summary_recover = run_check(&config, &registry, &state_path, CheckOptions::default()).await;
    assert_eq!(summary_recover.exit_code, ExitCode::NewsAnnounced);
    assert_eq!(summary_recover.reports[0].announced_groups, 2);

    let reqs = webhook_server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2);
    let post1: Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let post2: Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert_eq!(post1["embeds"][0]["title"], "Chapter 69");
    assert_eq!(post2["embeds"][0]["title"], "Chapter 70");
}

#[tokio::test]
async fn orchestrator_failed_post_is_retried_even_when_site_answers_304_for_stored_etag() {
    use wiremock::matchers::header;

    let site_server = MockServer::start().await;
    let webhook_server = MockServer::start().await;
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path().join("state.json");

    let registry = build_mock_registry(&site_server);
    let announce_wh = format!("{}/api/webhooks/100/announce_tok", webhook_server.uri());
    let config = build_test_config(&site_server, &announce_wh, None, 6, &registry);

    // Baseline with ETag "base".
    let base_mock = Mock::given(method("GET"))
        .and(path("/manga/261"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"base\"")
                .set_body_string(FIXTURE_BASE),
        )
        .mount_as_scoped(&site_server)
        .await;
    run_check(&config, &registry, &state_path, CheckOptions::default()).await;
    drop(base_mock);

    // New chapter page with ETag "new"; the site answers 304 when that ETag is sent back.
    Mock::given(method("GET"))
        .and(path("/manga/261"))
        .and(header("If-None-Match", "\"new\""))
        .respond_with(ResponseTemplate::new(304).insert_header("ETag", "\"new\""))
        .with_priority(1)
        .mount(&site_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/manga/261"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"new\"")
                .set_body_string(FIXTURE_NEW),
        )
        .with_priority(5)
        .mount(&site_server)
        .await;

    // Webhook fails on the first attempt.
    let fail = Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount_as_scoped(&webhook_server)
        .await;
    let s1 = run_check(&config, &registry, &state_path, CheckOptions::default()).await;
    assert_eq!(s1.exit_code, ExitCode::WebhookError);
    drop(fail);
    webhook_server.reset().await;

    // Webhook recovers: the chapter must still be announced (no 304 short-circuit).
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "1"})))
        .expect(1)
        .mount(&webhook_server)
        .await;
    let s2 = run_check(&config, &registry, &state_path, CheckOptions::default()).await;
    assert_eq!(s2.exit_code, ExitCode::NewsAnnounced);
    assert_eq!(s2.reports[0].announced_groups, 1);

    // After a successful delivery the ETag is kept, so the next check is a cheap 304.
    let s3 = run_check(&config, &registry, &state_path, CheckOptions::default()).await;
    assert_eq!(s3.exit_code, ExitCode::Ok);
    assert_eq!(s3.reports[0].outcome, "no_change");
}

#[tokio::test]
async fn orchestrator_health_alerts_go_only_to_health_webhook_and_send_recovery() {
    let site_server = MockServer::start().await;
    let announce_server = MockServer::start().await;
    let health_server = MockServer::start().await;
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path().join("state.json");

    let registry = build_mock_registry(&site_server);
    let announce_wh = format!("{}/api/webhooks/100/announce_tok", announce_server.uri());
    let health_wh = format!("{}/api/webhooks/200/health_tok", health_server.uri());
    let config = build_test_config(&site_server, &announce_wh, Some(&health_wh), 3, &registry);

    // Site returns garbage HTML 4 times (alert_after_failures = 3)
    let bad_mock = Mock::given(method("GET"))
        .and(path("/manga/261"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>broken page</html>"))
        .mount_as_scoped(&site_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/webhooks/200/health_tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "h1"})))
        .mount(&health_server)
        .await;

    for i in 1..=4 {
        let sum = run_check(&config, &registry, &state_path, CheckOptions::default()).await;
        assert_eq!(sum.exit_code, ExitCode::ParseError);
        let health_reqs = health_server.received_requests().await.unwrap();
        if i < 3 {
            assert_eq!(health_reqs.len(), 0, "no health alert before threshold");
        } else {
            assert_eq!(
                health_reqs.len(),
                1,
                "exactly one health alert sent on and after threshold"
            );
        }
    }

    // Announcement webhook received 0 POSTs!
    assert_eq!(
        announce_server.received_requests().await.unwrap().len(),
        0,
        "health alerts must never post to the announcement webhook"
    );

    // Now site recovers -> 1 recovery POST sent to health webhook
    drop(bad_mock);
    Mock::given(method("GET"))
        .and(path("/manga/261"))
        .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE_BASE))
        .mount(&site_server)
        .await;

    let sum_rec = run_check(&config, &registry, &state_path, CheckOptions::default()).await;
    assert_eq!(sum_rec.exit_code, ExitCode::Ok);

    let health_reqs = health_server.received_requests().await.unwrap();
    assert_eq!(health_reqs.len(), 2);
    let rec_body: Value = serde_json::from_slice(&health_reqs[1].body).unwrap();
    assert!(
        rec_body["embeds"][0]["title"]
            .as_str()
            .unwrap()
            .contains("recovered")
    );
}

#[tokio::test]
async fn orchestrator_dry_run_never_creates_state_and_held_lock_returns_exit_5() {
    let site_server = MockServer::start().await;
    let webhook_server = MockServer::start().await;
    let tmp = TempDir::new().unwrap();
    let state_path = tmp.path().join("state.json");

    let registry = build_mock_registry(&site_server);
    let announce_wh = format!("{}/api/webhooks/100/announce_tok", webhook_server.uri());
    let config = build_test_config(&site_server, &announce_wh, None, 6, &registry);

    Mock::given(method("GET"))
        .and(path("/manga/261"))
        .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE_BASE))
        .mount(&site_server)
        .await;

    // 1. --dry-run -> no state.json created, 0 POSTs
    let dry_summary = run_check(
        &config,
        &registry,
        &state_path,
        CheckOptions {
            dry_run: true,
            ..CheckOptions::default()
        },
    )
    .await;
    assert_eq!(dry_summary.exit_code, ExitCode::Ok);
    assert_eq!(dry_summary.reports[0].outcome, "baseline");
    assert!(
        !state_path.exists(),
        "--dry-run must never write state.json"
    );

    // 2. Held lock -> exit code 5 (AnotherInstanceRunning)
    let held_lock = InstanceLock::acquire_for_state(&state_path).expect("acquire lock");
    let locked_summary = run_check(&config, &registry, &state_path, CheckOptions::default()).await;
    assert_eq!(locked_summary.exit_code, ExitCode::AnotherInstanceRunning);
    assert_eq!(locked_summary.exit_code.as_i32(), 5);
    drop(held_lock);

    // 3. Normal run then `reset` command removes state entry
    run_check(&config, &registry, &state_path, CheckOptions::default()).await;
    assert!(
        StateFile::load(&state_path)
            .unwrap()
            .series
            .contains_key("mangaup:261")
    );
    let reset_code = run_reset(&state_path, &registry, Some("mangaup:261"), false);
    assert_eq!(reset_code, ExitCode::Ok);
    assert!(
        !StateFile::load(&state_path)
            .unwrap()
            .series
            .contains_key("mangaup:261")
    );
}

#[test]
fn backoff_schedule_and_exit_code_priority_follow_spec() {
    let base = Duration::from_secs(30 * 60);

    // Transient network: 30s, 90s, 270s, then normal interval
    assert_eq!(
        compute_next_delay(base, 0, Some(FailureKind::TransientNetwork { streak: 1 })),
        Duration::from_secs(30)
    );
    assert_eq!(
        compute_next_delay(base, 0, Some(FailureKind::TransientNetwork { streak: 2 })),
        Duration::from_secs(90)
    );
    assert_eq!(
        compute_next_delay(base, 0, Some(FailureKind::TransientNetwork { streak: 3 })),
        Duration::from_secs(270)
    );
    assert_eq!(
        compute_next_delay(base, 0, Some(FailureKind::TransientNetwork { streak: 4 })),
        base
    );

    // 403/429 block: max(Retry-After, interval), doubling per consecutive block up to 6h
    assert_eq!(
        compute_next_delay(
            base,
            0,
            Some(FailureKind::BlockedOrRateLimited {
                streak: 1,
                retry_after: Some(Duration::from_secs(3600)),
            })
        ),
        Duration::from_secs(3600)
    );
    assert_eq!(
        compute_next_delay(
            base,
            0,
            Some(FailureKind::BlockedOrRateLimited {
                streak: 2,
                retry_after: None,
            })
        ),
        Duration::from_secs(60 * 60)
    );
    assert_eq!(
        compute_next_delay(
            base,
            0,
            Some(FailureKind::BlockedOrRateLimited {
                streak: 20,
                retry_after: None,
            })
        ),
        Duration::from_secs(6 * 60 * 60)
    );

    // Exit code priority: 1 > 5 > 4 > 2 > 3 > 10 > 0
    assert_eq!(
        ExitCode::Ok.combine(ExitCode::NewsAnnounced),
        ExitCode::NewsAnnounced
    );
    assert_eq!(
        ExitCode::NewsAnnounced.combine(ExitCode::NetworkError),
        ExitCode::NetworkError
    );
    assert_eq!(
        ExitCode::NetworkError.combine(ExitCode::ParseError),
        ExitCode::ParseError
    );
    assert_eq!(
        ExitCode::ParseError.combine(ExitCode::WebhookError),
        ExitCode::WebhookError
    );
    assert_eq!(
        ExitCode::WebhookError.combine(ExitCode::AnotherInstanceRunning),
        ExitCode::AnotherInstanceRunning
    );
    assert_eq!(
        ExitCode::AnotherInstanceRunning.combine(ExitCode::ConfigOrStateError),
        ExitCode::ConfigOrStateError
    );
}

/// Live contract test against `https://global.manga-up.com/manga/261` (§8.7).
///
/// Ignored during normal `cargo test` runs so `cargo test` makes zero network calls.
/// Run manually with: `cargo test -- --ignored`
#[tokio::test]
#[ignore = "hits live https://global.manga-up.com/manga/261; run with `cargo test -- --ignored`"]
async fn live_manga_up_261_contract() {
    let site = MangaUp::default();
    let url = site.series_url("261");
    let client = SiteHttpClient::new("BesideME/0.1 (+personal use)", Duration::from_secs(20))
        .expect("build HTTP client");

    let outcome = client
        .fetch(&url, None)
        .await
        .expect("fetch live /manga/261");
    let FetchOutcome::Fetched { body, .. } = outcome else {
        panic!("expected Fetched body from unconditional GET");
    };

    let snap = site.parse("261", &body).expect("parse live /manga/261");
    assert_eq!(snap.title, "Smoking Behind the Supermarket with You");
    assert!(
        snap.chapters_newest_first.len() >= 100,
        "expected >= 100 chapters, found {}",
        snap.chapters_newest_first.len()
    );

    let head = &snap.chapters_newest_first[0];
    assert_ne!(head.id, "");
    assert!(head.label.starts_with("Chapter "));
    assert!(
        head.url
            .starts_with("https://global.manga-up.com/manga/261/")
    );
    assert!(
        head.thumbnail
            .as_deref()
            .unwrap_or_default()
            .starts_with("https://global-img.manga-up.com/")
    );
}

/// Live check of the GUI lookups (search catalog + link preview). Run with `--ignored`.
#[tokio::test]
#[ignore = "hits the real Manga UP! site"]
async fn live_gui_lookups_catalog_and_preview() {
    use besideme::lookup::{load_catalog, preview_series};
    use besideme::sites::search_catalog;
    let reg = Registry::with_defaults();
    let cat = load_catalog(&reg).await.expect("catalog");
    assert!(cat.len() > 100, "catalog unexpectedly small: {}", cat.len());
    let hits = search_catalog(&cat, "smoking behind", 5);
    assert_eq!(hits[0].key, "261");

    let p = preview_series(&reg, "https://global.manga-up.com/manga/261/?ref=x")
        .await
        .expect("preview");
    assert_eq!(p.url, "https://global.manga-up.com/manga/261");
    assert_ne!(p.snapshot.chapters_newest_first.len(), 0);
    assert!(
        preview_series(&reg, "https://example.com/manga/1")
            .await
            .is_err()
    );
}
