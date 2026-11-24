//! Background jobs for the GUI. Each job runs on its own thread with a single-threaded
//! tokio runtime and reports back through a channel, so the UI never blocks.

use std::future::Future;
use std::sync::mpsc::Sender;

use eframe::egui;
use manga_watcher::config::{Config, LoadOptions};
use manga_watcher::lookup::{self, SeriesPreview};
use manga_watcher::paths::resolve_state_path;
use manga_watcher::sites::{CatalogEntry, Registry};
use manga_watcher::watcher::{CheckOptions, ExitCode, run_check};

/// Result messages from background jobs.
pub enum Msg {
    Catalog(Result<Vec<CatalogEntry>, String>),
    Preview(Result<SeriesPreview, String>),
    Check(CheckOutcome),
}

/// Summary of a "check now" run, ready to show in the status bar.
pub struct CheckOutcome {
    pub ok: bool,
    pub message: String,
}

/// Runs `job` on a worker thread and delivers its message to the UI.
pub fn spawn<F, Fut>(ctx: &egui::Context, tx: &Sender<Msg>, job: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Msg> + 'static,
{
    let ctx = ctx.clone();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let msg = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt.block_on(job()),
            Err(e) => Msg::Check(CheckOutcome {
                ok: false,
                message: format!("Internal error: {e}"),
            }),
        };
        let _ = tx.send(msg);
        ctx.request_repaint();
    });
}

pub async fn load_catalog() -> Msg {
    Msg::Catalog(
        lookup::load_catalog(&Registry::with_defaults())
            .await
            .map_err(|e| e.to_string()),
    )
}

pub async fn preview(link: String) -> Msg {
    Msg::Preview(
        lookup::preview_series(&Registry::with_defaults(), &link)
            .await
            .map_err(|e| e.to_string()),
    )
}

/// Runs one normal `check` pass (same as the scheduled task). New series get a silent
/// baseline; real new chapters are announced to Discord.
pub async fn check_now() -> Msg {
    Msg::Check(run_check_once().await)
}

pub async fn run_check_once() -> CheckOutcome {
    let registry = Registry::with_defaults();
    let config = match Config::load(&LoadOptions::default(), &registry) {
        Ok(c) => c,
        Err(e) => {
            return CheckOutcome {
                ok: false,
                message: format!("Can't check: {e}"),
            };
        }
    };
    if config.series.is_empty() {
        return CheckOutcome {
            ok: true,
            message: "Nothing to check yet — add a manga first.".into(),
        };
    }
    let state_path = match resolve_state_path(None) {
        Ok(p) => p,
        Err(e) => {
            return CheckOutcome {
                ok: false,
                message: format!("Can't check: {e}"),
            };
        }
    };
    let summary = run_check(&config, &registry, &state_path, CheckOptions::default()).await;

    if summary.exit_code == ExitCode::AnotherInstanceRunning {
        return CheckOutcome {
            ok: false,
            message: "A background check is already running — try again in a minute.".into(),
        };
    }
    let mut parts = Vec::new();
    let mut ok = true;
    for r in &summary.reports {
        let name = r.title.clone().unwrap_or_else(|| r.series_key.clone());
        let head = r.head_label.clone().unwrap_or_default();
        match r.outcome.as_str() {
            "baseline" => parts.push(format!("Now tracking {name} (latest: {head})")),
            "announced" if r.error.is_none() => parts.push(format!(
                "Posted {} new chapter(s) of {name}",
                r.announced_parts
            )),
            "no_change" | "held_back" => {}
            _ => {
                ok = false;
                parts.push(format!(
                    "{name}: {}",
                    r.error.as_deref().unwrap_or(r.outcome.as_str())
                ));
            }
        }
    }
    if parts.is_empty() {
        parts.push("Checked — no new chapters.".into());
    }
    CheckOutcome {
        ok,
        message: parts.join(" · "),
    }
}
