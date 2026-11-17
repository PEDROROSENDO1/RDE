use crate::system::apps::AppIndexer;
use crate::ui::icons::{IconKind, draw_icon};
use crate::ui::{ShellUiState, parse_hex_color};
use crate::wm::pila::PilaConnector;
use egui::{
    Color32, Context, CornerRadius, FontId, Frame, Margin, Pos2, Rect, RichText, Sense, Stroke,
    TextureHandle, Vec2,
};

#[derive(Clone)]
struct DockEntry {
    pinned_index: Option<usize>,
    app_id: String,
    name: String,
    exec: String,
    color: Color32,
    texture: Option<TextureHandle>,
    running_window_id: Option<String>,
    focused: bool,
    bounce_offset: f32,
}

/// Renders the bottom-centered macOS Dock (ONLY Favorites/Pinned Apps + "+" Add App Button),
/// with real XDG `.svg`/`.png` application icons, magnification effect, and right-click context menu to remove or launch!
pub fn render_dock(ctx: &Context, state: &mut ShellUiState) -> Option<Rect> {
    render_dock_internal(ctx, state, false, 1.0, 0.0)
}

pub fn render_dock_in_overview(
    ctx: &Context,
    state: &mut ShellUiState,
    anim_alpha: f32,
    slide_y: f32,
) -> Option<Rect> {
    render_dock_internal(ctx, state, true, anim_alpha, slide_y)
}

