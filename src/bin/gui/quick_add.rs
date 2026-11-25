//! Quick Add: one keyboard-driven panel for adding a manga by title or by link.
//!
//! * A supported link is previewed with the existing preview service; anything else searches
//!   the cached Manga UP! catalog.
//! * Adding is always an explicit action (the Add button, or Enter on the highlighted result).
//! * Every asynchronous answer carries the request generation and the text it answers, so a
//!   preview for text the user has since changed can never be added.

use std::collections::HashMap;

use besideme::lookup::SeriesPreview;
use besideme::model::SeriesKey;
use besideme::sites::{CatalogEntry, Registry, search_catalog};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, Frame, Id, Key, Layout, Margin, Modifiers, Order,
    RichText, ScrollArea, Sense, Stroke, TextEdit, Ui, vec2,
};

use crate::data::Loadable;
use crate::theme;
use crate::widgets::{COVER_SMALL, cover, pill, primary, title_text};

/// Seconds to wait after typing stops before a link is previewed.
const LINK_DEBOUNCE: f64 = 0.35;
const MAX_RESULTS: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Empty,
    Link,
    Search,
}

/// Decides how a piece of input is handled.
#[must_use]
pub fn classify(text: &str) -> InputKind {
    let t = text.trim();
    if t.is_empty() {
        InputKind::Empty
    } else if t.contains("://") || t.to_ascii_lowercase().contains("manga-up.com") {
        InputKind::Link
    } else {
        InputKind::Search
    }
}

/// The URL to look up for link-like input (a missing scheme is assumed to be `https`).
#[must_use]
pub fn link_of(text: &str) -> String {
    let t = text.trim();
    if t.contains("://") {
        t.to_owned()
    } else {
        format!("https://{t}")
    }
}

pub enum Preview {
    Idle,
    Loading {
        generation: u64,
        input: String,
    },
    Ready {
        input: String,
        data: Box<SeriesPreview>,
    },
    Failed {
        input: String,
        message: String,
    },
}

pub struct QuickAdd {
    pub open: bool,
    input: String,
    selected: usize,
    preview: Preview,
    generation: u64,
    due_at: Option<f64>,
    prev_focus: Option<Id>,
    focus_input: bool,
    scroll_to_selected: bool,
}

pub struct Env<'a> {
    pub registry: &'a Registry,
    pub catalog: &'a Loadable<Vec<CatalogEntry>>,
    pub covers: &'a HashMap<String, String>,
    pub watched: &'a dyn Fn(&SeriesKey) -> bool,
    pub now: f64,
}

pub enum Action {
    Close,
    Add {
        url: String,
        title: String,
    },
    RetryCatalog,
    Lookup {
        generation: u64,
        input: String,
        link: String,
    },
}

impl QuickAdd {
    pub const fn new() -> Self {
        Self {
            open: false,
            input: String::new(),
            selected: 0,
            preview: Preview::Idle,
            generation: 0,
            due_at: None,
            prev_focus: None,
            focus_input: false,
            scroll_to_selected: false,
        }
    }

    /// Opens the panel, remembering which control had focus so Escape can return to it.
    pub fn open(&mut self, ctx: &egui::Context) {
        if self.open {
            self.focus_input = true;
            return;
        }
        self.open = true;
        self.prev_focus = ctx.memory(egui::Memory::focused);
        self.input.clear();
        self.selected = 0;
        self.preview = Preview::Idle;
        self.generation += 1;
        self.due_at = None;
        self.focus_input = true;
    }

    /// Closes the panel and returns focus to where it was (or to `fallback`).
    pub fn close(&mut self, ctx: &egui::Context, fallback: Option<Id>) {
        self.open = false;
        self.generation += 1;
        self.preview = Preview::Idle;
        self.due_at = None;
        if let Some(id) = self.prev_focus.take().or(fallback) {
            ctx.memory_mut(|m| m.request_focus(id));
        }
    }

    fn current(&self) -> &str {
        self.input.trim()
    }

