//! Release discovery, read markers, run summaries, and single-title retry.
//!
//! Everything runs against local mock servers and temporary directories.

use std::fmt::Write as _;
use std::time::Duration;

use reqwest::Url;
use serde_json::{Value, json};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use besideme::config::{Config, DEFAULT_WEBHOOK_ENV, LoadOptions};
use besideme::model::{ChapterRef, SeriesKey, SeriesSnapshot};
use besideme::reads::{
    MAX_READ_IDS_PER_SERIES, ReadMarkers, reads_path_for_state, unread_releases,
};
use besideme::sites::{MangaUp, ParseError, Registry, Site};
use besideme::state::{
    DiscoveredRelease, InstanceLock, MAX_DISCOVERED_PER_SERIES, RunPhase, RunRecord, SeriesState,
    StateFile, sanitize_error,
};
use besideme::watcher::{
    CheckOptions, CheckProgress, ExitCode, RETRY_COOLDOWN_SECS, retry_wait_secs, run_check,
    run_check_scoped, run_watch,
};

const KEY: &str = "mangaup:261";

fn page(chapters_newest_first: &[(u64, &str)]) -> String {
    let items: Vec<String> = chapters_newest_first
        .iter()
        .map(|(id, name)| format!(r#"{{"id":{id},"mainName":"{name}","published":"Nov 1, 2026"}}"#))
        .collect();
    format!(
        r#"<script id="__NEXT_DATA__" type="application/json">{{"props":{{"pageProps":{{"titleId":261,"error":null,"data":{{"titleName":"Fixture Title","chapters":[{}]}}}}}}}}</script>"#,
        items.join(",")
    )
}

fn registry_for(site: &MockServer) -> Registry {
    let mut reg = Registry::empty();
    reg.register(Box::new(MangaUp::new(
        Url::parse(&site.uri()).unwrap(),
        Url::parse("https://global-img.manga-up.com").unwrap(),
    )));
    reg
}

fn config_for(site: &MockServer, webhook: &str, series: &[u32], registry: &Registry) -> Config {
    let entries = series.iter().fold(String::new(), |mut acc, n| {
        let _ = writeln!(acc, "[[series]]\nurl = \"{}/manga/{n}\"", site.uri());
        acc
    });
    Config::from_toml_str(
        &format!("timeout_seconds = 5\nalert_after_failures = 0\n{entries}"),
        &LoadOptions {
            allow_any_webhook: true,
            ..LoadOptions::default()
        },
        registry,
        |key| (key == DEFAULT_WEBHOOK_ENV).then(|| webhook.to_owned()),
    )
    .unwrap()
}

async fn serve_page(site: &MockServer, series: u32, body: String) {
    site.reset().await;
    Mock::given(method("GET"))
        .and(path(format!("/manga/{series}")))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(site)
        .await;
}

async fn webhook_ok(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/webhooks/100/tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "1"})))
        .mount(server)
        .await;
}

struct Fixture {
    site: MockServer,
    webhook: MockServer,
    dir: TempDir,
    registry: Registry,
    config: Config,
}

impl Fixture {
    async fn new(series: &[u32]) -> Self {
        let site = MockServer::start().await;
        let webhook = MockServer::start().await;
        let registry = registry_for(&site);
        let url = format!("{}/api/webhooks/100/tok", webhook.uri());
        let config = config_for(&site, &url, series, &registry);
        Self {
            site,
            webhook,
            dir: TempDir::new().unwrap(),
            registry,
            config,
        }
    }

    fn state_path(&self) -> std::path::PathBuf {
        self.dir.path().join("state.json")
    }

    async fn check(&self) -> besideme::watcher::CheckRunSummary {
        run_check(
            &self.config,
            &self.registry,
            &self.state_path(),
            CheckOptions::default(),
        )
        .await
    }

    fn state(&self) -> StateFile {
        StateFile::load(&self.state_path()).unwrap()
    }
}

