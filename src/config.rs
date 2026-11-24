//! Configuration loading (`config.toml`), `.env` secret resolution, and validation.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::Url;
use serde::Deserialize;
use thiserror::Error;

use crate::model::SeriesKey;
use crate::paths::{PathError, resolve_config_path, resolve_env_file_path};
use crate::sites::Registry;

/// Default polling interval in minutes.
pub const DEFAULT_INTERVAL_MINUTES: u64 = 30;

/// Minimum allowed polling interval in minutes.
pub const MIN_INTERVAL_MINUTES: u64 = 10;

/// Default uniform jitter range in seconds (`[-jitter, +jitter]`).
pub const DEFAULT_JITTER_SECONDS: u64 = 120;

/// Default HTTP request timeout in seconds.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 20;

/// Default descriptive User-Agent header value.
pub const DEFAULT_USER_AGENT: &str = "BesideME/0.1 (+personal use)";

/// Default maximum chapter-group messages sent per series per cycle.
pub const DEFAULT_MAX_MESSAGES_PER_CYCLE: usize = 3;

/// Default consecutive failure threshold before sending a health alert.
pub const DEFAULT_ALERT_AFTER_FAILURES: u32 = 6;

/// Default mention string for chapter announcements.
pub const DEFAULT_MENTION: &str = "@everyone";

/// Default environment variable name for chapter announcement webhooks.
pub const DEFAULT_WEBHOOK_ENV: &str = "BESIDEME_WEBHOOK";

/// Environment variable name for health alert webhooks.
pub const HEALTH_WEBHOOK_ENV: &str = "BESIDEME_HEALTH_WEBHOOK";

pub(crate) const LEGACY_UPDATES_ENV: &str = "MANGA_WATCHER_WEBHOOK";
pub(crate) const LEGACY_HEALTH_ENV: &str = "MANGA_WATCHER_HEALTH_WEBHOOK";

pub(crate) fn legacy_webhook_key(key: &str) -> Option<&'static str> {
    match key {
        DEFAULT_WEBHOOK_ENV => Some(LEGACY_UPDATES_ENV),
        HEALTH_WEBHOOK_ENV => Some(LEGACY_HEALTH_ENV),
        _ => None,
    }
}

pub(crate) fn process_webhook_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .or_else(|| legacy_webhook_key(key).and_then(|old| std::env::var(old).ok()))
}

pub(crate) fn file_webhook_value(vars: &BTreeMap<String, String>, key: &str) -> Option<String> {
    vars.get(key)
        .or_else(|| legacy_webhook_key(key).and_then(|old| vars.get(old)))
        .cloned()
}

/// Redacted replacement text always used by [`WebhookUrl`]'s `Debug` and `Display`.
pub const REDACTED_WEBHOOK: &str = "https://discord.com/api/webhooks/***";

const ALLOWED_DISCORD_HOSTS: &[&str] = &[
    "discord.com",
    "discordapp.com",
    "ptb.discord.com",
    "canary.discord.com",
];