    /// The text changed: whatever was previewed or in flight no longer applies.
    fn on_input_changed(&mut self, now: f64) {
        self.selected = 0;
        self.generation += 1;
        self.preview = Preview::Idle;
        self.due_at = (classify(&self.input) == InputKind::Link).then_some(now + LINK_DEBOUNCE);
    }

    /// Starts a lookup for the current text, unless one for it is already running.
    fn begin_lookup(&mut self) -> Option<Action> {
        if classify(&self.input) != InputKind::Link {
            return None;
        }
        let input = self.current().to_owned();
        if matches!(&self.preview, Preview::Loading { input: i, .. } if *i == input) {
            return None;
        }
        self.generation += 1;
        self.due_at = None;
        self.preview = Preview::Loading {
            generation: self.generation,
            input: input.clone(),
        };
        let link = link_of(&input);
        Some(Action::Lookup {
            generation: self.generation,
            input,
            link,
        })
    }

    /// Applies a preview answer if (and only if) it still matches the panel's current request
    /// and text. Returns whether it was used.
    pub fn accept_preview(
        &mut self,
        generation: u64,
        input: &str,
        result: Result<SeriesPreview, String>,
    ) -> bool {
        let matches_request = self.open
            && matches!(
                &self.preview,
                Preview::Loading { generation: g, input: i } if *g == generation && i == input
            )
            && self.current() == input;
        if !matches_request {
            return false;
        }
        self.preview = match result {
            Ok(data) => Preview::Ready {
                input: input.to_owned(),
                data: Box::new(data),
            },
            Err(message) => Preview::Failed {
                input: input.to_owned(),
                message,
            },
        };
        true
    }

    /// The previewed series, only while it still describes the text in the box.
    fn addable(&self) -> Option<&SeriesPreview> {
        match &self.preview {
            Preview::Ready { input, data } if input == self.current() => Some(data),
            _ => None,
        }
    }

    #[cfg(test)]
    fn type_text(&mut self, text: &str, now: f64) {
        self.input = text.to_owned();
        self.on_input_changed(now);
    }

    #[cfg(test)]
    fn can_add(&self) -> bool {
        self.addable().is_some()
    }

