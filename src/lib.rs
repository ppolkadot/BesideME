//! `besideme` library crate exposing site parsers, pure chapter detection,
//! state persistence, Discord webhook notification building, and watch orchestration.

pub mod config;
pub mod config_edit;
pub mod detect;
pub mod http;
pub mod lookup;
pub mod model;
pub mod notify;
pub mod paths;
pub mod reads;
pub mod schedule;
pub mod sites;
pub mod state;
pub mod watcher;
#[cfg(feature = "gui")]
pub mod webhook_settings;
pub mod webhook_test;

pub use crate::config::{Config, ConfigError, LoadOptions, SeriesConfig, WebhookUrl};
pub use crate::detect::{ChapterGroup, Decision, Policy, decide};
pub use crate::model::{ChapterRef, SeriesKey, SeriesSnapshot};
pub use crate::sites::{MangaUp, ParseError, Registry, Site};
pub use crate::state::{
    DiscoveredRelease, InstanceLock, RunRecord, SeriesState, StateError, StateFile,
};
pub use crate::watcher::{
    CheckOptions, CheckProgress, CheckRunSummary, ExitCode, SeriesCheckReport, run_check,
    run_check_scoped, run_list, run_reset, run_test_webhook, run_watch,
};
