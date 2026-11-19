use crate::ui::ShellUiState;
use crate::ui::icons::{IconKind, draw_icon};
use crate::wm::pila::{PilaConnector, WmState, WorkspaceInfo};
use egui::{
    Color32, ColorImage, Context, CornerRadius, FontId, Frame, Key, Margin, Pos2, Rect, RichText,
    Sense, Stroke, TextureFilter, TextureHandle, TextureOptions, TextureWrapMode, Vec2,
};
use std::sync::Arc;

const OVERVIEW_OPEN_DURATION: f32 = 0.26;
const OVERVIEW_CLOSE_DURATION: f32 = 0.18;

#[inline]
fn ease_out_cubic(t: f32) -> f32 {
    let inv = 1.0 - t;
    1.0 - inv * inv * inv
}

#[derive(Clone)]
struct CachedWin {
    id: String,
    app_id: String,
    app_id_lower: String,
    title_lower: String,
    short_title: String,
    initial: String,
    is_focused: bool,
}

#[derive(Clone)]
struct CachedWorkspace {
    info: WorkspaceInfo,
    label: String,
    windows: Vec<CachedWin>,
}

#[derive(Clone)]
struct WmSnapCache {
    version: u64,
    active_ws_idx: usize,
    max_ws: usize,
    workspaces: Vec<CachedWorkspace>,
}

fn get_cached_wm_snap(ctx: &Context, state: &ShellUiState) -> Arc<WmSnapCache> {
    let cache_id = egui::Id::unique("rocketde_ov_wm_cache");
    let current_ver = state.wm.state.lock().map(|g| g.version).unwrap_or(0);

    if let Some(cached) = ctx.data(|d| d.get_temp::<Arc<WmSnapCache>>(cache_id))
        && cached.version == current_ver
    {
        return cached;
    }

    let snap: WmState = state
        .wm
        .state
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();

    let active_ws_idx = snap
        .workspaces
        .iter()
        .find(|w| w.active)
        .map(|w| w.index)
        .unwrap_or(1);
    let max_ws = snap
        .workspaces
        .iter()
        .map(|w| w.index)
        .max()
        .unwrap_or(1);
    let focused_id = snap.focused_window.as_ref().map(|fw| fw.id.as_str());

    let workspaces = snap
        .workspaces
        .iter()
        .map(|ws| {
            let windows = snap
                .windows
                .iter()
                .filter(|w| w.workspace == ws.index)
                .map(|w| {
                    let short_title: String = if !w.title.is_empty() {
                        w.title.chars().take(30).collect()
                    } else {
                        w.app_id.chars().take(24).collect()
                    };
                    let initial = w
                        .app_id
                        .chars()
                        .next()
                        .unwrap_or('A')
                        .to_ascii_uppercase()
                        .to_string();
                    CachedWin {
                        id: w.id.clone(),
                        app_id: w.app_id.clone(),
                        app_id_lower: w.app_id.to_lowercase(),
                        title_lower: w.title.to_lowercase(),
                        short_title,
                        initial,
                        is_focused: focused_id == Some(w.id.as_str()),
                    }
                })
                .collect();

            CachedWorkspace {
                info: ws.clone(),
                label: format!("Área {}", ws.index),
                windows,
            }
        })
        .collect();

    let built = Arc::new(WmSnapCache {
        version: snap.version,
        active_ws_idx,
        max_ws,
        workspaces,
    });
    ctx.data_mut(|d| d.insert_temp(cache_id, built.clone()));
    built
}

fn find_window_thumbnail(
    state: &ShellUiState,
    win: &CachedWin,
) -> Option<(TextureHandle, Vec2)> {
    if state.overview_thumbnails.is_empty() {
        return None;
    }
    if !win.id.is_empty()
        && let Some(t) = state
            .overview_thumbnails
            .iter()
            .find(|th| th.identifier == win.id)
    {
        return Some((t.texture.clone(), t.texture.size_vec2()));
    }
    if !win.app_id_lower.is_empty() && !win.title_lower.is_empty()
        && let Some(t) = state
            .overview_thumbnails
            .iter()
            .find(|th| th.app_id_lower == win.app_id_lower && th.title_lower == win.title_lower)
    {
        return Some((t.texture.clone(), t.texture.size_vec2()));
    }
    if !win.app_id_lower.is_empty()
        && let Some(t) = state
            .overview_thumbnails
            .iter()
            .find(|th| th.app_id_lower == win.app_id_lower)
    {
        return Some((t.texture.clone(), t.texture.size_vec2()));
    }
    None
}

fn fit_rect_preserving_aspect(container: Rect, tex_size: Vec2) -> Rect {
    if tex_size.x <= 1.0 || tex_size.y <= 1.0 {
        return container;
    }
    let scale = (container.width() / tex_size.x)
        .min(container.height() / tex_size.y)
        .max(0.01);
    let w = tex_size.x * scale;
    let h = tex_size.y * scale;
    Rect::from_center_size(container.center(), Vec2::new(w, h))
}