    pub fn show(&mut self, ctx: &egui::Context, env: &Env<'_>) -> Vec<Action> {
        let mut actions = Vec::new();
        if !self.open {
            return actions;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            actions.push(Action::Close);
            return actions;
        }

        let screen = ctx.content_rect();
        egui::Area::new(Id::unique("quick-add-backdrop"))
            .order(Order::Middle)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (rect, response) = ui.allocate_exact_size(screen.size(), Sense::click());
                ui.painter()
                    .rect_filled(rect, 0, Color32::from_black_alpha(170));
                if response.clicked() {
                    actions.push(Action::Close);
                }
            });

        let width = (screen.width() - 48.0).clamp(280.0, 560.0);
        egui::Area::new(Id::unique("quick-add-panel"))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_TOP, vec2(0.0, 64.0))
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(theme::SURFACE)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .corner_radius(CornerRadius::same(14))
                    .inner_margin(Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        self.panel(ui, env, &mut actions);
                    });
            });

        // Debounced link preview.
        if let Some(due) = self.due_at {
            if env.now >= due {
                actions.extend(self.begin_lookup());
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(120));
            }
        }
        actions
    }

    #[allow(clippy::too_many_lines)]
    fn panel(&mut self, ui: &mut Ui, env: &Env<'_>, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label(title_text("Quick Add", 17.0));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new("Esc to close")
                        .small()
                        .color(theme::TEXT_SECONDARY),
                );
            });
        });
        let label = ui.label(
            RichText::new("Search by title, or paste a Manga UP! link")
                .color(theme::TEXT_SECONDARY),
        );
        let edit = ui
            .add(
                TextEdit::singleline(&mut self.input)
                    .hint_text(
                        "Smoking Behind the Supermarket, or https://global.manga-up.com/manga/261",
                    )
                    .desired_width(f32::INFINITY)
                    .margin(Margin::symmetric(10, 8)),
            )
            .labelled_by(label.id);
        if self.focus_input {
            edit.request_focus();
            self.focus_input = false;
        }
        if edit.changed() {
            self.on_input_changed(env.now);
        }

        let kind = classify(&self.input);
        let hits: Vec<&CatalogEntry> = match (kind, env.catalog) {
            (InputKind::Search, Loadable::Ready(catalog)) => {
                search_catalog(catalog, self.current(), MAX_RESULTS)
            }
            _ => Vec::new(),
        };
        if !hits.is_empty() {
            self.selected = self.selected.min(hits.len() - 1);
        }

        // Keyboard: arrows move through results, Enter adds the highlighted one.
        let (down, up) = if edit.has_focus() || edit.lost_focus() {
            ui.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::ArrowDown),
                    i.consume_key(Modifiers::NONE, Key::ArrowUp),
                )
            })
        } else {
            (false, false)
        };
        if down && !hits.is_empty() {
            self.selected = (self.selected + 1).min(hits.len() - 1);
            self.scroll_to_selected = true;
        }
        if up && !hits.is_empty() {
            self.selected = self.selected.saturating_sub(1);
            self.scroll_to_selected = true;
        }
        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if enter {
            edit.request_focus();
        }

        ui.add_space(theme::GRID);
        match kind {
            InputKind::Empty => {
                muted(
                    ui,
                    "Type a title to search, or paste a link to a manga's page.",
                );
                if let Loadable::Loading | Loadable::Idle = env.catalog {
                    loading(ui, "Loading the Manga UP! catalog…");
                }
                if let Loadable::Failed(e) = env.catalog {
                    catalog_failed(ui, e, actions);
                }
                if let Loadable::Ready(c) = env.catalog {
                    muted(ui, &format!("{} titles available.", c.len()));
                }
            }
            InputKind::Link => self.link_section(ui, env, enter, actions),
            InputKind::Search => match env.catalog {
                Loadable::Idle | Loadable::Loading => {
                    loading(ui, "Loading the Manga UP! catalog…");
                    muted(ui, "You can already paste a link instead.");
                }
                Loadable::Failed(e) => {
                    catalog_failed(ui, e, actions);
                }
                Loadable::Ready(_) => {
                    if hits.is_empty() {
                        muted(
                            ui,
                            "No matching titles. You can paste a link to the manga's page instead.",
                        );
                    } else {
                        self.results(ui, env, &hits, enter, actions);
                    }
                }
            },
        }

        ui.add_space(theme::GRID);
        ui.horizontal(|ui| {
            if ui.add(pill("Close")).clicked() {
                actions.push(Action::Close);
            }
        });
    }

    fn results(
        &mut self,
        ui: &mut Ui,
        env: &Env<'_>,
        hits: &[&CatalogEntry],
        enter: bool,
        actions: &mut Vec<Action>,
    ) {
        let selected = self.selected;
        if enter && let Some(entry) = hits.get(selected) {
            let key = SeriesKey::new("mangaup", entry.key.clone());
            if !(env.watched)(&key) {
                actions.push(Action::Add {
                    url: entry.url.clone(),
                    title: entry.title.clone(),
                });
            }
        }
        let scroll = std::mem::take(&mut self.scroll_to_selected);
        ScrollArea::vertical()
            .id_salt("quick-results")
            .max_height(320.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (i, entry) in hits.iter().enumerate() {
                    let key = SeriesKey::new("mangaup", entry.key.clone());
                    let watched = (env.watched)(&key);
                    let is_selected = i == selected;
                    let frame = Frame::NONE
                        .fill(if is_selected {
                            theme::SURFACE_HOVER
                        } else {
                            Color32::TRANSPARENT
                        })
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(6))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                cover(
                                    ui,
                                    entry
                                        .cover
                                        .as_deref()
                                        .or_else(|| env.covers.get(&entry.key).map(String::as_str)),
                                    &entry.title,
                                    COVER_SMALL,
                                );
                                let text_w = (ui.available_width() - 132.0).max(80.0);
                                ui.allocate_ui_with_layout(
                                    vec2(text_w, COVER_SMALL.y),
                                    Layout::top_down(Align::Min),
                                    |ui| {
                                        ui.set_width(text_w);
                                        ui.add(
                                            egui::Label::new(RichText::new(&entry.title).strong())
                                                .truncate(),
                                        )
                                        .on_hover_text(&entry.title);
                                        if let Some(u) = &entry.last_updated {
                                            ui.label(
                                                RichText::new(format!("Updated {u}"))
                                                    .small()
                                                    .color(theme::TEXT_SECONDARY),
                                            );
                                        }
                                        if is_selected && !watched {
                                            ui.label(
                                                RichText::new("Enter to add")
                                                    .small()
                                                    .color(theme::TEXT_SECONDARY),
                                            );
                                        }
                                    },
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if watched {
                                        ui.label(
                                            RichText::new("✓ Already watching")
                                                .color(theme::SUCCESS),
                                        );
                                    } else {
                                        let add = ui.add(primary("Add"));
                                        add.widget_info(|| {
                                            egui::WidgetInfo::labeled(
                                                egui::Role::Button,
                                                true,
                                                format!("Add {}", entry.title),
                                            )
                                        });
                                        if add.clicked() {
                                            actions.push(Action::Add {
                                                url: entry.url.clone(),
                                                title: entry.title.clone(),
                                            });
                                        }
                                    }
                                });
                            });
                        });
                    if is_selected && scroll {
                        frame.response.scroll_to_me(None);
                    }
                }
            });
    }

    fn link_section(&mut self, ui: &mut Ui, env: &Env<'_>, enter: bool, actions: &mut Vec<Action>) {
        let text = self.current().to_owned();
        let supported = reqwest::Url::parse(&link_of(&text))
            .ok()
            .and_then(|u| env.registry.resolve_url(&u).map(|r| r.series_key));
        if supported.is_none() {
            inline_error(
                ui,
                "That isn't a Manga UP! manga page. Expected a link like https://global.manga-up.com/manga/261",
            );
            return;
        }
        if enter {
            actions.extend(self.begin_lookup());
        }
        match &self.preview {
            Preview::Idle | Preview::Loading { .. } => loading(ui, "Reading the page…"),
            Preview::Failed { message, input } if *input == text => {
                inline_error(ui, message);
                if ui.add(pill("Try again")).clicked() {
                    self.preview = Preview::Idle;
                    actions.extend(self.begin_lookup());
                }
            }
            // A failure for text the user has since changed, or a finished preview (drawn below).
            Preview::Failed { .. } | Preview::Ready { .. } => {}
        }
        let Some(data) = self.addable() else {
            return;
        };
        let watched = (env.watched)(&data.key);
        let thumb = env
            .covers
            .get(&data.key.key)
            .map(String::as_str)
            .or_else(|| {
                data.snapshot
                    .chapters_newest_first
                    .first()
                    .and_then(|c| c.thumbnail.as_deref())
            });
        let title = data.snapshot.title.clone();
        let url = data.url.clone();
        let latest = data
            .snapshot
            .chapters_newest_first
            .first()
            .map_or_else(String::new, |c| {
                c.subtitle
                    .as_ref()
                    .map_or_else(|| c.label.clone(), |s| format!("{} — {s}", c.label))
            });
        let summary = format!(
            "{} · {} chapters · latest: {latest}",
            data.site_name,
            data.snapshot.chapters_newest_first.len()
        );
        if enter && !watched {
            actions.push(Action::Add {
                url: url.clone(),
                title: title.clone(),
            });
        }
        ui.horizontal(|ui| {
            cover(ui, thumb, &title, COVER_SMALL);
            let text_w = (ui.available_width() - 132.0).max(80.0);
            ui.allocate_ui_with_layout(
                vec2(text_w, COVER_SMALL.y),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.set_width(text_w);
                    ui.add(egui::Label::new(RichText::new(&title).strong()).truncate())
                        .on_hover_text(&title);
                    ui.add(
                        egui::Label::new(
                            RichText::new(&summary).small().color(theme::TEXT_SECONDARY),
                        )
                        .truncate(),
                    );
                    if !watched {
                        ui.label(
                            RichText::new("Enter to add")
                                .small()
                                .color(theme::TEXT_SECONDARY),
                        );
                    }
                },
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if watched {
                    ui.label(RichText::new("✓ Already watching").color(theme::SUCCESS));
                } else {
                    let add = ui.add(primary("Add"));
                    add.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Add {title}"))
                    });
                    if add.clicked() {
                        actions.push(Action::Add {
                            url: url.clone(),
                            title: title.clone(),
                        });
                    }
                }
            });
        });
    }
}

