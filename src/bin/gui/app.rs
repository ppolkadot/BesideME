//! The `BesideME` window: list watched manga, add by name search or by link, remove,
//! and run a check on demand.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

use besideme::config_edit::{
    self, EditError, add_series, gui_config_path, list_series, read_or_default, remove_series,
    validate_and_write,
};
use besideme::lookup::SeriesPreview;
use besideme::model::SeriesKey;
use besideme::paths::resolve_state_path;
use besideme::sites::{CatalogEntry, Registry, search_catalog};
use besideme::state::{InstanceLock, StateFile};
use besideme::webhook_settings::WebhookSettings;
use eframe::egui::{
    self, Align, Button, Color32, CornerRadius, Frame, Image, Key, Layout, Margin, RichText,
    ScrollArea, Stroke, TextEdit, Ui, ViewportCommand, vec2,
};

use crate::chrome;
use crate::settings::{Settings, SettingsAction};
use crate::tasks::{self, Msg};
use crate::theme;

const COVER: egui::Vec2 = egui::Vec2::new(80.0, 45.0); // Manga UP! list art is 16:9
const STATUS_SECONDS: f64 = 8.0;

#[derive(PartialEq, Eq, Clone, Copy)]
enum Tab {
    Search,
    Link,
}

enum Loadable<T> {
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

struct WatchedRow {
    key: SeriesKey,
    url: String,
    title: String,
    last_label: Option<String>,
    last_success: Option<i64>,
    failures: u32,
}

struct Status {
    text: String,
    error: bool,
    at: f64,
}

pub struct WatcherApp {
    registry: Registry,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    config_path: PathBuf,
    watched: Vec<WatchedRow>,
    catalog: Loadable<Vec<CatalogEntry>>,
    tab: Tab,
    query: String,
    link: String,
    preview: Loadable<SeriesPreview>,
    checking: bool,
    status: Option<Status>,
    confirm_remove: Option<(SeriesKey, f64)>,
    webhook_missing: bool,
    settings: Option<Settings>,
    /// Set only by the red traffic light; every other close request is cancelled.
    allow_close: bool,
}

impl WatcherApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let (tx, rx) = channel();
        let mut app = Self {
            registry: Registry::with_defaults(),
            tx,
            rx,
            config_path: PathBuf::new(),
            watched: Vec::new(),
            catalog: Loadable::Idle,
            tab: Tab::Search,
            query: String::new(),
            link: String::new(),
            preview: Loadable::Idle,
            checking: false,
            status: None,
            confirm_remove: None,
            webhook_missing: false,
            settings: None,
            allow_close: false,
        };
        app.refresh();
        app.start_catalog(&cc.egui_ctx);
        app
    }

    // ---------------------------------------------------------------- data

    fn refresh(&mut self) {
        let p = gui_config_path();
        self.config_path = std::path::absolute(&p).unwrap_or(p);
        let text = read_or_default(&self.config_path).unwrap_or_default();
        let listed = match list_series(&text, &self.registry) {
            Ok(l) => l,
            Err(e) => {
                self.set_status(None, format!("config.toml problem: {e}"), true);
                Vec::new()
            }
        };
        let state = resolve_state_path(None)
            .ok()
            .and_then(|p| StateFile::load(&p).ok())
            .unwrap_or_default();
        let catalog_titles: HashMap<&str, &str> = match &self.catalog {
            Loadable::Ready(c) => c
                .iter()
                .map(|e| (e.key.as_str(), e.title.as_str()))
                .collect(),
            _ => HashMap::new(),
        };
        self.watched = listed
            .into_iter()
            .map(|s| {
                let st = state.series.get(&s.key.as_state_key());
                let title = s
                    .label
                    .clone()
                    .or_else(|| {
                        catalog_titles
                            .get(s.key.key.as_str())
                            .map(ToString::to_string)
                    })
                    .or(s.comment_title.clone())
                    .unwrap_or_else(|| format!("Manga #{}", s.key.key));
                WatchedRow {
                    title,
                    url: s.url,
                    last_label: st.and_then(|x| x.last_announced_label.clone()),
                    last_success: st.and_then(|x| x.last_success_unix),
                    failures: st.map_or(0, |x| x.consecutive_failures),
                    key: s.key,
                }
            })
            .collect();

        self.webhook_missing = !WebhookSettings::load(&self.config_path)
            .is_ok_and(|settings| settings.updates_configured());
    }

    fn is_watched(&self, key: &SeriesKey) -> bool {
        self.watched.iter().any(|w| &w.key == key)
    }

    fn start_catalog(&mut self, ctx: &egui::Context) {
        self.catalog = Loadable::Loading;
        tasks::spawn(ctx, &self.tx, tasks::load_catalog);
    }

    fn start_check(&mut self, ctx: &egui::Context) {
        if self.checking {
            return;
        }
        self.checking = true;
        tasks::spawn(ctx, &self.tx, tasks::check_now);
    }

    fn add(&mut self, ctx: &egui::Context, url: &str, title: &str) {
        let result = read_or_default(&self.config_path).and_then(|text| {
            let new = add_series(&text, &self.registry, url, Some(title))?;
            validate_and_write(&new, &self.config_path, &self.registry)
        });
        match result {
            Ok(()) => {
                self.refresh();
                if self.webhook_missing {
                    self.set_status(
                        Some(ctx),
                        format!("Added {title}. It will be checked once a Discord webhook is set."),
                        false,
                    );
                } else {
                    self.set_status(Some(ctx), format!("Added {title}. Setting it up…"), false);
                    // Silent baseline right away, so the app shows the latest chapter.
                    self.start_check(ctx);
                }
            }
            Err(EditError::AlreadyPresent) => {
                self.set_status(
                    Some(ctx),
                    format!("{title} is already being watched."),
                    false,
                );
            }
            Err(e) => self.set_status(Some(ctx), format!("Couldn't add: {e}"), true),
        }
    }

    fn remove(&mut self, ctx: &egui::Context, key: &SeriesKey, title: &str) {
        let result = read_or_default(&self.config_path).and_then(|text| {
            let new = remove_series(&text, &self.registry, key)?;
            validate_and_write(&new, &self.config_path, &self.registry)
        });
        match result {
            Ok(()) => {
                forget_state(key);
                self.refresh();
                self.set_status(Some(ctx), format!("Stopped watching {title}."), false);
            }
            Err(e) => self.set_status(Some(ctx), format!("Couldn't remove: {e}"), true),
        }
    }

    fn set_status(&mut self, ctx: Option<&egui::Context>, text: String, error: bool) {
        let at = ctx.map_or(0.0, |c| c.input(|i| i.time));
        self.status = Some(Status { text, error, at });
    }

    fn poll_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Catalog(Ok(c)) => {
                    self.catalog = Loadable::Ready(c);
                    self.refresh();
                }
                Msg::Catalog(Err(e)) => self.catalog = Loadable::Failed(e),
                Msg::Preview(r) => {
                    self.preview = match r {
                        Ok(p) => Loadable::Ready(p),
                        Err(e) => Loadable::Failed(e),
                    };
                }
                Msg::Check(out) => {
                    self.checking = false;
                    self.refresh();
                    self.set_status(Some(ctx), out.message, !out.ok);
                }
            }
        }
    }

    /// Cancels every close request (Alt+F4, taskbar "Close window", …) that did not come
    /// from the red traffic light.
    fn guard_close(&self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        }
    }

    // ---------------------------------------------------------------- ui

    fn body(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();

        if let Some(settings) = &mut self.settings {
            match settings.show(ui) {
                SettingsAction::Stay => {}
                SettingsAction::Back => self.settings = None,
                SettingsAction::Saved => {
                    self.settings = None;
                    self.refresh();
                    self.set_status(Some(&ctx), "Discord settings saved.".into(), false);
                }
            }
            return;
        }

        if self.webhook_missing {
            banner(
                ui,
                "Set up the updates webhook in Settings to receive chapter announcements.",
            );
        }

        // ---- Watching
        ui.horizontal(|ui| {
            ui.label(RichText::new("Watching").heading().strong());
            ui.label(
                RichText::new(self.watched.len().to_string())
                    .color(theme::TEXT_SECONDARY)
                    .heading(),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(pill("Settings", false)).clicked() {
                    self.settings = Some(Settings::new(&self.config_path));
                }
                let label = if self.checking { "Checking…" } else { "Check now" };
                let btn = ui
                    .add_enabled(!self.checking && !self.watched.is_empty(), pill(label, false))
                    .on_hover_text("Look for new chapters now (the background task also does this every 30 minutes)");
                if self.checking {
                    ui.add(egui::Spinner::new().size(16.0));
                }
                if btn.clicked() {
                    self.start_check(&ctx);
                }
            });
        });

        let now_unix = time_now_unix();
        let now = ctx.input(|i| i.time);
        if let Some((_, t)) = &self.confirm_remove
            && now - t > 4.0
        {
            self.confirm_remove = None;
        }
        let mut remove_clicked: Option<(SeriesKey, String)> = None;

        card(ui, |ui| {
            if self.watched.is_empty() {
                ui.add_space(6.0);
                ui.label(
                    RichText::new("No manga yet. Search for one below or paste its link.")
                        .color(theme::TEXT_SECONDARY),
                );
                ui.add_space(6.0);
                return;
            }
            ScrollArea::vertical()
                .id_salt("watched")
                .max_height(176.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let covers = self.cover_map();
                    for (i, row) in self.watched.iter().enumerate() {
                        if i > 0 {
                            ui.separator();
                        }
                        ui.horizontal(|ui| {
                            cover(ui, covers.get(&row.key.key).map(String::as_str));
                            let text_w = (ui.available_width() - 190.0).max(120.0);
                            ui.allocate_ui_with_layout(
                                vec2(text_w, COVER.y + 8.0),
                                Layout::top_down(Align::Min),
                                |ui| {
                                    ui.set_width(text_w);
                                    ui.add_space(2.0);
                                    ui.add(
                                        egui::Label::new(RichText::new(&row.title).strong())
                                            .truncate(),
                                    )
                                    .on_hover_text(&row.title);
                                    let detail = match (&row.last_label, row.last_success) {
                                        (Some(l), Some(t)) => {
                                            format!("Latest: {l} · checked {}", ago(now_unix - t))
                                        }
                                        (Some(l), None) => format!("Latest: {l}"),
                                        _ => "Waiting for the first check".into(),
                                    };
                                    ui.label(
                                        RichText::new(detail).small().color(theme::TEXT_SECONDARY),
                                    );
                                    if row.failures > 0 {
                                        ui.label(
                                            RichText::new(format!(
                                                "⚠ last {} check(s) failed",
                                                row.failures
                                            ))
                                            .small()
                                            .color(theme::WARNING),
                                        );
                                    }
                                },
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let confirming = self
                                    .confirm_remove
                                    .as_ref()
                                    .is_some_and(|(k, _)| k == &row.key);
                                let b = if confirming {
                                    pill("Confirm remove", true)
                                } else {
                                    pill("Remove", false)
                                };
                                if ui.add(b).clicked() {
                                    if confirming {
                                        remove_clicked = Some((row.key.clone(), row.title.clone()));
                                    } else {
                                        self.confirm_remove = Some((row.key.clone(), now));
                                    }
                                }
                                if ui
                                    .add(
                                        Button::new(RichText::new("Open ↗").color(theme::ACCENT))
                                            .frame(false),
                                    )
                                    .on_hover_text(&row.url)
                                    .clicked()
                                {
                                    ctx.open_url(egui::OpenUrl::new_tab(&row.url));
                                }
                            });
                        });
                    }
                });
        });
        if let Some((key, title)) = remove_clicked {
            self.confirm_remove = None;
            self.remove(&ctx, &key, &title);
        }

        ui.add_space(10.0);

        // ---- Add
        ui.horizontal(|ui| {
            ui.label(RichText::new("Add a manga").heading().strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                segmented(ui, &mut self.tab);
            });
        });

        card(ui, |ui| match self.tab {
            Tab::Search => self.search_tab(ui),
            Tab::Link => self.link_tab(ui),
        });
    }

    fn cover_map(&self) -> HashMap<String, String> {
        match &self.catalog {
            Loadable::Ready(c) => c
                .iter()
                .filter_map(|e| e.cover.clone().map(|u| (e.key.clone(), u)))
                .collect(),
            _ => HashMap::new(),
        }
    }

    fn search_tab(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        ui.add(
            TextEdit::singleline(&mut self.query)
                .hint_text("Type a title, e.g. Smoking Behind the Supermarket")
                .desired_width(f32::INFINITY)
                .margin(Margin::symmetric(10, 8)),
        );

        let mut add: Option<(String, String)> = None;
        match &self.catalog {
            Loadable::Idle | Loadable::Loading => {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label(
                        RichText::new("Loading the Manga UP! catalog…")
                            .color(theme::TEXT_SECONDARY),
                    );
                });
            }
            Loadable::Failed(e) => {
                ui.label(
                    RichText::new(format!("Couldn't load the catalog: {e}")).color(theme::DANGER),
                );
                if ui.add(pill("Retry", false)).clicked() {
                    self.start_catalog(&ctx);
                }
            }
            Loadable::Ready(catalog) => {
                let hits = search_catalog(catalog, &self.query, 40);
                if self.query.trim().is_empty() {
                    ui.label(
                        RichText::new(format!(
                            "{} titles available. Start typing to search.",
                            catalog.len()
                        ))
                        .color(theme::TEXT_SECONDARY),
                    );
                } else if hits.is_empty() {
                    ui.label(RichText::new("No matching titles.").color(theme::TEXT_SECONDARY));
                } else {
                    ScrollArea::vertical()
                        .id_salt("results")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (i, e) in hits.iter().enumerate() {
                                if i > 0 {
                                    ui.separator();
                                }
                                let watched =
                                    self.is_watched(&SeriesKey::new("mangaup", e.key.clone()));
                                ui.horizontal(|ui| {
                                    cover(ui, e.cover.as_deref());
                                    let text_w = (ui.available_width() - 100.0).max(120.0);
                                    ui.allocate_ui_with_layout(
                                        vec2(text_w, COVER.y + 8.0),
                                        Layout::top_down(Align::Min),
                                        |ui| {
                                            ui.set_width(text_w);
                                            ui.add_space(2.0);
                                            ui.add(
                                                egui::Label::new(RichText::new(&e.title).strong())
                                                    .truncate(),
                                            )
                                            .on_hover_text(&e.title);
                                            if let Some(u) = &e.last_updated {
                                                ui.label(
                                                    RichText::new(format!("Updated {u}"))
                                                        .small()
                                                        .color(theme::TEXT_SECONDARY),
                                                );
                                            }
                                        },
                                    );
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if watched {
                                            ui.label(
                                                RichText::new("✓ Watching").color(theme::SUCCESS),
                                            );
                                        } else if ui.add(primary("+ Add")).clicked() {
                                            add = Some((e.url.clone(), e.title.clone()));
                                        }
                                    });
                                });
                            }
                        });
                }
            }
        }
        if let Some((url, title)) = add {
            self.add(&ctx, &url, &title);
        }
    }

    fn link_tab(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut lookup = false;
        ui.horizontal(|ui| {
            let edit = ui.add(
                TextEdit::singleline(&mut self.link)
                    .hint_text("https://global.manga-up.com/manga/261")
                    .desired_width(ui.available_width() - 96.0)
                    .margin(Margin::symmetric(10, 8)),
            );
            if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                lookup = true;
            }
            let busy = matches!(self.preview, Loadable::Loading);
            if ui
                .add_enabled(!busy && !self.link.trim().is_empty(), primary("Look up"))
                .clicked()
            {
                lookup = true;
            }
        });
        if lookup && !self.link.trim().is_empty() {
            self.preview = Loadable::Loading;
            let link = self.link.trim().to_string();
            tasks::spawn(&ctx, &self.tx, move || tasks::preview(link));
        }

        let mut add: Option<(String, String)> = None;
        match &self.preview {
            Loadable::Idle => {
                ui.label(
                    RichText::new("Paste the link of a manga's page and press Look up.")
                        .color(theme::TEXT_SECONDARY),
                );
            }
            Loadable::Loading => {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label(RichText::new("Reading the page…").color(theme::TEXT_SECONDARY));
                });
            }
            Loadable::Failed(e) => {
                ui.label(RichText::new(e).color(theme::DANGER));
            }
            Loadable::Ready(p) => {
                let covers = self.cover_map();
                let thumb = covers.get(&p.key.key).map(String::as_str).or_else(|| {
                    p.snapshot
                        .chapters_newest_first
                        .first()
                        .and_then(|c| c.thumbnail.as_deref())
                });
                let watched = self.is_watched(&p.key);
                ui.horizontal(|ui| {
                    cover(ui, thumb);
                    let text_w = (ui.available_width() - 100.0).max(120.0);
                    ui.allocate_ui_with_layout(
                        vec2(text_w, COVER.y + 8.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_width(text_w);
                            ui.add_space(2.0);
                            ui.add(
                                egui::Label::new(RichText::new(&p.snapshot.title).strong())
                                    .truncate(),
                            )
                            .on_hover_text(&p.snapshot.title);
                            let n = p.snapshot.chapters_newest_first.len();
                            let latest = p.snapshot.chapters_newest_first.first().map_or_else(
                                String::new,
                                |c| {
                                    c.subtitle.as_ref().map_or_else(
                                        || c.label.clone(),
                                        |s| format!("{} — {s}", c.label),
                                    )
                                },
                            );
                            ui.label(
                                RichText::new(format!(
                                    "{} · {n} chapters · latest: {latest}",
                                    p.site_name
                                ))
                                .small()
                                .color(theme::TEXT_SECONDARY),
                            );
                        },
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if watched {
                            ui.label(RichText::new("✓ Watching").color(theme::SUCCESS));
                        } else if ui.add(primary("+ Add")).clicked() {
                            add = Some((p.url.clone(), p.snapshot.title.clone()));
                        }
                    });
                });
            }
        }
        if let Some((url, title)) = add {
            self.add(&ctx, &url, &title);
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
                let color = if s.error {
                    theme::DANGER
                } else {
                    theme::SUCCESS
                };
                ui.add(egui::Label::new(RichText::new(&s.text).small().color(color)).truncate());
                if s.error && ui.small_button("✕").clicked() {
                    self.status = None;
                }
            } else {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("Settings: {}", self.config_path.display()))
                            .small()
                            .color(theme::TEXT_SECONDARY),
                    )
                    .truncate(),
                )
                .on_hover_text(self.config_path.display().to_string());
            }
        });
        if self.status.as_ref().is_some_and(|s| !s.error) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
        }
    }
}

