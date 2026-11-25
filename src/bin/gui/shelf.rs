//! The release shelf: one cover-led card per watched title.

use std::collections::HashMap;
use std::fmt::Write as _;

use besideme::model::SeriesKey;
use eframe::egui::{self, Align, Layout, RichText, Ui, vec2};

use crate::data::Row;
use crate::status::ago;
use crate::theme;
use crate::widgets::{COVER, chip, cover, danger, link, pill, primary, title_text};

/// What the user asked for on a card.
pub enum ShelfAction {
    /// Open an official provider page in the browser.
    OpenUrl(String),
    MarkRead(SeriesKey),
    Retry(SeriesKey),
    AskRemove(SeriesKey),
    ConfirmRemove(SeriesKey, String),
}

pub struct ShelfEnv<'a> {
    pub now_unix: i64,
    pub covers: &'a HashMap<String, String>,
    pub confirm_remove: Option<&'a SeriesKey>,
    /// A check is running, so retries are unavailable.
    pub busy: bool,
    pub retrying: Option<&'a SeriesKey>,
    /// Title whose "Open series" button should receive keyboard focus.
    pub focus_anchor: Option<&'a SeriesKey>,
}

fn labelled(response: &egui::Response, name: &str) {
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, response.enabled(), name));
}

/// Draws the cards and returns what was clicked.
pub fn draw(ui: &mut Ui, rows: &[&Row], env: &ShelfEnv<'_>) -> Vec<ShelfAction> {
    let mut actions = Vec::new();
    for row in rows {
        ui.push_id(row.key.as_state_key(), |ui| {
            card(ui, row, env, &mut actions);
        });
    }
    actions
}

#[allow(clippy::too_many_lines)]
fn card(ui: &mut Ui, row: &Row, env: &ShelfEnv<'_>, actions: &mut Vec<ShelfAction>) {
    egui::Frame::NONE
        .fill(theme::SURFACE)
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                cover(
                    ui,
                    env.covers.get(&row.key.key).map(String::as_str),
                    &row.title,
                    COVER,
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.set_min_height(COVER.y);
                    ui.add(egui::Label::new(title_text(&row.title, 15.0)).truncate())
                        .on_hover_text(&row.title);

                    // Status line: unread / issue chips, then the quiet detail.
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if let Some(newest) = row.unread.first() {
                            let more = row.unread.len() - 1;
                            let mut text = format!("New · {}", newest.label);
                            if newest.parts > 1 {
                                let _ = write!(text, " ({} parts)", newest.parts);
                            }
                            if more > 0 {
                                let _ = write!(text, " +{more} more");
                            }
                            chip(ui, &text, theme::ACCENT);
                        }
                        if row.has_issue() {
                            chip(ui, "⚠ Couldn't check", theme::WARNING);
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new(detail(row, env.now_unix))
                                    .small()
                                    .color(theme::TEXT_SECONDARY),
                            )
                            .wrap(),
                        );
                    });
                    if let Some(reason) = &row.last_error {
                        let failed = if row.failures > 1 {
                            format!(" ({} checks in a row)", row.failures)
                        } else {
                            String::new()
                        };
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{reason}{failed}"))
                                    .small()
                                    .color(theme::WARNING),
                            )
                            .wrap(),
                        );
                    }

                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                        buttons(ui, row, env, actions);
                    });
                });
            });
        });
    ui.add_space(theme::GRID);
}

fn detail(row: &Row, now: i64) -> String {
    let latest = row.latest.as_deref().map_or_else(
        || "Waiting for the first check".to_owned(),
        |l| format!("Latest: {l}"),
    );
    match row.last_success {
        Some(t) if row.latest.is_some() => format!("{latest} · checked {}", ago(now - t)),
        _ => latest,
    }
}

fn buttons(ui: &mut Ui, row: &Row, env: &ShelfEnv<'_>, actions: &mut Vec<ShelfAction>) {
    if let Some(newest) = row.unread.first() {
        let open = ui.add(primary("Open chapter"));
        labelled(&open, &format!("Open {} of {}", newest.label, row.title));
        if open.clicked() {
            actions.push(ShelfAction::OpenUrl(newest.url.clone()));
        }
        let mark = ui.add(pill("Mark read"));
        labelled(&mark, &format!("Mark {} read", row.title));
        if mark.clicked() {
            actions.push(ShelfAction::MarkRead(row.key.clone()));
        }
    }

    let series = ui
        .push_id("open-series", |ui| ui.add(link("Open series ↗")))
        .inner;
    labelled(&series, &format!("Open {} on Manga UP!", row.title));
    // "Open series" outlives the card's other buttons, so it takes keyboard focus when the
    // button that had it (Mark read) disappears.
    if env.focus_anchor == Some(&row.key) {
        series.request_focus();
    }
    let series = series.on_hover_text(&row.url);
    if series.clicked() {
        actions.push(ShelfAction::OpenUrl(row.url.clone()));
    }

    if row.has_issue() {
        let remaining = row
            .retry_ready_at
            .map(|t| t - env.now_unix)
            .filter(|s| *s > 0);
        let retrying_this = env.retrying == Some(&row.key);
        let enabled = !env.busy && remaining.is_none();
        let retry = ui.add_enabled(
            enabled,
            pill(if retrying_this {
                "Retrying…"
            } else {
                "Retry"
            }),
        );
        labelled(&retry, &format!("Retry checking {}", row.title));
        let retry = retry.on_hover_text(match remaining {
            Some(s) => format!("Available again in {s}s"),
            None if env.busy => "Wait for the current check to finish".to_owned(),
            None => "Check only this title again".to_owned(),
        });
        if retry.clicked() {
            actions.push(ShelfAction::Retry(row.key.clone()));
        }
    }

    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let confirming = env.confirm_remove == Some(&row.key);
        let remove = ui.add(if confirming {
            danger("Confirm remove")
        } else {
            pill("Remove")
        });
        labelled(
            &remove,
            &if confirming {
                format!("Confirm removing {}", row.title)
            } else {
                format!("Remove {}", row.title)
            },
        );
        if remove.clicked() {
            actions.push(if confirming {
                ShelfAction::ConfirmRemove(row.key.clone(), row.title.clone())
            } else {
                ShelfAction::AskRemove(row.key.clone())
            });
        }
    });
}
