//! Shared look: a dark, translucent card similar to GNOME's notification bubbles.

use egui::{Color32, CornerRadius, FontId, Margin, RichText, Stroke};

pub const ACCENT: Color32 = Color32::from_rgb(0x8a, 0xb4, 0xf8);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0xa8, 0xc7, 0xfa);
pub const TEXT: Color32 = Color32::from_rgb(0xe8, 0xe8, 0xea);
pub const BODY: Color32 = Color32::from_rgb(0xc6, 0xc7, 0xcc);
pub const META: Color32 = Color32::from_rgb(0x9a, 0x9c, 0xa3);
pub const ERROR: Color32 = Color32::from_rgb(0xf2, 0x8b, 0x82);

pub fn card_fill() -> Color32 {
    Color32::from_rgba_unmultiplied(32, 33, 38, 238)
}

/// The rounded card every floating Fane window is drawn in.
pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(card_fill())
        .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 30)))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin { left: 16, right: 12, top: 10, bottom: 12 })
}

/// Dark visuals tuned for the cards (the settings window uses the system theme instead).
pub fn apply_card_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(TEXT);
    v.panel_fill = Color32::TRANSPARENT;
    v.window_fill = card_fill();
    v.extreme_bg_color = Color32::from_rgba_unmultiplied(255, 255, 255, 10);
    v.selection.bg_fill = ACCENT.linear_multiply(0.6);
    let radius = CornerRadius::same(7);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
        w.corner_radius = radius;
        w.bg_stroke = Stroke::NONE;
    }
    v.widgets.inactive.weak_bg_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 18);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 34);
    v.widgets.active.weak_bg_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 46);
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.button_padding = egui::vec2(12.0, 5.0);
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
    });
}

pub fn brand() -> RichText {
    RichText::new("✦ Fane").color(ACCENT).strong()
}

pub fn meta(text: &str) -> RichText {
    RichText::new(text).color(META).font(FontId::proportional(11.5))
}

pub fn title(text: &str) -> RichText {
    RichText::new(text).color(TEXT).font(FontId::proportional(15.0)).strong()
}

pub fn body(text: &str) -> RichText {
    RichText::new(text).color(BODY).font(FontId::proportional(13.5))
}

pub fn status(text: &str) -> RichText {
    RichText::new(text).color(META).font(FontId::proportional(12.5))
}

pub fn button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(label).color(TEXT).font(FontId::proportional(12.5))))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn primary(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let hovered_id = ui.next_auto_id();
    let hovered = ui.ctx().read_response(hovered_id).is_some_and(|r| r.hovered());
    let fill = if hovered { ACCENT_HOVER } else { ACCENT };
    ui.add(egui::Button::new(RichText::new(label).color(Color32::from_rgb(0x11, 0x11, 0x11)).strong().font(FontId::proportional(12.5))).fill(fill))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn close_button(ui: &mut egui::Ui) -> egui::Response {
    ui.add(egui::Button::new(RichText::new("✕").color(META).font(FontId::proportional(14.0))).frame(false))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}
