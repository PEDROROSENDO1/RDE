use egui::{Color32, CornerRadius, Frame, Margin, RichText, Stroke, Ui};
use rocketde_core::design::ThemeConfig;

pub fn accent_color(t: &ThemeConfig) -> Color32 {
    Color32::from_rgb(t.accent_rgb[0], t.accent_rgb[1], t.accent_rgb[2])
}

pub fn text_color(t: &ThemeConfig) -> Color32 {
    Color32::from_rgb(t.text_rgb[0], t.text_rgb[1], t.text_rgb[2])
}

pub fn text_muted_color(t: &ThemeConfig) -> Color32 {
    Color32::from_rgb(t.text_muted_rgb[0], t.text_muted_rgb[1], t.text_muted_rgb[2])
}

pub fn panel_bg(t: &ThemeConfig) -> Color32 {
    Color32::from_rgba_unmultiplied(
        t.panel_bg_rgba[0],
        t.panel_bg_rgba[1],
        t.panel_bg_rgba[2],
        t.panel_bg_rgba[3],
    )
}

pub fn card_bg(t: &ThemeConfig) -> Color32 {
    Color32::from_rgba_unmultiplied(
        t.surface.card_bg_rgba[0],
        t.surface.card_bg_rgba[1],
        t.surface.card_bg_rgba[2],
        t.surface.card_bg_rgba[3],
    )
}

pub fn elevated_bg(t: &ThemeConfig) -> Color32 {
    Color32::from_rgba_unmultiplied(
        t.surface.elevated_bg_rgba[0],
        t.surface.elevated_bg_rgba[1],
        t.surface.elevated_bg_rgba[2],
        t.surface.elevated_bg_rgba[3],
    )
}

pub fn bar_bg(t: &ThemeConfig) -> Color32 {
    Color32::from_rgba_unmultiplied(
        t.bar_bg_rgba[0],
        t.bar_bg_rgba[1],
        t.bar_bg_rgba[2],
        t.bar_bg_rgba[3],
    )
}

pub fn dock_bg(t: &ThemeConfig) -> Color32 {
    Color32::from_rgba_unmultiplied(
        t.dock_bg_rgba[0],
        t.dock_bg_rgba[1],
        t.dock_bg_rgba[2],
        t.dock_bg_rgba[3],
    )
}

pub fn danger_color(t: &ThemeConfig) -> Color32 {
    Color32::from_rgb(
        t.surface.danger_rgb[0],
        t.surface.danger_rgb[1],
        t.surface.danger_rgb[2],
    )
}

pub fn success_color(t: &ThemeConfig) -> Color32 {
    Color32::from_rgb(
        t.surface.success_rgb[0],
        t.surface.success_rgb[1],
        t.surface.success_rgb[2],
    )
}

pub fn warning_color(t: &ThemeConfig) -> Color32 {
    Color32::from_rgb(
        t.surface.warning_rgb[0],
        t.surface.warning_rgb[1],
        t.surface.warning_rgb[2],
    )
}

pub fn border_stroke(t: &ThemeConfig) -> Stroke {
    Stroke::new(
        1.0,
        Color32::from_rgba_unmultiplied(
            t.border_rgba[0],
            t.border_rgba[1],
            t.border_rgba[2],
            t.border_rgba[3],
        ),
    )
}

pub fn card_frame(t: &ThemeConfig) -> Frame {
    Frame::NONE
        .fill(card_bg(t))
        .stroke(border_stroke(t))
        .corner_radius(CornerRadius::same(t.radii.card.round().clamp(0.0, 255.0) as u8))
        .inner_margin(Margin::same(t.spacing.md.round().clamp(0.0, 127.0) as i8))
}

pub fn section_heading(ui: &mut Ui, t: &ThemeConfig, title: &str) {
    ui.label(
        RichText::new(title)
            .size(t.typography.heading)
            .strong()
            .color(text_color(t)),
    );
}
