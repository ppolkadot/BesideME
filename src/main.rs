//! CLI entry point for `besideme-cli`.

use std::path::PathBuf;
use std::process;

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use besideme::config::{Config, LoadOptions};
use besideme::paths::{resolve_log_dir, resolve_state_path};
use besideme::sites::Registry;
use besideme::watcher::{
    CheckOptions, ExitCode, run_check, run_list, run_reset, run_test_webhook, run_watch,
};

#[derive(Debug, Parser)]
#[command(
    name = "besideme-cli",
    version,
    about = "Lightweight manga chapter watcher with Discord webhook notifications"
)]
struct Cli {
    /// Increase logging verbosity to debug (`-v`).
    #[arg(short = 'v', long = "verbose", global = true, conflicts_with = "quiet")]
    verbose: bool,

    /// Decrease logging verbosity to warn (`-q`).
    #[arg(short = 'q', long = "quiet", global = true, conflicts_with = "verbose")]
    quiet: bool,

    /// Lift the Discord host check on webhook URLs (and allow loopback HTTP for local testing).
    #[arg(long = "allow-any-webhook", global = true)]
    allow_any_webhook: bool,

    /// Explicit path to a `.env` file containing webhook secrets.
    #[arg(long = "env-file", value_name = "P", global = true)]
    env_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run one sequential check pass over all configured series.
    Check(CheckArgs),
    /// Run the continuous polling loop with jitter, backoff, and Ctrl-C handling.
    Watch(WatchArgs),
    /// Send a test embed to configured Discord webhook(s) without pinging.
    TestWebhook(TestWebhookArgs),
    /// List configured series, persisted state, and (unless --offline) current live head chapters.
    List(ListArgs),
    /// Remove persisted state for a series (or all series) so the next run re-baselines.
    Reset(ResetArgs),
}

#[derive(Debug, Args)]
struct CheckArgs {
    /// Explicit path to `config.toml`.
    #[arg(long = "config", value_name = "P")]
    config: Option<PathBuf>,

    /// Explicit path to `state.json`.
    #[arg(long = "state", value_name = "P")]
    state: Option<PathBuf>,

    /// Print what would happen without writing state or posting to webhooks.
    #[arg(long = "dry-run")]
    dry_run: bool,

    /// Output one JSON object per series to stdout.
    #[arg(long = "json")]
    json: bool,

    /// On first run for an uninitialized series, baseline all chapters except the newest and announce the newest.
    #[arg(long = "announce-on-first-run")]
    announce_on_first_run: bool,
}

#[derive(Debug, Args)]
struct WatchArgs {
    /// Explicit path to `config.toml`.
    #[arg(long = "config", value_name = "P")]
    config: Option<PathBuf>,

    /// Explicit path to `state.json`.
    #[arg(long = "state", value_name = "P")]
    state: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct TestWebhookArgs {
    /// Explicit path to `config.toml`.
    #[arg(long = "config", value_name = "P")]
    config: Option<PathBuf>,

    /// Test only the announcement webhook for the matching series URL or key.
    #[arg(long = "series", value_name = "URL")]
    series: Option<String>,

    /// Test `BESIDEME_HEALTH_WEBHOOK` instead of announcement webhooks.
    #[arg(long = "health")]
    health: bool,
}

#[derive(Debug, Args)]
struct ListArgs {
    /// Explicit path to `config.toml`.
    #[arg(long = "config", value_name = "P")]
    config: Option<PathBuf>,

    /// Explicit path to `state.json`.
    #[arg(long = "state", value_name = "P")]
    state: Option<PathBuf>,

    /// Do not fetch current head chapters from the network.
    #[arg(long = "offline")]
    offline: bool,
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct ResetTarget {
    /// Series URL (or `<site>:<key>`) to reset.
    #[arg(value_name = "SERIES_URL")]
    series_url: Option<String>,

    /// Reset all series in `state.json`.
    #[arg(long = "all")]
    all: bool,
}

#[derive(Debug, Args)]
struct ResetArgs {
    #[command(flatten)]
    target: ResetTarget,

    /// Explicit path to `state.json`.
    #[arg(long = "state", value_name = "P")]
    state: Option<PathBuf>,
}

fn init_tracing(verbose: bool, quiet: bool) {
    let default_directive = if verbose {
        "debug"
    } else if quiet {
        "warn"
    } else {
        "info"
    };

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_directive));

    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false);

    let file_layer = resolve_log_dir().ok().and_then(|log_dir| {
        std::fs::create_dir_all(&log_dir).ok()?;
        let appender = RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix("besideme")
            .filename_suffix("log")
            .max_log_files(7)
            .build(log_dir)
            .ok()?;
        Some(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(appender)
                .with_target(false),
        )
    });

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();
}

