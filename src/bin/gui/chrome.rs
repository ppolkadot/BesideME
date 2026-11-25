//! Borderless window chrome: a draggable title bar with macOS/iOS-style "traffic light"
//! buttons (close / minimize / zoom). The native Windows caption buttons do not exist
//! (the window is created without decorations), and every OS-level close request is
//! cancelled unless it came from the red button (see `WatcherApp::guard_close`).

use eframe::egui::{
    self, Color32, CornerRadius, Pos2, Rect, Sense, Shape, Stroke, Ui, ViewportCommand, pos2, vec2,
};

use crate::theme;

/// Height of the custom title bar.
pub const TITLE_BAR_HEIGHT: f32 = 40.0;

const LIGHT_RADIUS: f32 = 6.5;
const LIGHT_SPACING: f32 = 21.0;
const FIRST_LIGHT_X: f32 = 20.0;

/// What the user clicked in the title bar this frame.
#[derive(Debug, Default, Clone, Copy)]
pub struct TitleBarResponse {
    /// Red button.
    pub close: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Light {
    Close,
    Minimize,
    Zoom,
}

impl Light {
    const ALL: [Self; 3] = [Self::Close, Self::Minimize, Self::Zoom];

    fn color(self) -> Color32 {
        match self {
            Self::Close => Color32::from_rgb(0xFF, 0x5F, 0x57),
            Self::Minimize => Color32::from_rgb(0xFE, 0xBC, 0x2E),
            Self::Zoom => Color32::from_rgb(0x28, 0xC8, 0x40),
        }
    }

    fn rim(self) -> Color32 {
        match self {
            Self::Close => Color32::from_rgb(0xE0, 0x44, 0x3E),
            Self::Minimize => Color32::from_rgb(0xDE, 0xA1, 0x23),
            Self::Zoom => Color32::from_rgb(0x1A, 0xAB, 0x29),
        }
    }

    fn glyph_color(self) -> Color32 {
        match self {
            Self::Close => Color32::from_rgb(0x4D, 0x00, 0x00),
            Self::Minimize => Color32::from_rgb(0x99, 0x58, 0x00),
            Self::Zoom => Color32::from_rgb(0x00, 0x65, 0x00),
        }
    }

    /// Accessible name announced by screen readers (also shown as the tooltip).
    fn name(self) -> &'static str {
        match self {
            Self::Close => "Close window",
            Self::Minimize => "Minimize window",
            Self::Zoom => "Zoom window",
        }
    }
}

/// Draws the title bar and handles dragging, double-click zoom and the traffic lights.
pub fn title_bar(ui: &mut Ui, title: &str, maximized: bool) -> TitleBarResponse {
    let mut out = TitleBarResponse::default();
    let (bar_rect, bar) = ui.allocate_exact_size(
        vec2(ui.available_width(), TITLE_BAR_HEIGHT),
        // Draggable and double-clickable, but not a keyboard stop: the traffic lights are.
        Sense::CLICK | Sense::DRAG,
    );
    let painter = ui.painter_at(bar_rect);

    painter.text(
        bar_rect.center(),
        egui::Align2::CENTER_CENTER,
        title,
        egui::FontId::new(13.5, theme::title_family()),
        theme::TEXT_SECONDARY,
    );
    painter.line_segment(
        [
            pos2(bar_rect.left(), bar_rect.bottom() - 0.5),
            pos2(bar_rect.right(), bar_rect.bottom() - 0.5),
        ],
        Stroke::new(1.0, theme::SEPARATOR),
    );

    let centers: Vec<Pos2> = (0..3)
        .map(|i| {
            #[allow(clippy::cast_precision_loss)]
            let x = bar_rect.left() + FIRST_LIGHT_X + LIGHT_SPACING * i as f32;
            pos2(x, bar_rect.center().y)
        })
        .collect();
    let group_rect = Rect::from_min_max(
        centers[0] - vec2(LIGHT_RADIUS + 4.0, LIGHT_RADIUS + 4.0),
        centers[2] + vec2(LIGHT_RADIUS + 4.0, LIGHT_RADIUS + 4.0),
    );
    let group_hovered = ui.rect_contains_pointer(group_rect);
    let focused = ui.ctx().input(|i| i.viewport().focused.unwrap_or(true));

    for (light, center) in Light::ALL.into_iter().zip(centers) {
        let hit = Rect::from_center_size(
            center,
            vec2(LIGHT_RADIUS * 2.0 + 3.0, LIGHT_RADIUS * 2.0 + 3.0),
        );
        // A stable id (not `Id::unique`) so keyboard focus survives from frame to frame.
        let resp = ui
            .interact(
                hit,
                egui::Id::unique(("traffic-light", light as u8)),
                Sense::click(),
            )
            .on_hover_text(light.name());
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), light.name())
        });

        let active = focused || group_hovered;
        let (mut fill, rim) = if active {
            (light.color(), light.rim())
        } else {
            (theme::LIGHT_INACTIVE, theme::LIGHT_INACTIVE_RIM)
        };
        if resp.is_pointer_button_down_on() {
            fill = fill.gamma_multiply(0.75);
        }
        painter.circle(center, LIGHT_RADIUS, fill, Stroke::new(0.8, rim));
        // Glyphs show on hover and whenever a light has keyboard focus.
        if group_hovered || resp.has_focus() {
            draw_glyph(&painter, light, center, maximized);
        }

        if resp.clicked() {
            match light {
                Light::Close => out.close = true,
                Light::Minimize => ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true)),
                Light::Zoom => ui
                    .ctx()
                    .send_viewport_cmd(ViewportCommand::Maximized(!maximized)),
            }
        }
    }

    if !group_hovered {
        if bar.double_clicked() {
            ui.ctx()
                .send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        } else if bar.drag_started_by(egui::PointerButton::Primary) {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
    }
    out
}