fn release(id: &str, parts: &[&str], at: i64) -> DiscoveredRelease {
    DiscoveredRelease {
        id: id.to_owned(),
        label: format!("Chapter {id}"),
        subtitle: None,
        url: format!("https://global.manga-up.com/manga/261/{id}"),
        part_ids: parts.iter().map(|p| (*p).to_owned()).collect(),
        discovered_unix: at,
    }
}

// ---------------------------------------------------------------- state upgrade

#[test]
fn state_files_from_earlier_versions_load_without_unread_releases() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("state.json");
    // Exactly the shape the previous release wrote.
    std::fs::write(
        &path,
        r#"{
  "version": 2,
  "series": {
    "mangaup:261": {
      "announced_ids": ["1", "2"],
      "last_announced_label": "Chapter 2",
      "last_snapshot_len": 2,
      "last_checked_unix": 1795618800,
      "last_success_unix": 1795618800,
      "consecutive_failures": 0,
      "alert_sent": false,
      "etag": null
    }
  }
}"#,
    )
    .unwrap();
    let state = StateFile::load(&path).unwrap();
    let series = &state.series[KEY];
    assert_eq!(series.discovered.len(), 0);
    assert!(series.last_error.is_none());
    assert!(state.last_run.is_none());
    let markers = ReadMarkers::default();
    assert_eq!(unread_releases(KEY, series, &markers).len(), 0);

    // Saving leaves the new optional fields out, so older readers still understand the file.
    state.save_atomic(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("discovered"));
    assert!(!text.contains("last_run"));
    assert!(!text.contains("last_error"));
}

#[tokio::test]
async fn upgrading_an_existing_install_and_baselining_a_new_series_create_no_unread() {
    let fx = Fixture::new(&[261]).await;
    serve_page(&fx.site, 261, page(&[(2, "Chapter 2"), (1, "Chapter 1")])).await;

    // Legacy state, as written by the previous version.
    std::fs::write(
        fx.state_path(),
        r#"{"version":2,"series":{"mangaup:261":{"announced_ids":["1","2"],"last_announced_label":"Chapter 2","last_snapshot_len":2,"last_checked_unix":1,"last_success_unix":1,"consecutive_failures":0,"alert_sent":false,"etag":null}}}"#,
    )
    .unwrap();
    let summary = fx.check().await;
    assert_eq!(summary.reports[0].outcome, "no_change");
    assert_eq!(fx.state().series[KEY].discovered.len(), 0);

    // A brand-new series is baselined silently, never unread.
    std::fs::remove_file(fx.state_path()).unwrap();
    let summary = fx.check().await;
    assert_eq!(summary.reports[0].outcome, "baseline");
    assert_eq!(fx.state().series[KEY].discovered.len(), 0);
    assert_eq!(fx.webhook.received_requests().await.unwrap().len(), 0);
}

// ---------------------------------------------------------------- discovery

#[tokio::test]
async fn a_discovered_group_is_recorded_once_with_an_official_link_across_restarts() {
    let fx = Fixture::new(&[261]).await;
    webhook_ok(&fx.webhook).await;
    serve_page(&fx.site, 261, page(&[(2, "Chapter 2"), (1, "Chapter 1")])).await;
    fx.check().await; // baseline

    serve_page(
        &fx.site,
        261,
        page(&[
            (4, "Chapter 3.2"),
            (3, "Chapter 3.1"),
            (2, "Chapter 2"),
            (1, "Chapter 1"),
        ]),
    )
    .await;
    let summary = fx.check().await;
    assert_eq!(summary.exit_code, ExitCode::NewsAnnounced);

    let state = fx.state();
    let discovered = &state.series[KEY].discovered;
    assert_eq!(discovered.len(), 1, "one split group, one release");
    assert_eq!(discovered[0].id, "4");
    assert_eq!(discovered[0].label, "Chapter 3.2");
    assert_eq!(discovered[0].part_ids, ["3", "4"]);
    assert_eq!(discovered[0].url, format!("{}/manga/261/4", fx.site.uri()));

    // Later runs, a reload ("restart") and a reset-free re-run never add it again.
    for _ in 0..2 {
        assert_eq!(fx.check().await.reports[0].outcome, "no_change");
    }
    assert_eq!(fx.state().series[KEY].discovered, *discovered);
}