#[inline]
fn grid_dims_for_count(w_count: usize) -> (usize, usize) {
    let cols = match w_count {
        0 | 1 => 1,
        2 => 2,
        3 | 4 => 2,
        5 | 6 => 3,
        _ => 4,
    };
    let rows = w_count.max(1).div_ceil(cols);
    (cols, rows)
}

/// Pre-baked 256x256 backdrop texture so the Overview background draws in 1 single GPU quad
/// instead of expensive fullscreen compositor blur.
fn get_or_create_overview_backdrop(ctx: &Context, accent: Color32) -> TextureHandle {
    let key = (accent.r(), accent.g(), accent.b());
    let id = egui::Id::unique("rocketde_overview_backdrop_tex");
    if let Some((cached_key, existing)) =
        ctx.data(|d| d.get_temp::<((u8, u8, u8), TextureHandle)>(id))
        && cached_key == key
    {
        return existing;
    }

    let size = 256usize;
    let mut pixels = vec![0u8; size * size * 4];
    let fsize = size as f32;

    for y in 0..size {
        let ny = y as f32 / fsize;
        let dy_top = ny - 0.18;
        let dy_bot = ny - 0.88;
        for x in 0..size {
            let nx = x as f32 / fsize;
            let dx_top = nx - 0.50;
            let dx_bot = nx - 0.50;

            let d_top = (dx_top * dx_top + dy_top * dy_top).sqrt();
            let d_bot = (dx_bot * dx_bot + dy_bot * dy_bot).sqrt();
            let g1 = (1.0 - (d_top / 0.65)).clamp(0.0, 1.0).powi(2);
            let g2 = (1.0 - (d_bot / 0.55)).clamp(0.0, 1.0).powi(2);

            let r = (9.0 + accent.r() as f32 * 0.06 * g1 + 42.0 * 0.08 * g2).clamp(0.0, 255.0) as u8;
            let g = (11.0 + accent.g() as f32 * 0.06 * g1 + 56.0 * 0.08 * g2).clamp(0.0, 255.0) as u8;
            let b = (18.0 + accent.b() as f32 * 0.06 * g1 + 92.0 * 0.08 * g2).clamp(0.0, 255.0) as u8;

            let off = (y * size + x) * 4;
            pixels[off] = r;
            pixels[off + 1] = g;
            pixels[off + 2] = b;
            pixels[off + 3] = 212;
        }
    }

    let img = ColorImage::from_rgba_unmultiplied([size, size], &pixels);
    let opts = TextureOptions {
        magnification: TextureFilter::Linear,
        minification: TextureFilter::Linear,
        wrap_mode: TextureWrapMode::ClampToEdge,
        mipmap_mode: None,
    };
    let handle = ctx.load_texture("rocketde_overview_backdrop_tex", img, opts);
    ctx.data_mut(|d| d.insert_temp(id, (key, handle.clone())));
    handle
}

