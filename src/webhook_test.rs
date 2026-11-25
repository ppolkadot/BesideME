//! One-shot Discord destination tests used by the desktop settings screen.
//!
//! A test sends exactly one clearly labelled, non-pinging embed to the URL it is given and
//! reports whether Discord accepted it. Results never contain the webhook URL or token.

use std::time::Duration;

use crate::config::{ConfigError, DEFAULT_TIMEOUT_SECONDS, DEFAULT_USER_AGENT, WebhookUrl};
use crate::notify::discord::{
    DiscordClient, DiscordError, build_test_webhook_payload, now_rfc3339_utc,
};

/// The two Discord destinations `BesideME` posts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestDestination {
    /// Chapter announcements (Yamada).
    Yamada,
    /// Health and recovery alerts (Sasaki).
    Sasaki,
}

impl TestDestination {
    /// Name of the character that posts to this destination.
    #[must_use]
    pub const fn character(self) -> &'static str {
        match self {
            Self::Yamada => "Yamada",
            Self::Sasaki => "Sasaki",
        }
    }

    /// Label placed in the test message so its channel can tell which destination was tested.
    #[must_use]
    pub const fn message_label(self) -> &'static str {
        match self {
            Self::Yamada => "Yamada · chapter updates destination",
            Self::Sasaki => "Sasaki · health alerts destination",
        }
    }

    const fn env_name(self) -> &'static str {
        match self {
            Self::Yamada => crate::config::DEFAULT_WEBHOOK_ENV,
            Self::Sasaki => crate::config::HEALTH_WEBHOOK_ENV,
        }
    }
}

/// What happened to a test message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestOutcome {
    /// Discord accepted the message.
    Delivered,
    /// The text is not a usable Discord webhook URL; nothing was sent.
    InvalidUrl(String),
    /// The webhook was deleted, revoked, or never existed (HTTP 401, 403 or 404).
    Revoked {
        /// HTTP status returned by Discord.
        status: u16,
    },
    /// Discord refused the message (another 4xx response).
    Rejected {
        /// HTTP status returned by Discord.
        status: u16,
    },
    /// Discord is rate limiting the webhook.
    RateLimited {
        /// Seconds Discord asked to wait.
        retry_after_secs: u64,
    },
    /// Discord could not be reached, or answered with a server error.
    Unreachable,
}

/// Result of one test, tied to the destination that was tested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestResult {
    /// Destination that was tested.
    pub destination: TestDestination,
    /// What happened.
    pub outcome: TestOutcome,
}

impl TestResult {
    /// Whether Discord accepted the message.
    #[must_use]
    pub fn delivered(&self) -> bool {
        self.outcome == TestOutcome::Delivered
    }

    /// A short, actionable explanation suitable for showing next to the field.
    #[must_use]
    pub fn message(&self) -> String {
        let name = self.destination.character();
        match &self.outcome {
            TestOutcome::Delivered => {
                format!(
                    "Discord accepted the test message for {name}. Check the channel to see it."
                )
            }
            TestOutcome::InvalidUrl(reason) => reason.clone(),
            TestOutcome::Revoked { status } => format!(
                "Discord says this webhook no longer exists (HTTP {status}). In Discord, open Channel Settings → Integrations → Webhooks, copy a current URL, and paste it here."
            ),
            TestOutcome::Rejected { status } => format!(
                "Discord refused the test message (HTTP {status}). Check that the whole URL was copied, then try again."
            ),
            TestOutcome::RateLimited { retry_after_secs } => format!(
                "Discord is rate limiting this webhook. Try again in {} s.",
                (*retry_after_secs).max(1)
            ),
            TestOutcome::Unreachable => {
                "Couldn't reach Discord. Check your internet connection and try again.".to_owned()
            }
        }
    }
}

/// Network settings for a test.
#[derive(Debug, Clone)]
pub struct TestSettings {
    /// Accept loopback `http` hosts and non-Discord hosts (local fixtures only).
    pub allow_any_webhook: bool,
    /// `User-Agent` header value.
    pub user_agent: String,
    /// Request timeout.
    pub timeout: Duration,
}

impl Default for TestSettings {
    fn default() -> Self {
        Self {
            allow_any_webhook: false,
            user_agent: DEFAULT_USER_AGENT.to_owned(),
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECONDS),
        }
    }
}

/// Checks that `raw` is a single, well-formed Discord webhook URL.
pub fn validate_url(
    destination: TestDestination,
    raw: &str,
    allow_any_webhook: bool,
) -> Result<WebhookUrl, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "Paste the {} webhook URL first.",
            destination.character()
        ));
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err("Paste a single Discord webhook URL without spaces.".to_owned());
    }
    WebhookUrl::parse(destination.env_name(), trimmed, allow_any_webhook).map_err(|err| {
        let reason = match err {
            ConfigError::InvalidWebhook { reason, .. } => reason,
            other => other.to_string(),
        };
        format!(
            "That doesn't look like a Discord webhook URL ({reason}). It should start with https://discord.com/api/webhooks/."
        )
    })
}

/// Sends one test message to `raw_url` and reports Discord's answer.
///
/// Nothing is sent when the URL is invalid. A rate limit is reported immediately rather
/// than waited out.
pub async fn send_test(
    destination: TestDestination,
    raw_url: &str,
    settings: &TestSettings,
) -> TestResult {
    let outcome = match validate_url(destination, raw_url, settings.allow_any_webhook) {
        Err(message) => TestOutcome::InvalidUrl(message),
        Ok(url) => deliver(destination, &url, settings).await,
    };
    TestResult {
        destination,
        outcome,
    }
}

async fn deliver(
    destination: TestDestination,
    url: &WebhookUrl,
    settings: &TestSettings,
) -> TestOutcome {
    let Ok(client) = DiscordClient::new(&settings.user_agent, settings.timeout) else {
        return TestOutcome::Unreachable;
    };
    let payload = build_test_webhook_payload(
        destination.message_label(),
        &now_rfc3339_utc(),
        destination == TestDestination::Sasaki,
    );
    match client.send_with_retry_cap(url, None, &payload, 0.0).await {
        Ok(()) => TestOutcome::Delivered,
        Err(DiscordError::WebhookGone { status }) => TestOutcome::Revoked { status },
        Err(DiscordError::WebhookRejected { status }) => TestOutcome::Rejected { status },
        Err(DiscordError::RateLimited { retry_after_secs }) => TestOutcome::RateLimited {
            // Rounded up so the user never retries a moment too early.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            retry_after_secs: retry_after_secs.ceil().max(0.0) as u64,
        },
        Err(DiscordError::Transient(_)) => TestOutcome::Unreachable,
    }
}