#[tokio::test]
async fn dry_runs_record_nothing() {
    let fx = Fixture::new(&[261]).await;
    webhook_ok(&fx.webhook).await;
    serve_page(&fx.site, 261, page(&[(1, "Chapter 1")])).await;
    fx.check().await;
    let before = std::fs::read(fx.state_path()).unwrap();

    serve_page(&fx.site, 261, page(&[(2, "Chapter 2"), (1, "Chapter 1")])).await;
    for _ in 0..2 {
        let summary = run_check(
            &fx.config,
            &fx.registry,
            &fx.state_path(),
            CheckOptions {
                dry_run: true,
                ..CheckOptions::default()
            },
        )
        .await;
        assert_eq!(summary.reports[0].announced_groups, 1);
    }
    assert_eq!(std::fs::read(fx.state_path()).unwrap(), before);
}

#[tokio::test]
async fn delivery_retries_never_duplicate_a_discovered_release() {
    let fx = Fixture::new(&[261]).await;
    serve_page(&fx.site, 261, page(&[(1, "Chapter 1")])).await;
    fx.check().await;

    serve_page(&fx.site, 261, page(&[(2, "Chapter 2"), (1, "Chapter 1")])).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&fx.webhook)
        .await;
    for _ in 0..3 {
        let summary = fx.check().await;
        assert_eq!(summary.exit_code, ExitCode::WebhookError);
        let state = fx.state();
        // Shown on the shelf even though Discord is down, and exactly once.
        assert_eq!(state.series[KEY].discovered.len(), 1);
        assert!(
            state.series[KEY]
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("Discord delivery failed"))
        );
    }

    fx.webhook.reset().await;
    webhook_ok(&fx.webhook).await;
    let summary = fx.check().await;
    assert_eq!(summary.exit_code, ExitCode::NewsAnnounced);
    let state = fx.state();
    assert_eq!(state.series[KEY].discovered.len(), 1);
    assert!(state.series[KEY].last_error.is_none());
}

#[tokio::test]
async fn a_group_that_grows_while_undelivered_replaces_its_earlier_record() {
    let fx = Fixture::new(&[261]).await;
    serve_page(&fx.site, 261, page(&[(1, "Chapter 1")])).await;
    fx.check().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&fx.webhook)
        .await;

    serve_page(&fx.site, 261, page(&[(3, "Chapter 2.1"), (1, "Chapter 1")])).await;
    fx.check().await;
    serve_page(
        &fx.site,
        261,
        page(&[(4, "Chapter 2.2"), (3, "Chapter 2.1"), (1, "Chapter 1")]),
    )
    .await;
    fx.check().await;

    let state = fx.state();
    let discovered = &state.series[KEY].discovered;
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].id, "4");
    assert_eq!(discovered[0].part_ids, ["3", "4"]);
}

#[tokio::test]
async fn announce_on_first_run_is_recorded_only_after_discord_accepts_it() {
    let fx = Fixture::new(&[261]).await;
    serve_page(&fx.site, 261, page(&[(2, "Chapter 2"), (1, "Chapter 1")])).await;
    let options = CheckOptions {
        announce_on_first_run: true,
        ..CheckOptions::default()
    };

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&fx.webhook)
        .await;
    run_check(&fx.config, &fx.registry, &fx.state_path(), options).await;
    assert!(
        fx.state()
            .series
            .get(KEY)
            .is_none_or(|s| s.discovered.is_empty()),
        "an undelivered first announcement is not unread"
    );

    fx.webhook.reset().await;
    webhook_ok(&fx.webhook).await;
    run_check(&fx.config, &fx.registry, &fx.state_path(), options).await;
    let state = fx.state();
    assert_eq!(state.series[KEY].discovered.len(), 1);
    assert_eq!(state.series[KEY].discovered[0].id, "2");
}

/// A provider whose chapter links may point off-site.
struct OffsiteSite {
    base: Url,
}

