use egui::{Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Style, TextStyle, Visuals};
use rocketde_core::config::RocketConfig;
use rocketde_core::design::ThemeConfig;
use std::sync::{Arc, OnceLock};

fn shared_system_fonts() -> &'static FontDefinitions {
    static FONTS: OnceLock<FontDefinitions> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut fonts = FontDefinitions::default();

        let prop_candidates = [
            ("adwaita_sans", "/usr/share/fonts/Adwaita/AdwaitaSans-Regular.ttf"),
            ("dejavu_sans", "/usr/share/fonts/TTF/DejaVuSans.ttf"),
            ("dejavu_sans_deb", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"),
            ("dejavu_sans_fed", "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf"),
            ("liberation_sans", "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf"),
        ];
        for (name, path) in prop_candidates {
            if let Ok(bytes) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert(name.to_owned(), Arc::new(FontData::from_owned(bytes)));
                if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
                    list.insert(0, name.to_owned());
                }
                break;
            }
        }

        let mono_candidates = [
            ("adwaita_mono", "/usr/share/fonts/Adwaita/AdwaitaMono-Regular.ttf"),
            ("dejavu_mono", "/usr/share/fonts/TTF/DejaVuSansMono.ttf"),
            ("dejavu_mono_deb", "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"),
            ("dejavu_mono_fed", "/usr/share/fonts/dejavu-sans-mono-fonts/DejaVuSansMono.ttf"),
            ("liberation_mono", "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf"),
        ];
        for (name, path) in mono_candidates {
            if let Ok(bytes) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert(name.to_owned(), Arc::new(FontData::from_owned(bytes)));
                if let Some(list) = fonts.families.get_mut(&FontFamily::Monospace) {
                    list.insert(0, name.to_owned());
                }
                break;
            }
        }

        fonts
    })
}

fn install_system_fonts(ctx: &Context) {
    let key = egui::Id::unique("rocketde_fonts_installed");
    let already_installed = ctx.data(|d| d.get_temp::<bool>(key)).unwrap_or(false);
    if !already_installed {
        ctx.set_fonts(shared_system_fonts().clone());
        ctx.data_mut(|d| d.insert_temp(key, true));
    }
}

/// Applies the centralized `.ron` design tokens to an `egui::Context`.
pub fn apply_design(ctx: &Context, t: &ThemeConfig) {
    install_system_fonts(ctx);
    let mut visuals = Visuals::dark();

    visuals.window_fill = Color32::from_rgba_unmultiplied(
        t.panel_bg_rgba[0],
        t.panel_bg_rgba[1],
        t.panel_bg_rgba[2],
        t.panel_bg_rgba[3],
    );
    visuals.window_corner_radius = CornerRadius::same(t.radii.window.round().clamp(0.0, 255.0) as u8);
    visuals.panel_fill = Color32::TRANSPARENT;
    visuals.override_text_color =
        Some(Color32::from_rgb(t.text_rgb[0], t.text_rgb[1], t.text_rgb[2]));
    visuals.weak_text_color = Some(Color32::from_rgb(
        t.text_muted_rgb[0],
        t.text_muted_rgb[1],
        t.text_muted_rgb[2],
    ));
    visuals.error_fg_color = Color32::from_rgb(
        t.surface.danger_rgb[0],
        t.surface.danger_rgb[1],
        t.surface.danger_rgb[2],
    );
    visuals.warn_fg_color = Color32::from_rgb(
        t.surface.warning_rgb[0],
        t.surface.warning_rgb[1],
        t.surface.warning_rgb[2],
    );
    visuals.selection.bg_fill =
        Color32::from_rgb(t.accent_rgb[0], t.accent_rgb[1], t.accent_rgb[2]);
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(
        1.0,
        Color32::from_rgba_unmultiplied(
            t.border_rgba[0],
            t.border_rgba[1],
            t.border_rgba[2],
            t.border_rgba[3],
        ),
    );
    visuals.widgets.inactive.bg_fill = Color32::from_rgba_unmultiplied(
        t.surface.inactive_rgba[0],
        t.surface.inactive_rgba[1],
        t.surface.inactive_rgba[2],
        t.surface.inactive_rgba[3],
    );
    let btn_r = t.radii.button.round().clamp(0.0, 255.0) as u8;
    visuals.widgets.inactive.corner_radius = CornerRadius::same(btn_r);
    visuals.widgets.hovered.bg_fill = Color32::from_rgba_unmultiplied(
        t.surface.hover_rgba[0],
        t.surface.hover_rgba[1],
        t.surface.hover_rgba[2],
        t.surface.hover_rgba[3],
    );
    visuals.widgets.hovered.corner_radius = CornerRadius::same(btn_r);
    visuals.widgets.active.bg_fill =
        Color32::from_rgb(t.accent_rgb[0], t.accent_rgb[1], t.accent_rgb[2]);
    visuals.widgets.active.corner_radius = CornerRadius::same(btn_r);

    let mut style = Style {
        visuals,
        ..Default::default()
    };

    style.text_styles.insert(
        TextStyle::Body,
        FontId::new(t.typography.body, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(t.typography.button, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(t.typography.heading, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(t.typography.small, FontFamily::Proportional),
    );
    style.spacing.item_spacing = egui::vec2(t.spacing.item_x, t.spacing.item_y);
    style.spacing.button_padding = egui::vec2(t.spacing.button_x, t.spacing.button_y);

    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
}

pub fn apply_macos_theme(ctx: &Context, cfg: &RocketConfig) {
    apply_design(ctx, &cfg.theme);
}
