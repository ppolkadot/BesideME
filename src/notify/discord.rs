//! Discord webhook payload construction (pure) and HTTP POST client with rate-limit handling.

use std::time::Duration;

use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::config::WebhookUrl;
use crate::detect::ChapterGroup;
use crate::model::ChapterRef;

/// Default display username sent in Discord webhook payloads.
pub const WEBHOOK_USERNAME: &str = "Manga Watcher";

/// Embed color for chapter announcements and test messages (`0x3498DB`, blue).
pub const COLOR_ANNOUNCEMENT: u32 = 3_447_003;

/// Embed color for operational health alerts (`0x95A5A6`, grey).
pub const COLOR_HEALTH_ALERT: u32 = 9_807_270;

/// Embed color for operational recovery messages (`0x2ECC71`, green).
pub const COLOR_HEALTH_RECOVERY: u32 = 3_066_993;

/// Footer note added to early-access (undated) chapter embeds.
pub const EARLY_ACCESS_FOOTER: &str = "Early access — no general release date yet";

/// Maximum characters allowed by Discord in an embed `title`.
pub const MAX_TITLE_CHARS: usize = 256;

/// Maximum characters allowed by Discord in an embed `description`.
pub const MAX_DESCRIPTION_CHARS: usize = 4096;

/// Maximum characters allowed by Discord in an embed `footer.text`.
pub const MAX_FOOTER_CHARS: usize = 2048;

/// Maximum characters allowed by Discord in an embed `author.name`.
pub const MAX_AUTHOR_CHARS: usize = 256;

/// Maximum total characters across all embeds in a single message (Discord hard limit is 6000).
pub const MAX_TOTAL_EMBED_CHARS: usize = 5900;

/// Maximum `retry_after` duration (in seconds) we will wait on a `429 Too Many Requests`.
pub const MAX_RETRY_AFTER_SECS: f64 = 60.0;

/// Delay between sequential chapter-group webhook POSTs.
pub const GROUP_POST_GAP: Duration = Duration::from_secs(1);

/// Errors returned when delivering a Discord webhook message.
#[derive(Debug, Error)]
pub enum DiscordError {
    /// Webhook returned `401`, `403`, or `404` — it has been deleted or revoked.
    #[error(
        "Discord webhook returned HTTP {status} (webhook deleted or revoked; update the webhook URL in your .env file)"
    )]
    WebhookGone {
        /// HTTP status code (`401`, `403`, or `404`).
        status: u16,
    },

    /// Webhook returned another `4xx` client error (e.g. `400 Bad Request`).
    #[error("Discord rejected webhook payload with HTTP {status}: {body}")]
    WebhookRejected {
        /// HTTP status code.
        status: u16,
        /// Truncated response body from Discord.
        body: String,
    },

    /// Webhook returned `429 Too Many Requests` and either exceeded 60 s or failed again on retry.
    #[error("Discord rate-limited webhook (HTTP 429, retry_after={retry_after_secs:.1}s)")]
    RateLimited {
        /// Requested retry delay in seconds.
        retry_after_secs: f64,
    },

    /// Transient network or `5xx` server error.
    #[error("transient Discord webhook error: {0}")]
    Transient(String),
}

/// Complete JSON body sent to a Discord webhook endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordPayload {
    /// Display username for the webhook message.
    pub username: String,
    /// Top-level message text (contains the configured mention and chapter summary).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Explicit mention allowlist so site-provided text can never trigger unexpected pings.
    pub allowed_mentions: AllowedMentions,
    /// List of rich embeds (`1..=10`).
    pub embeds: Vec<DiscordEmbed>,
}

/// Discord `allowed_mentions` object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowedMentions {
    /// Allowed mention categories (`["everyone"]` or `[]`).
    pub parse: Vec<String>,
}

impl AllowedMentions {
    /// Creates an [`AllowedMentions`] with no pings enabled (`{"parse": []}`).
    #[must_use]
    pub fn none() -> Self {
        Self { parse: Vec::new() }
    }

    /// Creates an [`AllowedMentions`] based on the configured `mention` string.
    ///
    /// Only enables `"everyone"` when `mention.trim() == "@everyone"`.
    #[must_use]
    pub fn for_mention(mention: &str) -> Self {
        if mention.trim() == "@everyone" {
            Self {
                parse: vec!["everyone".to_string()],
            }
        } else {
            Self::none()
        }
    }
}

