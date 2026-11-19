use crate::system::apps::{AppIndexer, DesktopApp, try_eval_math};
use crate::ui::ShellUiState;
use crate::ui::icons::{IconKind, draw_icon};
use crate::wm::pila::PilaConnector;
use egui::{
    Align, Color32, ColorImage, Context, CornerRadius, FontId, Frame, Key, Layout, Margin, Pos2,
    Rect, RichText, Sense, Stroke, TextureFilter, TextureHandle, TextureOptions, TextureWrapMode,
    Vec2,
};

const COLS: usize = 7;
const ROWS: usize = 5;
const ITEMS_PER_PAGE: usize = COLS * ROWS;
const LAUNCH_ANIM_DURATION: f32 = 0.24;

#[inline]
fn ease_out_cubic(t: f32) -> f32 {
    let inv = 1.0 - t;
    1.0 - inv * inv * inv
}

#[inline]
fn ease_out_quint(t: f32) -> f32 {
    let inv = 1.0 - t;
    1.0 - inv * inv * inv * inv * inv
}

#[inline]
fn ease_out_back_subtle(t: f32) -> f32 {
    let c1 = 0.95;
    let c3 = c1 + 1.0;
    let u = t - 1.0;
    1.0 + c3 * u * u * u + c1 * u * u
}

fn get_or_create_backdrop_texture(ctx: &Context, accent: Color32) -> TextureHandle {
    let key = (accent.r(), accent.g(), accent.b());
    let id = egui::Id::unique("rocketde_launchpad_backdrop_tex");
    if let Some((cached_key, existing)) =
        ctx.data(|d| d.get_temp::<((u8, u8, u8), TextureHandle)>(id))
    {
        if cached_key == key {
            return existing;
        }
    }

    // Pre-bake clean frosted matte glass backdrop (no noise/grain) into a single 256x256 texture
    // so the Launchpad backdrop draws in 1 single GPU quad at full framerate.
    let size = 256usize;
    let mut pixels = vec![0u8; size * size * 4];
    let fsize = size as f32;

    for y in 0..size {
        let ny = y as f32 / fsize;
        let dy_top = ny - 0.24;
        let dy_bot = ny - 0.86;
        for x in 0..size {
            let nx = x as f32 / fsize;
            let dx_top = nx - 0.36;
            let dx_bot = nx - 0.62;

            let d_top = (dx_top * dx_top + dy_top * dy_top).sqrt();
            let d_bot = (dx_bot * dx_bot + dy_bot * dy_bot).sqrt();
            let g_top = (1.0 - (d_top / 0.58)).clamp(0.0, 1.0);
            let g_bot = (1.0 - (d_bot / 0.50)).clamp(0.0, 1.0);
            let g1 = g_top * g_top;
            let g2 = g_bot * g_bot;

            let r = (14.0 + accent.r() as f32 * 0.06 * g1 + 95.0 * 0.04 * g2)
                .clamp(0.0, 255.0) as u8;
            let g = (17.0 + accent.g() as f32 * 0.06 * g1 + 130.0 * 0.04 * g2)
                .clamp(0.0, 255.0) as u8;
            let b = (26.0 + accent.b() as f32 * 0.06 * g1 + 200.0 * 0.04 * g2)
                .clamp(0.0, 255.0) as u8;

            let off = (y * size + x) * 4;
            pixels[off] = r;
            pixels[off + 1] = g;
            pixels[off + 2] = b;
            pixels[off + 3] = 192;
        }
    }

    let img = ColorImage::from_rgba_unmultiplied([size, size], &pixels);
    let opts = TextureOptions {
        magnification: TextureFilter::Linear,
        minification: TextureFilter::Linear,
        wrap_mode: TextureWrapMode::ClampToEdge,
        mipmap_mode: None,
    };
    let handle = ctx.load_texture("rocketde_launchpad_backdrop", img, opts);
    ctx.data_mut(|d| d.insert_temp(id, (key, handle.clone())));
    handle
}

