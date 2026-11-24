//! Unit and integration tests for `paths`, `config`, `.env` loading, secret redaction, and `state`.

use std::collections::BTreeMap;
use std::fs;
use std::sync::{Arc, Mutex};

use tempfile::TempDir;
use tracing_subscriber::layer::SubscriberExt;

use manga_watcher::config::{
    Config, ConfigError, DEFAULT_WEBHOOK_ENV, HEALTH_WEBHOOK_ENV, LoadOptions, REDACTED_WEBHOOK,
    WebhookUrl,
};
use manga_watcher::paths::{
    lock_path_for_state, resolve_config_path_with_dirs, resolve_config_path_with_exe_dir,
    resolve_env_file_path_with_cwd,
};
use manga_watcher::sites::Registry;
use manga_watcher::state::{
    InstanceLock, MAX_ANNOUNCED_IDS, STATE_VERSION, SeriesState, StateError, StateFile,
};

const VALID_WEBHOOK: &str =
    "https://discord.com/api/webhooks/123456789012345678/super_secret_token_abc123";
const VALID_HEALTH_WEBHOOK: &str =
    "https://discordapp.com/api/webhooks/987654321098765432/health_secret_token_xyz789";

#[test]
fn path_resolution_precedence_works() {
    let tmp = TempDir::new().expect("tempdir");
    let explicit_cfg = tmp.path().join("explicit.toml");
    let user_cfg_dir = tmp.path().join("appdata");
    let user_cfg = user_cfg_dir.join("manga-watcher").join("config.toml");
    let cwd_dir = tmp.path().join("cwd");
    let cwd_cfg = cwd_dir.join("config.toml");

    fs::create_dir_all(user_cfg.parent().expect("parent")).expect("mkdir");
    fs::create_dir_all(&cwd_dir).expect("mkdir");

    // Nothing exists yet -> ConfigNotFound
    let err = resolve_config_path_with_dirs(None, Some(&user_cfg_dir), &cwd_dir).unwrap_err();
    assert!(err.to_string().contains("no config.toml found"));

    // Create ./config.toml -> picked up as fallback
    fs::write(&cwd_cfg, "# cwd").expect("write");
    let resolved = resolve_config_path_with_dirs(None, Some(&user_cfg_dir), &cwd_dir).unwrap();
    assert_eq!(resolved, cwd_cfg);

    // Create dirs::config_dir()/manga-watcher/config.toml -> overrides ./config.toml
    fs::write(&user_cfg, "# user").expect("write");
    let resolved = resolve_config_path_with_dirs(None, Some(&user_cfg_dir), &cwd_dir).unwrap();
    assert_eq!(resolved, user_cfg);

    // Explicit --config -> overrides everything
    fs::write(&explicit_cfg, "# explicit").expect("write");
    let resolved =
        resolve_config_path_with_dirs(Some(&explicit_cfg), Some(&user_cfg_dir), &cwd_dir).unwrap();
    assert_eq!(resolved, explicit_cfg);

    // Explicit --config missing -> error
    let missing = tmp.path().join("missing.toml");
    assert!(resolve_config_path_with_dirs(Some(&missing), Some(&user_cfg_dir), &cwd_dir).is_err());
}

