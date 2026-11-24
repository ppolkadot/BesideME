//! iOS-dark inspired palette and egui style setup.

use eframe::egui::{self, Color32, CornerRadius, FontId, Stroke, TextStyle, Visuals};

pub const BG: Color32 = Color32::from_rgb(0x1C, 0x1C, 0x1E);
pub const SURFACE: Color32 = Color32::from_rgb(0x2C, 0x2C, 0x2E);
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x3A, 0x3A, 0x3C);
pub const SEPARATOR: Color32 = Color32::from_rgb(0x38, 0x38, 0x3A);
pub const BORDER: Color32 = Color32::from_rgb(0x48, 0x48, 0x4A);
pub const TEXT: Color32 = Color32::from_rgb(0xF2, 0xF2, 0xF7);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0x8E, 0x8E, 0x93);
pub const ACCENT: Color32 = Color32::from_rgb(0x0A, 0x84, 0xFF);
pub const SUCCESS: Color32 = Color32::from_rgb(0x30, 0xD1, 0x58);
pub const DANGER: Color32 = Color32::from_rgb(0xFF, 0x45, 0x3A);
pub const WARNING: Color32 = Color32::from_rgb(0xFF, 0xD6, 0x0A);
pub const LIGHT_INACTIVE: Color32 = Color32::from_rgb(0x4A, 0x4A, 0x4D);
pub const LIGHT_INACTIVE_RIM: Color32 = Color32::from_rgb(0x3E, 0x3E, 0x41);

const POPPINS_SEMIBOLD: &str = "Poppins SemiBold";

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
    let mut fallback = Vec::new();
    for (name, file) in [
        ("segoe-ui", "segoeui.ttf"),
        ("segoe-symbol", "seguisym.ttf"),
        ("segoe-emoji", "seguiemj.ttf"),
    ] {
        let path = std::path::Path::new(&windir).join("Fonts").join(file);
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(
                name.to_owned(),
                std::sync::Arc::new(egui::FontData::from_owned(bytes)),
            );
            fallback.push(name.to_owned());
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
    fonts
        .families
        .insert(egui::FontFamily::Name(POPPINS_SEMIBOLD.into()), headings);
    ctx.set_fonts(fonts);
}

/// Applies the app's dark style.
pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    let mut v = Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = SURFACE;
    v.extreme_bg_color = Color32::from_rgb(0x15, 0x15, 0x17);
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = ACCENT;
    v.selection.bg_fill = ACCENT.gamma_multiply(0.45);
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
    v.widgets.inactive.weak_bg_fill = SURFACE;
    v.widgets.inactive.bg_fill = SURFACE;
    v.widgets.hovered.weak_bg_fill = SURFACE_HOVER;
    v.widgets.hovered.bg_fill = SURFACE_HOVER;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, SEPARATOR);
    ctx.set_visuals(v);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.text_styles.insert(
            TextStyle::Heading,
            FontId::new(17.0, egui::FontFamily::Name(POPPINS_SEMIBOLD.into())),
        );
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
