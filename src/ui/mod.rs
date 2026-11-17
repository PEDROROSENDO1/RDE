pub mod capture_overlay;
pub mod control_center;
pub mod dock;
pub mod icons;
pub mod launcher;
pub mod overview;
pub mod top_bar;

use crate::config::RocketConfig;
use crate::system::apps::{AppIndexer, DesktopApp};
use crate::system::audio_brightness::HardwareController;
use crate::system::media::MediaController;
use crate::system::stats::SystemMonitor;
use crate::wayland::virtual_kbd::ShortcutChord;
use crate::wm::pila::PilaConnector;
use chrono::{Datelike, Local};
use egui::{
    Color32, Context, FontData, FontDefinitions, FontFamily, FontId, Style, TextStyle, Visuals,
};
use std::sync::{Arc, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsdKind {
    Volume,
    Brightness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveGlobalMenu {
    None,
    AppleMenu,
    FileMenu,
    EditMenu,
    ViewMenu,
    WindowMenu,
    HelpMenu,
    CalendarPopup,
    VolumePopup,
    BrightnessPopup,
    BatteryPopup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    Screenshot,
    Record,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureTarget {
    Selection,
    Screen,
    Window,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureDragHandle {
    None,
    Move,
    NewRect,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Top,
    Bottom,
    Left,
    Right,
}

#[derive(Debug, Clone)]
pub enum PendingCaptureAction {
    OpenOverlay(CaptureMode),
    ExecuteScreenshot {
        target: CaptureTarget,
        logical_rect: egui::Rect,
        screen_size: egui::Vec2,
        include_cursor: bool,
        copy_clipboard: bool,
        save_disk: bool,
        window_id: Option<String>,
    },
    StartRecording {
        target: CaptureTarget,
        logical_rect: egui::Rect,
        screen_size: egui::Vec2,
        include_cursor: bool,
        fps: u32,
        system_audio: bool,
        mic_audio: bool,
    },
    StopRecording,
}

pub struct OverviewThumb {
    pub object_key: String,
    pub identifier: String,
    pub app_id: String,
    pub app_id_lower: String,
    pub title: String,
    pub title_lower: String,
    pub texture: egui::TextureHandle,
}

pub struct ShellUiState {
    pub config: RocketConfig,
    pub wm: PilaConnector,
    pub sys: SystemMonitor,
    pub hw: HardwareController,
    pub media: MediaController,
    pub apps: AppIndexer,

    pub launcher_open: bool,
    pub launcher_query: String,
    pub launcher_selected: usize,
    pub launcher_page: usize,
    pub launcher_focus_requested: bool,
    pub launcher_anim_t: f32,
    pub launcher_icon_scales: Vec<f32>,
    pub launcher_context_menu: Option<(DesktopApp, egui::Pos2)>,
    pub launcher_drag_app: Option<DesktopApp>,
    pub launcher_kb_nav_active: bool,
    pub launcher_scroll_cooldown: f64,

    pub overview_open: bool,
    pub overview_closing: bool,
    pub overview_anim_t: f32,
    pub overview_drag_window_id: Option<String>,
    pub overview_view_ws: Option<usize>,
    pub overview_last_pila_ws: usize,
    pub overview_ws_slide: f32,
    pub overview_thumbnails: Vec<OverviewThumb>,
    pub pending_move_window: Option<(String, usize)>,
    pub fullscreen_hidden: bool,

    pub control_center_open: bool,
    pub control_center_rect: Option<egui::Rect>,
    pub cc_wifi_expanded: bool,
    pub cc_bt_expanded: bool,
    pub cc_wifi_selected_ssid: Option<String>,
    pub cc_wifi_password: String,
    pub cc_wifi_focus_requested: bool,
    pub active_menu: ActiveGlobalMenu,
    pub active_menu_anchor_x: f32,
    pub active_menu_rect: Option<egui::Rect>,
    pub calendar_year: i32,
    pub calendar_month: u32,
    pub pending_shortcuts: Vec<ShortcutChord>,

    // Dock animation & context menu state
    pub dock_hover_pos: Option<egui::Pos2>,
    pub dock_current_scales: Vec<f32>,
    pub dock_context_menu: Option<(usize, egui::Pos2)>,
    pub dock_launching_apps: Vec<(String, String, std::time::Instant)>,
    pub dock_obstructed: bool,
    pub dock_edge_hovered: bool,
    pub dock_reveal_t: f32,

    // GNOME-style Screenshot & Screen Recording state
    pub capture_overlay_open: bool,
    pub capture_mode: CaptureMode,
    pub capture_target: CaptureTarget,
    pub capture_selection_rect: Option<egui::Rect>,
    pub capture_drag_handle: CaptureDragHandle,
    pub capture_drag_start: Option<egui::Pos2>,
    pub capture_drag_orig_rect: Option<egui::Rect>,
    pub capture_include_cursor: bool,
    pub capture_copy_clipboard: bool,
    pub capture_save_disk: bool,
    pub capture_timer_secs: u8,
    pub capture_countdown_until: Option<std::time::Instant>,
    pub capture_fps: u32,
    pub capture_system_audio: bool,
    pub capture_mic_audio: bool,
    pub capture_selected_window_id: Option<String>,
    pub capture_bg_texture: Option<egui::TextureHandle>,
    pub pending_capture_action: Option<PendingCaptureAction>,
    pub recording_active: bool,
    pub recording_started_at: Option<std::time::Instant>,
    pub capture_toast: Option<(String, std::time::Instant)>,
    pub osd: Option<(OsdKind, f32, bool, std::time::Instant)>,
}

impl ShellUiState {
    pub fn new(
        config: RocketConfig,
        wm: PilaConnector,
        sys: SystemMonitor,
        hw: HardwareController,
        media: MediaController,
        apps: AppIndexer,
    ) -> Self {
        let count = config.dock.pinned_apps.len() + 8;
        let now = Local::now();
        let default_fps = config.capture.default_fps.clamp(15, 60);
        let include_cursor = config.capture.include_cursor;
        let copy_clipboard = config.capture.copy_to_clipboard;
        let save_disk = config.capture.save_to_disk;
        Self {
            config,
            wm,
            sys,
            hw,
            media,
            apps,
            launcher_open: false,
            launcher_query: String::new(),
            launcher_selected: 0,
            launcher_page: 0,
            launcher_focus_requested: false,
            launcher_anim_t: 0.0,
            launcher_icon_scales: Vec::new(),
            launcher_context_menu: None,
            launcher_drag_app: None,
            launcher_kb_nav_active: false,
            launcher_scroll_cooldown: 0.0,
            overview_open: false,
            overview_closing: false,
            overview_anim_t: 0.0,
            overview_drag_window_id: None,
            overview_view_ws: None,
            overview_last_pila_ws: 1,
            overview_ws_slide: 0.0,
            overview_thumbnails: Vec::new(),
            pending_move_window: None,
            fullscreen_hidden: false,
            control_center_open: false,
            control_center_rect: None,
            cc_wifi_expanded: false,
            cc_bt_expanded: false,
            cc_wifi_selected_ssid: None,
            cc_wifi_password: String::new(),
            cc_wifi_focus_requested: false,
            active_menu: ActiveGlobalMenu::None,
            active_menu_anchor_x: 10.0,
            active_menu_rect: None,
            calendar_year: now.year(),
            calendar_month: now.month(),
            pending_shortcuts: Vec::new(),
            dock_hover_pos: None,
            dock_current_scales: vec![1.0; count],
            dock_context_menu: None,
            dock_launching_apps: Vec::new(),
            dock_obstructed: false,
            dock_edge_hovered: false,
            dock_reveal_t: 1.0,
            capture_overlay_open: false,
            capture_mode: CaptureMode::Screenshot,
            capture_target: CaptureTarget::Selection,
            capture_selection_rect: None,
            capture_drag_handle: CaptureDragHandle::None,
            capture_drag_start: None,
            capture_drag_orig_rect: None,
            capture_include_cursor: include_cursor,
            capture_copy_clipboard: copy_clipboard,
            capture_save_disk: save_disk,
            capture_timer_secs: 0,
            capture_countdown_until: None,
            capture_fps: default_fps,
            capture_system_audio: true,
            capture_mic_audio: false,
            capture_selected_window_id: None,
            capture_bg_texture: None,
            pending_capture_action: None,
            recording_active: false,
            recording_started_at: None,
            capture_toast: None,
            osd: None,
        }
    }

    pub fn toggle_launcher(&mut self) {
        self.launcher_open = !self.launcher_open;
        self.launcher_context_menu = None;
        self.launcher_drag_app = None;
        self.dock_context_menu = None;
        if self.launcher_open {
            self.overview_open = false;
            self.overview_drag_window_id = None;
            self.launcher_query.clear();
            self.launcher_selected = 0;
            self.launcher_page = 0;
            self.launcher_anim_t = 0.0;
            self.launcher_focus_requested = true;
            self.launcher_kb_nav_active = false;
            self.launcher_scroll_cooldown = 0.0;
            self.control_center_open = false;
            self.cc_wifi_selected_ssid = None;
            self.cc_wifi_password.clear();
            self.active_menu = ActiveGlobalMenu::None;
            self.active_menu_rect = None;
        }
    }

    pub fn open_launcher(&mut self) {
        if !self.launcher_open {
            self.toggle_launcher();
        }
    }

    pub fn toggle_overview(&mut self) {
        if self.overview_open && !self.overview_closing {
            self.overview_closing = true;
            self.overview_drag_window_id = None;
            return;
        }
        self.overview_open = true;
        self.overview_closing = false;
        self.overview_drag_window_id = None;
        self.overview_anim_t = 0.0;
        self.overview_ws_slide = 0.0;
        let cur_ws = self
            .wm
            .state
            .lock()
            .ok()
            .and_then(|g| g.workspaces.iter().find(|w| w.active).map(|w| w.index))
            .unwrap_or(1);
        self.overview_view_ws = Some(cur_ws);
        self.overview_last_pila_ws = cur_ws;
        self.launcher_open = false;
        self.launcher_context_menu = None;
        self.launcher_drag_app = None;
        self.control_center_open = false;
        self.cc_wifi_selected_ssid = None;
        self.cc_wifi_password.clear();
        self.active_menu = ActiveGlobalMenu::None;
        self.active_menu_rect = None;
        self.dock_context_menu = None;
    }

    pub fn open_overview(&mut self) {
        if !self.overview_open || self.overview_closing {
            self.overview_open = false;
            self.toggle_overview();
        }
    }

    pub fn mark_app_launching(&mut self, app_id: &str, name: &str) {
        let id_l = app_id.trim().to_lowercase();
        let name_l = name.trim().to_lowercase();
        self.dock_launching_apps
            .retain(|(a, n, _)| !(a == &id_l && n == &name_l));
        self.dock_launching_apps
            .push((id_l, name_l, std::time::Instant::now()));
    }

    pub fn close_overlays(&mut self) {
        self.launcher_open = false;
        self.launcher_context_menu = None;
        self.launcher_drag_app = None;
        self.overview_open = false;
        self.overview_closing = false;
        self.overview_drag_window_id = None;
        self.overview_thumbnails.clear();
        self.control_center_open = false;
        self.cc_wifi_selected_ssid = None;
        self.cc_wifi_password.clear();
        self.active_menu = ActiveGlobalMenu::None;
        self.active_menu_rect = None;
        self.dock_context_menu = None;
        self.capture_overlay_open = false;
        self.capture_countdown_until = None;
        self.capture_bg_texture = None;
    }

    pub fn any_overlay_open(&self) -> bool {
        self.launcher_open
            || self.overview_open
            || self.launcher_context_menu.is_some()
            || self.control_center_open
            || self.active_menu != ActiveGlobalMenu::None
            || self.dock_context_menu.is_some()
            || self.capture_overlay_open
    }

    pub fn send_app_shortcut(&mut self, chord: ShortcutChord) {
        if let Ok(guard) = self.wm.state.lock() {
            if let Some(ref w) = guard.focused_window {
                if !w.id.is_empty() {
                    PilaConnector::focus_window(&w.id);
                }
            }
        }
        self.pending_shortcuts.push(chord);
        self.active_menu = ActiveGlobalMenu::None;
    }
}

fn shared_system_fonts() -> &'static FontDefinitions {
    static FONTS: OnceLock<FontDefinitions> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut fonts = FontDefinitions::default();

        let prop_candidates = [
            ("adwaita_sans", "/usr/share/fonts/Adwaita/AdwaitaSans-Regular.ttf"),
            ("dejavu_sans", "/usr/share/fonts/TTF/DejaVuSans.ttf"),
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
    ctx.set_fonts(shared_system_fonts().clone());
}

pub fn apply_macos_theme(ctx: &Context, cfg: &RocketConfig) {
    install_system_fonts(ctx);
    let mut visuals = Visuals::dark();
    let t = &cfg.theme;

    visuals.window_fill = Color32::from_rgba_unmultiplied(
        t.panel_bg_rgba[0],
        t.panel_bg_rgba[1],
        t.panel_bg_rgba[2],
        t.panel_bg_rgba[3],
    );
    visuals.panel_fill = Color32::TRANSPARENT;
    visuals.override_text_color =
        Some(Color32::from_rgb(t.text_rgb[0], t.text_rgb[1], t.text_rgb[2]));
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
    visuals.widgets.inactive.bg_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 18);
    visuals.widgets.hovered.bg_fill = Color32::from_rgba_unmultiplied(255, 255, 255, 38);
    visuals.widgets.active.bg_fill =
        Color32::from_rgb(t.accent_rgb[0], t.accent_rgb[1], t.accent_rgb[2]);

    let mut style = Style {
        visuals,
        ..Default::default()
    };

    style.text_styles.insert(
        TextStyle::Body,
        FontId::new(13.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(13.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(18.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(11.0, FontFamily::Proportional),
    );
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);

    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
}

pub fn render_osd(ctx: &Context, state: &mut ShellUiState) {
    let Some((kind, value, muted, created_at)) = state.osd else {
        return;
    };
    let elapsed = created_at.elapsed().as_secs_f32();
    let total_dur = 1.35;
    if elapsed >= total_dur {
        state.osd = None;
        return;
    }

    let alpha = if elapsed > 1.05 {
        ctx.request_repaint();
        ((total_dur - elapsed) / 0.30).clamp(0.0, 1.0)
    } else {
        ctx.request_repaint_after(std::time::Duration::from_secs_f32((1.05 - elapsed).max(0.01)));
        1.0
    };

    let t = &state.config.theme;
    let accent = Color32::from_rgb(t.accent_rgb[0], t.accent_rgb[1], t.accent_rgb[2]);
    let icon = match kind {
        OsdKind::Volume => {
            if muted || value <= 0.5 {
                icons::IconKind::VolumeMute
            } else if value < 45.0 {
                icons::IconKind::VolumeLow
            } else {
                icons::IconKind::VolumeHigh
            }
        }
        OsdKind::Brightness => icons::IconKind::Sun,
    };

    let screen = ctx.content_rect();
    let pill_w = 220.0;
    let pill_h = 42.0;
    let y_offset = if state.config.dock.enabled && !state.overview_open && !state.launcher_open {
        screen.max.y - state.config.dock.icon_size - 48.0 - pill_h
    } else {
        screen.max.y - 72.0 - pill_h
    };
    let pill_rect = egui::Rect::from_min_size(
        egui::pos2(screen.center().x - pill_w * 0.5, y_offset),
        egui::vec2(pill_w, pill_h),
    );

    egui::Area::new(egui::Id::unique("rocketde_osd_pill"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pill_rect.min)
        .interactable(false)
        .show(ctx, |ui| {
            let bg = Color32::from_rgba_unmultiplied(
                t.panel_bg_rgba[0],
                t.panel_bg_rgba[1],
                t.panel_bg_rgba[2],
                (238.0 * alpha) as u8,
            );
            let border = Color32::from_rgba_unmultiplied(255, 255, 255, (42.0 * alpha) as u8);
            ui.painter().rect(
                pill_rect,
                egui::CornerRadius::same(14),
                bg,
                egui::Stroke::new(1.0, border),
                egui::StrokeKind::Inside,
            );

            let icon_center = egui::pos2(pill_rect.min.x + 22.0, pill_rect.center().y);
            icons::draw_icon(
                ui.painter(),
                icon_center,
                16.0,
                icon,
                Color32::from_rgba_unmultiplied(255, 255, 255, (245.0 * alpha) as u8),
            );

            let bar_x = pill_rect.min.x + 42.0;
            let bar_w = pill_w - 88.0;
            let bar_rect = egui::Rect::from_min_size(
                egui::pos2(bar_x, pill_rect.center().y - 3.0),
                egui::vec2(bar_w, 6.0),
            );
            ui.painter().rect_filled(
                bar_rect,
                egui::CornerRadius::same(3),
                Color32::from_rgba_unmultiplied(255, 255, 255, (32.0 * alpha) as u8),
            );
            let is_vol = matches!(kind, OsdKind::Volume);
            let max_val = if is_vol { 150.0 } else { 100.0 };
            let frac = if muted {
                0.0
            } else {
                (value / max_val).clamp(0.0, 1.0)
            };
            if frac > 0.0 {
                let fill_col = if is_vol && value > 100.0 {
                    Color32::from_rgba_unmultiplied(255, 159, 10, (245.0 * alpha) as u8)
                } else {
                    Color32::from_rgba_unmultiplied(
                        accent.r(),
                        accent.g(),
                        accent.b(),
                        (245.0 * alpha) as u8,
                    )
                };
                let fill_rect = egui::Rect::from_min_size(
                    bar_rect.min,
                    egui::vec2((bar_w * frac).max(6.0), 6.0),
                );
                ui.painter()
                    .rect_filled(fill_rect, egui::CornerRadius::same(3), fill_col);
            }
            if is_vol {
                let mark_x = bar_rect.min.x + bar_w * (100.0 / 150.0);
                ui.painter().line_segment(
                    [
                        egui::pos2(mark_x, bar_rect.min.y - 1.5),
                        egui::pos2(mark_x, bar_rect.max.y + 1.5),
                    ],
                    egui::Stroke::new(
                        1.0,
                        Color32::from_rgba_unmultiplied(255, 255, 255, (110.0 * alpha) as u8),
                    ),
                );
            }

            let text = if muted {
                "Mudo".to_string()
            } else {
                format!("{}%", value.round() as u32)
            };
            ui.painter().text(
                egui::pos2(pill_rect.max.x - 14.0, pill_rect.center().y),
                egui::Align2::RIGHT_CENTER,
                text,
                egui::FontId::proportional(11.5),
                Color32::from_rgba_unmultiplied(230, 236, 246, (235.0 * alpha) as u8),
            );
        });
}

pub fn parse_hex_color(hex: &str, alpha: u8) -> Color32 {
    let h = hex.trim().trim_start_matches('#');
    if h.len() >= 6 {
        let r = u8::from_str_radix(&h[0..2], 16).unwrap_or(50);
        let g = u8::from_str_radix(&h[2..4], 16).unwrap_or(130);
        let b = u8::from_str_radix(&h[4..6], 16).unwrap_or(240);
        Color32::from_rgba_unmultiplied(r, g, b, alpha)
    } else {
        Color32::from_rgba_unmultiplied(10, 132, 255, alpha)
    }
}