impl eframe::App for WatcherApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_messages(&ctx);
        self.guard_close(&ctx);

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
                ui.scope_builder(egui::UiBuilder::new().max_rect(body_rect), |ui| {
                    self.body(ui);
                });
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(footer_rect)
                        .layout(Layout::left_to_right(Align::Center)),
                    |ui| self.status_bar(ui),
                );
                ui.allocate_rect(rest, egui::Sense::hover());
            });
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // transparent, so the rounded corners show
    }
}

// -------------------------------------------------------------------- helpers

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

fn ago(secs: i64) -> String {
    match secs {
        s if s < 90 => "just now".into(),
        s if s < 90 * 60 => format!("{} min ago", s / 60),
        s if s < 36 * 3600 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86400),
    }
}

fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::NONE
        .fill(theme::SURFACE)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn banner(ui: &mut Ui, text: &str) {
    Frame::NONE
        .fill(theme::WARNING.gamma_multiply(0.15))
        .stroke(Stroke::new(1.0, theme::WARNING.gamma_multiply(0.6)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("⚠  {text}")).color(theme::WARNING));
        });
    ui.add_space(8.0);
}

fn cover(ui: &mut Ui, url: Option<&str>) {
    if let Some(u) = url {
        ui.add(Image::new(u).fit_to_exact_size(COVER).corner_radius(6));
    } else {
        let (r, _) = ui.allocate_exact_size(COVER, egui::Sense::hover());
        ui.painter().rect_filled(r, 6, theme::SURFACE_HOVER);
        ui.painter().text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            "📖",
            egui::FontId::proportional(16.0),
            theme::TEXT_SECONDARY,
        );
    }
}