#[test]
fn config_next_to_executable_wins_over_cwd_but_not_appdata() {
    let tmp = TempDir::new().expect("tempdir");
    let user_cfg_dir = tmp.path().join("appdata");
    let exe_dir = tmp.path().join("tools");
    let cwd_dir = tmp.path().join("system32");
    for d in [&user_cfg_dir, &exe_dir, &cwd_dir] {
        fs::create_dir_all(d).expect("mkdir");
    }
    let exe_cfg = exe_dir.join("config.toml");
    let cwd_cfg = cwd_dir.join("config.toml");
    fs::write(&exe_cfg, "# exe").expect("write");
    fs::write(&cwd_cfg, "# cwd").expect("write");

    // Task Scheduler case: cwd is elsewhere, config sits next to the exe.
    let resolved =
        resolve_config_path_with_exe_dir(None, Some(&user_cfg_dir), Some(&exe_dir), &cwd_dir)
            .unwrap();
    assert_eq!(resolved, exe_cfg);

    // %APPDATA% config still takes precedence.
    let user_cfg = user_cfg_dir.join("manga-watcher").join("config.toml");
    fs::create_dir_all(user_cfg.parent().expect("parent")).expect("mkdir");
    fs::write(&user_cfg, "# user").expect("write");
    let resolved =
        resolve_config_path_with_exe_dir(None, Some(&user_cfg_dir), Some(&exe_dir), &cwd_dir)
            .unwrap();
    assert_eq!(resolved, user_cfg);

    // The .env next to the resolved (exe-dir) config is used even when cwd differs.
    fs::remove_file(&user_cfg).expect("rm");
    fs::write(exe_dir.join(".env"), "X=1").expect("write");
    let env = resolve_env_file_path_with_cwd(None, &exe_cfg, &cwd_dir).unwrap();
    assert_eq!(env, Some(exe_dir.join(".env")));
}

#[test]
fn env_file_resolution_precedence_works() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg_dir = tmp.path().join("cfg");
    let cwd_dir = tmp.path().join("cwd");
    fs::create_dir_all(&cfg_dir).expect("mkdir");
    fs::create_dir_all(&cwd_dir).expect("mkdir");

    let cfg_path = cfg_dir.join("config.toml");
    fs::write(&cfg_path, "").expect("write");

    // No .env anywhere -> Ok(None)
    assert_eq!(
        resolve_env_file_path_with_cwd(None, &cfg_path, &cwd_dir).unwrap(),
        None
    );

    // ./.env exists -> picked up
    let cwd_env = cwd_dir.join(".env");
    fs::write(&cwd_env, "A=1\n").expect("write");
    assert_eq!(
        resolve_env_file_path_with_cwd(None, &cfg_path, &cwd_dir).unwrap(),
        Some(cwd_env)
    );

    // Sibling .env next to config.toml overrides ./.env
    let sibling_env = cfg_dir.join(".env");
    fs::write(&sibling_env, "A=2\n").expect("write");
    assert_eq!(
        resolve_env_file_path_with_cwd(None, &cfg_path, &cwd_dir).unwrap(),
        Some(sibling_env)
    );

    // Explicit --env-file overrides sibling .env
    let explicit_env = tmp.path().join("custom.env");
    fs::write(&explicit_env, "A=3\n").expect("write");
    assert_eq!(
        resolve_env_file_path_with_cwd(Some(&explicit_env), &cfg_path, &cwd_dir).unwrap(),
        Some(explicit_env)
    );
}

#[test]
fn config_parses_defaults_and_clamps_short_intervals() {
    let registry = Registry::with_defaults();
    let toml = r#"
        interval_minutes = 5
        [[series]]
        url = "https://global.manga-up.com/manga/261"
        interval_minutes = 3
    "#;

    let cfg = Config::from_toml_str(toml, &LoadOptions::default(), &registry, |key| match key {
        DEFAULT_WEBHOOK_ENV => Some(VALID_WEBHOOK.to_string()),
        _ => None,
    })
    .expect("valid config");

    assert_eq!(
        cfg.interval.as_secs(),
        10 * 60,
        "global interval clamped to 10m"
    );
    assert_eq!(
        cfg.series[0].interval.as_secs(),
        10 * 60,
        "series interval clamped to 10m"
    );
    assert_eq!(cfg.jitter_seconds, 120);
    assert_eq!(cfg.timeout.as_secs(), 20);
    assert_eq!(cfg.max_messages_per_cycle, 3);
    assert_eq!(cfg.alert_after_failures, 6);
    assert_eq!(cfg.mention, "@everyone");
    assert!(!cfg.require_published);
    assert!(cfg.health_webhook.is_none());
}