fn main() {
    let cli = Cli::parse();
    init_tracing(cli.verbose, cli.quiet);

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to initialize single-threaded Tokio runtime")
    {
        Ok(rt) => rt,
        Err(err) => {
            tracing::error!(error = %err, "startup failure");
            process::exit(ExitCode::ConfigOrStateError.as_i32());
        }
    };

    let code = runtime.block_on(dispatch(cli));
    process::exit(code.as_i32());
}

#[allow(clippy::too_many_lines)]
async fn dispatch(cli: Cli) -> ExitCode {
    let registry = Registry::with_defaults();

    match cli.command {
        Commands::Check(args) => {
            let load_opts = LoadOptions {
                config_path: args.config,
                env_file_path: cli.env_file,
                allow_any_webhook: cli.allow_any_webhook,
                allow_missing_webhooks: args.dry_run,
            };
            let config = match Config::load(&load_opts, &registry) {
                Ok(c) => c,
                Err(err) => {
                    tracing::error!(error = %err, "configuration error");
                    return ExitCode::ConfigOrStateError;
                }
            };
            let state_path = match resolve_state_path(args.state.as_deref()) {
                Ok(p) => p,
                Err(err) => {
                    tracing::error!(error = %err, "state path resolution error");
                    return ExitCode::ConfigOrStateError;
                }
            };
            let summary = run_check(
                &config,
                &registry,
                &state_path,
                CheckOptions {
                    dry_run: args.dry_run,
                    json: args.json,
                    announce_on_first_run: args.announce_on_first_run,
                },
            )
            .await;
            summary.exit_code
        }
        Commands::Watch(args) => {
            let load_opts = LoadOptions {
                config_path: args.config,
                env_file_path: cli.env_file,
                allow_any_webhook: cli.allow_any_webhook,
                allow_missing_webhooks: false,
            };
            let config = match Config::load(&load_opts, &registry) {
                Ok(c) => c,
                Err(err) => {
                    tracing::error!(error = %err, "configuration error");
                    return ExitCode::ConfigOrStateError;
                }
            };
            let state_path = match resolve_state_path(args.state.as_deref()) {
                Ok(p) => p,
                Err(err) => {
                    tracing::error!(error = %err, "state path resolution error");
                    return ExitCode::ConfigOrStateError;
                }
            };
            run_watch(&config, &registry, &state_path).await
        }
        Commands::TestWebhook(args) => {
            let load_opts = LoadOptions {
                config_path: args.config,
                env_file_path: cli.env_file,
                allow_any_webhook: cli.allow_any_webhook,
                allow_missing_webhooks: args.health,
            };
            let config = match Config::load(&load_opts, &registry) {
                Ok(c) => c,
                Err(err) => {
                    tracing::error!(error = %err, "configuration error");
                    return ExitCode::ConfigOrStateError;
                }
            };
            run_test_webhook(&config, args.series.as_deref(), args.health).await
        }
        Commands::List(args) => {
            let load_opts = LoadOptions {
                config_path: args.config,
                env_file_path: cli.env_file,
                allow_any_webhook: cli.allow_any_webhook,
                allow_missing_webhooks: true,
            };
            let config = match Config::load(&load_opts, &registry) {
                Ok(c) => c,
                Err(err) => {
                    tracing::error!(error = %err, "configuration error");
                    return ExitCode::ConfigOrStateError;
                }
            };
            let state_path = match resolve_state_path(args.state.as_deref()) {
                Ok(p) => p,
                Err(err) => {
                    tracing::error!(error = %err, "state path resolution error");
                    return ExitCode::ConfigOrStateError;
                }
            };
            run_list(&config, &registry, &state_path, args.offline).await
        }
        Commands::Reset(args) => {
            let state_path = match resolve_state_path(args.state.as_deref()) {
                Ok(p) => p,
                Err(err) => {
                    tracing::error!(error = %err, "state path resolution error");
                    return ExitCode::ConfigOrStateError;
                }
            };
            run_reset(
                &state_path,
                &registry,
                args.target.series_url.as_deref(),
                args.target.all,
            )
        }
    }
}