/// Errors returned while loading or validating `config.toml` and `.env`.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Failed to resolve config or `.env` file path.
    #[error(transparent)]
    Path(#[from] PathError),

    /// Failed to read `config.toml` from disk.
    #[error("failed to read config file `{}`: {source}", path.display())]
    ReadConfig {
        /// Path to the config file.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Failed to parse `.env` file.
    #[error("failed to read .env file `{}`: {message}", path.display())]
    Dotenv {
        /// Path to the `.env` file.
        path: PathBuf,
        /// Error description (without secret values).
        message: String,
    },

    /// A webhook or secret key was placed inside `config.toml`.
    #[error(
        "invalid key in config.toml: secrets such as webhook URLs must be stored in .env (e.g. {DEFAULT_WEBHOOK_ENV}), never in config.toml ({0})"
    )]
    SecretInConfig(String),

    /// Invalid TOML syntax or unknown configuration field.
    #[error("failed to parse config.toml: {0}")]
    TomlParse(String),

    /// Invalid global configuration parameter.
    #[error("invalid configuration parameter `{field}`: {reason}")]
    InvalidParameter {
        /// Parameter name.
        field: &'static str,
        /// Explanation of why the value is invalid.
        reason: String,
    },

    /// Series URL could not be parsed as a valid URL.
    #[error("invalid series URL `{url}`: {reason}")]
    InvalidSeriesUrl {
        /// Raw URL string from config.
        url: String,
        /// Parse error message.
        reason: String,
    },

    /// Series URL does not match any registered site in [`Registry`].
    #[error("series URL `{0}` does not match any registered site")]
    UnsupportedSiteUrl(String),

    /// Explicit `site` key on `[[series]]` does not match the site inferred from `url`.
    #[error("series `{url}` specifies site `{configured}`, but URL resolves to site `{resolved}`")]
    SiteMismatch {
        /// Series URL.
        url: String,
        /// Configured `site` attribute.
        configured: String,
        /// Resolved site ID.
        resolved: String,
    },

    /// The same series (site + key) appears more than once in `config.toml`.
    #[error("duplicate series `{0}` in config.toml")]
    DuplicateSeries(String),

    /// Required webhook environment variable is missing or empty.
    #[error(
        "missing required webhook environment variable `{env_var}` for series `{series}` (set it in .env)"
    )]
    MissingWebhookEnv {
        /// Name of the environment variable.
        env_var: String,
        /// Series key or URL.
        series: String,
    },

    /// Webhook URL in an environment variable failed validation.
    #[error("invalid webhook URL in environment variable `{env_var}`: {reason}")]
    InvalidWebhook {
        /// Name of the environment variable (never the value).
        env_var: String,
        /// Validation failure description (never contains the URL or token).
        reason: String,
    },
}

/// A validated Discord webhook URL whose `Debug` and `Display` implementations are always redacted.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct WebhookUrl {
    inner: Url,
}

impl WebhookUrl {
    /// Parses and validates a webhook URL from an environment variable.
    ///
    /// Never includes the raw input string or token in any returned [`ConfigError`].
    pub fn parse(env_var: &str, raw: &str, allow_any_webhook: bool) -> Result<Self, ConfigError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(ConfigError::InvalidWebhook {
                env_var: env_var.to_string(),
                reason: "value is empty".to_string(),
            });
        }

        let url = Url::parse(trimmed).map_err(|_| ConfigError::InvalidWebhook {
            env_var: env_var.to_string(),
            reason: "not a valid URL".to_string(),
        })?;

        let scheme = url.scheme();
        let host = url.host_str().ok_or_else(|| ConfigError::InvalidWebhook {
            env_var: env_var.to_string(),
            reason: "URL is missing a host".to_string(),
        })?;

        if allow_any_webhook {
            let scheme_ok = scheme == "https" || (scheme == "http" && is_loopback_host(host));
            if !scheme_ok {
                return Err(ConfigError::InvalidWebhook {
                    env_var: env_var.to_string(),
                    reason: "must use https (http is only allowed for loopback hosts with --allow-any-webhook)"
                        .to_string(),
                });
            }
        } else {
            if scheme != "https" {
                return Err(ConfigError::InvalidWebhook {
                    env_var: env_var.to_string(),
                    reason: "scheme must be https".to_string(),
                });
            }
            let host_lower = host.to_ascii_lowercase();
            if !ALLOWED_DISCORD_HOSTS.contains(&host_lower.as_str()) {
                return Err(ConfigError::InvalidWebhook {
                    env_var: env_var.to_string(),
                    reason: "host must be discord.com, discordapp.com, ptb.discord.com, or canary.discord.com"
                        .to_string(),
                });
            }
        }

        // Validate path: /api/webhooks/<digits>/<token>
        let mut segments: Vec<&str> = url
            .path_segments()
            .ok_or_else(|| ConfigError::InvalidWebhook {
                env_var: env_var.to_string(),
                reason: "webhook URL has no path segments".to_string(),
            })?
            .collect();

        if segments.last() == Some(&"") {
            segments.pop();
        }

        match segments.as_slice() {
            ["api", "webhooks", id, token]
                if !id.is_empty()
                    && id.bytes().all(|b| b.is_ascii_digit())
                    && !token.is_empty()
                    && !token.chars().any(char::is_whitespace) => {}
            _ => {
                return Err(ConfigError::InvalidWebhook {
                    env_var: env_var.to_string(),
                    reason: "path must match /api/webhooks/<digits>/<token>".to_string(),
                });
            }
        }

        Ok(Self { inner: url })
    }

    /// Exposes the underlying [`Url`] for performing the HTTP POST request.
    ///
    /// Callers must never log or display the returned URL.
    #[must_use]
    pub(crate) fn expose_url(&self) -> &Url {
        &self.inner
    }
}