#[test]
fn config_rejects_secrets_and_unknown_keys_in_toml() {
    let registry = Registry::with_defaults();

    let toml_with_webhook = r#"
        discord_webhook = "https://discord.com/api/webhooks/123/abc"
        [[series]]
        url = "https://global.manga-up.com/manga/261"
    "#;
    let err = Config::from_toml_str(
        toml_with_webhook,
        &LoadOptions::default(),
        &registry,
        |_| Some(VALID_WEBHOOK.to_string()),
    )
    .unwrap_err();
    assert!(
        matches!(err, ConfigError::SecretInConfig(_)),
        "expected SecretInConfig error, got: {err}"
    );
    assert!(err.to_string().contains(".env"));

    let toml_unknown_key = r#"
         unknown_typo_key = 42
        [[series]]
        url = "https://global.manga-up.com/manga/261"
    "#;
    let err = Config::from_toml_str(toml_unknown_key, &LoadOptions::default(), &registry, |_| {
        Some(VALID_WEBHOOK.to_string())
    })
    .unwrap_err();
    assert!(matches!(err, ConfigError::TomlParse(_)));
}

#[test]
fn config_rejects_duplicate_series_and_unsupported_urls() {
    let registry = Registry::with_defaults();

    let duplicate_toml = r#"
        [[series]]
        url = "https://global.manga-up.com/manga/261"
        [[series]]
        url = "https://global.manga-up.com/manga/261/?ref=home"
    "#;
    let err = Config::from_toml_str(duplicate_toml, &LoadOptions::default(), &registry, |_| {
        Some(VALID_WEBHOOK.to_string())
    })
    .unwrap_err();
    assert!(matches!(err, ConfigError::DuplicateSeries(_)));

    let unsupported_toml = r#"
        [[series]]
        url = "https://example.com/manga/261"
    "#;
    let err = Config::from_toml_str(unsupported_toml, &LoadOptions::default(), &registry, |_| {
        Some(VALID_WEBHOOK.to_string())
    })
    .unwrap_err();
    assert!(matches!(err, ConfigError::UnsupportedSiteUrl(_)));

    let mismatch_site_toml = r#"
        [[series]]
        url = "https://global.manga-up.com/manga/261"
        site = "other_site"
    "#;
    let err = Config::from_toml_str(
        mismatch_site_toml,
        &LoadOptions::default(),
        &registry,
        |_| Some(VALID_WEBHOOK.to_string()),
    )
    .unwrap_err();
    assert!(matches!(err, ConfigError::SiteMismatch { .. }));
}

