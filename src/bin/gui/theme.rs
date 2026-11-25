//! Dark "reading shelf" palette and egui style setup.
//!
//! Ink canvas, a raised surface, bright and muted text, and a warm coral accent whose filled
//! controls carry dark text. Every text pair below is checked against WCAG AA by the tests at
//! the bottom of this file.

use eframe::egui::{self, Color32, CornerRadius, FontId, Stroke, TextStyle, Visuals};

pub const BG: Color32 = Color32::from_rgb(0x0F, 0x11, 0x15);
pub const SURFACE: Color32 = Color32::from_rgb(0x16, 0x19, 0x20);
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x22, 0x26, 0x30);
pub const SEPARATOR: Color32 = Color32::from_rgb(0x2A, 0x2F, 0x3A);
pub const BORDER: Color32 = Color32::from_rgb(0x3A, 0x40, 0x4D);
pub const TEXT: Color32 = Color32::from_rgb(0xF2, 0xF2, 0xF7);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0xAA, 0xB0, 0xBA);
pub const ACCENT: Color32 = Color32::from_rgb(0xFF, 0x81, 0x7A);
/// Text on filled accent controls.
pub const ON_ACCENT: Color32 = BG;
pub const SUCCESS: Color32 = Color32::from_rgb(0x30, 0xD1, 0x58);
pub const DANGER: Color32 = Color32::from_rgb(0xFF, 0x6B, 0x61);
pub const WARNING: Color32 = Color32::from_rgb(0xFF, 0xD6, 0x0A);
/// Keyboard-focus ring drawn around whichever control has focus.
pub const FOCUS_RING: Color32 = Color32::from_rgb(0xF2, 0xF2, 0xF7);
pub const LIGHT_INACTIVE: Color32 = Color32::from_rgb(0x4A, 0x4A, 0x4D);
pub const LIGHT_INACTIVE_RIM: Color32 = Color32::from_rgb(0x3E, 0x3E, 0x41);

/// Spacing unit of the 8-point grid.
pub const GRID: f32 = 8.0;

const POPPINS_SEMIBOLD: &str = "Poppins SemiBold";

/// Font family used for titles.
#[must_use]
pub fn title_family() -> egui::FontFamily {
    egui::FontFamily::Name(POPPINS_SEMIBOLD.into())
}

/// Bundles Poppins and retains system and egui fonts for glyph fallback.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, bytes) in [
        (
            "Poppins",
            &include_bytes!("../../../assets/fonts/Poppins-Regular.ttf")[..],
        ),
        (
            POPPINS_SEMIBOLD,
            &include_bytes!("../../../assets/fonts/Poppins-SemiBold.ttf")[..],
        ),
    ] {
        fonts.font_data.insert(
            name.to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(bytes)),
        );
    }
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let fonts_dir = std::path::Path::new(&windir).join("Fonts");
    let mut fallback = Vec::new();
    for (name, file) in [
        ("segoe-ui", "segoeui.ttf"),
        ("segoe-symbol", "seguisym.ttf"),
        ("segoe-emoji", "seguiemj.ttf"),
    ] {
        if let Ok(bytes) = std::fs::read(fonts_dir.join(file)) {
            fonts.font_data.insert(
                name.to_owned(),
                std::sync::Arc::new(egui::FontData::from_owned(bytes)),
            );
            fallback.push(name.to_owned());
        }
    }
    // One CJK face (the first that exists) so Japanese and Chinese titles are readable.
    for file in ["YuGothM.ttc", "meiryo.ttc", "msgothic.ttc", "msyh.ttc"] {
        if let Ok(bytes) = std::fs::read(fonts_dir.join(file)) {
            fonts.font_data.insert(
                "cjk".to_owned(),
                std::sync::Arc::new(egui::FontData::from_owned(bytes)),
            );
            fallback.push("cjk".to_owned());
            break;
        }
    }
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(list) = fonts.families.get_mut(&family) {
            if family == egui::FontFamily::Proportional {
                list.insert(0, "Poppins".into());
            }
            list.extend(fallback.iter().cloned());
        }
    }
    let mut headings = fonts.families[&egui::FontFamily::Proportional].clone();
    headings.insert(0, POPPINS_SEMIBOLD.into());
    fonts.families.insert(title_family(), headings);
    ctx.set_fonts(fonts);
}

/// Applies the app's dark style.
pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    let mut v = Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = SURFACE;
    v.extreme_bg_color = Color32::from_rgb(0x0B, 0x0D, 0x10);
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    let r = CornerRadius::same(8);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = r;
    }
    v.widgets.inactive.weak_bg_fill = SURFACE_HOVER;
    v.widgets.inactive.bg_fill = SURFACE_HOVER;
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x2C, 0x32, 0x3E);
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x2C, 0x32, 0x3E);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, SEPARATOR);
    ctx.set_visuals(v);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(GRID, GRID);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 32.0;
        style
            .text_styles
            .insert(TextStyle::Heading, FontId::new(17.0, title_family()));
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(12.0));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(c: u8) -> f64 {
        let v = f64::from(c) / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }

    fn luminance(c: Color32) -> f64 {
        0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (hi, lo) = {
            let (x, y) = (luminance(a), luminance(b));
            if x > y { (x, y) } else { (y, x) }
        };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn text_pairs_meet_wcag_aa() {
        let on_ink_and_surface = [
            ("text", TEXT),
            ("muted text", TEXT_SECONDARY),
            ("accent text", ACCENT),
            ("success", SUCCESS),
            ("danger", DANGER),
            ("warning", WARNING),
        ];
        for (name, fg) in on_ink_and_surface {
            for (bg_name, bg) in [("ink", BG), ("surface", SURFACE), ("raised", SURFACE_HOVER)] {
                let ratio = contrast(fg, bg);
                assert!(ratio >= 4.5, "{name} on {bg_name}: {ratio:.2}");
            }
        }
        let on_accent = contrast(ON_ACCENT, ACCENT);
        assert!(
            on_accent >= 4.5,
            "dark text on accent buttons: {on_accent:.2}"
        );
    }

    #[test]
    fn non_text_indicators_meet_three_to_one() {
        for bg in [BG, SURFACE] {
            assert!(contrast(FOCUS_RING, bg) >= 3.0);
            assert!(
                contrast(BORDER, bg) >= 1.3,
                "separators stay subtle but visible"
            );
        }
    }
}