impl fmt::Debug for WebhookUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED_WEBHOOK)
    }
}

impl fmt::Display for WebhookUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED_WEBHOOK)
    }
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    unbracketed
        .parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// Options controlling how configuration and environment variables are loaded.
#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    /// Explicit `--config` file path.
    pub config_path: Option<PathBuf>,
    /// Explicit `--env-file` path.
    pub env_file_path: Option<PathBuf>,
    /// Allow non-Discord hosts and loopback `http` webhooks (`--allow-any-webhook`).
    pub allow_any_webhook: bool,
    /// Allow missing announcement webhooks (true for `check --dry-run` and `list`).
    pub allow_missing_webhooks: bool,
}

/// Validated runtime configuration for `besideme-cli`.
#[derive(Debug, Clone)]
pub struct Config {
    /// Path from which `config.toml` was loaded (if loaded from disk).
    pub config_path: Option<PathBuf>,
    /// Path from which `.env` was loaded (if any `.env` file was found).
    pub env_file_path: Option<PathBuf>,
    /// Global default polling interval (`>= 10` minutes).
    pub interval: Duration,
    /// Uniform jitter bound in seconds (`[-jitter, +jitter]`).
    pub jitter_seconds: u64,
    /// HTTP total request timeout.
    pub timeout: Duration,
    /// Descriptive HTTP User-Agent string.
    pub user_agent: String,
    /// Global default for requiring a publication date before announcing.
    pub require_published: bool,
    /// Maximum chapter-group messages per series per cycle.
    pub max_messages_per_cycle: usize,
    /// Consecutive failures before sending one health alert (`0` = disabled).
    pub alert_after_failures: u32,
    /// Mention string prepended to announcement message `content` (`""` = no ping).
    pub mention: String,
    /// Optional global Discord thread/forum post ID.
    pub thread_id: Option<String>,
    /// Optional separate health alert webhook (`BESIDEME_HEALTH_WEBHOOK`).
    pub health_webhook: Option<WebhookUrl>,
    /// Configured series entries in declaration order.
    pub series: Vec<SeriesConfig>,
}

/// Validated configuration for a single watched manga series.
#[derive(Debug, Clone)]
pub struct SeriesConfig {
    /// Original URL from `config.toml`.
    pub configured_url: Url,
    /// Canonical fetch URL produced by the matched [`Site`](crate::sites::Site).
    pub fetch_url: Url,
    /// Resolved composite key (`"<site_id>:<key>"`, e.g. `"mangaup:261"`).
    pub key: SeriesKey,
    /// Optional human title override from config (`label`).
    pub label: Option<String>,
    /// Name of the environment variable used to resolve this series' announcement webhook.
    pub webhook_env: String,
    /// Resolved announcement webhook (`None` only when `allow_missing_webhooks` is true and env var is unset).
    pub webhook: Option<WebhookUrl>,
    /// Optional Discord thread ID for this series.
    pub thread_id: Option<String>,
    /// Polling interval for this series (`>= 10` minutes).
    pub interval: Duration,
    /// Whether undated (early-access) chapters are held back for this series.
    pub require_published: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default = "default_interval_minutes")]
    interval_minutes: u64,
    #[serde(default = "default_jitter_seconds")]
    jitter_seconds: u64,
    #[serde(default = "default_timeout_seconds")]
    timeout_seconds: u64,
    #[serde(default = "default_user_agent")]
    user_agent: String,
    #[serde(default)]
    require_published: bool,
    #[serde(default = "default_max_messages_per_cycle")]
    max_messages_per_cycle: usize,
    #[serde(default = "default_alert_after_failures")]
    alert_after_failures: u32,
    #[serde(default = "default_mention")]
    mention: String,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    series: Vec<RawSeriesConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSeriesConfig {
    url: String,
    #[serde(default)]
    site: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    webhook_env: Option<String>,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    interval_minutes: Option<u64>,
    #[serde(default)]
    require_published: Option<bool>,
}