#[test]
fn env_loading_and_per_series_webhook_override_works() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg_path = tmp.path().join("config.toml");
    let env_path = tmp.path().join(".env");

    let custom_wh = "https://ptb.discord.com/api/webhooks/5555555555/custom_smoking_token";
    fs::write(
        &env_path,
        format!(
            "{DEFAULT_WEBHOOK_ENV}={VALID_WEBHOOK}\n{HEALTH_WEBHOOK_ENV}={VALID_HEALTH_WEBHOOK}\nMANGA_WATCHER_WEBHOOK_SMOKING={custom_wh}\n"
        ),
    )
    .expect("write .env");

    fs::write(
        &cfg_path,
        r#"
        [[series]]
        url = "https://global.manga-up.com/manga/261"
        webhook_env = "MANGA_WATCHER_WEBHOOK_SMOKING"
        "#,
    )
    .expect("write config.toml");

    let registry = Registry::with_defaults();
    let cfg = Config::load(
        &LoadOptions {
            config_path: Some(cfg_path),
            env_file_path: Some(env_path.clone()),
            allow_any_webhook: false,
            allow_missing_webhooks: false,
        },
        &registry,
    )
    .expect("load config from disk");

    assert!(cfg.health_webhook.is_some());
    assert_eq!(cfg.series[0].webhook_env, "MANGA_WATCHER_WEBHOOK_SMOKING");
    assert!(cfg.series[0].webhook.is_some());

    // Verify real process environment precedence over .env values
    let dotenv_map = manga_watcher::config::parse_dotenv_file(&env_path).expect("parse dotenv");
    let override_wh = "https://canary.discord.com/api/webhooks/777777777/override_from_real_env";
    let mut simulated_real_env = BTreeMap::new();
    simulated_real_env.insert(
        "MANGA_WATCHER_WEBHOOK_SMOKING".to_string(),
        override_wh.to_string(),
    );

    let cfg_overridden = Config::from_toml_str(
        r#"
        [[series]]
        url = "https://global.manga-up.com/manga/261"
        webhook_env = "MANGA_WATCHER_WEBHOOK_SMOKING"
        "#,
        &LoadOptions::default(),
        &registry,
        |k| {
            simulated_real_env
                .get(k)
                .cloned()
                .or_else(|| dotenv_map.get(k).cloned())
        },
    )
    .expect("overridden config");

    let expected_url =
        WebhookUrl::parse("MANGA_WATCHER_WEBHOOK_SMOKING", override_wh, false).unwrap();
    assert_eq!(
        cfg_overridden.series[0].webhook.as_ref(),
        Some(&expected_url)
    );
}

#[test]
fn webhook_validation_and_redaction_never_leak_token() {
    let secret_token = "ULTRA_SECRET_DISCORD_TOKEN_DO_NOT_LEAK_999";
    let raw_good = format!("https://discord.com/api/webhooks/123456789/{secret_token}");
    let wh = WebhookUrl::parse(DEFAULT_WEBHOOK_ENV, &raw_good, false).expect("valid webhook");

    assert_eq!(format!("{wh}"), REDACTED_WEBHOOK);
    assert_eq!(format!("{wh:?}"), REDACTED_WEBHOOK);
    assert!(!format!("{wh}").contains(secret_token));
    assert!(!format!("{wh:?}").contains(secret_token));

    // Invalid webhooks must fail and never include the raw URL or token in the error message
    let bad_http = format!("http://discord.com/api/webhooks/123456789/{secret_token}");
    let err = WebhookUrl::parse(DEFAULT_WEBHOOK_ENV, &bad_http, false).unwrap_err();
    assert!(!err.to_string().contains(secret_token));

    let bad_host = format!("https://evil.example.com/api/webhooks/123456789/{secret_token}");
    let err = WebhookUrl::parse(DEFAULT_WEBHOOK_ENV, &bad_host, false).unwrap_err();
    assert!(!err.to_string().contains(secret_token));

    // --allow-any-webhook lifts host check for https, and allows http ONLY for loopback
    assert!(WebhookUrl::parse(DEFAULT_WEBHOOK_ENV, &bad_host, true).is_ok());
    assert!(WebhookUrl::parse(DEFAULT_WEBHOOK_ENV, &bad_http, true).is_err());

    let loopback_http = format!("http://127.0.0.1:8080/api/webhooks/123456789/{secret_token}");
    assert!(WebhookUrl::parse(DEFAULT_WEBHOOK_ENV, &loopback_http, false).is_err());
    assert!(WebhookUrl::parse(DEFAULT_WEBHOOK_ENV, &loopback_http, true).is_ok());

    // Capture tracing logs while logging the webhook and config; assert token never appears
    let log_buffer = Arc::new(Mutex::new(Vec::<u8>::new()));
    let writer_buf = Arc::clone(&log_buffer);
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(move || SharedBufferWriter(Arc::clone(&writer_buf))),
    );

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(webhook = %wh, debug_wh = ?wh, "testing webhook log redaction");
    });

    let logged = String::from_utf8(log_buffer.lock().expect("lock").clone()).expect("utf8");
    assert!(logged.contains(REDACTED_WEBHOOK));
    assert!(!logged.contains(secret_token));
}