fn muted(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).color(theme::TEXT_SECONDARY)).wrap());
}

fn inline_error(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).color(theme::DANGER)).wrap());
}

fn loading(ui: &mut Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add(egui::Spinner::new().size(14.0));
        ui.label(RichText::new(text).color(theme::TEXT_SECONDARY));
    });
}

fn catalog_failed(ui: &mut Ui, error: &str, actions: &mut Vec<Action>) {
    inline_error(ui, &format!("Couldn't load the catalog: {error}"));
    muted(
        ui,
        "Search is unavailable right now, but you can still paste a link to a manga's page.",
    );
    if ui.add(pill("Retry catalog")).clicked() {
        actions.push(Action::RetryCatalog);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use besideme::model::SeriesSnapshot;

    fn preview(title: &str) -> SeriesPreview {
        SeriesPreview {
            key: SeriesKey::new("mangaup", "261"),
            url: "https://global.manga-up.com/manga/261".into(),
            site_name: "Manga UP!".into(),
            snapshot: SeriesSnapshot {
                title: title.into(),
                chapters_newest_first: Vec::new(),
            },
        }
    }

    fn open_panel() -> QuickAdd {
        let mut q = QuickAdd::new();
        q.open = true;
        q
    }

    #[test]
    fn input_is_routed_to_link_preview_or_catalog_search() {
        assert_eq!(classify("   "), InputKind::Empty);
        assert_eq!(
            classify("Smoking Behind the Supermarket"),
            InputKind::Search
        );
        assert_eq!(
            classify("https://global.manga-up.com/manga/261"),
            InputKind::Link
        );
        assert_eq!(classify("global.manga-up.com/manga/261"), InputKind::Link);
        assert_eq!(classify("ftp://example.com/x"), InputKind::Link);
        assert_eq!(
            link_of(" global.manga-up.com/manga/261 "),
            "https://global.manga-up.com/manga/261"
        );
        assert_eq!(link_of("https://x.test/y"), "https://x.test/y");
    }

    #[test]
    fn a_preview_for_the_current_text_can_be_added() {
        let mut q = open_panel();
        q.type_text("https://global.manga-up.com/manga/261", 0.0);
        let Some(Action::Lookup {
            generation, input, ..
        }) = q.begin_lookup()
        else {
            panic!("expected a lookup");
        };
        assert!(!q.can_add(), "nothing to add while loading");
        assert!(q.accept_preview(generation, &input, Ok(preview("A"))));
        assert!(q.can_add());
    }

    #[test]
    fn a_stale_preview_is_never_accepted_or_addable() {
        let mut q = open_panel();
        q.type_text("https://global.manga-up.com/manga/261", 0.0);
        let Some(Action::Lookup {
            generation, input, ..
        }) = q.begin_lookup()
        else {
            panic!("expected a lookup");
        };

        // The text changes while the request is in flight.
        q.type_text("https://global.manga-up.com/manga/262", 1.0);
        assert!(!q.accept_preview(generation, &input, Ok(preview("Old"))));
        assert!(!q.can_add());

        // A newer request supersedes an older one for the same text.
        let Some(Action::Lookup {
            generation: g2,
            input: i2,
            ..
        }) = q.begin_lookup()
        else {
            panic!("expected a lookup");
        };
        assert!(!q.accept_preview(generation, &i2, Ok(preview("Older"))));
        assert!(q.accept_preview(g2, &i2, Ok(preview("New"))));
        assert!(q.can_add());

        // Once the text changes again the ready preview stops being addable.
        q.type_text("https://global.manga-up.com/manga/263", 2.0);
        assert!(!q.can_add());
    }

    #[test]
    fn answers_after_the_panel_closed_are_dropped() {
        let ctx = egui::Context::default();
        let mut q = open_panel();
        q.type_text("https://global.manga-up.com/manga/261", 0.0);
        let Some(Action::Lookup {
            generation, input, ..
        }) = q.begin_lookup()
        else {
            panic!("expected a lookup");
        };
        q.close(&ctx, None);
        assert!(!q.accept_preview(generation, &input, Ok(preview("A"))));
        assert!(!q.can_add());
    }

    #[test]
    fn failed_lookups_are_kept_for_display_and_can_be_retried() {
        let mut q = open_panel();
        q.type_text("https://global.manga-up.com/manga/261", 0.0);
        let Some(Action::Lookup {
            generation, input, ..
        }) = q.begin_lookup()
        else {
            panic!("expected a lookup");
        };
        assert!(q.accept_preview(generation, &input, Err("couldn't reach the site".into())));
        assert!(!q.can_add());
        assert!(matches!(q.preview, Preview::Failed { .. }));
        q.preview = Preview::Idle;
        assert!(q.begin_lookup().is_some());
    }

    #[test]
    fn links_are_previewed_after_a_pause_and_searches_are_not() {
        let mut q = open_panel();
        q.type_text("https://global.manga-up.com/manga/261", 10.0);
        assert!(q.due_at.is_some_and(|d| d > 10.0));
        q.type_text("smok", 11.0);
        assert!(q.due_at.is_none());
        assert!(q.begin_lookup().is_none());
    }

    /// Runs one headless frame in which a focusable widget exists for each of `ids`.
    fn frame(ctx: &egui::Context, ids: &[Id]) {
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            for id in ids {
                ui.interact(
                    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), vec2(10.0, 10.0)),
                    *id,
                    Sense::click(),
                );
            }
        });
        // Textures produced by a headless frame must be dropped explicitly.
        output.textures_delta.clear();
    }

    #[test]
    fn opening_remembers_focus_and_closing_returns_it() {
        let ctx = egui::Context::default();
        let remembered = Id::unique("remembered");
        let fallback = Id::unique("fallback");
        let ids = [remembered, fallback];
        frame(&ctx, &ids);
        ctx.memory_mut(|m| m.request_focus(remembered));
        frame(&ctx, &ids);
        assert_eq!(ctx.memory(egui::Memory::focused), Some(remembered));

        let mut q = QuickAdd::new();
        q.open(&ctx);
        assert_eq!(q.prev_focus, Some(remembered));

        // Focus moves into the panel; Escape closes it and focus returns.
        ctx.memory_mut(|m| m.request_focus(fallback));
        frame(&ctx, &ids);
        q.close(&ctx, Some(fallback));
        frame(&ctx, &ids);
        assert_eq!(ctx.memory(egui::Memory::focused), Some(remembered));

        // With nothing remembered, focus goes to the fallback (the Add button).
        let mut q = QuickAdd::new();
        ctx.memory_mut(|m| m.surrender_focus(remembered));
        frame(&ctx, &ids);
        q.open(&ctx);
        assert_eq!(q.prev_focus, None);
        q.close(&ctx, Some(fallback));
        frame(&ctx, &ids);
        assert_eq!(ctx.memory(egui::Memory::focused), Some(fallback));
    }
}