const fn default_interval_minutes() -> u64 {
    DEFAULT_INTERVAL_MINUTES
}
const fn default_jitter_seconds() -> u64 {
    DEFAULT_JITTER_SECONDS
}
const fn default_timeout_seconds() -> u64 {
    DEFAULT_TIMEOUT_SECONDS
}
fn default_user_agent() -> String {
    DEFAULT_USER_AGENT.to_string()
}
const fn default_max_messages_per_cycle() -> usize {
    DEFAULT_MAX_MESSAGES_PER_CYCLE
}
const fn default_alert_after_failures() -> u32 {
    DEFAULT_ALERT_AFTER_FAILURES
}
fn default_mention() -> String {
    DEFAULT_MENTION.to_string()
}

impl Config {
    /// Resolves paths, loads `.env` and `config.toml`, and validates everything against `registry`.
    pub fn load(options: &LoadOptions, registry: &Registry) -> Result<Self, ConfigError> {
        let config_path = resolve_config_path(options.config_path.as_deref())?;
        let env_file_path = resolve_env_file_path(options.env_file_path.as_deref(), &config_path)?;

        let dotenv_vars = if let Some(ref env_path) = env_file_path {
            let map = parse_dotenv_file(env_path)?;
            tracing::info!(path = %env_path.display(), "loaded .env file");
            map
        } else {
            BTreeMap::new()
        };

        let toml_text =
            std::fs::read_to_string(&config_path).map_err(|source| ConfigError::ReadConfig {
                path: config_path.clone(),
                source,
            })?;

        let mut cfg = Self::from_toml_str(&toml_text, options, registry, |key| {
            process_webhook_value(key).or_else(|| file_webhook_value(&dotenv_vars, key))
        })?;
        cfg.config_path = Some(config_path);
        cfg.env_file_path = env_file_path;
        Ok(cfg)
    }

    /// Parses and validates configuration from a TOML string and a custom environment lookup closure.
    ///
    /// This function never touches the filesystem or process-global environment, making it ideal for tests.
    #[allow(clippy::too_many_lines)]
    pub fn from_toml_str<F>(
        toml_text: &str,
        options: &LoadOptions,
        registry: &Registry,
        env_lookup: F,
    ) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let raw: RawConfig = toml::from_str(toml_text).map_err(|err| {
            let msg = err.to_string();
            let lower = msg.to_ascii_lowercase();
            if lower.contains("unknown field")
                && (lower.contains("webhook")
                    || lower.contains("secret")
                    || lower.contains("token"))
            {
                ConfigError::SecretInConfig(msg)
            } else {
                ConfigError::TomlParse(msg)
            }
        })?;

        if raw.timeout_seconds == 0 {
            return Err(ConfigError::InvalidParameter {
                field: "timeout_seconds",
                reason: "must be at least 1 second".to_string(),
            });
        }
        if raw.user_agent.trim().is_empty() {
            return Err(ConfigError::InvalidParameter {
                field: "user_agent",
                reason: "must not be empty".to_string(),
            });
        }
        if raw.max_messages_per_cycle == 0 {
            return Err(ConfigError::InvalidParameter {
                field: "max_messages_per_cycle",
                reason: "must be at least 1".to_string(),
            });
        }
        if raw.series.is_empty() {
            // A fresh install starts empty; manga are added later from the GUI.
            tracing::warn!("config.toml has no [[series]] entries; nothing will be watched");
        }

        let global_interval_mins = clamp_interval_minutes(raw.interval_minutes, "global");
        let global_thread_id = normalize_optional_str(raw.thread_id);

        let health_webhook = match env_lookup(HEALTH_WEBHOOK_ENV) {
            Some(val) if !val.trim().is_empty() => Some(WebhookUrl::parse(
                HEALTH_WEBHOOK_ENV,
                &val,
                options.allow_any_webhook,
            )?),
            _ => None,
        };

        let mut seen_keys = HashSet::with_capacity(raw.series.len());
        let mut series = Vec::with_capacity(raw.series.len());