/// Renders a lightweight, hardware-friendly GNOME-style Workspaces Overview:
/// - Top: 16:9 Workspace Switcher mini-desktops
/// - Center: 16:9 Workspace Canvas Carousel (active workspace centered, neighbors peeking left/right)
/// - Drag any window to any top workspace thumbnail or peeking neighbor workspace to move it cleanly.
pub fn render_overview(ctx: &Context, state: &mut ShellUiState) {
    if !state.overview_open {
        state.overview_anim_t = 0.0;
        state.overview_closing = false;
        state.overview_drag_window_id = None;
        return;
    }

    state.apps.poll_decoded_icons(ctx);

    let animations_enabled = state.config.overview.animations;
    let dt = ctx.input(|i| i.stable_dt.clamp(0.001, 0.05));

    if !animations_enabled {
        if state.overview_closing {
            state.overview_open = false;
            state.overview_closing = false;
            state.overview_drag_window_id = None;
            state.overview_thumbnails.clear();
            return;
        }
        state.overview_anim_t = 1.0;
        state.overview_ws_slide = 0.0;
    } else {
        if state.overview_closing {
            state.overview_anim_t = (state.overview_anim_t - dt / OVERVIEW_CLOSE_DURATION).max(0.0);
            if state.overview_anim_t <= 0.001 {
                state.overview_open = false;
                state.overview_closing = false;
                state.overview_drag_window_id = None;
                state.overview_thumbnails.clear();
                return;
            }
            ctx.request_repaint();
        } else if state.overview_anim_t < 1.0 {
            state.overview_anim_t = (state.overview_anim_t + dt / OVERVIEW_OPEN_DURATION).min(1.0);
            ctx.request_repaint();
        }

        if state.overview_ws_slide.abs() > 0.003 {
            let decay = (-16.0 * dt).exp();
            state.overview_ws_slide *= decay;
            ctx.request_repaint();
        } else {
            state.overview_ws_slide = 0.0;
        }
    }

    let anim = ease_out_cubic(state.overview_anim_t);

    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        if state.overview_drag_window_id.is_some() {
            state.overview_drag_window_id = None;
        } else {
            state.overview_closing = true;
            ctx.request_repaint();
            return;
        }
    }

    let screen = ctx.content_rect();
    let t = &state.config.theme;
    let accent = Color32::from_rgb(t.accent_rgb[0], t.accent_rgb[1], t.accent_rgb[2]);
    let wm_snap = get_cached_wm_snap(ctx, state);
    let active_ws_idx = wm_snap.active_ws_idx;

    // Sync Overview carousel whenever Pila switches workspaces (e.g. via 3-finger touchpad swipe)
    if active_ws_idx != state.overview_last_pila_ws {
        let prev = state.overview_last_pila_ws;
        state.overview_last_pila_ws = active_ws_idx;
        if state.overview_view_ws != Some(active_ws_idx) {
            state.overview_ws_slide = if animations_enabled {
                if active_ws_idx > prev { 1.0 } else { -1.0 }
            } else {
                0.0
            };
            state.overview_view_ws = Some(active_ws_idx);
        }
    }

    let viewed_ws = state.overview_view_ws.unwrap_or(active_ws_idx);
    let max_ws = wm_snap.max_ws;

    // Keyboard (arrows, PageUp/Down, 1..9) & horizontal scroll workspace switching inside Overview
    if !state.overview_closing && state.overview_drag_window_id.is_none() {
        let mut switch_dir: i32 = 0;
        let mut jump_ws: Option<usize> = None;
        ctx.input(|i| {
            if i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::PageDown) {
                switch_dir = 1;
            } else if i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::PageUp) {
                switch_dir = -1;
            } else if i.smooth_scroll_delta.x.abs() > 24.0 {
                switch_dir = if i.smooth_scroll_delta.x < 0.0 { 1 } else { -1 };
            } else {
                const NUM_KEYS: [Key; 9] = [
                    Key::Num1,
                    Key::Num2,
                    Key::Num3,
                    Key::Num4,
                    Key::Num5,
                    Key::Num6,
                    Key::Num7,
                    Key::Num8,
                    Key::Num9,
                ];
                for (idx, k) in NUM_KEYS.iter().enumerate() {
                    if i.key_pressed(*k) {
                        jump_ws = Some(idx + 1);
                        break;
                    }
                }
            }
        });
        let target_ws = jump_ws
            .map(|n| n.clamp(1, max_ws))
            .or_else(|| {
                if switch_dir != 0 {
                    Some((viewed_ws as i32 + switch_dir).clamp(1, max_ws as i32) as usize)
                } else {
                    None
                }
            });
        if let Some(next_ws) = target_ws
            && next_ws != viewed_ws
        {
            state.overview_view_ws = Some(next_ws);
            state.overview_ws_slide = if animations_enabled {
                if next_ws > viewed_ws { 1.0 } else { -1.0 }
            } else {
                0.0
            };
            PilaConnector::switch_workspace(next_ws);
            ctx.request_repaint();
        }
    }

    let pointer_pos = ctx.input(|i| i.pointer.hover_pos().or_else(|| i.pointer.latest_pos()));
    let pointer_released = ctx.input(|i| i.pointer.primary_released());

    // Single-quad pre-baked backdrop (or subtle tint when compositor blur is explicitly enabled)
    let compositor_blur = state.config.overview.compositor_blur;
    let backdrop_tex = if !compositor_blur {
        Some(get_or_create_overview_backdrop(ctx, accent))
    } else {
        None
    };
    egui::Area::new(egui::Id::unique("rocketde_overview_backdrop"))
        .order(egui::Order::Background)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (rect, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
            if let Some(tex) = backdrop_tex {
                ui.painter().image(
                    tex.id(),
                    rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::from_rgba_unmultiplied(255, 255, 255, (255.0 * anim) as u8),
                );
            } else {
                ui.painter().rect_filled(
                    rect,
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(10, 12, 18, (165.0 * anim) as u8),
                );
            }
            if resp.clicked() && state.overview_drag_window_id.is_none() {
                if state.dock_context_menu.is_some() {
                    state.dock_context_menu = None;
                } else {
                    state.overview_closing = true;
                }
                ctx.request_repaint();
            }
        });

    // Top-left corner luminous ripple & bloom when entering Overview
    if animations_enabled && !state.overview_closing && state.overview_anim_t < 1.0 {
        let raw_t = state.overview_anim_t.clamp(0.0, 1.0);
        let wave1 = ease_out_cubic(raw_t);
        let wave2 = ease_out_cubic(((raw_t - 0.08) / 0.92).clamp(0.0, 1.0));
        let fade = (1.0 - raw_t).powf(1.35);
        let origin = screen.min;
        let max_r = (screen.width().min(screen.height()) * 0.26).clamp(160.0, 320.0);

        egui::Area::new(egui::Id::unique("rocketde_overview_corner_fx"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .interactable(false)
            .show(ctx, |ui| {
                let p = ui.painter();
                // Soft ambient corner bloom
                p.circle_filled(
                    origin,
                    22.0 + 72.0 * wave1,
                    Color32::from_rgba_unmultiplied(
                        accent.r(),
                        accent.g(),
                        accent.b(),
                        (70.0 * fade) as u8,
                    ),
                );
                p.circle_filled(
                    origin,
                    10.0 + 30.0 * wave1,
                    Color32::from_rgba_unmultiplied(255, 255, 255, (110.0 * fade) as u8),
                );
                // Primary expanding ring
                p.circle_stroke(
                    origin,
                    14.0 + max_r * wave1,
                    Stroke::new(
                        2.5 * (1.0 - 0.5 * raw_t),
                        Color32::from_rgba_unmultiplied(
                            accent.r(),
                            accent.g(),
                            accent.b(),
                            (185.0 * fade) as u8,
                        ),
                    ),
                );
                // Secondary crisp inner ring
                if wave2 > 0.01 {
                    p.circle_stroke(
                        origin,
                        8.0 + max_r * 0.65 * wave2,
                        Stroke::new(
                            1.4,
                            Color32::from_rgba_unmultiplied(255, 255, 255, (140.0 * fade) as u8),
                        ),
                    );
                }
            });
    }

    let mut ws_drop_targets: Vec<(usize, Rect)> = Vec::with_capacity(wm_snap.workspaces.len() + 2);
    let mut close_overview_now = false;
    let dragging_id = state.overview_drag_window_id.clone();

    // Smooth per-workspace spring scales for top switcher thumbnails
    let scales_id = egui::Id::unique("rocketde_ov_ws_scales");
    let ws_count = wm_snap.workspaces.len().max(1);
    let mut ws_scales: Vec<f32> = ctx
        .data(|d| d.get_temp::<Vec<f32>>(scales_id))
        .unwrap_or_default();
    if ws_scales.len() != ws_count {
        ws_scales.resize(ws_count, 0.96);
    }
    let scale_lerp = if animations_enabled {
        1.0 - (-18.0 * dt).exp()
    } else {
        1.0
    };
    let mut any_scale_moving = false;

    egui::Area::new(egui::Id::unique("rocketde_overview_main"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            ui.set_min_size(screen.size());
            ui.set_max_size(screen.size());

            let pad_x = (screen.width() * 0.04).clamp(24.0, 64.0);
            let content_w = (screen.width() - pad_x * 2.0).max(400.0);

            ui.add_space(16.0 - (1.0 - anim) * 28.0);

            // 1. Top Workspace Switcher Strip (16:9 mini-desktops)
            let ws_gap = 18.0;
            let base_ws_w =
                ((content_w - ws_gap * (ws_count.saturating_sub(1) as f32)) / ws_count as f32)
                    .clamp(128.0, 176.0);
            let base_ws_h = (base_ws_w * 9.0 / 16.0).round();
            let strip_w = ws_count as f32 * base_ws_w + (ws_count - 1) as f32 * ws_gap;
            let strip_left = ((screen.width() - strip_w) * 0.5).max(pad_x);

            ui.horizontal(|ui| {
                ui.add_space(strip_left);
                for (w_idx, ws_cached) in wm_snap.workspaces.iter().enumerate() {
                    let ws = &ws_cached.info;
                    let is_viewed = ws.index == viewed_ws;
                    let (slot_rect, ws_resp) = ui.allocate_exact_size(
                        Vec2::new(base_ws_w, base_ws_h + 22.0),
                        Sense::click(),
                    );
                    let frame_base = Rect::from_min_size(
                        slot_rect.min,
                        Vec2::new(base_ws_w, base_ws_h),
                    );
                    let is_drop_hover = dragging_id.is_some()
                        && pointer_pos
                            .map(|p| frame_base.expand(12.0).contains(p))
                            .unwrap_or(false);

                    let target_sc = if is_drop_hover {
                        1.08
                    } else if ws_resp.hovered() {
                        1.04
                    } else if is_viewed {
                        1.02
                    } else {
                        0.94
                    };
                    let cur_sc = ws_scales[w_idx];
                    let next_sc = if (target_sc - cur_sc).abs() <= 0.002 {
                        target_sc
                    } else {
                        any_scale_moving = true;
                        cur_sc + (target_sc - cur_sc) * scale_lerp
                    };
                    ws_scales[w_idx] = next_sc;

                    let ws_rect = Rect::from_center_size(
                        frame_base.center(),
                        Vec2::new(base_ws_w * next_sc, base_ws_h * next_sc),
                    );
                    ws_drop_targets.push((ws.index, ws_rect.expand(10.0)));

                    let surface_fill = if is_drop_hover {
                        Color32::from_rgba_unmultiplied(
                            24 + accent.r() / 5,
                            28 + accent.g() / 5,
                            40 + accent.b() / 5,
                            (235.0 * anim) as u8,
                        )
                    } else if is_viewed {
                        Color32::from_rgba_unmultiplied(24, 29, 44, (235.0 * anim) as u8)
                    } else {
                        Color32::from_rgba_unmultiplied(16, 19, 29, (195.0 * anim) as u8)
                    };

                    let border_stroke = if is_drop_hover {
                        Stroke::new(2.2, accent)
                    } else if is_viewed {
                        Stroke::new(
                            2.0,
                            Color32::from_rgba_unmultiplied(
                                accent.r(),
                                accent.g(),
                                accent.b(),
                                (235.0 * anim) as u8,
                            ),
                        )
                    } else if ws_resp.hovered() {
                        Stroke::new(
                            1.2,
                            Color32::from_rgba_unmultiplied(255, 255, 255, (110.0 * anim) as u8),
                        )
                    } else {
                        Stroke::new(
                            1.0,
                            Color32::from_rgba_unmultiplied(255, 255, 255, (32.0 * anim) as u8),
                        )
                    };

                    ui.painter().rect(
                        ws_rect,
                        CornerRadius::same(9),
                        surface_fill,
                        border_stroke,
                        egui::StrokeKind::Inside,
                    );

                    // Lightweight stylized mini-window blocks inside top switcher thumbnail
                    if !ws_cached.windows.is_empty() {
                        let shown_len = ws_cached.windows.len().min(4);
                        let inner = ws_rect.shrink(6.0);
                        let (cols, rows) = grid_dims_for_count(shown_len);
                        let gap_in = 4.0;
                        let cw = (inner.width() - (cols as f32 - 1.0) * gap_in) / cols as f32;
                        let ch = (inner.height() - (rows as f32 - 1.0) * gap_in) / rows as f32;
                        for (i, w) in ws_cached.windows.iter().take(shown_len).enumerate() {
                            let r = i / cols;
                            let c = i % cols;
                            let cell = Rect::from_min_size(
                                Pos2::new(
                                    inner.min.x + c as f32 * (cw + gap_in),
                                    inner.min.y + r as f32 * (ch + gap_in),
                                ),
                                Vec2::new(cw, ch),
                            );
                            let block_col = if w.is_focused {
                                Color32::from_rgba_unmultiplied(
                                    accent.r(),
                                    accent.g(),
                                    accent.b(),
                                    (125.0 * anim) as u8,
                                )
                            } else {
                                Color32::from_rgba_unmultiplied(
                                    255,
                                    255,
                                    255,
                                    (if is_viewed { 46.0 } else { 28.0 } * anim) as u8,
                                )
                            };
                            ui.painter().rect_filled(cell, CornerRadius::same(3), block_col);
                        }
                    }

                    if is_drop_hover {
                        ui.painter().rect_filled(
                            ws_rect.shrink(2.0),
                            CornerRadius::same(7),
                            Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 36),
                        );
                    }

                    let label_col = if is_viewed || is_drop_hover {
                        Color32::from_rgba_unmultiplied(255, 255, 255, (245.0 * anim) as u8)
                    } else if ws_resp.hovered() {
                        Color32::from_rgba_unmultiplied(220, 226, 238, (220.0 * anim) as u8)
                    } else {
                        Color32::from_rgba_unmultiplied(145, 154, 172, (185.0 * anim) as u8)
                    };
                    ui.painter().text(
                        Pos2::new(frame_base.center().x, frame_base.max.y + 13.0),
                        egui::Align2::CENTER_CENTER,
                        &ws_cached.label,
                        FontId::proportional(11.0),
                        label_col,
                    );

                    if ws_resp.clicked() && dragging_id.is_none() {
                        if ws.index != viewed_ws {
                            state.overview_ws_slide = if animations_enabled {
                                if ws.index > viewed_ws { 1.0 } else { -1.0 }
                            } else {
                                0.0
                            };
                            state.overview_view_ws = Some(ws.index);
                            PilaConnector::switch_workspace(ws.index);
                            ctx.request_repaint();
                        } else {
                            close_overview_now = true;
                        }
                    }

                    ui.add_space(ws_gap);
                }
            });

            // 2. GNOME Workspace Carousel (subtle scale 0.94 -> 1.0 + fade for smooth 144Hz+ on any GPU)
            let stage_top = (base_ws_h + 50.0).max(118.0);
            let dock_reserve = 88.0;
            let avail_stage_h = (screen.max.y - stage_top - dock_reserve).max(260.0);

            let resting_h = avail_stage_h * 0.92;
            let resting_w = resting_h * 16.0 / 9.0;
            let max_w = screen.width() * 0.72;
            let (base_canvas_w, base_canvas_h) = if resting_w > max_w {
                (max_w, max_w * 9.0 / 16.0)
            } else {
                (resting_w, resting_h)
            };

            let subtle_scale = 0.88 + 0.12 * anim;
            let canvas_w = base_canvas_w * subtle_scale;
            let canvas_h = base_canvas_h * subtle_scale;
            let corner_shift_x = -(1.0 - anim) * 28.0;
            let canvas_center_y = stage_top + avail_stage_h * 0.5 - (1.0 - anim) * 16.0;
            let canvas_gap = (screen.width() * 0.035).clamp(24.0, 56.0);
            let slide_px = state.overview_ws_slide * (canvas_w + canvas_gap) + corner_shift_x;

            let viewed_pos = wm_snap
                .workspaces
                .iter()
                .position(|w| w.info.index == viewed_ws)
                .unwrap_or(0) as isize;

            // Draw neighbors first, active workspace last
            for pass in 0..2 {
                for (i, ws_cached) in wm_snap.workspaces.iter().enumerate() {
                    let rel = i as isize - viewed_pos;
                    let is_center = rel == 0;
                    if (pass == 0 && is_center) || (pass == 1 && !is_center) {
                        continue;
                    }
                    if rel.abs() > 1 && state.overview_ws_slide.abs() < 0.05 {
                        continue;
                    }

                    let ws = &ws_cached.info;
                    let neighbor_scale = if is_center { 1.0 } else { 0.86 };
                    let w_box = canvas_w * neighbor_scale;
                    let h_box = canvas_h * neighbor_scale;
                    let cx = screen.center().x
                        + (rel as f32) * (canvas_w * 0.93 + canvas_gap)
                        + slide_px;

                    let canvas_rect = Rect::from_center_size(
                        Pos2::new(cx, canvas_center_y),
                        Vec2::new(w_box, h_box),
                    );

                    if canvas_rect.max.x < screen.min.x - 20.0
                        || canvas_rect.min.x > screen.max.x + 20.0
                    {
                        continue;
                    }

                    if !is_center {
                        ws_drop_targets.push((ws.index, canvas_rect));
                    }

                    let is_drop_hover = !is_center
                        && dragging_id.is_some()
                        && pointer_pos.map(|p| canvas_rect.contains(p)).unwrap_or(false);

                    let canvas_alpha = if is_center {
                        anim
                    } else if is_drop_hover {
                        anim * 0.96
                    } else {
                        anim * 0.62
                    };

                    let canvas_bg = if is_drop_hover {
                        Color32::from_rgba_unmultiplied(
                            26 + accent.r() / 6,
                            32 + accent.g() / 6,
                            48 + accent.b() / 6,
                            (235.0 * canvas_alpha) as u8,
                        )
                    } else {
                        Color32::from_rgba_unmultiplied(20, 25, 38, (228.0 * canvas_alpha) as u8)
                    };
                    let canvas_border = if is_drop_hover {
                        Stroke::new(2.2, accent)
                    } else if is_center {
                        Stroke::new(
                            1.2,
                            Color32::from_rgba_unmultiplied(255, 255, 255, (48.0 * anim) as u8),
                        )
                    } else {
                        Stroke::new(
                            1.0,
                            Color32::from_rgba_unmultiplied(255, 255, 255, (24.0 * anim) as u8),
                        )
                    };

                    ui.painter().rect(
                        canvas_rect,
                        CornerRadius::same(18),
                        canvas_bg,
                        canvas_border,
                        egui::StrokeKind::Inside,
                    );

                    if dragging_id.is_none() && !is_center {
                        let n_resp = ui.interact(
                            canvas_rect,
                            ui.scope_id().with(("ov_neighbor_ws", ws.index)),
                            Sense::click(),
                        );
                        if n_resp.clicked() {
                            state.overview_ws_slide = if animations_enabled {
                                if ws.index > viewed_ws { 1.0 } else { -1.0 }
                            } else {
                                0.0
                            };
                            state.overview_view_ws = Some(ws.index);
                            PilaConnector::switch_workspace(ws.index);
                            ctx.request_repaint();
                        }
                    }

                    if ws_cached.windows.is_empty() {
                        if is_center {
                            ui.painter().text(
                                canvas_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                &ws_cached.label,
                                FontId::proportional(16.0),
                                Color32::from_rgba_unmultiplied(
                                    170,
                                    180,
                                    200,
                                    (110.0 * canvas_alpha) as u8,
                                ),
                            );
                        }
                        continue;
                    }

                    let inner = canvas_rect
                        .shrink2(Vec2::new(26.0 * neighbor_scale, 22.0 * neighbor_scale));
                    let w_count = ws_cached.windows.len();
                    let (cols, rows) = grid_dims_for_count(w_count);
                    let cell_gap = 18.0 * neighbor_scale;
                    let max_win_h =
                        ((inner.height() - cell_gap * (rows as f32 - 1.0)) / rows as f32)
                            .max(90.0);
                    let max_win_w =
                        ((inner.width() - cell_gap * (cols as f32 - 1.0)) / cols as f32)
                            .max(120.0);
                    let slot_w = if w_count == 1 {
                        (inner.width() * 0.78).min(max_win_w)
                    } else {
                        max_win_w
                    };
                    let slot_h = if w_count == 1 {
                        (inner.height() * 0.84).min(max_win_h)
                    } else {
                        max_win_h
                    };

                    let total_grid_h = rows as f32 * slot_h + (rows as f32 - 1.0) * cell_gap;
                    let grid_top_y = inner.center().y - total_grid_h * 0.5;
                    let mut any_win_hovered = false;

                    for (r_idx, chunk) in ws_cached.windows.chunks(cols).enumerate() {
                        let row_w =
                            chunk.len() as f32 * slot_w + (chunk.len() as f32 - 1.0) * cell_gap;
                        let row_start_x = inner.center().x - row_w * 0.5;
                        let row_y = grid_top_y + r_idx as f32 * (slot_h + cell_gap);

                        for (c_idx, win) in chunk.iter().enumerate() {
                            let win_rect = Rect::from_min_size(
                                Pos2::new(
                                    row_start_x + c_idx as f32 * (slot_w + cell_gap),
                                    row_y,
                                ),
                                Vec2::new(slot_w, slot_h),
                            );

                            let is_being_dragged =
                                dragging_id.as_deref() == Some(win.id.as_str());
                            let preview_rect = Rect::from_min_max(
                                win_rect.min,
                                Pos2::new(
                                    win_rect.max.x,
                                    win_rect.max.y - 24.0 * neighbor_scale,
                                ),
                            );

                            let (hovered, close_hovered, clicked, drag_started, close_clicked) =
                                if is_center {
                                    let card_resp = ui.interact(
                                        win_rect,
                                        ui.scope_id().with(("ov_win", &win.id)),
                                        Sense::click_and_drag(),
                                    );
                                    let close_rect = Rect::from_center_size(
                                        Pos2::new(win_rect.max.x - 14.0, win_rect.min.y + 14.0),
                                        Vec2::new(24.0, 24.0),
                                    );
                                    let close_resp = ui.interact(
                                        close_rect,
                                        ui.scope_id().with(("ov_close", &win.id)),
                                        Sense::click(),
                                    );
                                    (
                                        card_resp.hovered() || close_resp.hovered(),
                                        close_resp.hovered(),
                                        card_resp.clicked(),
                                        card_resp.drag_started(),
                                        close_resp.clicked(),
                                    )
                                } else {
                                    (false, false, false, false, false)
                                };

                            if hovered {
                                any_win_hovered = true;
                            }

                            let card_bg = if is_being_dragged {
                                Color32::from_rgba_unmultiplied(255, 255, 255, 8)
                            } else {
                                Color32::from_rgba_unmultiplied(
                                    16,
                                    20,
                                    30,
                                    (235.0 * canvas_alpha) as u8,
                                )
                            };
                            let card_stroke = if hovered {
                                Stroke::new(2.2, accent)
                            } else if win.is_focused {
                                Stroke::new(
                                    1.5,
                                    Color32::from_rgba_unmultiplied(
                                        accent.r(),
                                        accent.g(),
                                        accent.b(),
                                        (200.0 * canvas_alpha) as u8,
                                    ),
                                )
                            } else {
                                Stroke::new(
                                    1.0,
                                    Color32::from_rgba_unmultiplied(
                                        255,
                                        255,
                                        255,
                                        (32.0 * canvas_alpha) as u8,
                                    ),
                                )
                            };

                            ui.painter().rect(
                                preview_rect,
                                CornerRadius::same(10),
                                card_bg,
                                card_stroke,
                                egui::StrokeKind::Inside,
                            );

                            if !is_being_dragged
                                && let Some((tex, tsz)) = find_window_thumbnail(state, win)
                            {
                                let inner_prev = preview_rect.shrink(3.0);
                                let fit = fit_rect_preserving_aspect(inner_prev, tsz);
                                ui.painter().image(
                                    tex.id(),
                                    fit,
                                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                    Color32::from_rgba_unmultiplied(
                                        255,
                                        255,
                                        255,
                                        (255.0 * canvas_alpha) as u8,
                                    ),
                                );
                            }

                            // Overlapping GNOME app icon badge at bottom-center of window preview
                            let icon_sz = 32.0 * neighbor_scale;
                            let icon_center =
                                Pos2::new(preview_rect.center().x, preview_rect.max.y - 3.0);
                            let app_tex = state
                                .apps
                                .get_icon_texture(ctx, &win.app_id, None)
                                .or_else(|| state.apps.get_icon_texture(ctx, &win.title_lower, None));
                            let icon_rect =
                                Rect::from_center_size(icon_center, Vec2::new(icon_sz, icon_sz));
                            if let Some(t_handle) = app_tex {
                                ui.painter().image(
                                    t_handle.id(),
                                    icon_rect,
                                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                    Color32::from_rgba_unmultiplied(
                                        255,
                                        255,
                                        255,
                                        (255.0 * canvas_alpha) as u8,
                                    ),
                                );
                            } else {
                                ui.painter().rect_filled(icon_rect, CornerRadius::same(8), accent);
                                ui.painter().text(
                                    icon_rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    &win.initial,
                                    FontId::proportional(14.0 * neighbor_scale),
                                    Color32::WHITE,
                                );
                            }

                            if is_center {
                                ui.painter().text(
                                    Pos2::new(win_rect.center().x, win_rect.max.y - 6.0),
                                    egui::Align2::CENTER_CENTER,
                                    &win.short_title,
                                    FontId::proportional(11.5),
                                    if hovered {
                                        Color32::from_rgba_unmultiplied(
                                            255,
                                            255,
                                            255,
                                            (255.0 * canvas_alpha) as u8,
                                        )
                                    } else {
                                        Color32::from_rgba_unmultiplied(
                                            200,
                                            208,
                                            222,
                                            (215.0 * canvas_alpha) as u8,
                                        )
                                    },
                                );

                                if hovered {
                                    let close_pos =
                                        Pos2::new(win_rect.max.x - 14.0, win_rect.min.y + 14.0);
                                    let close_bg = if close_hovered {
                                        Color32::from_rgb(225, 65, 65)
                                    } else {
                                        Color32::from_rgba_unmultiplied(28, 32, 42, 235)
                                    };
                                    ui.painter().circle_filled(close_pos, 11.0, close_bg);
                                    draw_icon(
                                        ui.painter(),
                                        close_pos,
                                        9.5,
                                        IconKind::Close,
                                        Color32::WHITE,
                                    );
                                }

                                if close_clicked {
                                    PilaConnector::close_window(&win.id);
                                } else if drag_started {
                                    state.overview_drag_window_id = Some(win.id.clone());
                                } else if clicked && dragging_id.is_none() {
                                    PilaConnector::focus_window(&win.id);
                                    close_overview_now = true;
                                }
                            }
                        }
                    }

                    if is_center
                        && dragging_id.is_none()
                        && !any_win_hovered
                        && ctx.input(|i| i.pointer.primary_clicked())
                        && pointer_pos.map(|p| canvas_rect.contains(p)).unwrap_or(false)
                    {
                        close_overview_now = true;
                    }
                }
            }
        });

    ctx.data_mut(|d| d.insert_temp(scales_id, ws_scales));
    if any_scale_moving {
        ctx.request_repaint();
    }

    // 3. Bottom Dock (identical to the main screen Dock)
    if state.config.dock.enabled {
        let slide_y = (1.0 - anim) * 32.0;
        let _ = crate::ui::dock::render_dock_in_overview(ctx, state, anim, slide_y);
    }

    // Floating live drag preview under pointer
    if let (Some(drag_id), Some(p)) = (&state.overview_drag_window_id, pointer_pos) {
        let dragged_win = wm_snap
            .workspaces
            .iter()
            .flat_map(|ws| ws.windows.iter())
            .find(|w| &w.id == drag_id);
        let thumb_opt = dragged_win.and_then(|w| find_window_thumbnail(state, w));

        egui::Area::new(egui::Id::unique("rocketde_overview_drag_ghost"))
            .order(egui::Order::Tooltip)
            .fixed_pos(p - Vec2::new(85.0, 52.0))
            .interactable(false)
            .show(ctx, |ui| {
                if let Some((tex, tsz)) = thumb_opt {
                    let box_rect = Rect::from_min_size(ui.cursor().min, Vec2::new(170.0, 106.0));
                    let fit = fit_rect_preserving_aspect(box_rect, tsz);
                    ui.painter().rect_filled(
                        fit.expand(3.0),
                        CornerRadius::same(10),
                        Color32::from_rgba_unmultiplied(0, 0, 0, 130),
                    );
                    ui.painter().image(
                        tex.id(),
                        fit,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::from_rgba_unmultiplied(255, 255, 255, 238),
                    );
                } else {
                    let label = dragged_win
                        .map(|w| w.app_id.chars().take(20).collect::<String>())
                        .unwrap_or_else(|| "Janela".to_string());
                    Frame::NONE
                        .fill(Color32::from_rgba_unmultiplied(
                            accent.r(),
                            accent.g(),
                            accent.b(),
                            220,
                        ))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::symmetric(12, 8))
                        .stroke(Stroke::new(1.0, Color32::WHITE))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(label)
                                    .size(12.5)
                                    .strong()
                                    .color(Color32::WHITE),
                            );
                        });
                }
            });

        if pointer_released {
            if let Some((ws_idx, _)) = ws_drop_targets.iter().find(|(_, r)| r.contains(p)) {
                let target_ws = *ws_idx;
                let win_id = drag_id.clone();
                // Optimistically update local WmState & bump version so cached snapshot refreshes immediately
                if let Ok(mut guard) = state.wm.state.lock() {
                    let wm = &mut *guard;
                    if let Some(w) = wm.windows.iter_mut().find(|w| w.id == win_id) {
                        w.workspace = target_ws;
                    }
                    for ws in &mut wm.workspaces {
                        let idx = ws.index;
                        ws.occupied = wm.windows.iter().any(|w| w.workspace == idx);
                    }
                    wm.version = wm.version.wrapping_add(1);
                }
                state.pending_move_window = Some((win_id, target_ws));
            }
            state.overview_drag_window_id = None;
            ctx.request_repaint();
        } else if ctx.input(|i| i.pointer.delta() != Vec2::ZERO) {
            ctx.request_repaint();
        }
    }

    if close_overview_now {
        state.overview_closing = true;
        state.overview_drag_window_id = None;
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_dims_balances_window_counts() {
        assert_eq!(grid_dims_for_count(1), (1, 1));
        assert_eq!(grid_dims_for_count(2), (2, 1));
        assert_eq!(grid_dims_for_count(4), (2, 2));
        assert_eq!(grid_dims_for_count(5), (3, 2));
        assert_eq!(grid_dims_for_count(7), (4, 2));
    }

    #[test]
    fn fit_rect_preserves_aspect_ratio() {
        let container = Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 200.0));
        let fitted = fit_rect_preserving_aspect(container, Vec2::new(1920.0, 1080.0));
        assert!((fitted.width() - 200.0).abs() < 0.01);
        assert!((fitted.height() - 112.5).abs() < 0.01);
    }
}
