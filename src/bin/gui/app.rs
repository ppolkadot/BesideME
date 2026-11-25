//! The `BesideME` window: a release shelf of the manga you follow, a status strip, Quick Add,
//! and Discord settings.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use besideme::config_edit::{
    EditError, add_series, gui_config_path, read_or_default, remove_series, validate_and_write,
};
use besideme::model::SeriesKey;
use besideme::paths::resolve_state_path;
use besideme::reads::{ReadMarkers, reads_path_for_state};
use besideme::sites::{CatalogEntry, Registry};
use besideme::state::{InstanceLock, RunPhase, RunRecord, StateFile};
use besideme::watcher::CheckProgress;
use besideme::webhook_settings::WebhookSettings;
use eframe::egui::{
    self, Align, Frame, Id, Key, Layout, Modifiers, ProgressBar, RichText, ScrollArea, Stroke, Ui,
    ViewportCommand, vec2,
};

use crate::chrome;
use crate::data::{self, FileStamps, Filter, Loadable, Paths, Row};
use crate::quick_add::{self, QuickAdd};
use crate::settings::{Settings, SettingsAction};
use crate::shelf::{self, ShelfAction, ShelfEnv};
use crate::status::{self, TaskView, Tone};
use crate::tasks::{self, Msg};
use crate::theme;
use crate::widgets::{banner, card, pill, primary, segmented, title_text};

const STATUS_SECONDS: f64 = 8.0;
/// How often the window looks for files written by the scheduled check.
const POLL_SECONDS: f64 = 1.5;
/// How often the Windows task status is asked for again.
const TASK_REFRESH_SECONDS: f64 = 60.0;
/// Largest and preferred window size, in points. Smaller screens get a smaller window.
const WINDOW: egui::Vec2 = egui::vec2(720.0, 680.0);
/// Body height (points) below which the whole page scrolls as one.
const CRAMPED_HEIGHT: f32 = 400.0;
/// Room reserved for the Add manga and Settings buttons.
const TOOLBAR_BUTTONS_WIDTH: f32 = 200.0;
/// Width (points) below which the toolbar wraps instead of right-aligning its buttons.
const NARROW_WIDTH: f32 = 520.0;

#[derive(PartialEq, Eq, Clone)]
enum Busy {
    Idle,
    Full,
    Retry(SeriesKey),
}

struct Progress {
    done: usize,
    total: usize,
    current: Option<String>,
}

struct Status {
    text: String,
    error: bool,
    at: f64,
}

#[allow(clippy::struct_excessive_bools)]
pub struct WatcherApp {
    registry: Registry,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    paths: Paths,
    stamps: FileStamps,
    next_poll: f64,
    rows: Vec<Row>,
    last_run: Option<RunRecord>,
    config_error: Option<String>,
    catalog: Loadable<Vec<CatalogEntry>>,
    catalog_generation: u64,
    catalog_titles: HashMap<String, String>,
    covers: HashMap<String, String>,
    filter: Filter,
    quick: QuickAdd,
    busy: Busy,
    progress: Option<Progress>,
    task: TaskView,
    task_asked_at: f64,
    task_inflight: bool,
    status: Option<Status>,
    confirm_remove: Option<(SeriesKey, f64)>,
    webhook_missing: bool,
    settings: Option<Settings>,
    /// Set only by the red traffic light; every other close request is cancelled.
    allow_close: bool,
    fitted: bool,
    /// Title whose "Open series" button should get keyboard focus (and for how many more frames).
    focus_anchor: Option<(SeriesKey, u8)>,
    add_button_id: Option<Id>,
}