/// Renders a refined, Apple macOS Launchpad-style full-screen application launcher:
/// - Smooth fade-in and icon hover scaling
/// - Minimal frosted search pill (no tutorial clutter)
/// - Real multi-page grid (7x5 per page) with interactive page dots & chevrons
/// - Right-click context menu on any app to "Abrir" or "Adicionar/Remover dos Favoritos"
pub fn render_launcher(ctx: &Context, state: &mut ShellUiState) {
    if !state.launcher_open {
        state.launcher_anim_t = 0.0;
        state.launcher_context_menu = None;
        state.launcher_drag_app = None;
        return;
    }

    state.apps.poll_decoded_icons(ctx);

    // Frame-rate independent smooth entrance animation
    let (dt, now_sec) = ctx.input(|i| (i.stable_dt.clamp(0.001, 0.05), i.time));
    if state.launcher_anim_t < 1.0 {
        state.launcher_anim_t = (state.launcher_anim_t + dt / LAUNCH_ANIM_DURATION).min(1.0);
        ctx.request_repaint();
    }
    let raw_t = state.launcher_anim_t;
    let anim = ease_out_cubic(raw_t);

    let screen = ctx.content_rect();
    let mut filtered_indices: Vec<usize> = state.apps.search_indices(&state.launcher_query).to_vec();
    filtered_indices.retain(|&i| {
        let app = &state.apps.apps[i];
        !state.config.is_app_pinned(&app.app_id, &app.name)
    });
    let total_results = filtered_indices.len();
    let total_pages = if total_results == 0 {
        1
    } else {
        (total_results - 1) / ITEMS_PER_PAGE + 1
    };

    if state.launcher_page >= total_pages {
        state.launcher_page = total_pages.saturating_sub(1);
    }

    let page_start = state.launcher_page * ITEMS_PER_PAGE;
    let page_end = (page_start + ITEMS_PER_PAGE).min(total_results);
    let page_indices: Vec<usize> = if page_start < filtered_indices.len() {
        filtered_indices[page_start..page_end].to_vec()
    } else {
        Vec::new()
    };

    if !page_indices.is_empty() && state.launcher_selected >= page_indices.len() {
        state.launcher_selected = 0;
    }

    if state.launcher_icon_scales.len() != page_indices.len() {
        state.launcher_icon_scales.resize(page_indices.len(), 1.0);
    }

    // Keyboard & scroll/trackpad navigation (disabled while context menu is open except Esc)
    let mut launch_selected_now = false;
    let mut close_now = false;
    let ctx_menu_open = state.launcher_context_menu.is_some();

    ctx.input(|i| {
        if i.key_pressed(Key::Escape) {
            if ctx_menu_open {
                state.launcher_context_menu = None;
            } else {
                close_now = true;
            }
        } else if !ctx_menu_open {
            if total_pages > 1 && now_sec >= state.launcher_scroll_cooldown {
                let scroll = i.smooth_scroll_delta;
                let delta = if scroll.x.abs() > scroll.y.abs() {
                    -scroll.x
                } else {
                    -scroll.y
                };
                if delta > 18.0 && state.launcher_page + 1 < total_pages {
                    state.launcher_page += 1;
                    state.launcher_selected = 0;
                    state.launcher_scroll_cooldown = now_sec + 0.18;
                } else if delta < -18.0 && state.launcher_page > 0 {
                    state.launcher_page -= 1;
                    state.launcher_selected = 0;
                    state.launcher_scroll_cooldown = now_sec + 0.18;
                }
            }

            if i.key_pressed(Key::PageDown) && state.launcher_page + 1 < total_pages {
                state.launcher_page += 1;
                state.launcher_selected = 0;
                state.launcher_kb_nav_active = true;
            } else if i.key_pressed(Key::PageUp) && state.launcher_page > 0 {
                state.launcher_page -= 1;
                state.launcher_selected = 0;
                state.launcher_kb_nav_active = true;
            } else if !page_indices.is_empty() {
                let len = page_indices.len();
                if i.key_pressed(Key::ArrowRight)
                    || (i.key_pressed(Key::Tab) && !i.modifiers.shift)
                {
                    state.launcher_kb_nav_active = true;
                    if state.launcher_selected + 1 < len {
                        state.launcher_selected += 1;
                    } else if state.launcher_page + 1 < total_pages {
                        state.launcher_page += 1;
                        state.launcher_selected = 0;
                    } else {
                        state.launcher_selected = 0;
                    }
                } else if i.key_pressed(Key::ArrowLeft)
                    || (i.key_pressed(Key::Tab) && i.modifiers.shift)
                {
                    state.launcher_kb_nav_active = true;
                    if state.launcher_selected > 0 {
                        state.launcher_selected -= 1;
                    } else if state.launcher_page > 0 {
                        state.launcher_page -= 1;
                        state.launcher_selected = ITEMS_PER_PAGE - 1;
                    } else {
                        state.launcher_selected = len - 1;
                    }
                } else if i.key_pressed(Key::ArrowDown) {
                    state.launcher_kb_nav_active = true;
                    let next = state.launcher_selected + COLS;
                    state.launcher_selected = if next < len { next } else { len - 1 };
                } else if i.key_pressed(Key::ArrowUp) {
                    state.launcher_kb_nav_active = true;
                    state.launcher_selected = state.launcher_selected.saturating_sub(COLS);
                } else if i.key_pressed(Key::Enter) {
                    launch_selected_now = true;
                }
            } else if i.key_pressed(Key::Enter) {
                launch_selected_now = true;
            }
        }
    });

    if close_now {
        close_launchpad_and_restore_focus(state);
        return;
    }

    let t = &state.config.theme;
    let accent = Color32::from_rgb(t.accent_rgb[0], t.accent_rgb[1], t.accent_rgb[2]);
    let mut clicked_interactive_item = false;
    let mut pending_launch: Option<(Option<(String, String)>, String)> = None;

    egui::Area::new(egui::Id::unique("rocketde_fullscreen_launchpad"))
        .order(egui::Order::Foreground)
        .fixed_pos(Pos2::ZERO)
        .show(ctx, |ui| {
            ui.set_min_size(screen.size());
            ui.set_max_size(screen.size());

            // 1. Frosted Glassmorphic Backdrop (single-quad pre-baked frosted matte glass)
            let backdrop_tex = get_or_create_backdrop_texture(ctx, accent);
            ui.painter().image(
                backdrop_tex.id(),
                screen,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::from_rgba_unmultiplied(255, 255, 255, (255.0 * anim).round() as u8),
            );

            // 2. Minimal Top-Centered Glassmorphic Search Pill
            let search_w = 360.0f32.min(screen.width() - 48.0);
            let search_h = 36.0;
            let search_rect = Rect::from_min_size(
                Pos2::new((screen.width() - search_w) * 0.5, 42.0),
                Vec2::new(search_w, search_h),
            );

            if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                if search_rect.expand(6.0).contains(p) {
                    clicked_interactive_item = true;
                }
            }

            ui.painter().rect_filled(
                search_rect.translate(Vec2::new(0.0, 3.0)).expand(1.5),
                CornerRadius::same(12),
                Color32::from_rgba_unmultiplied(0, 0, 0, (48.0 * anim) as u8),
            );
            ui.painter().rect(
                search_rect,
                CornerRadius::same(11),
                Color32::from_rgba_unmultiplied(255, 255, 255, (24.0 * anim) as u8),
                Stroke::new(
                    1.0,
                    Color32::from_rgba_unmultiplied(255, 255, 255, (48.0 * anim) as u8),
                ),
                egui::StrokeKind::Inside,
            );
            ui.painter().line_segment(
                [
                    Pos2::new(search_rect.min.x + 12.0, search_rect.min.y + 1.0),
                    Pos2::new(search_rect.max.x - 12.0, search_rect.min.y + 1.0),
                ],
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, (60.0 * anim) as u8)),
            );

            let inner_search = search_rect.shrink2(Vec2::new(12.0, 6.0));
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(inner_search)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    let (icon_rect, _) =
                        ui.allocate_exact_size(Vec2::new(18.0, 18.0), Sense::hover());
                    draw_icon(
                        ui.painter(),
                        icon_rect.center(),
                        13.5,
                        IconKind::Search,
                        Color32::from_rgba_unmultiplied(210, 216, 228, 200),
                    );
                    ui.add_space(6.0);
                    let edit = egui::TextEdit::singleline(&mut state.launcher_query)
                        .hint_text("Buscar")
                        .frame(Frame::NONE)
                        .font(FontId::proportional(14.0))
                        .desired_width(inner_search.width() - 32.0);

                    let resp = ui.add(edit);
                    if state.launcher_focus_requested {
                        resp.request_focus();
                        state.launcher_focus_requested = false;
                    }
                    if resp.changed() {
                        state.launcher_selected = 0;
                        state.launcher_page = 0;
                        state.launcher_context_menu = None;
                    }
                },
            );

            // 3. Compact Math or Shell Command Pill
            let mut grid_top_y = 104.0;
            if let Some(ans) = try_eval_math(&state.launcher_query) {
                let banner_rect = Rect::from_min_size(
                    Pos2::new((screen.width() - search_w) * 0.5, 90.0),
                    Vec2::new(search_w, 38.0),
                );
                ui.painter().rect(
                    banner_rect,
                    CornerRadius::same(10),
                    Color32::from_rgba_unmultiplied(255, 255, 255, 18),
                    Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 34)),
                    egui::StrokeKind::Inside,
                );
                draw_icon(
                    ui.painter(),
                    Pos2::new(banner_rect.min.x + 20.0, banner_rect.center().y),
                    15.0,
                    IconKind::Calculator,
                    accent,
                );
                ui.painter().text(
                    Pos2::new(banner_rect.min.x + 38.0, banner_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("= {}", ans),
                    FontId::proportional(15.5),
                    Color32::WHITE,
                );
                grid_top_y = 144.0;
            } else if let Some(shell_cmd) = state.launcher_query.trim().strip_prefix('>') {
                let shell_cmd = shell_cmd.trim().to_string();
                let banner_w = (search_w + 60.0).min(screen.width() - 48.0);
                let banner_rect = Rect::from_min_size(
                    Pos2::new((screen.width() - banner_w) * 0.5, 90.0),
                    Vec2::new(banner_w, 38.0),
                );
                ui.painter().rect(
                    banner_rect,
                    CornerRadius::same(10),
                    Color32::from_rgba_unmultiplied(255, 255, 255, 18),
                    Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 34)),
                    egui::StrokeKind::Inside,
                );
                draw_icon(
                    ui.painter(),
                    Pos2::new(banner_rect.min.x + 20.0, banner_rect.center().y),
                    15.0,
                    IconKind::Terminal,
                    Color32::from_rgb(48, 209, 88),
                );
                ui.painter().text(
                    Pos2::new(banner_rect.min.x + 38.0, banner_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    if shell_cmd.is_empty() {
                        "Executar comando...".to_string()
                    } else {
                        format!("Executar: {}", shell_cmd)
                    },
                    FontId::proportional(14.0),
                    Color32::WHITE,
                );
                if launch_selected_now && !shell_cmd.is_empty() {
                    pending_launch = Some((None, shell_cmd));
                    launch_selected_now = false;
                }
                grid_top_y = 144.0;
            }

            // 4. Apple-Style 7x5 Application Grid
            let dock_reserve = if state.config.dock.enabled {
                state.config.dock.icon_size + 52.0
            } else {
                44.0
            };
            let grid_max_w = (screen.width() * 0.78).clamp(640.0, 1220.0);
            let grid_start_x = (screen.width() - grid_max_w) * 0.5;
            let cell_w = grid_max_w / (COLS as f32);
            let cell_h =
                ((screen.height() - grid_top_y - dock_reserve - 26.0) / (ROWS as f32)).clamp(94.0, 140.0);
            let base_icon_size = (cell_h * 0.56).clamp(52.0, 72.0);

            // Origin at the very bottom-center, below the bottom dock edge
            let dock_origin = Pos2::new(screen.center().x, screen.max.y + 34.0);
            let is_animating = raw_t < 1.0;

            if page_indices.is_empty() {
                ui.painter().text(
                    Pos2::new(screen.center().x, screen.center().y - 16.0),
                    egui::Align2::CENTER_CENTER,
                    "Nenhum aplicativo encontrado",
                    FontId::proportional(16.0),
                    Color32::from_rgb(155, 162, 178),
                );
            }

            let mut needs_scale_repaint = false;
            let scale_alpha = 1.0 - (-16.0 * dt).exp();

            for (idx, &app_idx) in page_indices.iter().enumerate() {
                let col = idx % COLS;
                let row = idx / COLS;

                let cell_center_x = grid_start_x + (col as f32 + 0.5) * cell_w;
                let cell_top_y = grid_top_y + (row as f32) * cell_h;
                let target_icon_center =
                    Pos2::new(cell_center_x, cell_top_y + base_icon_size * 0.54 + 8.0);

                let (icon_center, anim_scale, icon_alpha, label_alpha) = if is_animating {
                    let row_from_bottom = (ROWS - 1).saturating_sub(row) as f32;
                    let col_dist = (col as f32 - (COLS as f32 - 1.0) * 0.5).abs();
                    let delay = (row_from_bottom * 0.024 + col_dist * 0.015).min(0.18);
                    let local_t = ((raw_t - delay) / (1.0 - delay)).clamp(0.0, 1.0);
                    if local_t <= 0.001 {
                        continue;
                    }
                    let pos_eased = ease_out_quint(local_t);
                    let scale_eased = ease_out_back_subtle(local_t);
                    let pos = Pos2::new(
                        dock_origin.x + (target_icon_center.x - dock_origin.x) * pos_eased,
                        dock_origin.y + (target_icon_center.y - dock_origin.y) * pos_eased,
                    );
                    let sc = (0.06 + 0.94 * scale_eased).max(0.05);
                    let i_alpha = (local_t * 2.6).min(1.0);
                    let l_alpha = ((pos_eased - 0.45) / 0.55).clamp(0.0, 1.0);
                    (pos, sc, i_alpha, l_alpha)
                } else {
                    (target_icon_center, 1.0, 1.0, 1.0)
                };

                let cell_rect = Rect::from_min_size(
                    Pos2::new(cell_center_x - cell_w * 0.42, cell_top_y),
                    Vec2::new(cell_w * 0.84, cell_h - 8.0),
                );

                let resp = ui.interact(
                    cell_rect,
                    egui::Id::unique(("launcher_app", app_idx)),
                    Sense::click_and_drag(),
                );
                if resp.hovered() || resp.clicked() || resp.secondary_clicked() || resp.dragged() {
                    clicked_interactive_item = true;
                }

                let is_selected = idx == state.launcher_selected
                    && (!state.launcher_query.is_empty() || state.launcher_kb_nav_active);
                let is_hovered = resp.hovered() && !ctx_menu_open && raw_t >= 0.5;
                if is_hovered {
                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                }

                // Smooth frame-rate independent hover scale interpolation
                let target_scale = if is_hovered { 1.08 } else { 1.0 };
                let cur_scale = state.launcher_icon_scales[idx];
                let mut next_scale = cur_scale + (target_scale - cur_scale) * scale_alpha;
                if (next_scale - target_scale).abs() > 0.004 {
                    needs_scale_repaint = true;
                } else {
                    next_scale = target_scale;
                }
                state.launcher_icon_scales[idx] = next_scale;

                let cur_icon_size = base_icon_size * next_scale * anim_scale;
                let icon_rect = Rect::from_center_size(
                    icon_center,
                    Vec2::new(cur_icon_size, cur_icon_size),
                );

                // Subtle highlight behind selected or hovered icon
                if is_selected || is_hovered {
                    let fill_a = if is_selected { 28.0 } else { 18.0 };
                    ui.painter().rect_filled(
                        icon_rect.expand(8.0),
                        CornerRadius::same(18),
                        Color32::from_rgba_unmultiplied(255, 255, 255, (fill_a * icon_alpha) as u8),
                    );
                }

                let (icon_name, icon_path) = {
                    let app = &state.apps.apps[app_idx];
                    (app.icon_name.clone(), app.icon_path.clone())
                };
                let tex_opt = state.apps.get_icon_texture(
                    ctx,
                    &icon_name,
                    icon_path.as_deref(),
                );
                let app = &state.apps.apps[app_idx];

                if let Some(tex) = tex_opt {
                    // Clean XDG icon rendered directly with no artificial box behind it
                    ui.painter().image(
                        tex.id(),
                        icon_rect,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::from_rgba_unmultiplied(255, 255, 255, (255.0 * icon_alpha) as u8),
                    );
                } else {
                    // Refined muted squircle with clean monogram when no system icon exists
                    let base_tile = app_tile_color(&app.name);
                    let tile_color = Color32::from_rgba_unmultiplied(
                        base_tile.r(),
                        base_tile.g(),
                        base_tile.b(),
                        (base_tile.a() as f32 * icon_alpha) as u8,
                    );
                    ui.painter().rect(
                        icon_rect,
                        CornerRadius::same((16.0 * anim_scale).round().max(4.0) as u8),
                        tile_color,
                        Stroke::new(
                            1.0,
                            Color32::from_rgba_unmultiplied(255, 255, 255, (42.0 * icon_alpha) as u8),
                        ),
                        egui::StrokeKind::Inside,
                    );
                    let monogram: String = app
                        .name
                        .chars()
                        .next()
                        .unwrap_or('A')
                        .to_uppercase()
                        .to_string();
                    ui.painter().text(
                        icon_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        monogram,
                        FontId::proportional(cur_icon_size * 0.42),
                        Color32::from_rgba_unmultiplied(255, 255, 255, (255.0 * icon_alpha) as u8),
                    );
                }

                // Clean Apple-style app label with subtle shadow
                if label_alpha > 0.02 {
                    let name_trunc: String = if app.name.chars().count() > 16 {
                        let mut s: String = app.name.chars().take(15).collect();
                        s.push('…');
                        s
                    } else {
                        app.name.clone()
                    };

                    let label_pos = Pos2::new(icon_center.x, icon_rect.max.y + 15.0 * anim_scale);
                    ui.painter().text(
                        Pos2::new(label_pos.x, label_pos.y + 1.0),
                        egui::Align2::CENTER_CENTER,
                        &name_trunc,
                        FontId::proportional(12.5),
                        Color32::from_rgba_unmultiplied(0, 0, 0, (140.0 * label_alpha) as u8),
                    );
                    ui.painter().text(
                        label_pos,
                        egui::Align2::CENTER_CENTER,
                        &name_trunc,
                        FontId::proportional(12.5),
                        Color32::from_rgba_unmultiplied(245, 247, 252, (245.0 * label_alpha) as u8),
                    );
                }

                // Interactions: Drag to Dock, Right-click Context Menu, Left-click / Enter launches app
                let dragged_far = ctx.input(|i| {
                    i.pointer
                        .press_origin()
                        .zip(i.pointer.interact_pos())
                        .is_some_and(|(a, b)| a.distance(b) >= 6.0)
                });
                if resp.dragged()
                    && dragged_far
                    && state.launcher_drag_app.is_none()
                    && raw_t >= 0.85
                    && !ctx_menu_open
                    && !is_animating
                {
                    state.launcher_drag_app = Some(app.clone());
                } else if resp.secondary_clicked() {
                    let pointer = ctx
                        .input(|i| i.pointer.interact_pos())
                        .unwrap_or(icon_rect.center());
                    state.launcher_context_menu = Some((app.clone(), pointer));
                } else if resp.clicked() && !ctx_menu_open && state.launcher_drag_app.is_none() {
                    pending_launch = Some((Some((app.app_id.clone(), app.name.clone())), app.exec.clone()));
                } else if launch_selected_now && idx == state.launcher_selected {
                    pending_launch = Some((Some((app.app_id.clone(), app.name.clone())), app.exec.clone()));
                    launch_selected_now = false;
                }
            }

            if needs_scale_repaint {
                ctx.request_repaint();
            }

            // 5. Real Page Navigation (Chevrons & Bottom Dots when > 1 page)
            if total_pages > 1 {
                let mid_y = grid_top_y + (cell_h * (ROWS as f32)) * 0.48;

                if state.launcher_page > 0 {
                    let prev_rect = Rect::from_center_size(
                        Pos2::new((grid_start_x * 0.5).max(36.0), mid_y),
                        Vec2::new(36.0, 36.0),
                    );
                    let prev_resp = ui.allocate_rect(prev_rect, Sense::click());
                    if prev_resp.hovered() || prev_resp.clicked() {
                        clicked_interactive_item = true;
                    }
                    if prev_resp.hovered() {
                        ui.painter().circle_filled(
                            prev_rect.center(),
                            18.0,
                            Color32::from_rgba_unmultiplied(255, 255, 255, 24),
                        );
                    }
                    draw_icon(
                        ui.painter(),
                        prev_rect.center(),
                        16.0,
                        IconKind::ChevronLeft,
                        Color32::from_rgba_unmultiplied(255, 255, 255, 180),
                    );
                    if prev_resp.clicked() {
                        state.launcher_page -= 1;
                        state.launcher_selected = 0;
                        state.launcher_context_menu = None;
                    }
                }

                if state.launcher_page + 1 < total_pages {
                    let next_rect = Rect::from_center_size(
                        Pos2::new((screen.width() - grid_start_x * 0.5).min(screen.width() - 36.0), mid_y),
                        Vec2::new(36.0, 36.0),
                    );
                    let next_resp = ui.allocate_rect(next_rect, Sense::click());
                    if next_resp.hovered() || next_resp.clicked() {
                        clicked_interactive_item = true;
                    }
                    if next_resp.hovered() {
                        ui.painter().circle_filled(
                            next_rect.center(),
                            18.0,
                            Color32::from_rgba_unmultiplied(255, 255, 255, 24),
                        );
                    }
                    draw_icon(
                        ui.painter(),
                        next_rect.center(),
                        16.0,
                        IconKind::ChevronRight,
                        Color32::from_rgba_unmultiplied(255, 255, 255, 180),
                    );
                    if next_resp.clicked() {
                        state.launcher_page += 1;
                        state.launcher_selected = 0;
                        state.launcher_context_menu = None;
                    }
                }

                // Bottom Page Dots (positioned cleanly above the bottom Dock)
                let dots_y = if state.config.dock.enabled {
                    screen.height() - state.config.dock.icon_size - 40.0
                } else {
                    screen.height() - 56.0
                };
                let spacing = 16.0;
                let total_dots_w = (total_pages.saturating_sub(1) as f32) * spacing;
                let start_dot_x = screen.center().x - total_dots_w * 0.5;
                for dot_i in 0..total_pages {
                    let dot_center = Pos2::new(start_dot_x + (dot_i as f32) * spacing, dots_y);
                    let dot_rect = Rect::from_center_size(dot_center, Vec2::new(14.0, 14.0));
                    let dot_resp = ui.allocate_rect(dot_rect, Sense::click());
                    if dot_resp.hovered() || dot_resp.clicked() {
                        clicked_interactive_item = true;
                    }
                    let col = if dot_i == state.launcher_page {
                        Color32::WHITE
                    } else if dot_resp.hovered() {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 150)
                    } else {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 65)
                    };
                    ui.painter().circle_filled(dot_center, 3.5, col);
                    if dot_resp.clicked() {
                        state.launcher_page = dot_i;
                        state.launcher_selected = 0;
                        state.launcher_context_menu = None;
                    }
                }
            }

            // Clicking empty Launchpad background closes context menu first, or Launchpad itself
            let bg_clicked = ctx.input(|i| i.pointer.primary_clicked());
            let over_bottom_dock = state.config.dock.enabled
                && ctx.input(|i| {
                    i.pointer.interact_pos().is_some_and(|p| {
                        p.y >= screen.max.y - (state.config.dock.icon_size + 26.0 + state.config.dock.margin_bottom as f32)
                    })
                });
            if bg_clicked
                && !clicked_interactive_item
                && !over_bottom_dock
                && state.launcher_drag_app.is_none()
            {
                if state.launcher_context_menu.is_some() {
                    state.launcher_context_menu = None;
                } else if state.dock_context_menu.is_some() {
                    state.dock_context_menu = None;
                } else {
                    close_launchpad_and_restore_focus(state);
                }
            }
        });

    // Bottom Dock inside Launchpad (allows dragging apps straight onto the Dock!)
    let dock_rect = if state.config.dock.enabled {
        let slide_y = (1.0 - anim) * 32.0;
        crate::ui::dock::render_dock_in_overview(ctx, state, anim, slide_y)
    } else {
        None
    };

    // Drag-and-drop preview & drop onto Dock
    if let Some(dragged_app) = state.launcher_drag_app.clone() {
        let ptr_pos = ctx.input(|i| i.pointer.hover_pos().or_else(|| i.pointer.latest_pos()));
        let released = ctx.input(|i| i.pointer.primary_released() || !i.pointer.primary_down());
        let drop_zone = dock_rect.map(|r| r.expand(24.0));
        let over_dock = match (drop_zone, ptr_pos) {
            (Some(zone), Some(p)) => zone.contains(p),
            _ => false,
        };

        if let Some(p) = ptr_pos {
            let tex_opt = state.apps.get_icon_texture(
                ctx,
                &dragged_app.icon_name,
                dragged_app.icon_path.as_deref(),
            );
            egui::Area::new(egui::Id::unique("rocketde_launcher_drag_preview"))
                .order(egui::Order::Tooltip)
                .fixed_pos(Pos2::ZERO)
                .interactable(false)
                .show(ctx, |ui| {
                    if let (true, Some(zone)) = (over_dock, dock_rect) {
                        ui.painter().rect_stroke(
                            zone.expand(4.0),
                            CornerRadius::same(22),
                            Stroke::new(2.0, accent),
                            egui::StrokeKind::Outside,
                        );
                    }
                    let icon_rect = Rect::from_center_size(p, Vec2::new(56.0, 56.0));
                    if let Some(tex) = tex_opt {
                        ui.painter().image(
                            tex.id(),
                            icon_rect,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::from_rgba_unmultiplied(255, 255, 255, 235),
                        );
                    } else {
                        ui.painter().rect_filled(
                            icon_rect,
                            CornerRadius::same(14),
                            accent,
                        );
                    }
                    if over_dock {
                        let already = state
                            .config
                            .is_app_pinned(&dragged_app.app_id, &dragged_app.name);
                        let hint = if already {
                            "Já está na Dock"
                        } else {
                            "Solte para fixar na Dock"
                        };
                        let tip_rect = Rect::from_center_size(
                            Pos2::new(p.x, p.y - 42.0),
                            Vec2::new(155.0, 22.0),
                        );
                        ui.painter().rect_filled(
                            tip_rect,
                            CornerRadius::same(7),
                            Color32::from_rgba_unmultiplied(20, 24, 34, 242),
                        );
                        ui.painter().text(
                            tip_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            hint,
                            FontId::proportional(11.0),
                            Color32::WHITE,
                        );
                    }
                });
        }

        if released {
            if over_dock
                && !state
                    .config
                    .is_app_pinned(&dragged_app.app_id, &dragged_app.name)
            {
                state.config.add_pinned_app(
                    &dragged_app.name,
                    &dragged_app.app_id,
                    &dragged_app.exec,
                    &dragged_app.glyph,
                );
            }
            state.launcher_drag_app = None;
            ctx.request_repaint();
        } else if ctx.input(|i| i.pointer.delta() != Vec2::ZERO) {
            ctx.request_repaint();
        }
    }

    if let Some((app_meta, exec_cmd)) = pending_launch {
        if let Some((app_id, name)) = app_meta {
            state.mark_app_launching(&app_id, &name);
        } else if let Some(p) = state
            .config
            .dock
            .pinned_apps
            .iter()
            .find(|p| p.exec == exec_cmd)
        {
            let app_id = p.app_id.clone();
            let name = p.name.clone();
            state.mark_app_launching(&app_id, &name);
        }
        AppIndexer::launch(&exec_cmd);
        state.launcher_open = false;
        state.launcher_context_menu = None;
        return;
    }

    // 6. Right-Click Context Menu for Launchpad Apps (Abrir / Adicionar ou Remover dos Favoritos)
    render_launcher_context_menu(ctx, state, accent);
}