impl Site for OffsiteSite {
    fn id(&self) -> &'static str {
        "fake"
    }
    fn display_name(&self) -> &'static str {
        "Fake"
    }
    fn series_key(&self, url: &Url) -> Option<String> {
        let rest = url.path().strip_prefix("/series/")?;
        (url.origin() == self.base.origin() && !rest.is_empty()).then(|| rest.to_owned())
    }
    fn series_url(&self, key: &str) -> Url {
        self.base.join(&format!("/series/{key}")).unwrap()
    }
    fn is_official_url(&self, url: &Url) -> bool {
        url.origin() == self.base.origin()
    }
    fn parse(&self, _key: &str, body: &str) -> Result<SeriesSnapshot, ParseError> {
        let v: Value =
            serde_json::from_str(body).map_err(|e| ParseError::InvalidJson(e.to_string()))?;
        let chapters = v["chapters"]
            .as_array()
            .ok_or_else(|| ParseError::SchemaMismatch("chapters".into()))?
            .iter()
            .map(|c| ChapterRef {
                id: c["id"].as_str().unwrap_or_default().to_owned(),
                label: c["label"].as_str().unwrap_or_default().to_owned(),
                subtitle: None,
                url: c["url"].as_str().unwrap_or_default().to_owned(),
                thumbnail: None,
                published: Some("today".into()),
                early_access: false,
            })
            .collect();
        Ok(SeriesSnapshot {
            title: "Off-site".into(),
            chapters_newest_first: chapters,
        })
    }
}

#[tokio::test]
async fn links_that_are_not_official_are_replaced_by_the_provider_series_page() {
    let site = MockServer::start().await;
    let webhook = MockServer::start().await;
    webhook_ok(&webhook).await;
    let mut registry = Registry::empty();
    registry.register(Box::new(OffsiteSite {
        base: Url::parse(&site.uri()).unwrap(),
    }));
    let config = Config::from_toml_str(
        &format!(
            "timeout_seconds = 5\n[[series]]\nurl = \"{}/series/abc\"\n",
            site.uri()
        ),
        &LoadOptions {
            allow_any_webhook: true,
            ..LoadOptions::default()
        },
        &registry,
        |key| {
            (key == DEFAULT_WEBHOOK_ENV).then(|| format!("{}/api/webhooks/100/tok", webhook.uri()))
        },
    )
    .unwrap();
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("state.json");

    let serve = |chapters: Value| {
        let site = &site;
        async move {
            site.reset().await;
            Mock::given(method("GET"))
                .and(path("/series/abc"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({"chapters": chapters})),
                )
                .mount(site)
                .await;
        }
    };
    serve(json!([{"id":"1","label":"Chapter 1","url":format!("{}/series/abc/1", site.uri())}]))
        .await;
    run_check(&config, &registry, &state_path, CheckOptions::default()).await;
    serve(json!([
        {"id":"3","label":"Chapter 3","url":"https://evil.example/phish"},
        {"id":"2","label":"Chapter 2","url":format!("{}/series/abc/2", site.uri())},
        {"id":"1","label":"Chapter 1","url":format!("{}/series/abc/1", site.uri())},
    ]))
    .await;
    run_check(&config, &registry, &state_path, CheckOptions::default()).await;

    let state = StateFile::load(&state_path).unwrap();
    let urls: Vec<&str> = state.series["fake:abc"]
        .discovered
        .iter()
        .map(|r| r.url.as_str())
        .collect();
    let series_page = format!("{}/series/abc", site.uri());
    let official_chapter = format!("{}/series/abc/2", site.uri());
    // Chapters 2 and 3 are separate groups; the off-site link fell back to the series page.
    assert_eq!(urls, [official_chapter.as_str(), series_page.as_str()]);
}

// ---------------------------------------------------------------- state helpers

