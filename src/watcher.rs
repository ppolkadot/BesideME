//! Orchestrator for `check`, `watch`, `test-webhook`, `list`, and `reset` commands.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use time::OffsetDateTime;

use crate::config::{Config, SeriesConfig, WebhookUrl};
use crate::detect::{Decision, Policy, REBASELINE_AFTER_CONSECUTIVE_ANOMALIES, decide};
use crate::http::{FetchOutcome, HttpError, SiteHttpClient};
use crate::model::SeriesSnapshot;
use crate::notify::discord::{
    AnnouncementContext, DiscordClient, DiscordError, GROUP_POST_GAP, build_announcement_payload,
    build_health_alert_payload, build_health_recovery_payload, build_test_webhook_payload,
    build_webhook_gone_alert_payload, now_rfc3339_utc,
};
use crate::sites::{ParseError, Registry};
use crate::state::{InstanceLock, SeriesState, StateError, StateFile};

/// Minimum delay between sequential series requests in `watch` mode (§7).
pub const INTER_SERIES_GAP: Duration = Duration::from_secs(2);

/// Maximum wall-clock sleep slice in `watch` mode before re-checking due times (§7).
pub const WATCH_MAX_SLEEP_SLICE: Duration = Duration::from_secs(60);

/// Maximum backoff cap for consecutive 403/429 blocks (6 hours, §7).
pub const MAX_BLOCK_BACKOFF: Duration = Duration::from_secs(6 * 60 * 60);

/// Exit codes returned by `manga-watcher`, ordered by severity ("worst wins", §6).
///
/// Priority: `1` config/state error > `5` another instance running > `4` webhook error
/// > `2` parse error > `3` network error > `10` news announced > `0` ok.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// `0` — completed with no errors and no announcements.
    Ok = 0,
    /// `10` — at least one new chapter group was announced and no errors occurred.
    NewsAnnounced = 10,
    /// `3` — site HTTP / network error.
    NetworkError = 3,
    /// `2` — HTML/JSON parse or schema mismatch error.
    ParseError = 2,
    /// `4` — Discord webhook delivery error.
    WebhookError = 4,
    /// `5` — another instance holds `state.lock`.
    AnotherInstanceRunning = 5,
    /// `1` — invalid configuration or corrupt/unreadable state file.
    ConfigOrStateError = 1,
}

impl ExitCode {
    /// Returns the numeric process exit code (`i32`).
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    const fn severity_rank(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::NewsAnnounced => 1,
            Self::NetworkError => 2,
            Self::ParseError => 3,
            Self::WebhookError => 4,
            Self::AnotherInstanceRunning => 5,
            Self::ConfigOrStateError => 6,
        }
    }

    /// Combines two [`ExitCode`]s according to the "worst wins" rule (§6).
    #[must_use]
    pub const fn combine(self, other: Self) -> Self {
        if other.severity_rank() > self.severity_rank() {
            other
        } else {
            self
        }
    }
}

/// Options for a single `check` pass.
#[derive(Debug, Clone, Copy, Default)]
pub struct CheckOptions {
    /// If `true`, never writes state and never sends webhook POSTs.
    pub dry_run: bool,
    /// If `true`, prints one JSON object per series to stdout.
    pub json: bool,
    /// If `true`, baselines all chapters except `chapters[0]` on first run and announces `chapters[0]`.
    pub announce_on_first_run: bool,
}

/// Per-series outcome report produced by [`run_check`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct SeriesCheckReport {
    /// Composite series key (`"<site_id>:<key>"`).
    pub series_key: String,
    /// Canonical series URL.
    pub url: String,
    /// Display title of the manga series (when available).
    pub title: Option<String>,
    /// Outcome label (`"baseline"`, `"no_change"`, `"announced"`, `"held_back"`, `"anomaly"`, `"rebaselined"`, `"error"`).
    pub outcome: String,
    /// Current head chapter label (when available).
    pub head_label: Option<String>,
    /// Number of chapter-group messages announced (or that would be announced in dry-run).
    pub announced_groups: usize,
    /// Total number of chapter parts announced across groups.
    pub announced_parts: usize,
    /// Number of older chapter parts folded without their own message.
    pub folded_parts: usize,
    /// Number of new undated chapters held back by `require_published`.
    pub held_back_parts: usize,
    /// Whether this check ran in `--dry-run` mode.
    pub dry_run: bool,
    /// Error description if `outcome == "error"`.
    pub error: Option<String>,
}

/// Aggregated result of a [`run_check`] invocation.
#[derive(Debug, Clone)]
pub struct CheckRunSummary {
    /// Combined exit code across all series ("worst wins").
    pub exit_code: ExitCode,
    /// Per-series reports in declaration order.
    pub reports: Vec<SeriesCheckReport>,
}

/// Failure category used to compute the next attempt time in `watch` mode (§7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// Timeout, connection failure, or HTTP 5xx (`30s -> 90s -> 270s -> normal interval`).
    TransientNetwork {
        /// Consecutive transient network failure count (`1..`).
        streak: u32,
    },
    /// HTTP 403 or 429 (`max(Retry-After, interval)`, doubling per consecutive block up to 6 h).
    BlockedOrRateLimited {
        /// Consecutive 403/429 block count (`1..`).
        streak: u32,
        /// Optional `Retry-After` header duration.
        retry_after: Option<Duration>,
    },
    /// HTML/JSON parse error or other error (uses normal interval).
    Other,
}

/// Computes a jittered polling interval (`interval ± uniform[-jitter_secs, +jitter_secs]`).
#[must_use]
pub fn apply_jitter(interval: Duration, jitter_secs: u64) -> Duration {
    if jitter_secs == 0 {
        return interval;
    }
    let span = jitter_secs.saturating_mul(2).saturating_add(1);
    let offset = fastrand::u64(0..span);
    let base_secs = interval.as_secs();
    let jittered_secs = base_secs
        .saturating_add(offset)
        .saturating_sub(jitter_secs)
        .max(60);
    Duration::from_secs(jittered_secs)
}