impl WatcherApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let (tx, rx) = channel();
        let config = {
            let p = gui_config_path();
            std::path::absolute(&p).unwrap_or(p)
        };
        let state = resolve_state_path(None).unwrap_or_else(|_| PathBuf::from("state.json"));
        let paths = Paths {
            reads: reads_path_for_state(&state),
            state,
            config,
        };
        let mut app = Self {
            registry: Registry::with_defaults(),
            tx,
            rx,
            paths,
            stamps: FileStamps::default(),
            next_poll: 0.0,
            rows: Vec::new(),
            last_run: None,
            config_error: None,
            catalog: Loadable::Idle,
            catalog_generation: 0,
            catalog_titles: HashMap::new(),
            covers: HashMap::new(),
            filter: Filter::All,
            quick: QuickAdd::new(),
            busy: Busy::Idle,
            progress: None,
            task: TaskView::Loading,
            task_asked_at: f64::NEG_INFINITY,
            task_inflight: false,
            status: None,
            confirm_remove: None,
            webhook_missing: false,
            settings: None,
            allow_close: false,
            fitted: false,
            focus_anchor: None,
            add_button_id: None,
        };
        app.reload();
        app.start_catalog(&cc.egui_ctx);
        app
    }

    // ---------------------------------------------------------------- data

    fn reload(&mut self) {
        let snapshot = data::load(
            &self.registry,
            &self.paths,
            &self.catalog_titles,
            time_now_unix(),
        );
        self.rows = snapshot.rows;
        self.last_run = snapshot.last_run;
        self.config_error = snapshot.config_error;
        self.stamps = FileStamps::read(&self.paths.config, &self.paths.state, &self.paths.reads);
        self.webhook_missing = !WebhookSettings::load(&self.paths.config)
            .is_ok_and(|settings| settings.updates_configured());
        if let Some(error) = self.config_error.clone() {
            self.status = Some(Status {
                text: error,
                error: true,
                at: 0.0,
            });
        }
    }

    /// Picks up changes made by the scheduled check (or another window) without a restart.
    fn poll_files(&mut self, now: f64) {
        if now < self.next_poll {
            return;
        }
        self.next_poll = now + POLL_SECONDS;
        let current = FileStamps::read(&self.paths.config, &self.paths.state, &self.paths.reads);
        if current != self.stamps {
            self.reload();
        }
    }

    fn title_of(&self, series_key: &str) -> String {
        self.rows
            .iter()
            .find(|r| r.key.as_state_key() == series_key)
            .map_or_else(|| series_key.to_owned(), |r| r.title.clone())
    }

    fn start_catalog(&mut self, ctx: &egui::Context) {
        self.catalog = Loadable::Loading;
        self.catalog_generation += 1;
        let generation = self.catalog_generation;
        tasks::spawn(ctx, &self.tx, move || tasks::load_catalog(generation));
    }

    fn external_check_running(&self) -> bool {
        self.last_run
            .as_ref()
            .is_some_and(|r| r.phase(time_now_unix()) == RunPhase::Running)
    }

    /// Whether this window is already running a check. A run by another process only shows
    /// in the status strip: the instance lock refuses an overlapping one with a clear message,
    /// and a check that was killed mid-run must not leave the button disabled.
    fn checks_blocked(&self) -> bool {
        self.busy != Busy::Idle
    }

    fn start_check(&mut self, ctx: &egui::Context, only: Option<SeriesKey>) {
        if self.checks_blocked() {
            return;
        }
        self.busy = only.clone().map_or(Busy::Full, Busy::Retry);
        self.progress = Some(Progress {
            done: 0,
            total: if only.is_some() { 1 } else { self.rows.len() },
            current: None,
        });
        tasks::spawn_check(ctx, &self.tx, only);
    }

    /// Adds a series to the watchlist. Returns whether it is now watched.
    fn add(&mut self, ctx: &egui::Context, url: &str, title: &str) -> bool {
        let result = read_or_default(&self.paths.config).and_then(|text| {
            let new = add_series(&text, &self.registry, url, Some(title))?;
            validate_and_write(&new, &self.paths.config, &self.registry)
        });
        match result {
            Ok(()) => {
                self.reload();
                if self.webhook_missing {
                    self.set_status(
                        Some(ctx),
                        format!("Added {title}. It will be checked once a Discord webhook is set."),
                        false,
                    );
                } else {
                    self.set_status(Some(ctx), format!("Added {title}. Setting it up…"), false);
                    // Silent baseline right away, so the shelf shows the latest chapter.
                    self.start_check(ctx, None);
                }
                true
            }
            Err(EditError::AlreadyPresent) => {
                self.set_status(
                    Some(ctx),
                    format!("{title} is already being watched."),
                    false,
                );
                true
            }
            Err(e) => {
                self.set_status(Some(ctx), format!("Couldn't add: {e}"), true);
                false
            }
        }
    }

    fn remove(&mut self, ctx: &egui::Context, key: &SeriesKey, title: &str) {
        let result = read_or_default(&self.paths.config).and_then(|text| {
            let new = remove_series(&text, &self.registry, key)?;
            validate_and_write(&new, &self.paths.config, &self.registry)
        });
        match result {
            Ok(()) => {
                forget_state(key);
                let state_key = key.as_state_key();
                let _ = ReadMarkers::update(&self.paths.reads, |m| {
                    m.forget(&state_key);
                });
                self.reload();
                self.set_status(Some(ctx), format!("Stopped watching {title}."), false);
            }
            Err(e) => self.set_status(Some(ctx), format!("Couldn't remove: {e}"), true),
        }
    }

    fn mark_read(&mut self, ctx: &egui::Context, key: &SeriesKey) {
        let Some(row) = self.rows.iter().find(|r| &r.key == key) else {
            return;
        };
        let title = row.title.clone();
        let ids: Vec<String> = row.unread.iter().map(|u| u.id.clone()).collect();
        let state_key = key.as_state_key();
        // Only the read-marker file is written, so a running check is never in the way.
        match ReadMarkers::update(&self.paths.reads, |m| {
            m.mark_read(&state_key, ids.iter().map(String::as_str));
        }) {
            Ok(()) => {
                self.focus_anchor = Some((key.clone(), 3));
                self.reload();
                self.set_status(Some(ctx), format!("Marked {title} as read."), false);
            }
            Err(e) => self.set_status(Some(ctx), format!("Couldn't mark as read: {e}"), true),
        }
    }

    fn set_status(&mut self, ctx: Option<&egui::Context>, text: String, error: bool) {
        let at = ctx.map_or(0.0, |c| c.input(|i| i.time));
        self.status = Some(Status { text, error, at });
    }

    fn poll_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Catalog { generation, result } => {
                    if generation != self.catalog_generation {
                        continue;
                    }
                    match result {
                        Ok(catalog) => {
                            self.catalog_titles = catalog
                                .iter()
                                .map(|e| (e.key.clone(), e.title.clone()))
                                .collect();
                            self.covers = catalog
                                .iter()
                                .filter_map(|e| e.cover.clone().map(|u| (e.key.clone(), u)))
                                .collect();
                            self.catalog = Loadable::Ready(catalog);
                            self.reload();
                        }
                        Err(e) => self.catalog = Loadable::Failed(e),
                    }
                }
                Msg::Preview {
                    generation,
                    input,
                    result,
                } => {
                    self.quick.accept_preview(generation, &input, result);
                }
                Msg::Progress(event) => self.apply_progress(&event),
                Msg::Check(out) => {
                    self.busy = Busy::Idle;
                    self.progress = None;
                    self.reload();
                    self.task_asked_at = f64::NEG_INFINITY; // refresh the Windows task soon
                    // Reports name series by key; show their titles instead.
                    let mut message = out.message;
                    for row in &self.rows {
                        message = message.replace(&row.key.as_state_key(), &row.title);
                    }
                    self.set_status(Some(ctx), message, !out.ok);
                }
                Msg::Schedule(status) => {
                    self.task_inflight = false;
                    self.task = TaskView::Known(status);
                }
            }
        }
    }

    fn apply_progress(&mut self, event: &CheckProgress) {
        let current = match event {
            CheckProgress::SeriesStarted { series_key, .. } => Some(self.title_of(series_key)),
            _ => None,
        };
        let Some(progress) = self.progress.as_mut() else {
            return;
        };
        match event {
            CheckProgress::Started { total } => progress.total = *total,
            CheckProgress::SeriesStarted { index, total, .. } => {
                progress.done = *index;
                progress.total = *total;
                progress.current = current;
            }
            CheckProgress::SeriesFinished { index, total, .. } => {
                progress.done = index + 1;
                progress.total = *total;
            }
        }
    }

    fn refresh_task(&mut self, ctx: &egui::Context, now: f64) {
        if cfg!(not(windows)) {
            self.task = TaskView::Known(besideme::schedule::TaskStatus::Unsupported);
            return;
        }
        if !self.task_inflight && now - self.task_asked_at >= TASK_REFRESH_SECONDS {
            self.task_inflight = true;
            self.task_asked_at = now;
            tasks::query_schedule(ctx, &self.tx);
        }
    }

    /// Cancels every close request (Alt+F4, taskbar "Close window", …) that did not come
    /// from the red traffic light.
    fn guard_close(&self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        }
    }

    /// Shrinks the window on small or heavily scaled screens so it never opens off-screen.
    fn fit_to_monitor(&mut self, ctx: &egui::Context) {
        if self.fitted {
            return;
        }
        let Some(monitor) = ctx.input(|i| i.viewport().monitor_size) else {
            return;
        };
        self.fitted = true;
        let target = vec2(
            WINDOW.x.min(monitor.x * 0.94),
            WINDOW.y.min(monitor.y * 0.86),
        )
        .max(vec2(420.0, 420.0));
        let current = ctx.input(|i| i.viewport().inner_rect.map(|r| r.size()));
        if current.is_none_or(|c| (c - target).abs().max_elem() > 1.0) {
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(target));
            ctx.send_viewport_cmd(
                ViewportCommand::center_on_screen(ctx).unwrap_or(ViewportCommand::Focus),
            );
        }
    }

    // ---------------------------------------------------------------- ui

    fn body(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();

        if let Some(settings) = &mut self.settings {
            // Scrolls on small or heavily zoomed windows so Save stays reachable.
            let action = ScrollArea::vertical()
                .id_salt("settings")
                .auto_shrink([false, false])
                .show(ui, |ui| settings.show(ui))
                .inner;
            match action {
                SettingsAction::Stay => {}
                SettingsAction::Back => self.settings = None,
                SettingsAction::Saved => {
                    self.settings = None;
                    self.reload();
                    self.set_status(Some(&ctx), "Discord settings saved.".into(), false);
                }
            }
            return;
        }

        // Normally the status strip and filters stay put and the shelf takes the rest of
        // the window. When there is hardly any height (small screens, large zoom) the
        // whole page scrolls instead, so nothing is ever clipped or overlapped.
        if ui.available_height() < CRAMPED_HEIGHT {
            ScrollArea::vertical()
                .id_salt("page")
                .auto_shrink([false, false])
                .show(ui, |ui| self.page(ui, &ctx, false));
        } else {
            self.page(ui, &ctx, true);
        }
    }

    fn page(&mut self, ui: &mut Ui, ctx: &egui::Context, scroll_shelf: bool) {
        if self.webhook_missing {
            banner(
                ui,
                "Set up the updates webhook in Settings to receive chapter announcements.",
            );
        }

        self.status_strip(ui, ctx);
        ui.add_space(theme::GRID);
        self.toolbar(ui, ctx);
        ui.add_space(theme::GRID);
        self.shelf(ui, ctx, scroll_shelf);
    }

    fn status_strip(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let now = time_now_unix();
        let (last, last_tone) = status::last_check_line(self.last_run.as_ref(), &self.rows, now);
        let schedule = status::schedule_line(&self.task, now);
        let external = self.external_check_running();
        let blocked = self.checks_blocked();
        let mut check_clicked = false;

        card(ui, |ui| {
            ui.horizontal(|ui| {
                // Leave room for the button so long lines wrap instead of running under it.
                let text_width = (ui.available_width() - 132.0).max(160.0);
                ui.vertical(|ui| {
                    ui.set_max_width(text_width);
                    ui.spacing_mut().item_spacing.y = 4.0;
                    tone_line(ui, &last, last_tone);
                    if let Some((text, tone)) = &schedule {
                        tone_line(ui, text, *tone);
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = if self.busy == Busy::Full {
                        "Checking…"
                    } else {
                        "Check now"
                    };
                    let button = ui
                        .add_enabled(!blocked && !self.rows.is_empty(), primary(label))
                        .on_hover_text(if external {
                            "A background check seems to be running; if it is, this says so instead of starting another"
                        } else {
                            "Look for new chapters now"
                        });
                    button.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::Role::Button,
                            !blocked && !self.rows.is_empty(),
                            "Check all titles now",
                        )
                    });
                    check_clicked = button.clicked();
                });
            });
            if let Some(progress) = &self.progress {
                ui.add_space(4.0);
                #[allow(clippy::cast_precision_loss)]
                let fraction = if progress.total == 0 {
                    0.0
                } else {
                    progress.done as f32 / progress.total as f32
                };
                let what = progress
                    .current
                    .as_deref()
                    .map_or_else(String::new, |t| format!(" · {t}"));
                ui.add(
                    ProgressBar::new(fraction)
                        .desired_height(14.0)
                        .corner_radius(7)
                        .fill(theme::ACCENT)
                        .text(
                            RichText::new(format!(
                                "Checking {} of {}{what}",
                                (progress.done + 1).min(progress.total.max(1)),
                                progress.total.max(1)
                            ))
                            .small()
                            .color(theme::ON_ACCENT),
                        ),
                );
            }
        });
        if check_clicked {
            self.start_check(ctx, None);
        }
    }

    fn toolbar(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let counts = data::counts(&self.rows);
        let options: Vec<(Filter, String)> = Filter::ALL
            .iter()
            .zip(counts)
            .map(|(f, n)| (*f, format!("{} {n}", f.label())))
            .collect();
        let mut open_quick = false;
        let mut open_settings = false;
        let mut chosen = None;
        let mut actions = |ui: &mut Ui, add_id: &mut Option<Id>| {
            let add = ui
                .add(primary("Add manga"))
                .on_hover_text("Search by title or paste a link (Ctrl+K)");
            add.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, "Add manga, shortcut Control K")
            });
            *add_id = Some(add.id);
            open_quick = add.clicked();
            if ui.add(pill("Settings")).clicked() {
                open_settings = true;
            }
        };
        if ui.available_width() < NARROW_WIDTH {
            // Narrow: everything flows left to right and wraps.
            ui.horizontal_wrapped(|ui| {
                chosen = segmented(ui, self.filter, &options);
                actions(ui, &mut self.add_button_id);
            });
        } else {
            ui.horizontal(|ui| {
                chosen = segmented(ui, self.filter, &options);
                // Right-aligned, but laid out left to right so Tab follows the visual order.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.allocate_ui_with_layout(
                        vec2(TOOLBAR_BUTTONS_WIDTH, 32.0),
                        Layout::left_to_right(Align::Center),
                        |ui| actions(ui, &mut self.add_button_id),
                    );
                });
            });
        }
        if let Some(f) = chosen {
            self.filter = f;
        }
        if open_quick {
            self.quick.open(ctx);
        }
        if open_settings {
            self.settings = Some(Settings::new(&self.paths.config));
        }
    }

    fn shelf(&mut self, ui: &mut Ui, ctx: &egui::Context, scroll: bool) {
        let now = ctx.input(|i| i.time);
        if let Some((_, t)) = &self.confirm_remove
            && now - t > 4.0
        {
            self.confirm_remove = None;
        }

        let visible: Vec<&Row> = self
            .rows
            .iter()
            .filter(|r| self.filter.matches(r))
            .collect();
        if visible.is_empty() {
            self.empty_state(ui, ctx);
            return;
        }

        let env = ShelfEnv {
            now_unix: time_now_unix(),
            covers: &self.covers,
            confirm_remove: self.confirm_remove.as_ref().map(|(k, _)| k),
            busy: self.checks_blocked(),
            retrying: match &self.busy {
                Busy::Retry(k) => Some(k),
                _ => None,
            },
            focus_anchor: self.focus_anchor.as_ref().map(|(k, _)| k),
        };
        let actions = if scroll {
            ScrollArea::vertical()
                .id_salt("shelf")
                .auto_shrink([false, false])
                .show(ui, |ui| shelf::draw(ui, &visible, &env))
                .inner
        } else {
            shelf::draw(ui, &visible, &env)
        };
        // Request focus for a few frames: egui applies a request one frame later.
        if let Some((_, frames)) = &mut self.focus_anchor {
            ctx.request_repaint();
            *frames = frames.saturating_sub(1);
            if *frames == 0 {
                self.focus_anchor = None;
            }
        }

        for action in actions {
            match action {
                ShelfAction::OpenUrl(url) => {
                    let fallback = "https://global.manga-up.com/";
                    let safe = data::official_or(&self.registry, &url, fallback);
                    ctx.open_url(egui::OpenUrl::new_tab(safe));
                }
                ShelfAction::MarkRead(key) => self.mark_read(ctx, &key),
                ShelfAction::Retry(key) => self.start_check(ctx, Some(key)),
                ShelfAction::AskRemove(key) => self.confirm_remove = Some((key, now)),
                ShelfAction::ConfirmRemove(key, title) => {
                    self.confirm_remove = None;
                    self.remove(ctx, &key, &title);
                }
            }
        }
    }

    fn empty_state(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let mut open_quick = false;
        let mut check = false;
        let blocked = self.checks_blocked();
        card(ui, |ui| {
            ui.add_space(8.0);
            match (self.rows.is_empty(), self.filter) {
                (true, _) => {
                    ui.label(title_text("No manga yet", 17.0));
                    ui.label(
                        RichText::new(
                            "Search the Manga UP! catalog by title or paste a link to a manga's page.",
                        )
                        .color(theme::TEXT_SECONDARY),
                    );
                    ui.add_space(4.0);
                    if ui.add(primary("Add your first manga")).clicked() {
                        open_quick = true;
                    }
                }
                (false, Filter::Unread) => {
                    ui.label(title_text("You're all caught up", 17.0));
                    ui.label(
                        RichText::new("New chapters show up here after the next check.")
                            .color(theme::TEXT_SECONDARY),
                    );
                    ui.add_space(4.0);
                    if ui.add_enabled(!blocked, pill("Check now")).clicked() {
                        check = true;
                    }
                }
                (false, Filter::Issues) => {
                    ui.label(title_text("No problems", 17.0));
                    ui.label(
                        RichText::new("Every title was checked successfully.")
                            .color(theme::TEXT_SECONDARY),
                    );
                }
                (false, Filter::All) => {
                    ui.label(RichText::new("Nothing to show.").color(theme::TEXT_SECONDARY));
                }
            }
            ui.add_space(8.0);
        });
        if open_quick {
            self.quick.open(ctx);
        }
        if check {
            self.start_check(ctx, None);
        }
    }

    fn quick_add_overlay(&mut self, ctx: &egui::Context) {
        if !self.quick.open {
            return;
        }
        let rows = &self.rows;
        let watched = |k: &SeriesKey| rows.iter().any(|r| &r.key == k);
        let env = quick_add::Env {
            registry: &self.registry,
            catalog: &self.catalog,
            covers: &self.covers,
            watched: &watched,
            now: ctx.input(|i| i.time),
        };
        let actions = self.quick.show(ctx, &env);
        for action in actions {
            match action {
                quick_add::Action::Close => self.quick.close(ctx, self.add_button_id),
                quick_add::Action::Add { url, title } => {
                    if self.add(ctx, &url, &title) {
                        self.quick.close(ctx, self.add_button_id);
                    }
                }
                quick_add::Action::RetryCatalog => self.start_catalog(ctx),
                quick_add::Action::Lookup {
                    generation,
                    input,
                    link,
                } => {
                    tasks::spawn(ctx, &self.tx, move || {
                        tasks::preview(generation, input, link)
                    });
                }
            }
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let now = ui.ctx().input(|i| i.time);
        if let Some(s) = &self.status
            && now - s.at > STATUS_SECONDS
            && !s.error
        {
            self.status = None;
        }
        ui.horizontal(|ui| {
            if let Some(s) = &self.status {
                let (mark, color) = if s.error {
                    ("✕ ", theme::DANGER)
                } else {
                    ("✓ ", theme::SUCCESS)
                };
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{mark}{}", s.text))
                            .small()
                            .color(color),
                    )
                    .truncate(),
                );
                if s.error && ui.small_button("Dismiss").clicked() {
                    self.status = None;
                }
            } else {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("Settings: {}", self.paths.config.display()))
                            .small()
                            .color(theme::TEXT_SECONDARY),
                    )
                    .truncate(),
                )
                .on_hover_text(self.paths.config.display().to_string());
            }
        });
        if self.status.as_ref().is_some_and(|s| !s.error) {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
    }
}

