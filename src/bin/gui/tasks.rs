//! Background jobs for the GUI. Each job runs on its own thread with a single-threaded
//! tokio runtime and reports back through a channel, so the UI never blocks.

use std::future::Future;
use std::sync::mpsc::Sender;

use besideme::config::{Config, LoadOptions, allow_any_webhook_from_env};
use besideme::lookup::{self, SeriesPreview};
use besideme::model::SeriesKey;
use besideme::paths::resolve_state_path;
use besideme::schedule::{self, TaskStatus};
use besideme::sites::{CatalogEntry, Registry};
use besideme::watcher::{CheckOptions, CheckProgress, ExitCode, run_check_scoped};
use eframe::egui;

/// Result messages from background jobs.
///
/// Responses that can become stale carry the request generation (and, for previews, the
/// input they answer) so the receiver can discard answers to superseded requests.
pub enum Msg {
    Catalog {
        generation: u64,
        result: Result<Vec<CatalogEntry>, String>,
    },
    Preview {
        generation: u64,
        input: String,
        result: Result<SeriesPreview, String>,
    },
    Progress(CheckProgress),
    Check(CheckOutcome),
    Schedule(TaskStatus),
}

/// Summary of a manual check run, ready to show in the status bar.
pub struct CheckOutcome {
    pub ok: bool,
    pub message: String,
}

/// Runs `job` on a worker thread and delivers its messages to the UI.
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

pub async fn load_catalog(generation: u64) -> Msg {
    Msg::Catalog {
        generation,
        result: lookup::load_catalog(&Registry::with_defaults())
            .await
            .map_err(|e| e.to_string()),
    }
}

/// Previews the series behind `link`; `input` is the text the user typed, echoed back so the
/// caller can tell whether the answer is still wanted.
pub async fn preview(generation: u64, input: String, link: String) -> Msg {
    let result = lookup::preview_series(&Registry::with_defaults(), &link)
        .await
        .map_err(|e| e.to_string());
    Msg::Preview {
        generation,
        input,
        result,
    }
}

/// Reads the Windows scheduled-task status off the UI thread.
pub fn query_schedule(ctx: &egui::Context, tx: &Sender<Msg>) {
    let ctx = ctx.clone();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(Msg::Schedule(schedule::query()));
        ctx.request_repaint();
    });
}

/// Runs one check on a worker thread, streaming progress to the UI.
///
/// `only` limits the run to a single title (retry); otherwise every title is checked exactly
/// as the scheduled task does.
pub fn spawn_check(ctx: &egui::Context, tx: &Sender<Msg>, only: Option<SeriesKey>) {
    let ctx = ctx.clone();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let outcome = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => {
                let progress_tx = tx.clone();
                let progress_ctx = ctx.clone();
                rt.block_on(run_check_job(only.as_ref(), &mut move |event| {
                    let _ = progress_tx.send(Msg::Progress(event));
                    progress_ctx.request_repaint();
                }))
            }
            Err(e) => CheckOutcome {
                ok: false,
                message: format!("Internal error: {e}"),
            },
        };
        let _ = tx.send(Msg::Check(outcome));
        ctx.request_repaint();
    });
}

/// Runs the scheduled-task check once (`BesideME.exe --check`).
pub async fn run_check_once() -> CheckOutcome {
    run_check_job(None, &mut |_| {}).await
}

/// Runs one normal `check` pass (same as the scheduled task). New series get a silent
/// baseline; real new chapters are announced to Discord.
async fn run_check_job(
    only: Option<&SeriesKey>,
    progress: &mut (dyn FnMut(CheckProgress) + Send),
) -> CheckOutcome {
    let fail = |message: String| CheckOutcome { ok: false, message };
    let registry = Registry::with_defaults();
    let options = LoadOptions {
        allow_any_webhook: allow_any_webhook_from_env(),
        ..LoadOptions::default()
    };
    let config = match Config::load(&options, &registry) {
        Ok(c) => c,
        Err(e) => return fail(format!("Can't check: {e}")),
    };
    if config.series.is_empty() {
        return CheckOutcome {
            ok: true,
            message: "Nothing to check yet — add a manga first.".into(),
        };
    }
    let state_path = match resolve_state_path(None) {
        Ok(p) => p,
        Err(e) => return fail(format!("Can't check: {e}")),
    };
    let summary = run_check_scoped(
        &config,
        &registry,
        &state_path,
        CheckOptions::default(),
        only,
        progress,
    )
    .await;

    if summary.exit_code == ExitCode::AnotherInstanceRunning {
        return fail("A background check is already running — try again in a minute.".into());
    }
    if summary.reports.is_empty() && summary.exit_code != ExitCode::Ok {
        return fail("Couldn't run the check. See the log for details.".into());
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
            "cooldown" => {
                // Not a failure: the retry was refused to stay within the polling rules.
                parts.push(format!(
                    "{name}: {}",
                    r.error.as_deref().unwrap_or("too soon to check again")
                ));
            }
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