fn render_launcher_context_menu(ctx: &Context, state: &mut ShellUiState, accent: Color32) {
    let Some((app, raw_pos)) = state.launcher_context_menu.clone() else {
        return;
    };

    let screen = ctx.content_rect();
    let menu_w = 225.0;
    let menu_h = 104.0;
    let pos = Pos2::new(
        raw_pos.x.clamp(12.0, (screen.width() - menu_w - 12.0).max(12.0)),
        raw_pos.y.clamp(12.0, (screen.height() - menu_h - 12.0).max(12.0)),
    );
    let menu_rect_id = egui::Id::unique("rocketde_launchpad_ctx_actual_rect");
    let hit_rect = ctx
        .data(|d| d.get_temp::<Rect>(menu_rect_id))
        .filter(|r| (r.min.x - pos.x).abs() < 4.0 && (r.min.y - pos.y).abs() < 4.0)
        .unwrap_or_else(|| Rect::from_min_size(pos, Vec2::new(menu_w, menu_h)));

    // Close context menu if user clicks outside of it
    if ctx.input(|i| i.pointer.any_pressed()) {
        if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
            if !hit_rect.expand(2.0).contains(p) {
                state.launcher_context_menu = None;
                return;
            }
        }
    }

    let is_pinned = state.config.is_app_pinned(&app.app_id, &app.name);
    let t = &state.config.theme;
    let frame = Frame::NONE
        .fill(Color32::from_rgba_unmultiplied(
            t.panel_bg_rgba[0],
            t.panel_bg_rgba[1],
            t.panel_bg_rgba[2],
            248,
        ))
        .corner_radius(CornerRadius::same(11))
        .inner_margin(Margin::same(8))
        .stroke(Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 45),
        ));

    let mut close_menu = false;
    let mut launch_app: Option<DesktopApp> = None;
    let mut toggle_pin: Option<(DesktopApp, bool)> = None;

    let area_resp = egui::Area::new(egui::Id::unique("rocketde_launchpad_context_menu"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            frame.show(ui, |ui| {
                ui.set_width(menu_w - 16.0);
                ui.spacing_mut().item_spacing.y = 3.0;

                ui.label(
                    RichText::new(format!("  {}", app.name))
                        .size(11.5)
                        .strong()
                        .color(Color32::from_rgb(155, 165, 185)),
                );
                ui.separator();

                if context_menu_row(ui, IconKind::OpenApp, "Abrir", accent) {
                    launch_app = Some(app.clone());
                    close_menu = true;
                }

                if is_pinned {
                    if context_menu_row(
                        ui,
                        IconKind::Trash,
                        "Remover dos Favoritos",
                        Color32::from_rgb(231, 76, 60),
                    ) {
                        toggle_pin = Some((app.clone(), true));
                        close_menu = true;
                    }
                } else if context_menu_row(
                    ui,
                    IconKind::Star,
                    "Adicionar aos Favoritos",
                    accent,
                ) {
                    toggle_pin = Some((app.clone(), false));
                    close_menu = true;
                }
            });
        });

    ctx.data_mut(|d| d.insert_temp(menu_rect_id, area_resp.response.rect));

    if let Some((target_app, was_pinned)) = toggle_pin {
        if was_pinned {
            state
                .config
                .remove_pinned_by_id(&target_app.app_id, &target_app.name);
        } else {
            state.config.add_pinned_app(
                &target_app.name,
                &target_app.app_id,
                &target_app.exec,
                &target_app.glyph,
            );
        }
    }

    if let Some(target_app) = launch_app {
        state.mark_app_launching(&target_app.app_id, &target_app.name);
        AppIndexer::launch(&target_app.exec);
        state.launcher_open = false;
    }

    if close_menu {
        state.launcher_context_menu = None;
    }
}