impl eframe::App for WatcherApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        self.poll_messages(&ctx);
        self.guard_close(&ctx);
        self.fit_to_monitor(&ctx);
        self.poll_files(now);
        self.refresh_task(&ctx, now);

        if self.settings.is_none() && ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::K)) {
            self.quick.open(&ctx);
        }

        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        Frame::NONE
            .fill(theme::BG)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .corner_radius(chrome::window_radius(maximized))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                let tb = chrome::title_bar(ui, "BesideME", maximized);
                if tb.close {
                    self.allow_close = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
                // Split the remaining area: body on top, a fixed status bar at the bottom.
                let rest = ui.available_rect_before_wrap();
                let footer_h = 34.0;
                let body_rect = egui::Rect::from_min_max(
                    rest.min + vec2(18.0, 14.0),
                    egui::pos2(rest.max.x - 18.0, rest.max.y - footer_h),
                );
                let footer_rect = egui::Rect::from_min_max(
                    egui::pos2(rest.min.x + 18.0, rest.max.y - footer_h),
                    egui::pos2(rest.max.x - 18.0, rest.max.y),
                );
                ui.painter().line_segment(
                    [footer_rect.left_top(), footer_rect.right_top()],
                    Stroke::new(1.0, theme::SEPARATOR),
                );
                let modal = self.quick.open;
                ui.scope_builder(egui::UiBuilder::new().max_rect(body_rect), |ui| {
                    // While Quick Add is open the shelf behind it is inert, which also keeps
                    // keyboard focus inside the panel.
                    ui.add_enabled_ui(!modal, |ui| self.body(ui));
                });
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(footer_rect)
                        .layout(Layout::left_to_right(Align::Center)),
                    |ui| self.status_bar(ui),
                );
                ui.allocate_rect(rest, egui::Sense::hover());
            });

        self.quick_add_overlay(&ctx);
        chrome::paint_focus_ring(&ctx);
        // Keep relative times, scheduled-task results and progress fresh while idle.
        ctx.request_repaint_after(Duration::from_secs(2));
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // transparent, so the rounded corners show
    }
}

// -------------------------------------------------------------------- helpers

fn tone_line(ui: &mut Ui, text: &str, tone: Tone) {
    let (mark, color) = match tone {
        Tone::Good => ("✓ ", theme::SUCCESS),
        Tone::Warn => ("⚠ ", theme::WARNING),
        Tone::Neutral => ("", theme::TEXT_SECONDARY),
    };
    ui.add(egui::Label::new(RichText::new(format!("{mark}{text}")).small().color(color)).wrap());
}

fn forget_state(key: &SeriesKey) {
    let Ok(path) = resolve_state_path(None) else {
        return;
    };
    if !path.exists() {
        return;
    }
    // If a background check holds the lock, leave state alone; it is harmless.
    let Ok(_lock) = InstanceLock::acquire_for_state(&path) else {
        return;
    };
    if let Ok(mut st) = StateFile::load(&path)
        && st.series.remove(&key.as_state_key()).is_some()
    {
        let _ = st.save_atomic(&path);
    }
}

fn time_now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}