#[test]
fn discovered_list_is_bounded_idempotent_and_merges_overlaps() {
    let mut s = SeriesState::default();
    assert!(s.record_discovered([release("2", &["1", "2"], 100)]));
    assert!(
        !s.record_discovered([release("2", &["1", "2"], 999)]),
        "same group again"
    );
    assert!(
        !s.record_discovered([release("1", &["1"], 999)]),
        "subset of a known group"
    );
    assert_eq!(s.discovered[0].discovered_unix, 100);

    // A longer group replaces the shorter one and keeps the earliest detection time.
    assert!(s.record_discovered([release("3", &["1", "2", "3"], 200)]));
    assert_eq!(s.discovered.len(), 1);
    assert_eq!(s.discovered[0].id, "3");
    assert_eq!(s.discovered[0].part_ids, ["1", "2", "3"]);
    assert_eq!(s.discovered[0].discovered_unix, 100);

    let many = (0..40).map(|i| release(&format!("x{i}"), &[&format!("x{i}")], i64::from(i)));
    s.record_discovered(many);
    assert_eq!(s.discovered.len(), MAX_DISCOVERED_PER_SERIES);
    assert_eq!(s.discovered.last().unwrap().id, "x39");
}

#[test]
fn persisted_errors_never_contain_urls_and_stay_short() {
    let cleaned =
        sanitize_error("request to https://discord.com/api/webhooks/1/secret failed\n\tbadly");
    assert!(!cleaned.contains("secret"));
    assert!(!cleaned.contains("discord.com"));
    assert_eq!(cleaned, "request to <link> failed badly");
    assert!(sanitize_error(&"x".repeat(1000)).chars().count() <= 200);
}

#[test]
fn run_records_report_running_finished_and_interrupted() {
    let running = RunRecord {
        started_unix: 1_000,
        ..RunRecord::default()
    };
    assert_eq!(running.phase(1_060), RunPhase::Running);
    assert_eq!(running.phase(1_000 + 16 * 60), RunPhase::Interrupted);
    let done = RunRecord {
        started_unix: 1_000,
        finished_unix: Some(1_010),
        ..RunRecord::default()
    };
    assert_eq!(done.phase(99_999), RunPhase::Finished);
    assert_eq!(done.last_attempt_unix(), 1_010);
}

// ---------------------------------------------------------------- read markers

#[test]
fn read_markers_live_in_their_own_file_and_do_not_touch_checker_state() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("state.json");
    let reads_path = reads_path_for_state(&state_path);
    assert_eq!(reads_path, dir.path().join("reads.json"));

    let mut state = StateFile::default();
    let mut series = SeriesState {
        announced_ids: vec!["1".into()],
        ..SeriesState::default()
    };
    series.record_discovered([release("2", &["2"], 1), release("3", &["3"], 2)]);
    state.series.insert(KEY.into(), series);
    state.save_atomic(&state_path).unwrap();
    let state_bytes = std::fs::read(&state_path).unwrap();

    // The checker holds its instance lock for a whole run; marking read must not need it.
    let _checker = InstanceLock::acquire_for_state(&state_path).unwrap();
    ReadMarkers::update(&reads_path, |m| m.mark_read(KEY, ["2"])).unwrap();

    assert_eq!(std::fs::read(&state_path).unwrap(), state_bytes);
    let markers = ReadMarkers::load(&reads_path);
    let loaded = StateFile::load(&state_path).unwrap();
    let unread = unread_releases(KEY, &loaded.series[KEY], &markers);
    assert_eq!(unread.len(), 1);
    assert_eq!(unread[0].id, "3");
}

#[test]
fn a_scheduled_check_and_a_mark_read_can_interleave_without_losing_either() {
    let dir = TempDir::new().unwrap();
    let state_path = dir.path().join("state.json");
    let reads_path = reads_path_for_state(&state_path);

    let writer = {
        let state_path = state_path.clone();
        std::thread::spawn(move || {
            for i in 0..40 {
                let mut st = StateFile::default();
                let mut s = SeriesState::default();
                s.record_discovered([release(&format!("c{i}"), &[&format!("c{i}")], i)]);
                st.series.insert(KEY.into(), s);
                st.save_atomic(&state_path).unwrap();
            }
        })
    };
    for i in 0..40 {
        ReadMarkers::update(&reads_path, |m| {
            m.mark_read(KEY, [format!("c{i}").as_str()]);
        })
        .unwrap();
    }
    writer.join().unwrap();

    let markers = ReadMarkers::load(&reads_path);
    assert!((0..40).all(|i| markers.is_read(KEY, &format!("c{i}"))));
    assert!(StateFile::load(&state_path).is_ok());
}