struct SharedBufferWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedBufferWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn state_atomic_write_roundtrip_and_corruption_handling() {
    let tmp = TempDir::new().expect("tempdir");
    let state_path = tmp.path().join("state.json");

    // Non-existent file -> default v2 empty state
    let loaded = StateFile::load(&state_path).expect("load missing");
    assert_eq!(loaded.version, STATE_VERSION);
    assert!(loaded.series.is_empty());

    // Save and reload round-trip
    let mut state = StateFile::default();
    state.series.insert(
        "mangaup:261".to_string(),
        SeriesState {
            announced_ids: vec!["28665".to_string(), "130422".to_string()],
            last_announced_label: Some("Chapter 67.3".to_string()),
            last_snapshot_len: 159,
            last_checked_unix: Some(1_795_618_800),
            last_success_unix: Some(1_795_618_800),
            consecutive_failures: 0,
            alert_sent: false,
            etag: Some("\"sln4x33xql78nt\"".to_string()),
            consecutive_anomalies: 0,
        },
    );
    state.save_atomic(&state_path).expect("save atomic");
    let reloaded = StateFile::load(&state_path).expect("reload");
    assert_eq!(reloaded, state);

    // Corrupt JSON -> StateError::Corrupt mentioning file and reset command
    fs::write(&state_path, "{ not valid json ").expect("write corrupt");
    let err = StateFile::load(&state_path).unwrap_err();
    assert!(matches!(err, StateError::Corrupt { .. }));
    assert!(err.to_string().contains("reset"));

    // Unknown version -> StateError::UnsupportedVersion
    fs::write(&state_path, r#"{"version": 1, "series": {}}"#).expect("write v1");
    let err = StateFile::load(&state_path).unwrap_err();
    assert!(matches!(
        err,
        StateError::UnsupportedVersion { version: 1, .. }
    ));
}

#[test]
fn state_pruning_prefers_removing_ids_absent_from_current_snapshot() {
    let mut s = SeriesState::default();
    // Insert 5,005 IDs: "old_0".."old_9" (absent from snapshot) + "snap_0".."snap_4994"
    for i in 0..10 {
        s.announced_ids.push(format!("old_{i}"));
    }
    let mut current_snapshot_ids = Vec::with_capacity(MAX_ANNOUNCED_IDS - 5);
    for i in 0..(MAX_ANNOUNCED_IDS - 5) {
        let id = format!("snap_{i}");
        s.announced_ids.push(id.clone());
        current_snapshot_ids.push(id);
    }
    assert_eq!(s.announced_ids.len(), MAX_ANNOUNCED_IDS + 5);

    s.prune_to_cap(&current_snapshot_ids);
    assert_eq!(s.announced_ids.len(), MAX_ANNOUNCED_IDS);
    // The first 5 absent IDs ("old_0".."old_4") were pruned first; all snapshot IDs remain!
    assert!(!s.announced_ids.contains(&"old_0".to_string()));
    assert!(!s.announced_ids.contains(&"old_4".to_string()));
    assert!(s.announced_ids.contains(&"old_5".to_string()));
    assert!(s.announced_ids.contains(&"snap_0".to_string()));
}

#[test]
fn single_instance_lock_prevents_second_acquisition() {
    let tmp = TempDir::new().expect("tempdir");
    let state_path = tmp.path().join("state.json");
    let lock_path = lock_path_for_state(&state_path);

    let lock1 = InstanceLock::acquire_for_state(&state_path).expect("first lock succeeds");
    assert_eq!(lock1.path(), lock_path);

    let err = InstanceLock::acquire_for_state(&state_path).unwrap_err();
    assert!(matches!(err, StateError::Locked(_)));

    drop(lock1);
    let _lock2 =
        InstanceLock::acquire_for_state(&state_path).expect("lock succeeds after first dropped");
}