/// A single Discord rich embed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordEmbed {
    /// Embed title (chapter label and optional subtitle).
    pub title: String,
    /// Clickable URL on the embed title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Embed description body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// RGB color integer.
    pub color: u32,
    /// RFC 3339 UTC timestamp string.
    pub timestamp: String,
    /// Optional author block (`site.display_name()`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<EmbedAuthor>,
    /// Optional cover thumbnail block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<EmbedThumbnail>,
    /// Optional footer block (early-access note and/or folded chapter count).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footer: Option<EmbedFooter>,
}

/// Author block inside a [`DiscordEmbed`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedAuthor {
    /// Author display name.
    pub name: String,
}

/// Thumbnail block inside a [`DiscordEmbed`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedThumbnail {
    /// Absolute thumbnail URL.
    pub url: String,
}

/// Footer block inside a [`DiscordEmbed`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedFooter {
    /// Footer text.
    pub text: String,
}

/// Input parameters for building a chapter-group announcement payload.
#[derive(Debug, Clone)]
pub struct AnnouncementContext<'a> {
    /// Manga series title (config `label` override if set, otherwise scraped title).
    pub series_title: &'a str,
    /// Site display name for the embed author field (e.g. `"Manga UP!"`).
    pub site_display_name: &'a str,
    /// Configured mention string (e.g. `"@everyone"` or `""`).
    pub mention: &'a str,
    /// Number of earlier chapter parts folded into this first message (`0` if none).
    pub folded_count: usize,
    /// Detection timestamp in RFC 3339 UTC format (e.g. `"2026-11-23T12:00:00Z"`).
    pub timestamp_rfc3339: &'a str,
}