fn context_menu_row(
    ui: &mut egui::Ui,
    icon: IconKind,
    label: &str,
    hover_col: Color32,
) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), hover_col);
    }
    draw_icon(
        ui.painter(),
        Pos2::new(rect.min.x + 14.0, rect.center().y),
        13.0,
        icon,
        Color32::WHITE,
    );
    ui.painter().text(
        Pos2::new(rect.min.x + 28.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.5),
        Color32::WHITE,
    );
    resp.clicked()
}

fn close_launchpad_and_restore_focus(state: &mut ShellUiState) {
    state.launcher_open = false;
    state.launcher_context_menu = None;
    if let Ok(guard) = state.wm.state.lock() {
        if let Some(ref w) = guard.focused_window {
            if !w.id.is_empty() {
                PilaConnector::focus_window(&w.id);
            }
        }
    }
}

fn app_tile_color(name: &str) -> Color32 {
    let palette = [
        Color32::from_rgba_unmultiplied(48, 92, 148, 225),
        Color32::from_rgba_unmultiplied(44, 126, 92, 225),
        Color32::from_rgba_unmultiplied(108, 72, 144, 225),
        Color32::from_rgba_unmultiplied(166, 86, 48, 225),
        Color32::from_rgba_unmultiplied(42, 122, 118, 225),
        Color32::from_rgba_unmultiplied(154, 62, 62, 225),
        Color32::from_rgba_unmultiplied(62, 74, 96, 225),
    ];
    let hash: usize = name.bytes().map(|b| b as usize).sum();
    palette[hash % palette.len()]
}