/// Purely computes the delay until the next poll attempt for a series in `watch` mode (§7).
#[must_use]
pub fn compute_next_delay(
    base_interval: Duration,
    jitter_secs: u64,
    failure: Option<FailureKind>,
) -> Duration {
    match failure {
        None | Some(FailureKind::Other) => apply_jitter(base_interval, jitter_secs),
        Some(FailureKind::TransientNetwork { streak }) => match streak {
            1 => Duration::from_secs(30),
            2 => Duration::from_secs(90),
            3 => Duration::from_secs(270),
            _ => apply_jitter(base_interval, jitter_secs),
        },
        Some(FailureKind::BlockedOrRateLimited {
            streak,
            retry_after,
        }) => {
            let shift = streak.saturating_sub(1).min(16);
            let factor = 1u64 << shift;
            let scaled_secs = base_interval.as_secs().saturating_mul(factor);
            let retry_secs = retry_after.map_or(0, |d| d.as_secs());
            let chosen_secs = scaled_secs.max(retry_secs).min(MAX_BLOCK_BACKOFF.as_secs());
            Duration::from_secs(chosen_secs)
        }
    }
}

/// Runs a single sequential check pass over all configured series (`manga-watcher check`).
pub async fn run_check(
    config: &Config,
    registry: &Registry,
    state_path: &Path,
    options: CheckOptions,
) -> CheckRunSummary {
    let _lock = if options.dry_run {
        None
    } else {
        match InstanceLock::acquire_for_state(state_path) {
            Ok(lock) => Some(lock),
            Err(StateError::Locked(p)) => {
                tracing::error!(lock_path = %p.display(), "another instance is already running");
                return CheckRunSummary {
                    exit_code: ExitCode::AnotherInstanceRunning,
                    reports: Vec::new(),
                };
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to acquire state lock");
                return CheckRunSummary {
                    exit_code: ExitCode::ConfigOrStateError,
                    reports: Vec::new(),
                };
            }
        }
    };

    let mut state = match StateFile::load(state_path) {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(error = %err, "failed to load state file");
            return CheckRunSummary {
                exit_code: ExitCode::ConfigOrStateError,
                reports: Vec::new(),
            };
        }
    };

    if config.health_webhook.is_none() && config.alert_after_failures > 0 {
        tracing::warn!(
            "MANGA_WATCHER_HEALTH_WEBHOOK is not set; operational health alerts are disabled"
        );
    }

    let http_client = match SiteHttpClient::new(&config.user_agent, config.timeout) {
        Ok(c) => c,
        Err(err) => {
            tracing::error!(error = %err, "failed to build site HTTP client");
            return CheckRunSummary {
                exit_code: ExitCode::NetworkError,
                reports: Vec::new(),
            };
        }
    };

    let discord_client = match DiscordClient::new(&config.user_agent, config.timeout) {
        Ok(c) => c,
        Err(err) => {
            tracing::error!(error = %err, "failed to build Discord HTTP client");
            return CheckRunSummary {
                exit_code: ExitCode::WebhookError,
                reports: Vec::new(),
            };
        }
    };

    let mut gone_webhooks: HashSet<WebhookUrl> = HashSet::new();
    let mut overall_code = ExitCode::Ok;
    let mut reports = Vec::with_capacity(config.series.len());

    for series_cfg in &config.series {
        let mut outcome = check_single_series(
            config,
            series_cfg,
            registry,
            &http_client,
            &discord_client,
            &mut state,
            state_path,
            &mut gone_webhooks,
            options,
        )
        .await;

        overall_code = overall_code.combine(outcome.exit_code);
        if options.json
            && let Ok(line) = serde_json::to_string(&outcome.report)
        {
            println!("{line}");
        }
        reports.push(std::mem::take(&mut outcome.report));
    }

    CheckRunSummary {
        exit_code: overall_code,
        reports,
    }
}