/// Formats an [`OffsetDateTime`] as a UTC RFC 3339 string (e.g. `"2026-11-23T12:00:00Z"`).
#[must_use]
pub fn format_rfc3339_utc(dt: OffsetDateTime) -> String {
    let utc = dt.to_offset(time::UtcOffset::UTC);
    // Discard subsecond nanos for clean RFC 3339 timestamps matching the spec example.
    let truncated = utc.replace_nanosecond(0).unwrap_or(utc);
    truncated
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// Returns the current wall-clock UTC time formatted as RFC 3339.
#[must_use]
pub fn now_rfc3339_utc() -> String {
    format_rfc3339_utc(OffsetDateTime::now_utc())
}

/// Strips control characters (except nothing — all ASCII/Unicode control chars are removed).
#[must_use]
pub fn strip_control_chars(input: &str) -> String {
    input.chars().filter(|c| !c.is_control()).collect()
}

/// Strips `@everyone`, `@here`, and `<@...>` mention tokens from site-provided text so
/// untrusted strings can never trigger a ping in `content`.
#[must_use]
pub fn neutralize_mentions(input: &str) -> String {
    let cleaned = strip_control_chars(input);
    let mut out = String::with_capacity(cleaned.len());
    let mut rest = cleaned.as_str();

    while !rest.is_empty() {
        let lower = rest.to_ascii_lowercase();
        if lower.starts_with("@everyone") {
            out.push_str("everyone");
            rest = &rest["@everyone".len()..];
        } else if lower.starts_with("@here") {
            out.push_str("here");
            rest = &rest["@here".len()..];
        } else if rest.starts_with("<@") {
            if let Some(end_idx) = rest.find('>') {
                rest = &rest[end_idx + 1..];
            } else {
                out.push('<');
                rest = &rest[1..];
            }
        } else if let Some(ch) = rest.chars().next() {
            out.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }

    out
}

/// Escapes Discord markdown characters (`* _ ~ \` | > \`) in site-provided text.
#[must_use]
pub fn escape_discord_markdown(input: &str) -> String {
    let sanitized = neutralize_mentions(input);
    let mut out = String::with_capacity(sanitized.len() + 8);
    for ch in sanitized.chars() {
        if matches!(ch, '*' | '_' | '~' | '`' | '|' | '>' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Truncates a string on a UTF-8 character boundary to at most `max_chars` characters.
///
/// If truncation occurs and `max_chars >= 1`, the final character is replaced with `…`.
#[must_use]
pub fn truncate_chars(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = input.chars().count();
    if count <= max_chars {
        return input.to_string();
    }
    let mut out: String = input.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

/// Purely builds a [`DiscordPayload`] for a single [`ChapterGroup`].
#[must_use]
pub fn build_announcement_payload(
    group: &ChapterGroup,
    ctx: &AnnouncementContext<'_>,
) -> DiscordPayload {
    let escaped_title = escape_discord_markdown(ctx.series_title.trim());
    let safe_title_for_content = truncate_chars(&escaped_title, 256);

    let summary = match group.number {
        Some(n) if group.parts.len() > 1 => {
            format!(
                "New chapter {n} ({} parts) of **{safe_title_for_content}**",
                group.parts.len()
            )
        }
        Some(n) => {
            format!("New chapter {n} of **{safe_title_for_content}**")
        }
        None => {
            let first_label = group.parts.first().map_or("Chapter", |p| p.label.as_str());
            let safe_label = truncate_chars(&escape_discord_markdown(first_label.trim()), 120);
            format!("New release: {safe_label} of **{safe_title_for_content}**")
        }
    };

    let mention_trimmed = ctx.mention.trim();
    let content = if mention_trimmed.is_empty() {
        summary
    } else {
        format!("{mention_trimmed} {summary}")
    };

    let author_name = truncate_chars(
        &neutralize_mentions(ctx.site_display_name.trim()),
        MAX_AUTHOR_CHARS,
    );
    let description_raw = format!("New chapter of **{escaped_title}**");
    let description_base = truncate_chars(&description_raw, MAX_DESCRIPTION_CHARS);

    let mut embeds = Vec::with_capacity(group.parts.len());
    for part in &group.parts {
        embeds.push(build_part_embed(
            part,
            &description_base,
            &author_name,
            ctx.folded_count,
            ctx.timestamp_rfc3339,
        ));
    }

    enforce_total_embed_char_limit(&mut embeds);

    DiscordPayload {
        username: WEBHOOK_USERNAME.to_string(),
        content: Some(content),
        allowed_mentions: AllowedMentions::for_mention(mention_trimmed),
        embeds,
    }
}

fn build_part_embed(
    part: &ChapterRef,
    description: &str,
    author_name: &str,
    folded_count: usize,
    timestamp_rfc3339: &str,
) -> DiscordEmbed {
    let clean_label = neutralize_mentions(part.label.trim());
    let raw_title = match part
        .subtitle
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(sub) => {
            let clean_sub = neutralize_mentions(sub);
            format!("{clean_label} — {clean_sub}")
        }
        None => clean_label,
    };
    let title = truncate_chars(&raw_title, MAX_TITLE_CHARS);

    let mut footer_parts: Vec<String> = Vec::with_capacity(2);
    if part.early_access {
        footer_parts.push(EARLY_ACCESS_FOOTER.to_string());
    }
    if folded_count > 0 {
        footer_parts.push(format!("+{folded_count} earlier chapters since last check"));
    }

    let footer = if footer_parts.is_empty() {
        None
    } else {
        Some(EmbedFooter {
            text: truncate_chars(&footer_parts.join(" · "), MAX_FOOTER_CHARS),
        })
    };

    let thumbnail = part.thumbnail.as_ref().map(|u| EmbedThumbnail {
        url: strip_control_chars(u.trim()),
    });

    let author = if author_name.is_empty() {
        None
    } else {
        Some(EmbedAuthor {
            name: author_name.to_string(),
        })
    };

    DiscordEmbed {
        title,
        url: Some(strip_control_chars(part.url.trim())),
        description: Some(description.to_string()),
        color: COLOR_ANNOUNCEMENT,
        timestamp: timestamp_rfc3339.to_string(),
        author,
        thumbnail,
        footer,
    }
}

fn embed_char_count(embed: &DiscordEmbed) -> usize {
    let title_len = embed.title.chars().count();
    let desc_len = embed
        .description
        .as_deref()
        .map_or(0, |s| s.chars().count());
    let author_len = embed.author.as_ref().map_or(0, |a| a.name.chars().count());
    let footer_len = embed.footer.as_ref().map_or(0, |f| f.text.chars().count());
    title_len + desc_len + author_len + footer_len
}

fn enforce_total_embed_char_limit(embeds: &mut [DiscordEmbed]) {
    let total: usize = embeds.iter().map(embed_char_count).sum();
    if total <= MAX_TOTAL_EMBED_CHARS || embeds.is_empty() {
        return;
    }

    let per_embed_budget = (MAX_TOTAL_EMBED_CHARS / embeds.len()).max(64);
    for embed in embeds {
        let non_desc = embed.title.chars().count()
            + embed.author.as_ref().map_or(0, |a| a.name.chars().count())
            + embed.footer.as_ref().map_or(0, |f| f.text.chars().count());
        let allowed_desc = per_embed_budget.saturating_sub(non_desc).max(16);
        if let Some(ref desc) = embed.description {
            embed.description = Some(truncate_chars(desc, allowed_desc));
        }
    }
}

/// Purely builds an operational health alert [`DiscordPayload`] (never pings anyone).
#[must_use]
pub fn build_health_alert_payload(
    series_title: &str,
    error_kind: &str,
    last_success_relative: &str,
    timestamp_rfc3339: &str,
) -> DiscordPayload {
    let safe_title = escape_discord_markdown(series_title.trim());
    let safe_error = neutralize_mentions(error_kind.trim());
    let safe_rel = neutralize_mentions(last_success_relative.trim());

    let title = truncate_chars(
        &format!(
            "⚠ Watcher can't read {}",
            neutralize_mentions(series_title.trim())
        ),
        MAX_TITLE_CHARS,
    );
    let description = truncate_chars(
        &format!("⚠ Watcher can't read **{safe_title}**: {safe_error}. Last success {safe_rel}."),
        MAX_DESCRIPTION_CHARS,
    );

    DiscordPayload {
        username: WEBHOOK_USERNAME.to_string(),
        content: None,
        allowed_mentions: AllowedMentions::none(),
        embeds: vec![DiscordEmbed {
            title,
            url: None,
            description: Some(description),
            color: COLOR_HEALTH_ALERT,
            timestamp: timestamp_rfc3339.to_string(),
            author: None,
            thumbnail: None,
            footer: None,
        }],
    }
}

/// Purely builds an operational health alert when the announcement webhook itself returns `WebhookGone`.
#[must_use]
pub fn build_webhook_gone_alert_payload(
    series_title: &str,
    timestamp_rfc3339: &str,
) -> DiscordPayload {
    let safe_title = escape_discord_markdown(series_title.trim());
    let title = truncate_chars(
        &format!(
            "⚠ Announcement webhook revoked for {}",
            neutralize_mentions(series_title.trim())
        ),
        MAX_TITLE_CHARS,
    );
    let description = truncate_chars(
        &format!(
            "⚠ The announcement webhook for **{safe_title}** was deleted or revoked; update `.env`."
        ),
        MAX_DESCRIPTION_CHARS,
    );

    DiscordPayload {
        username: WEBHOOK_USERNAME.to_string(),
        content: None,
        allowed_mentions: AllowedMentions::none(),
        embeds: vec![DiscordEmbed {
            title,
            url: None,
            description: Some(description),
            color: COLOR_HEALTH_ALERT,
            timestamp: timestamp_rfc3339.to_string(),
            author: None,
            thumbnail: None,
            footer: None,
        }],
    }
}

/// Purely builds an operational recovery [`DiscordPayload`] (never pings anyone).
#[must_use]
pub fn build_health_recovery_payload(
    series_title: &str,
    timestamp_rfc3339: &str,
) -> DiscordPayload {
    let safe_title = escape_discord_markdown(series_title.trim());
    let title = truncate_chars(
        &format!(
            "✅ Watcher recovered for {}",
            neutralize_mentions(series_title.trim())
        ),
        MAX_TITLE_CHARS,
    );
    let description = truncate_chars(
        &format!("✅ Watcher recovered for **{safe_title}**."),
        MAX_DESCRIPTION_CHARS,
    );

    DiscordPayload {
        username: WEBHOOK_USERNAME.to_string(),
        content: None,
        allowed_mentions: AllowedMentions::none(),
        embeds: vec![DiscordEmbed {
            title,
            url: None,
            description: Some(description),
            color: COLOR_HEALTH_RECOVERY,
            timestamp: timestamp_rfc3339.to_string(),
            author: None,
            thumbnail: None,
            footer: None,
        }],
    }
}

/// Purely builds a `test-webhook` verification [`DiscordPayload`] (never pings anyone).
#[must_use]
pub fn build_test_webhook_payload(target_label: &str, timestamp_rfc3339: &str) -> DiscordPayload {
    let safe_label = escape_discord_markdown(target_label.trim());
    let description = truncate_chars(
        &format!(
            "Test notification from **manga-watcher** ({safe_label}). Webhook delivery is working."
        ),
        MAX_DESCRIPTION_CHARS,
    );

    DiscordPayload {
        username: WEBHOOK_USERNAME.to_string(),
        content: None,
        allowed_mentions: AllowedMentions::none(),
        embeds: vec![DiscordEmbed {
            title: "Manga Watcher — Test Webhook".to_string(),
            url: None,
            description: Some(description),
            color: COLOR_ANNOUNCEMENT,
            timestamp: timestamp_rfc3339.to_string(),
            author: None,
            thumbnail: None,
            footer: Some(EmbedFooter {
                text: "Test message (no @everyone ping)".to_string(),
            }),
        }],
    }
}

#[derive(Debug, Deserialize)]
struct RateLimitBody {
    retry_after: Option<f64>,
}

/// HTTP client for posting [`DiscordPayload`]s to Discord webhooks.
#[derive(Debug, Clone)]
pub struct DiscordClient {
    client: Client,
}

impl DiscordClient {
    /// Creates a new [`DiscordClient`] with the specified request timeout and User-Agent.
    pub fn new(user_agent: &str, timeout: Duration) -> Result<Self, DiscordError> {
        let client = Client::builder()
            .user_agent(user_agent)
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| DiscordError::Transient(format!("failed to build HTTP client: {e}")))?;
        Ok(Self { client })
    }

    /// Sends a [`DiscordPayload`] to `<webhook>?wait=true[&thread_id=...]`.
    ///
    /// On HTTP 429, parses `retry_after` from the JSON body (falling back to the `Retry-After` header),
    /// waits if `<= 60s`, and retries at most once.
    pub async fn send(
        &self,
        webhook: &WebhookUrl,
        thread_id: Option<&str>,
        payload: &DiscordPayload,
    ) -> Result<(), DiscordError> {
        let target_url = build_execution_url(webhook.expose_url(), thread_id);

        for attempt in 0..2 {
            let response = self
                .client
                .post(target_url.clone())
                .json(payload)
                .send()
                .await
                .map_err(|e| {
                    // Never format `e` with its URL in case reqwest attached the request URL to the error!
                    DiscordError::Transient(sanitize_reqwest_error(&e))
                })?;

            let status = response.status();
            if status.is_success() {
                return Ok(());
            }

            if matches!(
                status,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
            ) {
                tracing::error!(
                    status = status.as_u16(),
                    webhook = %webhook,
                    "Discord webhook was deleted or revoked; update the webhook URL in .env"
                );
                return Err(DiscordError::WebhookGone {
                    status: status.as_u16(),
                });
            }

            if status == StatusCode::TOO_MANY_REQUESTS {
                let header_retry = parse_retry_after_header(response.headers());
                let body_bytes = response.bytes().await.unwrap_or_default();
                let body_retry = serde_json::from_slice::<RateLimitBody>(&body_bytes)
                    .ok()
                    .and_then(|b| b.retry_after);
                let retry_after_secs = body_retry.or(header_retry).unwrap_or(61.0).max(0.0);

                if attempt == 0 && retry_after_secs <= MAX_RETRY_AFTER_SECS {
                    tracing::warn!(
                        retry_after_secs,
                        "Discord webhook rate-limited (429); waiting and retrying once"
                    );
                    tokio::time::sleep(Duration::from_secs_f64(retry_after_secs)).await;
                    continue;
                }
                return Err(DiscordError::RateLimited { retry_after_secs });
            }

            if status.is_client_error() {
                let body_text = response.text().await.unwrap_or_default();
                let truncated = truncate_chars(&strip_control_chars(&body_text), 300);
                tracing::error!(
                    status = status.as_u16(),
                    body = %truncated,
                    "Discord rejected webhook payload"
                );
                return Err(DiscordError::WebhookRejected {
                    status: status.as_u16(),
                    body: truncated,
                });
            }

            return Err(DiscordError::Transient(format!(
                "Discord returned HTTP {}",
                status.as_u16()
            )));
        }

        Err(DiscordError::Transient(
            "exhausted webhook delivery attempts".to_string(),
        ))
    }
}

fn build_execution_url(base: &Url, thread_id: Option<&str>) -> Url {
    let mut url = base.clone();
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("wait", "true");
        if let Some(tid) = thread_id.map(str::trim).filter(|s| !s.is_empty()) {
            pairs.append_pair("thread_id", tid);
        }
    }
    url
}

fn parse_retry_after_header(headers: &reqwest::header::HeaderMap) -> Option<f64> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()
}

fn sanitize_reqwest_error(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        "request timed out".to_string()
    } else if err.is_connect() {
        "connection failed".to_string()
    } else if let Some(status) = err.status() {
        format!("HTTP status {}", status.as_u16())
    } else {
        "network I/O error".to_string()
    }
}