#[test]
fn read_markers_are_bounded_and_unreadable_files_are_set_aside() {
    let mut m = ReadMarkers::default();
    m.mark_read(
        KEY,
        (0..300)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .iter()
            .map(String::as_str),
    );
    assert_eq!(m.series[KEY].len(), MAX_READ_IDS_PER_SERIES);
    assert!(!m.is_read(KEY, "0"));
    assert!(m.is_read(KEY, "299"));
    assert!(m.forget(KEY));

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("reads.json");
    std::fs::write(&path, "{ nope").unwrap();
    assert!(ReadMarkers::load(&path).series.is_empty());
    assert!(dir.path().join("reads.json.bad").is_file());
}

// ---------------------------------------------------------------- run summary

#[tokio::test]
async fn full_runs_persist_a_summary_and_per_title_failure_reasons() {
    let fx = Fixture::new(&[261]).await;
    serve_page(&fx.site, 261, page(&[(1, "Chapter 1")])).await;
    let summary = fx.check().await;
    assert_eq!(summary.exit_code, ExitCode::Ok);

    let run = fx.state().last_run.expect("run summary");
    assert!(run.finished_unix.is_some());
    assert_eq!((run.ok, run.failed), (1, 0));
    assert_eq!(run.exit_code, Some(0));
    let first_success = run.last_success_unix.expect("success time");
    assert_eq!(run.phase(run.started_unix), RunPhase::Finished);

    // The site now fails: attempt advances, last success does not, the reason is stored.
    fx.site.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&fx.site)
        .await;
    fx.check().await;
    let state = fx.state();
    let run = state.last_run.as_ref().unwrap();
    assert_eq!((run.ok, run.failed), (0, 1));
    assert_eq!(run.last_success_unix, Some(first_success));
    assert_eq!(
        state.series[KEY].last_error.as_deref(),
        Some("server error: HTTP 503")
    );
    assert_eq!(state.series[KEY].consecutive_failures, 1);

    // Recovery clears the reason.
    serve_page(&fx.site, 261, page(&[(1, "Chapter 1")])).await;
    fx.check().await;
    assert!(fx.state().series[KEY].last_error.is_none());
}

#[tokio::test]
async fn dry_run_and_lock_contention_leave_the_run_summary_alone() {
    let fx = Fixture::new(&[261]).await;
    serve_page(&fx.site, 261, page(&[(1, "Chapter 1")])).await;
    fx.check().await;
    let before = fx.state().last_run;

    run_check(
        &fx.config,
        &fx.registry,
        &fx.state_path(),
        CheckOptions {
            dry_run: true,
            ..CheckOptions::default()
        },
    )
    .await;
    let _held = InstanceLock::acquire_for_state(&fx.state_path()).unwrap();
    let summary = fx.check().await;
    assert_eq!(summary.exit_code, ExitCode::AnotherInstanceRunning);
    assert_eq!(fx.state().last_run, before);
}

#[tokio::test]
async fn manual_runs_report_progress_per_series() {
    let fx = Fixture::new(&[261, 262]).await;
    for n in [261, 262] {
        Mock::given(method("GET"))
            .and(path(format!("/manga/{n}")))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(&[(1, "Chapter 1")])))
            .mount(&fx.site)
            .await;
    }
    let mut events = Vec::new();
    run_check_scoped(
        &fx.config,
        &fx.registry,
        &fx.state_path(),
        CheckOptions::default(),
        None,
        &mut |e| events.push(e),
    )
    .await;
    assert_eq!(events.len(), 5);
    assert_eq!(events[0], CheckProgress::Started { total: 2 });
    assert!(matches!(
        &events[1],
        CheckProgress::SeriesStarted { index: 0, total: 2, series_key } if series_key == "mangaup:261"
    ));
    assert!(matches!(
        &events[4],
        CheckProgress::SeriesFinished { index: 1, outcome, .. } if outcome == "baseline"
    ));
}

// ---------------------------------------------------------------- single-title retry