fn pill(text: &str, danger: bool) -> Button<'static> {
    let (fg, bg) = if danger {
        (Color32::WHITE, theme::DANGER)
    } else {
        (theme::TEXT, theme::SURFACE_HOVER)
    };
    Button::new(RichText::new(text.to_owned()).color(fg))
        .fill(bg)
        .corner_radius(CornerRadius::same(14))
}

fn primary(text: &str) -> Button<'static> {
    Button::new(
        RichText::new(text.to_owned())
            .color(Color32::WHITE)
            .strong(),
    )
    .fill(theme::ACCENT)
    .corner_radius(CornerRadius::same(14))
    .min_size(vec2(72.0, 28.0))
}

fn segmented(ui: &mut Ui, tab: &mut Tab) {
    Frame::NONE
        .fill(theme::SURFACE)
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::same(2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            // right_to_left parent: add in reverse visual order
            for (t, label) in [(Tab::Link, "Paste a link"), (Tab::Search, "Search by name")] {
                let selected = *tab == t;
                let b = Button::new(RichText::new(label).color(if selected {
                    theme::TEXT
                } else {
                    theme::TEXT_SECONDARY
                }))
                .fill(if selected {
                    theme::SURFACE_HOVER
                } else {
                    Color32::TRANSPARENT
                })
                .stroke(Stroke::NONE)
                .corner_radius(CornerRadius::same(7));
                if ui.add(b).clicked() {
                    *tab = t;
                }
            }
        });
}

#[allow(dead_code)]
fn _assert_send() {
    fn is_send<T: Send>() {}
    is_send::<config_edit::ListedSeries>();
}