fn render_dock_internal(
    ctx: &Context,
    state: &mut ShellUiState,
    in_overview: bool,
    anim_alpha: f32,
    slide_y: f32,
) -> Option<Rect> {
    if !state.config.dock.enabled {
        return None;
    }

    state.apps.poll_decoded_icons(ctx);

    // Prune launching apps that have timed out (>12s)
    state
        .dock_launching_apps
        .retain(|(_, _, started)| started.elapsed().as_secs_f32() < 12.0);

    let wm_version = state.wm.state.lock().map(|g| g.version).unwrap_or(0);
    let pinned_len = state.config.dock.pinned_apps.len();
    let tex_gen = state.apps.texture_gen;
    let cache_key = (wm_version, pinned_len, tex_gen);
    let dock_cache_id = egui::Id::unique("rocketde_dock_entries_cache");

    let mut entries: Vec<DockEntry> = if let Some((cached_key, cached_entries)) =
        ctx.data(|d| d.get_temp::<((u64, usize, u64), Vec<DockEntry>)>(dock_cache_id))
    {
        if cached_key == cache_key {
            cached_entries
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    if entries.is_empty() && (pinned_len > 0 || wm_version > 0) {
        let wm_windows = state
            .wm
            .state
            .lock()
            .map(|g| g.windows.clone())
            .unwrap_or_default();
        entries.reserve(pinned_len);

        for idx in 0..pinned_len {
            let pinned = &state.config.dock.pinned_apps[idx];
            let pinned_app_lower = pinned.app_id.to_lowercase();
            let pinned_name_lower = pinned.name.to_lowercase();
            let matching_win = wm_windows.iter().find(|w| {
                (!pinned_app_lower.is_empty() && w.app_id_lower.contains(&pinned_app_lower))
                    || w.app_id_lower.contains(&pinned_name_lower)
                    || (!pinned_app_lower.is_empty() && w.title_lower.contains(&pinned_app_lower))
            });

            if matching_win.is_some() {
                state.dock_launching_apps.retain(|(a, n, _)| {
                    !(a == &pinned_app_lower && n == &pinned_name_lower)
                });
            }

            let tex = if !pinned.exec.starts_with('@') {
                state
                    .apps
                    .get_icon_texture(ctx, &pinned.app_id, None)
                    .or_else(|| state.apps.get_icon_texture(ctx, &pinned.name, None))
            } else {
                None
            };

            entries.push(DockEntry {
                pinned_index: Some(idx),
                app_id: pinned.app_id.clone(),
                name: pinned.name.clone(),
                exec: pinned.exec.clone(),
                color: parse_hex_color(&pinned.accent_hex, 230),
                texture: tex,
                running_window_id: matching_win.map(|w| w.id.clone()),
                focused: matching_win.map(|w| w.focused).unwrap_or(false),
                bounce_offset: 0.0,
            });
        }

        // Append running non-pinned apps (macOS Dock style after a subtle separator)
        let mut seen_running: Vec<String> = Vec::new();
        for w in &wm_windows {
            let is_pinned = state.config.dock.pinned_apps.iter().any(|p| {
                let pa = p.app_id.to_lowercase();
                let pn = p.name.to_lowercase();
                (!pa.is_empty() && (w.app_id_lower.contains(&pa) || w.title_lower.contains(&pa)))
                    || (!pn.is_empty() && w.app_id_lower.contains(&pn))
            });
            if is_pinned {
                continue;
            }
            let dedup_key = if !w.app_id_lower.is_empty() {
                w.app_id_lower.clone()
            } else {
                w.title_lower.clone()
            };
            if dedup_key.is_empty() || seen_running.contains(&dedup_key) {
                continue;
            }
            seen_running.push(dedup_key);

            let tex = state
                .apps
                .get_icon_texture(ctx, &w.app_id, None)
                .or_else(|| state.apps.get_icon_texture(ctx, &w.title, None));
            let display_name = if !w.app_id.is_empty() {
                w.app_id.clone()
            } else {
                w.title.chars().take(20).collect()
            };
            entries.push(DockEntry {
                pinned_index: None,
                app_id: w.app_id.clone(),
                name: display_name,
                exec: String::new(),
                color: Color32::from_rgba_unmultiplied(62, 74, 98, 225),
                texture: tex,
                running_window_id: Some(w.id.clone()),
                focused: w.focused,
                bounce_offset: 0.0,
            });
        }
        ctx.data_mut(|d| d.insert_temp(dock_cache_id, (cache_key, entries.clone())));
    }

    let mut any_bouncing = false;
    let base_size_for_bounce = state.config.dock.icon_size;
    if !state.dock_launching_apps.is_empty() {
        for entry in &mut entries {
            if entry.pinned_index.is_none() {
                continue;
            }
            let app_l = entry.app_id.to_lowercase();
            let name_l = entry.name.to_lowercase();
            if entry.running_window_id.is_some() {
                state
                    .dock_launching_apps
                    .retain(|(a, n, _)| !(a == &app_l && n == &name_l));
                entry.bounce_offset = 0.0;
                continue;
            }
            entry.bounce_offset = state
                .dock_launching_apps
                .iter()
                .find(|(a, n, _)| a == &app_l && n == &name_l)
                .map(|(_, _, started)| {
                    any_bouncing = true;
                    let cycle = started.elapsed().as_secs_f32() % 0.72;
                    let u = (cycle / 0.56).min(1.0);
                    let hop = 4.0 * u * (1.0 - u);
                    hop * (base_size_for_bounce * 0.44)
                })
                .unwrap_or(0.0);
        }
    }
    let sep_index = if entries.len() > pinned_len && pinned_len > 0 {
        Some(pinned_len)
    } else {
        None
    };
    let sep_extra_w = if sep_index.is_some() { 14.0 } else { 0.0 };

    if any_bouncing {
        ctx.request_repaint();
    }

    if state.dock_current_scales.len() != entries.len() {
        state.dock_current_scales.resize(entries.len(), 1.0);
    }

    let screen_rect = ctx.content_rect();
    let base_size = state.config.dock.icon_size;
    let max_mag = state.config.dock.magnification.max(1.0);
    let spacing = 10.0;
    let pointer_pos = ctx.input(|i| i.pointer.hover_pos());
    let dt = ctx.input(|i| i.stable_dt.clamp(0.001, 0.05));
    let scale_alpha = 1.0 - (-18.0 * dt).exp();

    let approx_total_w = entries.len() as f32 * (base_size + spacing) + 28.0;
    let dock_center_y =
        screen_rect.max.y - (state.config.dock.margin_bottom as f32) - base_size * 0.65;
    let start_x = screen_rect.center().x - approx_total_w * 0.5 + 14.0;

    let mut needs_repaint = false;
    for i in 0..entries.len() {
        let icon_center_x = start_x + i as f32 * (base_size + spacing) + base_size * 0.5;
        let target_scale = if let Some(p) = pointer_pos {
            let dy = (p.y - dock_center_y).abs();
            if dy < base_size * 1.45 {
                let dx = (p.x - icon_center_x).abs();
                let radius = base_size * 2.2;
                if dx < radius {
                    let factor = ((dx / radius) * std::f32::consts::FRAC_PI_2).cos();
                    1.0 + (max_mag - 1.0) * factor * factor
                } else {
                    1.0
                }
            } else {
                1.0
            }
        } else {
            1.0
        };

        let cur = state.dock_current_scales[i];
        let mut next = cur + (target_scale - cur) * scale_alpha;
        if (next - target_scale).abs() > 0.005 {
            needs_repaint = true;
        } else {
            next = target_scale;
        }
        state.dock_current_scales[i] = next;
    }

    if needs_repaint {
        ctx.request_repaint();
    }

    let want_visible = in_overview
        || !state.dock_obstructed
        || state.dock_edge_hovered
        || state.dock_context_menu.is_some()
        || !state.dock_launching_apps.is_empty();
    let target_reveal = if want_visible { 1.0 } else { 0.0 };
    let (eased_reveal, base_pill_rect, pill_rect) = if in_overview {
        let total_icons_w: f32 = (0..entries.len())
            .map(|i| base_size * state.dock_current_scales[i])
            .sum::<f32>()
            + spacing * (entries.len().saturating_sub(1) as f32)
            + sep_extra_w;
        let pill_w = total_icons_w + 28.0;
        let pill_h = base_size + 18.0;
        let base = Rect::from_min_size(
            Pos2::new(
                screen_rect.center().x - pill_w * 0.5,
                screen_rect.max.y - pill_h - (state.config.dock.margin_bottom as f32),
            ),
            Vec2::new(pill_w, pill_h),
        );
        (anim_alpha.clamp(0.0, 1.0), base, base.translate(Vec2::new(0.0, slide_y)))
    } else {
        let reveal_alpha = 1.0 - (-16.0 * dt).exp();
        let next_reveal =
            state.dock_reveal_t + (target_reveal - state.dock_reveal_t) * reveal_alpha;
        if (next_reveal - target_reveal).abs() > 0.005 {
            state.dock_reveal_t = next_reveal;
            ctx.request_repaint();
        } else {
            state.dock_reveal_t = target_reveal;
        }

        let total_icons_w: f32 = (0..entries.len())
            .map(|i| base_size * state.dock_current_scales[i])
            .sum::<f32>()
            + spacing * (entries.len().saturating_sub(1) as f32)
            + sep_extra_w;

        let pill_w = total_icons_w + 28.0;
        let pill_h = base_size + 18.0;
        let base = Rect::from_min_size(
            Pos2::new(
                screen_rect.center().x - pill_w * 0.5,
                screen_rect.max.y - pill_h - (state.config.dock.margin_bottom as f32),
            ),
            Vec2::new(pill_w, pill_h),
        );

        if state.dock_reveal_t <= 0.005 && !want_visible {
            return Some(base.expand(8.0));
        }

        let eased = 1.0 - (1.0 - state.dock_reveal_t.clamp(0.0, 1.0)).powi(3);
        let hide_offset = (1.0 - eased) * (pill_h + state.config.dock.margin_bottom as f32 + 12.0);
        (eased, base, base.translate(Vec2::new(0.0, hide_offset)))
    };
    let _ = (eased_reveal, base_pill_rect);

    let mut dock_bounds = pill_rect;
    let mut context_icon_center_x: Option<f32> = None;

    egui::Area::new(egui::Id::unique("rocketde_macos_dock_area"))
        .order(egui::Order::Foreground)
        .fixed_pos(Pos2::new(0.0, 0.0))
        .show(ctx, |ui| {
            let t = &state.config.theme;

            // Glassmorphism Dock Pill Background
            let dock_bg = Color32::from_rgba_unmultiplied(
                t.dock_bg_rgba[0],
                t.dock_bg_rgba[1],
                t.dock_bg_rgba[2],
                t.dock_bg_rgba[3],
            );
            let border_col = Color32::from_rgba_unmultiplied(
                t.border_rgba[0],
                t.border_rgba[1],
                t.border_rgba[2],
                58,
            );

            ui.painter().rect(
                pill_rect,
                CornerRadius::same(20),
                dock_bg,
                Stroke::new(1.2, border_col),
                egui::StrokeKind::Inside,
            );

            // Inner top highlight reflection line (macOS glass effect)
            ui.painter().line_segment(
                [
                    Pos2::new(pill_rect.min.x + 16.0, pill_rect.min.y + 1.0),
                    Pos2::new(pill_rect.max.x - 16.0, pill_rect.min.y + 1.0),
                ],
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 40)),
            );

            let mut cursor_x = pill_rect.min.x + 14.0;
            let baseline_y = pill_rect.max.y - 10.0;

            for (i, entry) in entries.iter().enumerate() {
                if sep_index == Some(i) {
                    let sep_x = cursor_x + 2.0;
                    ui.painter().line_segment(
                        [
                            Pos2::new(sep_x, pill_rect.min.y + 12.0),
                            Pos2::new(sep_x, pill_rect.max.y - 12.0),
                        ],
                        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 45)),
                    );
                    cursor_x += sep_extra_w;
                }
                let scale = state.dock_current_scales[i];
                let icon_w = base_size * scale;
                let icon_h = base_size * scale;
                let lift = (scale - 1.0) * 14.0;

                let icon_rect = Rect::from_min_size(
                    Pos2::new(cursor_x, baseline_y - icon_h - lift - entry.bounce_offset),
                    Vec2::new(icon_w, icon_h),
                );
                dock_bounds = dock_bounds.union(icon_rect);

                let resp = ui.allocate_rect(icon_rect, Sense::click());
                let painter = ui.painter();

                let corner = ((13.0 * scale).round() as u8).clamp(8, 24);

                if let Some(ref tex) = entry.texture {
                    // Subtle glass backing behind real application icon
                    painter.rect(
                        icon_rect,
                        CornerRadius::same(corner),
                        Color32::from_rgba_unmultiplied(255, 255, 255, 16),
                        Stroke::new(
                            if entry.focused { 1.6 } else { 0.8 },
                            if entry.focused {
                                Color32::from_rgb(10, 132, 255)
                            } else {
                                Color32::from_rgba_unmultiplied(255, 255, 255, 40)
                            },
                        ),
                        egui::StrokeKind::Inside,
                    );

                    // Draw real high-resolution app icon image!
                    let img_rect = icon_rect.shrink(3.0 * scale);
                    painter.image(
                        tex.id(),
                        img_rect,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                } else {
                    // Squircle Icon Background for Launchpad / Add button / fallback
                    let bg_col = if resp.hovered() {
                        brighten(entry.color, 25)
                    } else {
                        entry.color
                    };
                    painter.rect(
                        icon_rect,
                        CornerRadius::same(corner),
                        bg_col,
                        Stroke::new(
                            if entry.focused { 1.8 } else { 1.0 },
                            if entry.focused {
                                Color32::WHITE
                            } else {
                                Color32::from_rgba_unmultiplied(255, 255, 255, 65)
                            },
                        ),
                        egui::StrokeKind::Inside,
                    );

                    if entry.exec == "@launcher" {
                        draw_icon(
                            painter,
                            icon_rect.center(),
                            22.0 * scale,
                            IconKind::Rocket,
                            Color32::WHITE,
                        );
                    } else {
                        let initial = entry
                            .name
                            .chars()
                            .next()
                            .unwrap_or('A')
                            .to_uppercase()
                            .to_string();
                        painter.text(
                            icon_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            initial,
                            FontId::proportional(20.0 * scale),
                            Color32::WHITE,
                        );
                    }
                }

                // Running / Focused Dot Indicator under Icon
                let is_active_overlay =
                    entry.exec == "@launcher" && state.launcher_open;
                if entry.running_window_id.is_some() || is_active_overlay {
                    let dot_center = Pos2::new(icon_rect.center().x, pill_rect.max.y - 4.5);
                    let dot_col = if entry.focused || is_active_overlay {
                        Color32::from_rgb(10, 132, 255)
                    } else {
                        Color32::from_rgba_unmultiplied(235, 240, 255, 210)
                    };
                    let dot_r = if entry.focused { 3.2 } else { 2.4 };
                    painter.circle_filled(dot_center, dot_r, dot_col);
                }
                if state
                    .dock_context_menu
                    .is_some_and(|(idx, _)| entry.pinned_index == Some(idx))
                {
                    context_icon_center_x = Some(icon_rect.center().x);
                }

                // Tooltip Pill Above Hovered Dock Icon (macOS style)
                if resp.hovered() && state.dock_context_menu.is_none() {
                    let tip_center = Pos2::new(icon_rect.center().x, icon_rect.min.y - 16.0);
                    let tip_w = (entry.name.chars().count() as f32 * 7.5 + 22.0).max(68.0);
                    let tip_rect = Rect::from_center_size(tip_center, Vec2::new(tip_w, 22.0));
                    painter.rect(
                        tip_rect,
                        CornerRadius::same(7),
                        Color32::from_rgba_unmultiplied(20, 22, 32, 240),
                        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 50)),
                        egui::StrokeKind::Inside,
                    );
                    painter.text(
                        tip_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        &entry.name,
                        FontId::proportional(11.5),
                        Color32::WHITE,
                    );
                }

                if resp.clicked() {
                    state.dock_context_menu = None;
                    match entry.exec.as_str() {
                        "@launcher" => state.toggle_launcher(),
                        "@control-center" => {
                            if in_overview {
                                state.overview_closing = true;
                                state.control_center_open = true;
                                ctx.request_repaint();
                            } else {
                                state.control_center_open = !state.control_center_open;
                                state.launcher_open = false;
                            }
                        }
                        cmd => {
                            if let Some(ref wid) = entry.running_window_id {
                                PilaConnector::focus_window(wid);
                            } else {
                                state.mark_app_launching(&entry.app_id, &entry.name);
                                AppIndexer::launch(cmd);
                                ctx.request_repaint();
                            }
                            if in_overview {
                                state.overview_closing = true;
                                state.overview_drag_window_id = None;
                                ctx.request_repaint();
                            }
                        }
                    }
                } else if resp.secondary_clicked() {
                    if let Some(p_idx) = entry.pinned_index {
                        context_icon_center_x = Some(icon_rect.center().x);
                        state.dock_context_menu = Some((p_idx, icon_rect.center()));
                    }
                }

                cursor_x += icon_w + spacing;
            }
        });

    // Render Right-Click Dock Context Menu (Remove from Dock, New Window) anchored directly above the clicked icon.
    if let Some((p_idx, fallback_pos)) = state.dock_context_menu {
        if p_idx < state.config.dock.pinned_apps.len() {
            let pinned_item = state.config.dock.pinned_apps[p_idx].clone();
            let menu_w = 215.0;
            let half_w = menu_w * 0.5;
            let surface_w = ctx.content_rect().width();
            let anchor_x = context_icon_center_x
                .unwrap_or(fallback_pos.x)
                .clamp(half_w + 8.0, (surface_w - half_w - 8.0).max(half_w + 8.0));
            let anchor_y = (pill_rect.min.y - 10.0).max(20.0);
            let approx_rect = Rect::from_min_size(
                Pos2::new(anchor_x - half_w, (anchor_y - 130.0).max(0.0)),
                Vec2::new(menu_w, 130.0),
            );
            dock_bounds = dock_bounds.union(approx_rect);

            // Click outside closes Dock context menu
            if ctx.input(|i| i.pointer.any_pressed()) {
                if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                    if !approx_rect.contains(p) && !pill_rect.contains(p) {
                        state.dock_context_menu = None;
                    }
                }
            }

            let t = &state.config.theme;
            let accent = Color32::from_rgb(t.accent_rgb[0], t.accent_rgb[1], t.accent_rgb[2]);
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
                    Color32::from_rgba_unmultiplied(255, 255, 255, 50),
                ));

            egui::Area::new(egui::Id::new("rocketde_dock_context_menu"))
                .order(egui::Order::Tooltip)
                .pivot(egui::Align2::CENTER_BOTTOM)
                .fixed_pos(Pos2::new(anchor_x, anchor_y))
                .show(ctx, |ui| {
                    frame.show(ui, |ui| {
                        ui.set_width(menu_w - 16.0);
                        ui.label(
                            RichText::new(format!("  {}", pinned_item.name))
                                .size(12.0)
                                .strong()
                                .color(Color32::from_rgb(165, 175, 195)),
                        );
                        ui.separator();

                        if !pinned_item.exec.starts_with('@') {
                            if dock_menu_row(
                                ui,
                                Some(IconKind::OpenApp),
                                "Abrir Nova Instância",
                                accent,
                            ) {
                                AppIndexer::launch(&pinned_item.exec);
                                state.dock_context_menu = None;
                            }
                        }

                        if pinned_item.exec != "@launcher" {
                            ui.separator();
                            if dock_menu_row(
                                ui,
                                Some(IconKind::Trash),
                                "Remover dos Favoritos",
                                Color32::from_rgb(231, 76, 60),
                            ) {
                                state.config.remove_pinned_app_at(p_idx);
                                state.dock_context_menu = None;
                            }
                        }
                    });
                });
        } else {
            state.dock_context_menu = None;
        }
    }

    Some(dock_bounds.expand(8.0))
}

fn dock_menu_row(
    ui: &mut egui::Ui,
    icon: Option<IconKind>,
    label: &str,
    hover_col: Color32,
) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), hover_col);
    }
    let text_x = if let Some(kind) = icon {
        draw_icon(
            ui.painter(),
            Pos2::new(rect.min.x + 14.0, rect.center().y),
            13.0,
            kind,
            Color32::WHITE,
        );
        rect.min.x + 28.0
    } else {
        rect.min.x + 8.0
    };
    ui.painter().text(
        Pos2::new(text_x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.5),
        Color32::WHITE,
    );
    resp.clicked()
}

fn brighten(c: Color32, delta: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(
        c.r().saturating_add(delta),
        c.g().saturating_add(delta),
        c.b().saturating_add(delta),
        c.a(),
    )
}