fn draw_glyph(painter: &egui::Painter, light: Light, c: Pos2, maximized: bool) {
    let col = light.glyph_color();
    let stroke = Stroke::new(1.3, col);
    let s = 2.6;
    match light {
        Light::Close => {
            painter.line_segment([c + vec2(-s, -s), c + vec2(s, s)], stroke);
            painter.line_segment([c + vec2(-s, s), c + vec2(s, -s)], stroke);
        }
        Light::Minimize => {
            painter.line_segment([c + vec2(-s - 0.6, 0.0), c + vec2(s + 0.6, 0.0)], stroke);
        }
        Light::Zoom => {
            // Two small triangles pointing out (zoom) or in (restore), like macOS.
            let (a, b) = if maximized {
                (
                    [
                        c + vec2(-0.6, -0.6),
                        c + vec2(-3.2, -0.6),
                        c + vec2(-0.6, -3.2),
                    ],
                    [c + vec2(0.6, 0.6), c + vec2(3.2, 0.6), c + vec2(0.6, 3.2)],
                )
            } else {
                (
                    [
                        c + vec2(-3.2, 3.2),
                        c + vec2(-3.2, -1.0),
                        c + vec2(1.0, 3.2),
                    ],
                    [
                        c + vec2(3.2, -3.2),
                        c + vec2(3.2, 1.0),
                        c + vec2(-1.0, -3.2),
                    ],
                )
            };
            painter.add(Shape::convex_polygon(a.to_vec(), col, Stroke::NONE));
            painter.add(Shape::convex_polygon(b.to_vec(), col, Stroke::NONE));
        }
    }
}

/// Corner radius of the window body (square when maximized).
#[must_use]
pub fn window_radius(maximized: bool) -> CornerRadius {
    if maximized {
        CornerRadius::ZERO
    } else {
        CornerRadius::same(12)
    }
}

/// Draws a clear ring around whichever control currently has keyboard focus.
///
/// egui's own focus styling is subtle on a dark canvas and custom-painted controls (the
/// traffic lights) have none, so one ring is painted above everything for all of them.
pub fn paint_focus_ring(ctx: &egui::Context) {
    let Some(id) = ctx.memory(egui::Memory::focused) else {
        return;
    };
    let Some(response) = ctx.read_response(id) else {
        return;
    };
    if !response.has_focus() {
        return;
    }
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::unique("focus-ring"),
    ));
    let rect = response.rect.expand(2.0);
    // Round rings for the round traffic lights, softly rounded ones for everything else.
    let radius = if rect.width() <= 24.0 { 12 } else { 10 };
    painter.rect_stroke(
        rect,
        CornerRadius::same(radius),
        Stroke::new(2.0, theme::FOCUS_RING),
        egui::StrokeKind::Outside,
    );
}
