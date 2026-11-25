//! The `BesideME` desktop app.
//!
//! * `BesideME.exe` — opens the window (borderless, macOS-style traffic lights).
//! * `BesideME.exe --check` — runs one headless check and exits. Used by the scheduled
//!   task: this binary has no console, so nothing flashes on screen every 30 minutes.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(clippy::too_many_lines, missing_docs)]

mod app;
mod chrome;
mod data;
mod quick_add;
mod settings;
mod shelf;
mod status;
mod tasks;
mod theme;
mod widgets;

use besideme::paths::resolve_log_dir;
use eframe::egui;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

const ICON_PNG: &[u8] = include_bytes!("../../../assets/yamada-256.png");

fn init_file_logging() {
    let Some(log_dir) = resolve_log_dir().ok() else {
        return;
    };
    if std::fs::create_dir_all(&log_dir).is_err() {
        return;
    }
    let Ok(appender) = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("besideme")
        .filename_suffix("log")
        .max_log_files(7)
        .build(log_dir)
    else {
        return;
    };
    let _ = tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_target(false)
                .with_writer(appender),
        )
        .try_init();
}

fn headless_check() -> i32 {
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => {
            let out = rt.block_on(tasks::run_check_once());
            tracing::info!(ok = out.ok, "scheduled check finished: {}", out.message);
            i32::from(!out.ok)
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to start runtime");
            1
        }
    }
}

fn main() {
    init_file_logging();

    if std::env::args().skip(1).any(|a| a == "--check") {
        std::process::exit(headless_check());
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("BesideME")
        .with_app_id("BesideME")
        .with_inner_size([720.0, 680.0])
        .with_min_inner_size([420.0, 420.0])
        // No native caption: no Windows minimize/maximize/close buttons to interact with.
        .with_decorations(false)
        .with_transparent(true)
        .with_resizable(false);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(ICON_PNG) {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        centered: true,
        ..Default::default()
    };
    if let Err(e) = eframe::run_native(
        "BesideME",
        options,
        Box::new(|cc| Ok(Box::new(app::WatcherApp::new(cc)))),
    ) {
        tracing::error!(error = %e, "GUI failed to start");
    }
}