#[tokio::test]
async fn single_title_retry_checks_only_that_series_and_respects_the_lock() {
    let fx = Fixture::new(&[261, 262]).await;
    for n in [261, 262] {
        Mock::given(method("GET"))
            .and(path(format!("/manga/{n}")))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(&[(1, "Chapter 1")])))
            .mount(&fx.site)
            .await;
    }
    fx.check().await;
    let full_run = fx.state().last_run.clone();

    // Pretend the last check was a while ago so the cool-down has passed.
    let mut state = fx.state();
    for s in state.series.values_mut() {
        s.last_checked_unix = Some(1);
    }
    state.save_atomic(&fx.state_path()).unwrap();
    fx.site.reset().await;
    for n in [261, 262] {
        Mock::given(method("GET"))
            .and(path(format!("/manga/{n}")))
            .respond_with(ResponseTemplate::new(200).set_body_string(page(&[(1, "Chapter 1")])))
            .mount(&fx.site)
            .await;
    }

    let target = SeriesKey::new("mangaup", "262");
    let summary = run_check_scoped(
        &fx.config,
        &fx.registry,
        &fx.state_path(),
        CheckOptions::default(),
        Some(&target),
        &mut |_| {},
    )
    .await;
    assert_eq!(summary.reports.len(), 1);
    assert_eq!(summary.reports[0].series_key, "mangaup:262");
    let hits: Vec<String> = fx
        .site
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.url.path().to_owned())
        .collect();
    assert_eq!(hits, ["/manga/262"]);
    assert_eq!(
        fx.state().last_run,
        full_run,
        "a retry does not replace the run summary"
    );

    // Immediately retrying again is refused without touching the site.
    let again = run_check_scoped(
        &fx.config,
        &fx.registry,
        &fx.state_path(),
        CheckOptions::default(),
        Some(&target),
        &mut |_| {},
    )
    .await;
    assert_eq!(again.reports[0].outcome, "cooldown");
    assert_eq!(fx.site.received_requests().await.unwrap().len(), 1);

    // And the instance lock still applies.
    let mut state = fx.state();
    state
        .series
        .get_mut("mangaup:262")
        .unwrap()
        .last_checked_unix = Some(1);
    state.save_atomic(&fx.state_path()).unwrap();
    let _held = InstanceLock::acquire_for_state(&fx.state_path()).unwrap();
    let locked = run_check_scoped(
        &fx.config,
        &fx.registry,
        &fx.state_path(),
        CheckOptions::default(),
        Some(&target),
        &mut |_| {},
    )
    .await;
    assert_eq!(locked.exit_code, ExitCode::AnotherInstanceRunning);
    assert_eq!(fx.site.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn retry_of_an_unknown_series_is_a_configuration_error() {
    let fx = Fixture::new(&[261]).await;
    let unknown = SeriesKey::new("mangaup", "999");
    let summary = run_check_scoped(
        &fx.config,
        &fx.registry,
        &fx.state_path(),
        CheckOptions::default(),
        Some(&unknown),
        &mut |_| {},
    )
    .await;
    assert_eq!(summary.exit_code, ExitCode::ConfigOrStateError);
}

#[tokio::test]
async fn a_rate_limited_series_cannot_be_retried_until_retry_after_passes() {
    let fx = Fixture::new(&[261]).await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "120"))
        .mount(&fx.site)
        .await;
    fx.check().await;
    let state = fx.state();
    let series = &state.series[KEY];
    let until = series.retry_not_before_unix.expect("block recorded");
    assert!(
        retry_wait_secs(series, until - 119).is_some(),
        "still blocked inside the window"
    );
    assert!(
        retry_wait_secs(series, until + 1).is_none(),
        "free again once the window passes"
    );

    let target = SeriesKey::new("mangaup", "261");
    let retry = run_check_scoped(
        &fx.config,
        &fx.registry,
        &fx.state_path(),
        CheckOptions::default(),
        Some(&target),
        &mut |_| {},
    )
    .await;
    assert_eq!(retry.reports[0].outcome, "cooldown");
    assert_eq!(fx.site.received_requests().await.unwrap().len(), 1);
}