        for raw_series in raw.series {
            let parsed_url =
                Url::parse(raw_series.url.trim()).map_err(|e| ConfigError::InvalidSeriesUrl {
                    url: raw_series.url.clone(),
                    reason: e.to_string(),
                })?;

            let resolved = registry
                .resolve_url(&parsed_url)
                .ok_or_else(|| ConfigError::UnsupportedSiteUrl(raw_series.url.clone()))?;

            if let Some(ref explicit_site) = raw_series.site {
                let trimmed_site = explicit_site.trim();
                if trimmed_site != resolved.site.id() {
                    return Err(ConfigError::SiteMismatch {
                        url: raw_series.url.clone(),
                        configured: trimmed_site.to_string(),
                        resolved: resolved.site.id().to_string(),
                    });
                }
            }

            if !seen_keys.insert(resolved.series_key.clone()) {
                return Err(ConfigError::DuplicateSeries(
                    resolved.series_key.as_state_key(),
                ));
            }

            let webhook_env = raw_series
                .webhook_env
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(DEFAULT_WEBHOOK_ENV)
                .to_string();

            let webhook = match env_lookup(&webhook_env) {
                Some(val) if !val.trim().is_empty() => Some(WebhookUrl::parse(
                    &webhook_env,
                    &val,
                    options.allow_any_webhook,
                )?),
                _ if options.allow_missing_webhooks => None,
                _ => {
                    return Err(ConfigError::MissingWebhookEnv {
                        env_var: webhook_env,
                        series: resolved.series_key.as_state_key(),
                    });
                }
            };

            let series_interval_mins = raw_series
                .interval_minutes
                .map_or(global_interval_mins, |mins| {
                    clamp_interval_minutes(mins, &resolved.series_key.as_state_key())
                });

            let thread_id =
                normalize_optional_str(raw_series.thread_id).or_else(|| global_thread_id.clone());
            let label = normalize_optional_str(raw_series.label);
            let require_published = raw_series
                .require_published
                .unwrap_or(raw.require_published);

            series.push(SeriesConfig {
                configured_url: parsed_url,
                fetch_url: resolved.canonical_url,
                key: resolved.series_key,
                label,
                webhook_env,
                webhook,
                thread_id,
                interval: Duration::from_secs(series_interval_mins * 60),
                require_published,
            });
        }

        Ok(Self {
            config_path: None,
            env_file_path: None,
            interval: Duration::from_secs(global_interval_mins * 60),
            jitter_seconds: raw.jitter_seconds,
            timeout: Duration::from_secs(raw.timeout_seconds),
            user_agent: raw.user_agent.trim().to_string(),
            require_published: raw.require_published,
            max_messages_per_cycle: raw.max_messages_per_cycle,
            alert_after_failures: raw.alert_after_failures,
            mention: raw.mention,
            thread_id: global_thread_id,
            health_webhook,
            series,
        })
    }

    /// Returns all distinct announcement webhooks across configured series, in order of appearance.
    #[must_use]
    pub fn distinct_announcement_webhooks(&self) -> Vec<(&WebhookUrl, Option<&str>)> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for s in &self.series {
            if let Some(ref wh) = s.webhook {
                let key = (wh.clone(), s.thread_id.as_deref());
                if seen.insert(key) {
                    out.push((wh, s.thread_id.as_deref()));
                }
            }
        }
        out
    }
}

/// Parses a `.env` file into a key-value map without mutating the process environment.
pub fn parse_dotenv_file(path: &Path) -> Result<BTreeMap<String, String>, ConfigError> {
    let iter = dotenvy::from_path_iter(path).map_err(|e| ConfigError::Dotenv {
        path: path.to_path_buf(),
        message: sanitize_dotenv_err(&e),
    })?;

    let mut map = BTreeMap::new();
    for item in iter {
        let (k, v) = item.map_err(|e| ConfigError::Dotenv {
            path: path.to_path_buf(),
            message: sanitize_dotenv_err(&e),
        })?;
        map.insert(k, v);
    }
    Ok(map)
}

fn sanitize_dotenv_err(err: &dotenvy::Error) -> String {
    match err {
        dotenvy::Error::LineParse(_, idx) => format!("syntax error in .env at byte offset {idx}"),
        dotenvy::Error::Io(io_err) => format!("I/O error: {io_err}"),
        dotenvy::Error::EnvVar(var_err) => format!("env var error: {var_err}"),
        _ => "invalid .env file".to_string(),
    }
}

fn clamp_interval_minutes(minutes: u64, scope: &str) -> u64 {
    if minutes < MIN_INTERVAL_MINUTES {
        tracing::warn!(
            scope,
            configured_minutes = minutes,
            clamped_minutes = MIN_INTERVAL_MINUTES,
            "interval_minutes is below minimum (10); clamping to 10 minutes"
        );
        MIN_INTERVAL_MINUTES
    } else {
        minutes
    }
}

fn normalize_optional_str(opt: Option<String>) -> Option<String> {
    opt.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
