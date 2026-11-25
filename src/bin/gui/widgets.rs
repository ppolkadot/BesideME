//! Small shared widgets: cards, buttons, chips, covers and the filter control.

use eframe::egui::{
    self, Align2, Button, Color32, CornerRadius, FontId, Frame, Image, Margin, RichText, Stroke,
    Ui, Vec2, vec2,
};

use crate::theme;

/// Stable 16:9 cover slot used by the shelf (8-point grid: 14 × 8 and 8 × 8 multiples).
pub const COVER: Vec2 = Vec2::new(112.0, 63.0);
/// Compact 16:9 cover slot used in Quick Add results.
pub const COVER_SMALL: Vec2 = Vec2::new(80.0, 45.0);

pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
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

pub fn banner(ui: &mut Ui, text: &str) {
    Frame::NONE
        .fill(theme::WARNING.gamma_multiply(0.15))
        .stroke(Stroke::new(1.0, theme::WARNING.gamma_multiply(0.6)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(RichText::new(format!("⚠  {text}")).color(theme::WARNING)).wrap(),
            );
        });
    ui.add_space(theme::GRID);
}

/// Title text in Poppins `SemiBold`.
pub fn title_text(text: &str, size: f32) -> RichText {
    RichText::new(text).font(FontId::new(size, theme::title_family()))
}

/// Secondary button.
pub fn pill(text: &str) -> Button<'static> {
    Button::new(RichText::new(text.to_owned()).color(theme::TEXT))
        .fill(theme::SURFACE_HOVER)
        .corner_radius(CornerRadius::same(14))
        .min_size(vec2(0.0, 32.0))
}

/// Destructive confirmation button.
pub fn danger(text: &str) -> Button<'static> {
    Button::new(
        RichText::new(text.to_owned())
            .color(Color32::BLACK)
            .strong(),
    )
    .fill(theme::DANGER)
    .corner_radius(CornerRadius::same(14))
    .min_size(vec2(0.0, 32.0))
}

/// Filled accent button with dark text.
pub fn primary(text: &str) -> Button<'static> {
    Button::new(
        RichText::new(text.to_owned())
            .color(theme::ON_ACCENT)
            .strong(),
    )
    .fill(theme::ACCENT)
    .corner_radius(CornerRadius::same(14))
    .min_size(vec2(72.0, 32.0))
}

/// Quiet text-style button.
pub fn link(text: &str) -> Button<'static> {
    Button::new(RichText::new(text.to_owned()).color(theme::ACCENT))
        .frame(false)
        .min_size(vec2(0.0, 32.0))
}

/// A small rounded status chip. The label always carries the meaning; colour only reinforces it.
pub fn chip(ui: &mut Ui, text: &str, color: Color32) {
    Frame::NONE
        .fill(color.gamma_multiply(0.18))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.7)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(text).small().color(color)).truncate());
        });
}

/// Cover art in a fixed slot. A missing or failed image keeps the slot, with the title's
/// initial on top, so row heights never change and the title stays visible next to it.
pub fn cover(ui: &mut Ui, url: Option<&str>, title: &str, size: Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, 6, theme::SURFACE_HOVER);
    if let Some(url) = url {
        Image::new(url)
            .fit_to_exact_size(size)
            .corner_radius(6)
            .show_loading_spinner(false)
            .paint_at(ui, rect);
        return;
    }
    let initial = title
        .chars()
        .find(|c| c.is_alphanumeric())
        .map_or_else(|| "?".to_owned(), |c| c.to_uppercase().to_string());
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        initial,
        FontId::new(size.y * 0.45, theme::title_family()),
        theme::TEXT_SECONDARY,
    );
}

/// Segmented filter control. Returns the clicked option, if any.
pub fn segmented<T: Copy + PartialEq>(
    ui: &mut Ui,
    current: T,
    options: &[(T, String)],
) -> Option<T> {
    let mut chosen = None;
    Frame::NONE
        .fill(theme::SURFACE)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(|ui| {
                for (value, label) in options {
                    let selected = *value == current;
                    let button = Button::new(RichText::new(label).color(if selected {
                        theme::ON_ACCENT
                    } else {
                        theme::TEXT
                    }))
                    .fill(if selected {
                        theme::ACCENT
                    } else {
                        Color32::TRANSPARENT
                    })
                    .corner_radius(CornerRadius::same(10))
                    .min_size(vec2(0.0, 32.0));
                    let response = ui.add(button);
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(egui::Role::Tab, true, selected, label)
                    });
                    if response.clicked() {
                        chosen = Some(*value);
                    }
                }
            });
        });
    chosen
}