#[test]
fn retry_cooldown_arithmetic() {
    let s = SeriesState {
        last_checked_unix: Some(1_000),
        ..SeriesState::default()
    };
    assert_eq!(retry_wait_secs(&s, 1_000), Some(RETRY_COOLDOWN_SECS));
    assert_eq!(retry_wait_secs(&s, 1_000 + RETRY_COOLDOWN_SECS), None);
    assert_eq!(retry_wait_secs(&SeriesState::default(), 5), None);
    let blocked = SeriesState {
        retry_not_before_unix: Some(5_000),
        last_checked_unix: Some(1_000),
        ..SeriesState::default()
    };
    assert_eq!(retry_wait_secs(&blocked, 4_000), Some(1_000));
}

#[tokio::test]
async fn single_title_retry_delivers_through_the_same_rules() {
    let fx = Fixture::new(&[261]).await;
    webhook_ok(&fx.webhook).await;
    serve_page(&fx.site, 261, page(&[(1, "Chapter 1")])).await;
    fx.check().await;
    let mut state = fx.state();
    state.series.get_mut(KEY).unwrap().last_checked_unix = Some(1);
    state.save_atomic(&fx.state_path()).unwrap();

    serve_page(&fx.site, 261, page(&[(2, "Chapter 2"), (1, "Chapter 1")])).await;
    let target = SeriesKey::new("mangaup", "261");
    let summary = run_check_scoped(
        &fx.config,
        &fx.registry,
        &fx.state_path(),
        CheckOptions::default(),
        Some(&target),
        &mut |_| {},
    )
    .await;
    assert_eq!(summary.exit_code, ExitCode::NewsAnnounced);
    assert_eq!(fx.webhook.received_requests().await.unwrap().len(), 1);
    assert_eq!(fx.state().series[KEY].discovered.len(), 1);
    tokio::time::sleep(Duration::from_millis(1)).await;
}

#[tokio::test]
async fn announcement_cap_does_not_hide_discovered_releases() {
    let mut fx = Fixture::new(&[261]).await;
    fx.config.max_messages_per_cycle = 1;
    webhook_ok(&fx.webhook).await;
    serve_page(&fx.site, 261, page(&[(1, "Chapter 1")])).await;
    assert_eq!(fx.check().await.exit_code, ExitCode::Ok);

    serve_page(
        &fx.site,
        261,
        page(&[
            (5, "Chapter 4"),
            (4, "Chapter 3.2"),
            (3, "Chapter 3.1"),
            (2, "Chapter 2"),
            (1, "Chapter 1"),
        ]),
    )
    .await;
    let summary = fx.check().await;
    assert_eq!(summary.exit_code, ExitCode::NewsAnnounced);
    assert_eq!(summary.reports[0].announced_groups, 1);
    assert_eq!(summary.reports[0].folded_parts, 3);
    let state = fx.state();
    let groups = state.series[KEY]
        .discovered
        .iter()
        .map(|release| {
            release
                .part_ids
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(groups, [vec!["2"], vec!["3", "4"], vec!["5"]]);
    assert_eq!(fx.check().await.exit_code, ExitCode::Ok);
    assert_eq!(
        fx.state().series[KEY].discovered,
        state.series[KEY].discovered
    );
}

#[tokio::test]
async fn watch_persists_summary_when_a_pass_finishes() {
    let fx = Fixture::new(&[261]).await;
    serve_page(&fx.site, 261, page(&[(1, "Chapter 1")])).await;
    let path = fx.state_path();
    let wait_for_summary = async {
        loop {
            if let Some(record) = fx.state().last_run
                && record.finished_unix.is_some()
            {
                return record;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    let record = tokio::select! {
        result = run_watch(&fx.config, &fx.registry, &path) => {
            panic!("watch exited before producing a summary: {result:?}");
        }
        record = tokio::time::timeout(Duration::from_secs(3), wait_for_summary) => {
            record.expect("watch should persist a finished run summary")
        }
    };
    assert_eq!(record.ok, 1);
    assert_eq!(record.failed, 0);
    assert_eq!(record.exit_code, Some(ExitCode::Ok.as_i32()));
    assert_eq!(record.last_success_unix, record.finished_unix);
}