struct SingleSeriesOutcome {
    exit_code: ExitCode,
    failure_kind: Option<FailureKind>,
    report: SeriesCheckReport,
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn check_single_series(
    config: &Config,
    series_cfg: &SeriesConfig,
    registry: &Registry,
    http_client: &SiteHttpClient,
    discord_client: &DiscordClient,
    state: &mut StateFile,
    state_path: &Path,
    gone_webhooks: &mut HashSet<WebhookUrl>,
    options: CheckOptions,
) -> SingleSeriesOutcome {
    let state_key = series_cfg.key.as_state_key();
    let now_unix = OffsetDateTime::now_utc().unix_timestamp();

    let Some(site) = registry.get_by_id(&series_cfg.key.site_id) else {
        return SingleSeriesOutcome {
            exit_code: ExitCode::ConfigOrStateError,
            failure_kind: Some(FailureKind::Other),
            report: SeriesCheckReport {
                series_key: state_key,
                url: series_cfg.fetch_url.to_string(),
                outcome: "error".to_string(),
                dry_run: options.dry_run,
                error: Some(format!("unregistered site `{}`", series_cfg.key.site_id)),
                ..SeriesCheckReport::default()
            },
        };
    };

    let prior_state = state.series.get(&state_key).cloned();
    let prior_etag = prior_state.as_ref().and_then(|s| s.etag.as_deref());

    let fetch_res = http_client.fetch(&series_cfg.fetch_url, prior_etag).await;

    let (body, response_etag) = match fetch_res {
        Err(http_err) => {
            if http_err == HttpError::Timeout
                && prior_state
                    .as_ref()
                    .and_then(|s| s.last_success_unix)
                    .is_none()
            {
                tracing::warn!(
                    series = %state_key,
                    url = %series_cfg.fetch_url,
                    "URL is probably wrong (Manga UP hangs instead of returning 404)"
                );
            } else {
                tracing::warn!(
                    series = %state_key,
                    error = %http_err,
                    "HTTP fetch failed for series"
                );
            }

            let streak = record_series_failure(
                config,
                series_cfg,
                state,
                state_path,
                discord_client,
                gone_webhooks,
                &http_err.kind_label(),
                now_unix,
                options.dry_run,
            )
            .await;

            let failure_kind = match http_err {
                HttpError::BlockedOrRateLimited {
                    retry_after_secs, ..
                } => FailureKind::BlockedOrRateLimited {
                    streak,
                    retry_after: retry_after_secs.map(Duration::from_secs),
                },
                HttpError::Timeout | HttpError::Connect(_) | HttpError::ServerError { .. } => {
                    FailureKind::TransientNetwork { streak }
                }
                _ => FailureKind::Other,
            };

            let code = persist_state_if_needed(state, state_path, options.dry_run)
                .combine(ExitCode::NetworkError);

            return SingleSeriesOutcome {
                exit_code: code,
                failure_kind: Some(failure_kind),
                report: SeriesCheckReport {
                    series_key: state_key,
                    url: series_cfg.fetch_url.to_string(),
                    title: series_cfg.label.clone(),
                    outcome: "error".to_string(),
                    dry_run: options.dry_run,
                    error: Some(http_err.to_string()),
                    ..SeriesCheckReport::default()
                },
            };
        }
        Ok(FetchOutcome::NotModified { etag }) => {
            tracing::info!(series = %state_key, "HTTP 304 Not Modified (no change)");
            let display_title = series_cfg
                .label
                .clone()
                .unwrap_or_else(|| state_key.clone());

            handle_recovery_if_needed(
                config,
                state,
                &state_key,
                &display_title,
                discord_client,
                gone_webhooks,
                now_unix,
                etag,
                options.dry_run,
            )
            .await;

            let code = persist_state_if_needed(state, state_path, options.dry_run);
            let head_label = state
                .series
                .get(&state_key)
                .and_then(|s| s.last_announced_label.clone());

            if options.dry_run && !options.json {
                println!("{state_key} ({display_title}): 304 Not Modified (no change)");
            }

            return SingleSeriesOutcome {
                exit_code: code,
                failure_kind: None,
                report: SeriesCheckReport {
                    series_key: state_key,
                    url: series_cfg.fetch_url.to_string(),
                    title: Some(display_title),
                    outcome: "no_change".to_string(),
                    head_label,
                    dry_run: options.dry_run,
                    ..SeriesCheckReport::default()
                },
            };
        }
        Ok(FetchOutcome::Fetched { body, etag }) => (body, etag),
    };

    let snapshot = match site.parse(&series_cfg.key.key, &body) {
        Ok(snap) => {
            // Drop body immediately after parsing (§7).
            drop(body);
            snap
        }
        Err(parse_err) => {
            drop(body);
            tracing::warn!(
                series = %state_key,
                error = %parse_err,
                "failed to parse series page"
            );
            let err_label = classify_parse_error(&parse_err);
            let _ = record_series_failure(
                config,
                series_cfg,
                state,
                state_path,
                discord_client,
                gone_webhooks,
                &err_label,
                now_unix,
                options.dry_run,
            )
            .await;

            let code = persist_state_if_needed(state, state_path, options.dry_run)
                .combine(ExitCode::ParseError);

            return SingleSeriesOutcome {
                exit_code: code,
                failure_kind: Some(FailureKind::Other),
                report: SeriesCheckReport {
                    series_key: state_key,
                    url: series_cfg.fetch_url.to_string(),
                    title: series_cfg.label.clone(),
                    outcome: "error".to_string(),
                    dry_run: options.dry_run,
                    error: Some(parse_err.to_string()),
                    ..SeriesCheckReport::default()
                },
            };
        }
    };

    let display_title = series_cfg
        .label
        .as_deref()
        .unwrap_or(&snapshot.title)
        .to_string();

    handle_recovery_if_needed(
        config,
        state,
        &state_key,
        &display_title,
        discord_client,
        gone_webhooks,
        now_unix,
        response_etag.clone(),
        options.dry_run,
    )
    .await;

    let policy = Policy {
        require_published: series_cfg.require_published,
        max_messages_per_cycle: config.max_messages_per_cycle,
        announce_on_first_run: options.announce_on_first_run,
    };

    let was_uninitialized = prior_state
        .as_ref()
        .is_none_or(|s| s.announced_ids.is_empty());
    let decision = decide(prior_state.as_ref(), &snapshot, &policy);
    apply_decision(
        config,
        series_cfg,
        site.display_name(),
        &display_title,
        &snapshot,
        decision,
        was_uninitialized,
        response_etag,
        now_unix,
        state,
        state_path,
        discord_client,
        gone_webhooks,
        options,
    )
    .await
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn apply_decision(
    config: &Config,
    series_cfg: &SeriesConfig,
    site_display_name: &str,
    display_title: &str,
    snapshot: &SeriesSnapshot,
    decision: Decision,
    was_uninitialized: bool,
    response_etag: Option<String>,
    now_unix: i64,
    state: &mut StateFile,
    state_path: &Path,
    discord_client: &DiscordClient,
    gone_webhooks: &mut HashSet<WebhookUrl>,
    options: CheckOptions,
) -> SingleSeriesOutcome {
    let state_key = series_cfg.key.as_state_key();
    let head_label = snapshot
        .chapters_newest_first
        .first()
        .map(|c| c.label.clone());

    match decision {
        Decision::Baseline { ids, head_label } => {
            if options.dry_run {
                tracing::info!(
                    series = %state_key,
                    title = %display_title,
                    head = %head_label,
                    "dry-run: would establish baseline at {head_label}"
                );
                if !options.json {
                    println!("would establish baseline at {head_label}");
                }
                SingleSeriesOutcome {
                    exit_code: ExitCode::Ok,
                    failure_kind: None,
                    report: SeriesCheckReport {
                        series_key: state_key,
                        url: series_cfg.fetch_url.to_string(),
                        title: Some(display_title.to_string()),
                        outcome: "baseline".to_string(),
                        head_label: Some(head_label),
                        dry_run: true,
                        ..SeriesCheckReport::default()
                    },
                }
            } else {
                let entry = state.series.entry(state_key.clone()).or_default();
                let current_ids: Vec<String> = snapshot
                    .chapters_newest_first
                    .iter()
                    .map(|c| c.id.clone())
                    .collect();
                entry.announced_ids.clear();
                entry.record_announced_ids(ids, &current_ids);
                entry.last_announced_label = Some(head_label.clone());
                entry.last_snapshot_len = snapshot.chapters_newest_first.len();
                entry.last_checked_unix = Some(now_unix);
                entry.last_success_unix = Some(now_unix);
                entry.consecutive_failures = 0;
                entry.consecutive_anomalies = 0;
                entry.etag = response_etag;

                tracing::info!(
                    series = %state_key,
                    title = %display_title,
                    "baseline established at {head_label}"
                );

                let code = persist_state_if_needed(state, state_path, false);
                SingleSeriesOutcome {
                    exit_code: code,
                    failure_kind: None,
                    report: SeriesCheckReport {
                        series_key: state_key,
                        url: series_cfg.fetch_url.to_string(),
                        title: Some(display_title.to_string()),
                        outcome: "baseline".to_string(),
                        head_label: Some(head_label),
                        dry_run: false,
                        ..SeriesCheckReport::default()
                    },
                }
            }
        }

        Decision::NoChange => {
            if !options.dry_run {
                let entry = state.series.entry(state_key.clone()).or_default();
                entry.last_snapshot_len = snapshot.chapters_newest_first.len();
                entry.consecutive_anomalies = 0;
            }
            tracing::info!(
                series = %state_key,
                title = %display_title,
                head = ?head_label,
                "no new chapters"
            );
            if options.dry_run && !options.json {
                let head_str = head_label.as_deref().unwrap_or("unknown");
                println!("{state_key} ({display_title}): no new chapters (head: {head_str})");
            }
            let code = persist_state_if_needed(state, state_path, options.dry_run);
            SingleSeriesOutcome {
                exit_code: code,
                failure_kind: None,
                report: SeriesCheckReport {
                    series_key: state_key,
                    url: series_cfg.fetch_url.to_string(),
                    title: Some(display_title.to_string()),
                    outcome: "no_change".to_string(),
                    head_label,
                    dry_run: options.dry_run,
                    ..SeriesCheckReport::default()
                },
            }
        }

        Decision::Anomaly(reason) => {
            let current_anomalies = state
                .series
                .get(&state_key)
                .map_or(0, |s| s.consecutive_anomalies)
                + 1;

            if current_anomalies >= REBASELINE_AFTER_CONSECUTIVE_ANOMALIES {
                let new_head = head_label.clone().unwrap_or_default();
                tracing::warn!(
                    series = %state_key,
                    title = %display_title,
                    consecutive_anomalies = current_anomalies,
                    reason = %reason,
                    "reached {REBASELINE_AFTER_CONSECUTIVE_ANOMALIES} consecutive anomalies; re-baselining series at {new_head}"
                );
                if !options.dry_run {
                    let entry = state.series.entry(state_key.clone()).or_default();
                    let all_ids: Vec<String> = snapshot
                        .chapters_newest_first
                        .iter()
                        .rev()
                        .map(|c| c.id.clone())
                        .collect();
                    let current_ids: Vec<String> = snapshot
                        .chapters_newest_first
                        .iter()
                        .map(|c| c.id.clone())
                        .collect();
                    entry.announced_ids.clear();
                    entry.record_announced_ids(all_ids, &current_ids);
                    entry.last_announced_label = Some(new_head);
                    entry.last_snapshot_len = snapshot.chapters_newest_first.len();
                    entry.consecutive_anomalies = 0;
                }
                let code = persist_state_if_needed(state, state_path, options.dry_run);
                SingleSeriesOutcome {
                    exit_code: code,
                    failure_kind: None,
                    report: SeriesCheckReport {
                        series_key: state_key,
                        url: series_cfg.fetch_url.to_string(),
                        title: Some(display_title.to_string()),
                        outcome: "rebaselined".to_string(),
                        head_label,
                        dry_run: options.dry_run,
                        ..SeriesCheckReport::default()
                    },
                }
            } else {
                tracing::warn!(
                    series = %state_key,
                    title = %display_title,
                    consecutive_anomalies = current_anomalies,
                    reason = %reason,
                    "anomaly detected; skipping announcement and preserving announced_ids"
                );
                if !options.dry_run {
                    let entry = state.series.entry(state_key.clone()).or_default();
                    entry.consecutive_anomalies = current_anomalies;
                    // Re-evaluate the page next cycle instead of accepting a 304 for it.
                    entry.etag = None;
                }
                let code = persist_state_if_needed(state, state_path, options.dry_run);
                SingleSeriesOutcome {
                    exit_code: code,
                    failure_kind: None,
                    report: SeriesCheckReport {
                        series_key: state_key,
                        url: series_cfg.fetch_url.to_string(),
                        title: Some(display_title.to_string()),
                        outcome: "anomaly".to_string(),
                        head_label,
                        dry_run: options.dry_run,
                        error: Some(reason),
                        ..SeriesCheckReport::default()
                    },
                }
            }
        }

        Decision::Announce {
            groups,
            folded,
            held_back,
        } => {
            if groups.is_empty() {
                if !options.dry_run {
                    let entry = state.series.entry(state_key.clone()).or_default();
                    entry.last_snapshot_len = snapshot.chapters_newest_first.len();
                    entry.consecutive_anomalies = 0;
                }
                tracing::info!(
                    series = %state_key,
                    title = %display_title,
                    held_back,
                    "new early-access chapter(s) held back waiting for publication date"
                );
                if options.dry_run && !options.json {
                    println!("would hold back {held_back} undated chapter(s) for {display_title}");
                }
                let code = persist_state_if_needed(state, state_path, options.dry_run);
                return SingleSeriesOutcome {
                    exit_code: code,
                    failure_kind: None,
                    report: SeriesCheckReport {
                        series_key: state_key,
                        url: series_cfg.fetch_url.to_string(),
                        title: Some(display_title.to_string()),
                        outcome: "held_back".to_string(),
                        head_label,
                        held_back_parts: held_back,
                        dry_run: options.dry_run,
                        ..SeriesCheckReport::default()
                    },
                };
            }

            let total_parts: usize = groups.iter().map(|g| g.parts.len()).sum();
            if options.dry_run {
                for g in &groups {
                    let labels: Vec<&str> = g.parts.iter().map(|p| p.label.as_str()).collect();
                    tracing::info!(
                        series = %state_key,
                        title = %display_title,
                        parts = ?labels,
                        "dry-run: would announce chapter group"
                    );
                    if !options.json {
                        println!("would announce {} for {display_title}", labels.join(", "));
                    }
                }
                return SingleSeriesOutcome {
                    exit_code: ExitCode::Ok,
                    failure_kind: None,
                    report: SeriesCheckReport {
                        series_key: state_key,
                        url: series_cfg.fetch_url.to_string(),
                        title: Some(display_title.to_string()),
                        outcome: "announced".to_string(),
                        head_label,
                        announced_groups: groups.len(),
                        announced_parts: total_parts,
                        folded_parts: folded.len(),
                        held_back_parts: held_back,
                        dry_run: true,
                        error: None,
                    },
                };
            }

            let Some(ref webhook) = series_cfg.webhook else {
                return SingleSeriesOutcome {
                    exit_code: ExitCode::ConfigOrStateError,
                    failure_kind: Some(FailureKind::Other),
                    report: SeriesCheckReport {
                        series_key: state_key,
                        url: series_cfg.fetch_url.to_string(),
                        title: Some(display_title.to_string()),
                        outcome: "error".to_string(),
                        head_label,
                        dry_run: false,
                        error: Some("missing announcement webhook".to_string()),
                        ..SeriesCheckReport::default()
                    },
                };
            };

            if gone_webhooks.contains(webhook) {
                tracing::warn!(
                    series = %state_key,
                    "skipping announcement because webhook was previously rejected as deleted/revoked"
                );
                return SingleSeriesOutcome {
                    exit_code: ExitCode::WebhookError,
                    failure_kind: Some(FailureKind::Other),
                    report: SeriesCheckReport {
                        series_key: state_key,
                        url: series_cfg.fetch_url.to_string(),
                        title: Some(display_title.to_string()),
                        outcome: "error".to_string(),
                        head_label,
                        dry_run: false,
                        error: Some("announcement webhook is revoked".to_string()),
                        ..SeriesCheckReport::default()
                    },
                };
            }

            let current_snap_ids: Vec<String> = snapshot
                .chapters_newest_first
                .iter()
                .map(|c| c.id.clone())
                .collect();

            let first_run_baseline_ids: Vec<String> =
                if was_uninitialized && options.announce_on_first_run {
                    snapshot
                        .chapters_newest_first
                        .iter()
                        .skip(1)
                        .rev()
                        .map(|c| c.id.clone())
                        .collect()
                } else {
                    Vec::new()
                };

            let mut groups_sent = 0usize;
            let mut parts_sent = 0usize;
            let mut post_error: Option<String> = None;
            let mut state_save_code = ExitCode::Ok;
            let timestamp = now_rfc3339_utc();

            for (idx, group) in groups.iter().enumerate() {
                if idx > 0 {
                    tokio::time::sleep(GROUP_POST_GAP).await;
                }

                let ctx = AnnouncementContext {
                    series_title: display_title,
                    site_display_name,
                    mention: &config.mention,
                    folded_count: if idx == 0 { folded.len() } else { 0 },
                    timestamp_rfc3339: &timestamp,
                };
                let payload = build_announcement_payload(group, &ctx);

                match discord_client
                    .send(webhook, series_cfg.thread_id.as_deref(), &payload)
                    .await
                {
                    Ok(()) => {
                        groups_sent += 1;
                        parts_sent += group.parts.len();

                        let entry = state.series.entry(state_key.clone()).or_default();
                        if idx == 0 {
                            if !first_run_baseline_ids.is_empty() {
                                entry.record_announced_ids(
                                    first_run_baseline_ids.clone(),
                                    &current_snap_ids,
                                );
                            }
                            if !folded.is_empty() {
                                entry.record_announced_ids(folded.clone(), &current_snap_ids);
                            }
                        }

                        let group_ids: Vec<String> =
                            group.parts.iter().map(|p| p.id.clone()).collect();
                        entry.record_announced_ids(group_ids, &current_snap_ids);
                        if let Some(last_part) = group.parts.last() {
                            entry.last_announced_label = Some(last_part.label.clone());
                        }
                        entry.last_snapshot_len = snapshot.chapters_newest_first.len();
                        entry.consecutive_anomalies = 0;

                        // Persist immediately after each successful group POST (§4.4).
                        state_save_code = state_save_code
                            .combine(persist_state_if_needed(state, state_path, false));
                    }
                    Err(err) => {
                        tracing::error!(
                            series = %state_key,
                            group_index = idx,
                            error = %err,
                            "failed to deliver chapter announcement to Discord"
                        );
                        if matches!(err, DiscordError::WebhookGone { .. }) {
                            gone_webhooks.insert(webhook.clone());
                            send_webhook_gone_health_alert(
                                config,
                                display_title,
                                discord_client,
                                gone_webhooks,
                            )
                            .await;
                        }
                        post_error = Some(err.to_string());
                        break;
                    }
                }
            }

            // Persist end-of-cycle timestamps and metadata (§4.4).
            if let Some(entry) = state.series.get_mut(&state_key) {
                entry.last_snapshot_len = snapshot.chapters_newest_first.len();
                entry.consecutive_anomalies = 0;
                if post_error.is_some() {
                    // Undelivered chapters must be retried next cycle: drop the ETag so the
                    // next fetch is a full GET instead of a 304 that would skip `decide()`.
                    entry.etag = None;
                }
            }
            state_save_code =
                state_save_code.combine(persist_state_if_needed(state, state_path, false));

            let mut exit_code = state_save_code;
            if groups_sent > 0 {
                exit_code = exit_code.combine(ExitCode::NewsAnnounced);
            }
            if post_error.is_some() {
                exit_code = exit_code.combine(ExitCode::WebhookError);
            }

            SingleSeriesOutcome {
                exit_code,
                failure_kind: if post_error.is_some() {
                    Some(FailureKind::Other)
                } else {
                    None
                },
                report: SeriesCheckReport {
                    series_key: state_key,
                    url: series_cfg.fetch_url.to_string(),
                    title: Some(display_title.to_string()),
                    outcome: if post_error.is_some() && groups_sent == 0 {
                        "error".to_string()
                    } else {
                        "announced".to_string()
                    },
                    head_label,
                    announced_groups: groups_sent,
                    announced_parts: parts_sent,
                    folded_parts: if groups_sent > 0 { folded.len() } else { 0 },
                    held_back_parts: held_back,
                    dry_run: false,
                    error: post_error,
                },
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn record_series_failure(
    config: &Config,
    series_cfg: &SeriesConfig,
    state: &mut StateFile,
    state_path: &Path,
    discord_client: &DiscordClient,
    gone_webhooks: &mut HashSet<WebhookUrl>,
    error_kind: &str,
    now_unix: i64,
    dry_run: bool,
) -> u32 {
    let state_key = series_cfg.key.as_state_key();
    let display_title = series_cfg.label.as_deref().unwrap_or(&state_key);

    if dry_run {
        return state
            .series
            .get(&state_key)
            .map_or(1, |s| s.consecutive_failures + 1);
    }

    let entry = state.series.entry(state_key.clone()).or_default();
    entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
    entry.last_checked_unix = Some(now_unix);
    let streak = entry.consecutive_failures;
    let should_alert = config.alert_after_failures > 0
        && streak >= config.alert_after_failures
        && !entry.alert_sent;
    let last_success_rel = format_relative_time(entry.last_success_unix, now_unix);

    if should_alert {
        if let Some(ref health_wh) = config.health_webhook {
            if !gone_webhooks.contains(health_wh) {
                let payload = build_health_alert_payload(
                    display_title,
                    error_kind,
                    &last_success_rel,
                    &now_rfc3339_utc(),
                );
                match discord_client.send(health_wh, None, &payload).await {
                    Ok(()) => {
                        if let Some(e) = state.series.get_mut(&state_key) {
                            e.alert_sent = true;
                        }
                        tracing::info!(
                            series = %state_key,
                            consecutive_failures = streak,
                            "sent operational health alert to health webhook"
                        );
                    }
                    Err(err) => {
                        if matches!(err, DiscordError::WebhookGone { .. }) {
                            gone_webhooks.insert(health_wh.clone());
                        }
                        tracing::error!(
                            series = %state_key,
                            error = %err,
                            "failed to deliver health alert to MANGA_WATCHER_HEALTH_WEBHOOK"
                        );
                    }
                }
            }
        } else {
            tracing::warn!(
                series = %state_key,
                consecutive_failures = streak,
                "series reached alert_after_failures threshold, but MANGA_WATCHER_HEALTH_WEBHOOK is not set"
            );
        }
    }

    let _ = persist_state_if_needed(state, state_path, false);
    streak
}

#[allow(clippy::too_many_arguments)]
async fn handle_recovery_if_needed(
    config: &Config,
    state: &mut StateFile,
    state_key: &str,
    display_title: &str,
    discord_client: &DiscordClient,
    gone_webhooks: &mut HashSet<WebhookUrl>,
    now_unix: i64,
    response_etag: Option<String>,
    dry_run: bool,
) {
    if dry_run {
        return;
    }

    let had_alert = state.series.get(state_key).is_some_and(|s| s.alert_sent);

    if let Some(entry) = state.series.get_mut(state_key) {
        entry.consecutive_failures = 0;
        entry.last_checked_unix = Some(now_unix);
        entry.last_success_unix = Some(now_unix);
        entry.etag = response_etag;
        if had_alert {
            entry.alert_sent = false;
        }
    }

    if had_alert {
        if let Some(ref health_wh) = config.health_webhook {
            if !gone_webhooks.contains(health_wh) {
                let payload = build_health_recovery_payload(display_title, &now_rfc3339_utc());
                match discord_client.send(health_wh, None, &payload).await {
                    Ok(()) => {
                        tracing::info!(
                            series = %state_key,
                            "sent operational recovery notice to health webhook"
                        );
                    }
                    Err(err) => {
                        if matches!(err, DiscordError::WebhookGone { .. }) {
                            gone_webhooks.insert(health_wh.clone());
                        }
                        tracing::error!(
                            series = %state_key,
                            error = %err,
                            "failed to deliver health recovery notice"
                        );
                    }
                }
            }
        } else {
            tracing::info!(
                series = %state_key,
                "watcher recovered after previous failure streak (MANGA_WATCHER_HEALTH_WEBHOOK unset)"
            );
        }
    }
}

async fn send_webhook_gone_health_alert(
    config: &Config,
    display_title: &str,
    discord_client: &DiscordClient,
    gone_webhooks: &mut HashSet<WebhookUrl>,
) {
    let Some(ref health_wh) = config.health_webhook else {
        tracing::warn!(
            title = %display_title,
            "announcement webhook returned WebhookGone, and MANGA_WATCHER_HEALTH_WEBHOOK is not configured"
        );
        return;
    };
    if gone_webhooks.contains(health_wh) {
        return;
    }
    let payload = build_webhook_gone_alert_payload(display_title, &now_rfc3339_utc());
    if let Err(err) = discord_client.send(health_wh, None, &payload).await {
        if matches!(err, DiscordError::WebhookGone { .. }) {
            gone_webhooks.insert(health_wh.clone());
        }
        tracing::error!(error = %err, "failed to send WebhookGone alert to health webhook");
    }
}

fn persist_state_if_needed(state: &StateFile, state_path: &Path, dry_run: bool) -> ExitCode {
    if dry_run {
        return ExitCode::Ok;
    }
    match state.save_atomic(state_path) {
        Ok(()) => ExitCode::Ok,
        Err(err) => {
            tracing::error!(error = %err, "failed to persist state file");
            ExitCode::ConfigOrStateError
        }
    }
}

fn classify_parse_error(err: &ParseError) -> String {
    match err {
        ParseError::NextDataMissing => "missing __NEXT_DATA__".to_string(),
        ParseError::InvalidJson(_) => "invalid JSON".to_string(),
        ParseError::SchemaMismatch(path) => format!("schema mismatch at {path}"),
        ParseError::PageError(_) => "site page error".to_string(),
        ParseError::EmptyChapterList => "empty chapter list".to_string(),
    }
}

/// Formats a past Unix timestamp relative to `now_unix` for health alerts.
#[must_use]
pub fn format_relative_time(last_success_unix: Option<i64>, now_unix: i64) -> String {
    let Some(ts) = last_success_unix else {
        return "never".to_string();
    };
    let delta = (now_unix - ts).max(0);
    if delta < 60 {
        format!("{delta}s ago")
    } else if delta < 3600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86_400 {
        format!("{}h ago", delta / 3600)
    } else {
        format!("{}d ago", delta / 86_400)
    }
}

/// Runs the long-lived polling loop (`manga-watcher watch`, §7).
#[allow(clippy::too_many_lines)]
pub async fn run_watch(config: &Config, registry: &Registry, state_path: &Path) -> ExitCode {
    let _lock = match InstanceLock::acquire_for_state(state_path) {
        Ok(lock) => lock,
        Err(StateError::Locked(p)) => {
            tracing::error!(lock_path = %p.display(), "another instance is already running");
            return ExitCode::AnotherInstanceRunning;
        }
        Err(err) => {
            tracing::error!(error = %err, "failed to acquire state lock");
            return ExitCode::ConfigOrStateError;
        }
    };

    let mut state = match StateFile::load(state_path) {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(error = %err, "failed to load state file");
            return ExitCode::ConfigOrStateError;
        }
    };

    if config.health_webhook.is_none() && config.alert_after_failures > 0 {
        tracing::warn!(
            "MANGA_WATCHER_HEALTH_WEBHOOK is not set; operational health alerts are disabled"
        );
    }

    let Ok(http_client) = SiteHttpClient::new(&config.user_agent, config.timeout) else {
        return ExitCode::ConfigOrStateError;
    };
    let Ok(discord_client) = DiscordClient::new(&config.user_agent, config.timeout) else {
        return ExitCode::ConfigOrStateError;
    };

    let mut gone_webhooks: HashSet<WebhookUrl> = HashSet::new();
    let now = SystemTime::now();
    // On startup, all series are immediately due (§7).
    let mut next_due: Vec<SystemTime> = vec![now; config.series.len()];

    tracing::info!(
        series_count = config.series.len(),
        "starting watch loop (press Ctrl-C to stop)"
    );

    loop {
        let check_now = SystemTime::now();
        let mut ran_any_in_pass = false;

        for (idx, series_cfg) in config.series.iter().enumerate() {
            if SystemTime::now() < next_due[idx] {
                continue;
            }

            if ran_any_in_pass {
                // Enforce >= 2s gap between requests while listening for shutdown signal.
                tokio::select! {
                    () = shutdown_signal() => {
                        tracing::info!("received shutdown signal; persisting state and exiting");
                        let _ = state.save_atomic(state_path);
                        return ExitCode::Ok;
                    }
                    () = tokio::time::sleep(INTER_SERIES_GAP) => {}
                }
            }

            let check_fut = check_single_series(
                config,
                series_cfg,
                registry,
                &http_client,
                &discord_client,
                &mut state,
                state_path,
                &mut gone_webhooks,
                CheckOptions::default(),
            );

            let outcome = tokio::select! {
                () = shutdown_signal() => {
                    tracing::info!("received shutdown signal during request; persisting state and exiting");
                    let _ = state.save_atomic(state_path);
                    return ExitCode::Ok;
                }
                out = check_fut => out,
            };

            ran_any_in_pass = true;
            let delay = compute_next_delay(
                series_cfg.interval,
                config.jitter_seconds,
                outcome.failure_kind,
            );
            next_due[idx] = SystemTime::now() + delay;

            tracing::debug!(
                series = %series_cfg.key,
                next_poll_in_secs = delay.as_secs(),
                "scheduled next poll for series"
            );
        }

        // Sleep min(time until earliest next_due, 60s) and re-check the wall clock (§7).
        let earliest = next_due
            .iter()
            .copied()
            .min()
            .unwrap_or(check_now + WATCH_MAX_SLEEP_SLICE);

        let until_earliest = earliest
            .duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO);
        let sleep_dur = until_earliest.min(WATCH_MAX_SLEEP_SLICE);

        if sleep_dur > Duration::ZERO {
            tokio::select! {
                () = shutdown_signal() => {
                    tracing::info!("received shutdown signal; persisting state and exiting");
                    let _ = state.save_atomic(state_path);
                    return ExitCode::Ok;
                }
                () = tokio::time::sleep(sleep_dur) => {}
            }
        }
    }
}

#[allow(clippy::missing_const_for_fn)]
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async {
                if let Some(ref mut s) = sigterm {
                    s.recv().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Sends a test embed via the configured announcement or health webhook (`manga-watcher test-webhook`).
pub async fn run_test_webhook(
    config: &Config,
    series_url_filter: Option<&str>,
    test_health: bool,
) -> ExitCode {
    let Ok(discord_client) = DiscordClient::new(&config.user_agent, config.timeout) else {
        return ExitCode::WebhookError;
    };
    let timestamp = now_rfc3339_utc();

    if test_health {
        let Some(ref health_wh) = config.health_webhook else {
            tracing::error!(
                "cannot test health webhook: MANGA_WATCHER_HEALTH_WEBHOOK is not set in .env"
            );
            return ExitCode::ConfigOrStateError;
        };
        let payload = build_test_webhook_payload("health alert channel", &timestamp);
        return match discord_client.send(health_wh, None, &payload).await {
            Ok(()) => {
                println!("Successfully delivered test message to MANGA_WATCHER_HEALTH_WEBHOOK.");
                ExitCode::Ok
            }
            Err(err) => {
                tracing::error!(error = %err, "health webhook test failed");
                ExitCode::WebhookError
            }
        };
    }

    if let Some(target_url) = series_url_filter {
        let matched = config.series.iter().find(|s| {
            s.configured_url.as_str() == target_url.trim()
                || s.fetch_url.as_str() == target_url.trim()
                || s.key.as_state_key() == target_url.trim()
        });
        let Some(series_cfg) = matched else {
            tracing::error!(
                series = target_url,
                "no configured series matched --series filter"
            );
            return ExitCode::ConfigOrStateError;
        };
        let Some(ref wh) = series_cfg.webhook else {
            tracing::error!(
                series = %series_cfg.key,
                "series has no resolved announcement webhook"
            );
            return ExitCode::ConfigOrStateError;
        };
        let label = format!("series {}", series_cfg.key);
        let payload = build_test_webhook_payload(&label, &timestamp);
        return match discord_client
            .send(wh, series_cfg.thread_id.as_deref(), &payload)
            .await
        {
            Ok(()) => {
                println!(
                    "Successfully delivered test message for {}.",
                    series_cfg.key
                );
                ExitCode::Ok
            }
            Err(err) => {
                tracing::error!(series = %series_cfg.key, error = %err, "test-webhook failed");
                ExitCode::WebhookError
            }
        };
    }

    let targets = config.distinct_announcement_webhooks();
    if targets.is_empty() {
        tracing::error!("no announcement webhooks configured");
        return ExitCode::ConfigOrStateError;
    }

    let mut code = ExitCode::Ok;
    for (idx, (wh, thread_id)) in targets.iter().enumerate() {
        if idx > 0 {
            tokio::time::sleep(GROUP_POST_GAP).await;
        }
        let label = format!("announcement webhook #{}", idx + 1);
        let payload = build_test_webhook_payload(&label, &timestamp);
        match discord_client.send(wh, *thread_id, &payload).await {
            Ok(()) => {
                println!("Successfully delivered test message to {label}.");
            }
            Err(err) => {
                tracing::error!(webhook_index = idx + 1, error = %err, "test-webhook failed");
                code = code.combine(ExitCode::WebhookError);
            }
        }
    }
    code
}

/// Prints the status of configured series and orphaned state entries (`manga-watcher list`, §6).
pub async fn run_list(
    config: &Config,
    registry: &Registry,
    state_path: &Path,
    offline: bool,
) -> ExitCode {
    let state = match StateFile::load(state_path) {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(error = %err, "failed to load state file");
            return ExitCode::ConfigOrStateError;
        }
    };

    let http_client = if offline {
        None
    } else {
        match SiteHttpClient::new(&config.user_agent, config.timeout) {
            Ok(c) => Some(c),
            Err(err) => {
                tracing::error!(error = %err, "failed to build HTTP client");
                return ExitCode::NetworkError;
            }
        }
    };

    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));

    let mut configured_keys = HashSet::with_capacity(config.series.len());
    let mut exit_code = ExitCode::Ok;

    for series_cfg in &config.series {
        let state_key = series_cfg.key.as_state_key();
        configured_keys.insert(state_key.clone());

        let series_state: Option<&SeriesState> = state.series.get(&state_key);
        let initialized = series_state.is_some();
        let last_announced = series_state
            .and_then(|s| s.last_announced_label.as_deref())
            .unwrap_or("none");
        let last_success =
            format_relative_time(series_state.and_then(|s| s.last_success_unix), now_unix);
        let failures = series_state.map_or(0, |s| s.consecutive_failures);

        let mut live_head = String::from("(offline)");
        if let Some(ref client) = http_client
            && let Some(site) = registry.get_by_id(&series_cfg.key.site_id)
        {
            match client.fetch(&series_cfg.fetch_url, None).await {
                Ok(FetchOutcome::Fetched { body, .. }) => {
                    match site.parse(&series_cfg.key.key, &body) {
                        Ok(snap) => {
                            let head = snap
                                .chapters_newest_first
                                .first()
                                .map_or("empty", |c| c.label.as_str());
                            live_head = format!("{} — {}", snap.title, head);
                        }
                        Err(err) => {
                            live_head = format!("(parse error: {err})");
                            exit_code = exit_code.combine(ExitCode::ParseError);
                        }
                    }
                }
                Ok(FetchOutcome::NotModified { .. }) => {
                    live_head = "(not modified)".to_string();
                }
                Err(err) => {
                    live_head = format!("(network error: {err})");
                    exit_code = exit_code.combine(ExitCode::NetworkError);
                }
            }
        }

        println!(
            "{state_key} | url={} | initialized={initialized} | last_announced={last_announced} | last_success={last_success} | failures={failures} | live={live_head}",
            series_cfg.fetch_url
        );
    }

    let orphans: Vec<&String> = state
        .series
        .keys()
        .filter(|k| !configured_keys.contains(*k))
        .collect();

    if !orphans.is_empty() {
        println!("Orphaned state entries (present in state.json but not in config.toml):");
        for key in orphans {
            println!("  - {key}");
        }
    }

    exit_code
}

/// Removes one or all series state entries so the next run re-baselines (`manga-watcher reset`, §6).
pub fn run_reset(
    state_path: &Path,
    registry: &Registry,
    target_url_or_key: Option<&str>,
    all: bool,
) -> ExitCode {
    let _lock = match InstanceLock::acquire_for_state(state_path) {
        Ok(lock) => lock,
        Err(StateError::Locked(p)) => {
            tracing::error!(lock_path = %p.display(), "another instance is already running");
            return ExitCode::AnotherInstanceRunning;
        }
        Err(err) => {
            tracing::error!(error = %err, "failed to acquire state lock");
            return ExitCode::ConfigOrStateError;
        }
    };

    if all {
        let fresh = StateFile::default();
        return match fresh.save_atomic(state_path) {
            Ok(()) => {
                println!("Reset all series state in `{}`.", state_path.display());
                ExitCode::Ok
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to write reset state file");
                ExitCode::ConfigOrStateError
            }
        };
    }

    let Some(raw_target) = target_url_or_key.map(str::trim).filter(|s| !s.is_empty()) else {
        tracing::error!("specify either a <series-url> or --all for `reset`");
        return ExitCode::ConfigOrStateError;
    };

    let resolved_key = if let Ok(parsed_url) = reqwest::Url::parse(raw_target)
        && let Some(resolved) = registry.resolve_url(&parsed_url)
    {
        resolved.series_key.as_state_key()
    } else if raw_target.contains(':') {
        raw_target.to_string()
    } else {
        tracing::error!(
            target = raw_target,
            "series URL does not match any registered site"
        );
        return ExitCode::ConfigOrStateError;
    };

    let mut state = match StateFile::load(state_path) {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(error = %err, "failed to load state file (use `reset --all` to overwrite a corrupt state file)");
            return ExitCode::ConfigOrStateError;
        }
    };

    if state.series.remove(&resolved_key).is_some() {
        match state.save_atomic(state_path) {
            Ok(()) => {
                println!("Reset state for `{resolved_key}`.");
                ExitCode::Ok
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to save state file");
                ExitCode::ConfigOrStateError
            }
        }
    } else {
        println!("No state entry found for `{resolved_key}` (nothing to reset).");
        ExitCode::Ok
    }
}
