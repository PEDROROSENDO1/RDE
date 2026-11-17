use crate::ui::icons::{IconKind, draw_icon};
use crate::ui::{
    CaptureDragHandle, CaptureMode, CaptureTarget, PendingCaptureAction, ShellUiState,
};
use egui::{
    Color32, Context, CornerRadius, Frame, Key, Margin, Pos2, Rect, RichText, Sense, Stroke, Vec2,
    pos2, vec2,
};
use std::time::{Duration, Instant};

const MIN_SEL_SIZE: f32 = 24.0;
const HANDLE_HIT_R: f32 = 10.0;

pub fn render_capture_overlay(ctx: &Context, state: &mut ShellUiState) {
    if !state.capture_overlay_open {
        return;
    }

    let screen = ctx.content_rect();
    let screen_w = screen.width().max(640.0);
    let screen_h = screen.height().max(400.0);

    // Cancel with Escape
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        state.capture_overlay_open = false;
        state.capture_countdown_until = None;
        state.capture_bg_texture = None;
        restore_wm_focus(state);
        return;
    }

    // Countdown active (delayed screenshot)
    if let Some(deadline) = state.capture_countdown_until {
        let now = Instant::now();
        if now >= deadline {
            state.capture_countdown_until = None;
            state.capture_overlay_open = false;
            state.capture_bg_texture = None;
            let sel = state
                .capture_selection_rect
                .unwrap_or_else(|| default_selection_rect(screen));
            state.pending_capture_action = Some(PendingCaptureAction::ExecuteScreenshot {
                target: state.capture_target,
                logical_rect: sel,
                screen_size: screen.size(),
                include_cursor: state.capture_include_cursor,
                copy_clipboard: state.capture_copy_clipboard,
                save_disk: state.capture_save_disk,
                window_id: state.capture_selected_window_id.clone(),
            });
            return;
        }

        let rem = (deadline - now).as_secs_f32().ceil() as u32;
        egui::Area::new(egui::Id::unique("rocketde_capture_countdown"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(Color32::from_rgba_unmultiplied(20, 22, 28, 225))
                    .corner_radius(CornerRadius::same(28))
                    .inner_margin(Margin::symmetric(28, 18))
                    .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 50)))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!("{rem}"))
                                .size(44.0)
                                .strong()
                                .color(Color32::WHITE),
                        );
                    });
            });
        ctx.request_repaint_after(Duration::from_millis(80));
        return;
    }

    let sel_rect = *state
        .capture_selection_rect
        .get_or_insert_with(|| default_selection_rect(screen));

    // Reserve toolbar area at bottom center so pointer drags don't fight with buttons
    let toolbar_est = Rect::from_center_size(
        pos2(screen.center().x, screen_h - 68.0),
        vec2((screen_w - 40.0).min(780.0), 84.0),
    );

    // Background frozen frame + selection mask on Background layer
    egui::Area::new(egui::Id::unique("rocketde_capture_backdrop"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (_rect, resp) = ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
            let painter = ui.painter();

            if let Some(ref tex) = state.capture_bg_texture {
                painter.image(
                    tex.id(),
                    screen,
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }

            let mask_color = Color32::from_rgba_unmultiplied(0, 0, 0, 125);
            match state.capture_target {
                CaptureTarget::Selection => {
                    draw_selection_mask(&painter, screen, sel_rect, mask_color);
                    draw_selection_chrome(&painter, sel_rect);
                }
                CaptureTarget::Screen => {
                    painter.rect_stroke(
                        screen.shrink(2.0),
                        CornerRadius::ZERO,
                        Stroke::new(3.0, Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                        egui::StrokeKind::Inside,
                    );
                }
                CaptureTarget::Window => {
                    painter.rect_filled(screen, CornerRadius::ZERO, mask_color);
                }
            }

            // Handle selection drag/resize when in Selection mode and outside toolbar
            if state.capture_target == CaptureTarget::Selection {
                handle_selection_pointer(ctx, state, &resp, screen, sel_rect, toolbar_est);
            }
        });

    // Window picker strip when Target == Window
    if state.capture_target == CaptureTarget::Window {
        render_window_picker(ctx, state, screen_w, screen_h);
    }

    // Bottom Floating Toolbar (GNOME style pill)
    let t = &state.config.theme;
    let accent = Color32::from_rgb(t.accent_rgb[0], t.accent_rgb[1], t.accent_rgb[2]);
    let pill_frame = Frame::NONE
        .fill(Color32::from_rgba_unmultiplied(24, 26, 32, 242))
        .corner_radius(CornerRadius::same(18))
        .inner_margin(Margin::symmetric(14, 10))
        .stroke(Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 45),
        ));

    let mut trigger_action = false;
    let mut close_requested = false;

    egui::Area::new(egui::Id::unique("rocketde_capture_toolbar"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_BOTTOM, vec2(0.0, -24.0))
        .show(ctx, |ui| {
            pill_frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;

                    // Target selector
                    if seg_button(
                        ui,
                        "Seleção",
                        state.capture_target == CaptureTarget::Selection,
                        accent,
                    )
                    .clicked()
                    {
                        state.capture_target = CaptureTarget::Selection;
                    }
                    if seg_button(
                        ui,
                        "Tela",
                        state.capture_target == CaptureTarget::Screen,
                        accent,
                    )
                    .clicked()
                    {
                        state.capture_target = CaptureTarget::Screen;
                    }
                    if state.capture_mode == CaptureMode::Screenshot {
                        if seg_button(
                            ui,
                            "Janela",
                            state.capture_target == CaptureTarget::Window,
                            accent,
                        )
                        .clicked()
                        {
                            state.capture_target = CaptureTarget::Window;
                        }
                    }

                    toolbar_divider(ui);

                    // Mode segmented toggle: Print vs Gravar
                    if icon_seg_button(
                        ui,
                        IconKind::Camera,
                        "Print",
                        state.capture_mode == CaptureMode::Screenshot,
                        accent,
                    ) {
                        state.capture_mode = CaptureMode::Screenshot;
                    }
                    if icon_seg_button(
                        ui,
                        IconKind::VideoRecord,
                        "Gravar",
                        state.capture_mode == CaptureMode::Record,
                        Color32::from_rgb(220, 58, 58),
                    ) {
                        state.capture_mode = CaptureMode::Record;
                        if state.capture_target == CaptureTarget::Window {
                            state.capture_target = CaptureTarget::Selection;
                        }
                    }

                    toolbar_divider(ui);

                    // Cursor toggle
                    if mini_toggle(
                        ui,
                        IconKind::Cursor,
                        "Cursor",
                        state.capture_include_cursor,
                        accent,
                    )
                    .on_hover_text("Incluir ponteiro do mouse")
                    .clicked()
                    {
                        state.capture_include_cursor = !state.capture_include_cursor;
                    }

                    if state.capture_mode == CaptureMode::Screenshot {
                        // Timer cycle (0s -> 3s -> 5s)
                        let timer_label = if state.capture_timer_secs == 0 {
                            "0s".to_string()
                        } else {
                            format!("{}s", state.capture_timer_secs)
                        };
                        if seg_button(ui, &timer_label, state.capture_timer_secs > 0, accent)
                            .on_hover_text("Atraso antes de capturar (0s / 3s / 5s)")
                            .clicked()
                        {
                            state.capture_timer_secs = match state.capture_timer_secs {
                                0 => 3,
                                3 => 5,
                                _ => 0,
                            };
                        }

                        if seg_button(ui, "Copiar", state.capture_copy_clipboard, accent)
                            .on_hover_text("Copiar PNG para a área de transferência")
                            .clicked()
                        {
                            state.capture_copy_clipboard = !state.capture_copy_clipboard;
                        }

                        if seg_button(ui, "Salvar", state.capture_save_disk, accent)
                            .on_hover_text("Salvar arquivo PNG em $XDG_PICTURES_DIR/Screenshots")
                            .clicked()
                        {
                            state.capture_save_disk = !state.capture_save_disk;
                        }
                    } else {
                        // Recording options: System audio, Mic, FPS
                        if mini_toggle(
                            ui,
                            IconKind::VolumeHigh,
                            "Som",
                            state.capture_system_audio,
                            accent,
                        )
                        .on_hover_text("Gravar áudio do sistema")
                        .clicked()
                        {
                            state.capture_system_audio = !state.capture_system_audio;
                        }

                        if mini_toggle(ui, IconKind::Mic, "Mic", state.capture_mic_audio, accent)
                            .on_hover_text("Gravar microfone")
                            .clicked()
                        {
                            state.capture_mic_audio = !state.capture_mic_audio;
                        }

                        let fps_label = format!("{}fps", state.capture_fps);
                        if seg_button(ui, &fps_label, state.capture_fps == 60, accent)
                            .on_hover_text("Alternar entre 30 fps e 60 fps")
                            .clicked()
                        {
                            state.capture_fps = if state.capture_fps == 30 { 60 } else { 30 };
                        }
                    }

                    toolbar_divider(ui);

                    // Primary Capture / Record button
                    let (btn_text, btn_bg) = match state.capture_mode {
                        CaptureMode::Screenshot => ("Capturar", accent),
                        CaptureMode::Record => ("● Gravar", Color32::from_rgb(220, 48, 48)),
                    };
                    let primary_btn = egui::Button::new(
                        RichText::new(btn_text).size(12.5).strong().color(Color32::WHITE),
                    )
                    .fill(btn_bg)
                    .corner_radius(CornerRadius::same(12))
                    .min_size(vec2(88.0, 30.0));
                    if ui.add(primary_btn).clicked() {
                        trigger_action = true;
                    }

                    // Close button
                    let (close_r, close_resp) =
                        ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
                    ui.painter().rect_filled(
                        close_r,
                        CornerRadius::same(14),
                        if close_resp.hovered() {
                            Color32::from_rgba_unmultiplied(255, 255, 255, 38)
                        } else {
                            Color32::from_rgba_unmultiplied(255, 255, 255, 18)
                        },
                    );
                    draw_icon(
                        ui.painter(),
                        close_r.center(),
                        11.0,
                        IconKind::Close,
                        Color32::WHITE,
                    );
                    if close_resp.on_hover_text("Cancelar (Esc)").clicked() {
                        close_requested = true;
                    }
                });
            });
        });

    if ctx.input(|i| i.key_pressed(Key::Enter)) {
        trigger_action = true;
    }

    if close_requested {
        state.capture_overlay_open = false;
        state.capture_bg_texture = None;
        restore_wm_focus(state);
        return;
    }

    if trigger_action {
        let active_rect = state
            .capture_selection_rect
            .unwrap_or_else(|| default_selection_rect(screen));

        match state.capture_mode {
            CaptureMode::Screenshot => {
                if state.capture_timer_secs > 0 {
                    state.capture_countdown_until =
                        Some(Instant::now() + Duration::from_secs(state.capture_timer_secs as u64));
                } else {
                    state.capture_overlay_open = false;
                    state.capture_bg_texture = None;
                    state.pending_capture_action = Some(PendingCaptureAction::ExecuteScreenshot {
                        target: state.capture_target,
                        logical_rect: active_rect,
                        screen_size: screen.size(),
                        include_cursor: state.capture_include_cursor,
                        copy_clipboard: state.capture_copy_clipboard,
                        save_disk: state.capture_save_disk,
                        window_id: state.capture_selected_window_id.clone(),
                    });
                }
            }
            CaptureMode::Record => {
                state.capture_overlay_open = false;
                state.capture_bg_texture = None;
                state.pending_capture_action = Some(PendingCaptureAction::StartRecording {
                    target: state.capture_target,
                    logical_rect: active_rect,
                    screen_size: screen.size(),
                    include_cursor: state.capture_include_cursor,
                    fps: state.capture_fps,
                    system_audio: state.capture_system_audio,
                    mic_audio: state.capture_mic_audio,
                });
            }
        }
    }
}

pub fn render_capture_toast(ctx: &Context, state: &mut ShellUiState) {
    let Some((ref msg, ts)) = state.capture_toast else {
        return;
    };
    if ts.elapsed() > Duration::from_secs(3) {
        state.capture_toast = None;
        return;
    }
    let msg_clone = msg.clone();
    egui::Area::new(egui::Id::unique("rocketde_capture_toast"))
        .order(egui::Order::Tooltip)
        .anchor(egui::Align2::CENTER_TOP, vec2(0.0, 42.0))
        .show(ctx, |ui| {
            Frame::NONE
                .fill(Color32::from_rgba_unmultiplied(24, 26, 32, 235))
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::symmetric(16, 8))
                .stroke(Stroke::new(
                    1.0,
                    Color32::from_rgba_unmultiplied(255, 255, 255, 45),
                ))
                .show(ui, |ui| {
                    ui.label(RichText::new(msg_clone).size(12.5).color(Color32::WHITE));
                });
        });
    ctx.request_repaint_after(Duration::from_millis(200));
}

fn default_selection_rect(screen: Rect) -> Rect {
    let w = (screen.width() * 0.5).max(240.0);
    let h = (screen.height() * 0.5).max(160.0);
    Rect::from_center_size(screen.center(), vec2(w, h))
}

fn draw_selection_mask(painter: &egui::Painter, screen: Rect, sel: Rect, mask: Color32) {
    // Top
    painter.rect_filled(
        Rect::from_min_max(screen.min, pos2(screen.max.x, sel.min.y)),
        CornerRadius::ZERO,
        mask,
    );
    // Bottom
    painter.rect_filled(
        Rect::from_min_max(pos2(screen.min.x, sel.max.y), screen.max),
        CornerRadius::ZERO,
        mask,
    );
    // Left
    painter.rect_filled(
        Rect::from_min_max(pos2(screen.min.x, sel.min.y), pos2(sel.min.x, sel.max.y)),
        CornerRadius::ZERO,
        mask,
    );
    // Right
    painter.rect_filled(
        Rect::from_min_max(pos2(sel.max.x, sel.min.y), pos2(screen.max.x, sel.max.y)),
        CornerRadius::ZERO,
        mask,
    );
}

fn draw_selection_chrome(painter: &egui::Painter, sel: Rect) {
    painter.rect_stroke(
        sel,
        CornerRadius::ZERO,
        Stroke::new(1.5, Color32::WHITE),
        egui::StrokeKind::Outside,
    );

    let handles = [
        sel.left_top(),
        sel.right_top(),
        sel.left_bottom(),
        sel.right_bottom(),
        pos2(sel.center().x, sel.top()),
        pos2(sel.center().x, sel.bottom()),
        pos2(sel.left(), sel.center().y),
        pos2(sel.right(), sel.center().y),
    ];
    for p in handles {
        painter.circle_filled(p, 4.5, Color32::WHITE);
        painter.circle_stroke(p, 4.5, Stroke::new(1.0, Color32::from_rgb(30, 32, 38)));
    }

    // Dimensions pill above/inside top-left of selection
    let label = format!("{} × {}", sel.width().round() as i32, sel.height().round() as i32);
    let badge_pos = pos2(sel.left(), (sel.top() - 24.0).max(4.0));
    let badge_rect = Rect::from_min_size(badge_pos, vec2(84.0, 20.0));
    painter.rect_filled(
        badge_rect,
        CornerRadius::same(6),
        Color32::from_rgba_unmultiplied(20, 22, 28, 215),
    );
    painter.text(
        badge_rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(11.0),
        Color32::WHITE,
    );
}

fn hit_test_handle(pos: Pos2, sel: Rect) -> CaptureDragHandle {
    let near = |a: Pos2| a.distance(pos) <= HANDLE_HIT_R;
    if near(sel.left_top()) {
        return CaptureDragHandle::TopLeft;
    }
    if near(sel.right_top()) {
        return CaptureDragHandle::TopRight;
    }
    if near(sel.left_bottom()) {
        return CaptureDragHandle::BottomLeft;
    }
    if near(sel.right_bottom()) {
        return CaptureDragHandle::BottomRight;
    }
    if (pos.x - sel.left()).abs() <= HANDLE_HIT_R && pos.y >= sel.top() && pos.y <= sel.bottom() {
        return CaptureDragHandle::Left;
    }
    if (pos.x - sel.right()).abs() <= HANDLE_HIT_R && pos.y >= sel.top() && pos.y <= sel.bottom() {
        return CaptureDragHandle::Right;
    }
    if (pos.y - sel.top()).abs() <= HANDLE_HIT_R && pos.x >= sel.left() && pos.x <= sel.right() {
        return CaptureDragHandle::Top;
    }
    if (pos.y - sel.bottom()).abs() <= HANDLE_HIT_R && pos.x >= sel.left() && pos.x <= sel.right()
    {
        return CaptureDragHandle::Bottom;
    }
    if sel.contains(pos) {
        return CaptureDragHandle::Move;
    }
    CaptureDragHandle::NewRect
}

fn handle_selection_pointer(
    ctx: &Context,
    state: &mut ShellUiState,
    resp: &egui::Response,
    screen: Rect,
    sel: Rect,
    toolbar_est: Rect,
) {
    if resp.drag_started() {
        if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
            if !toolbar_est.contains(pos) {
                state.capture_drag_handle = hit_test_handle(pos, sel);
                state.capture_drag_start = Some(pos);
                state.capture_drag_orig_rect = Some(sel);
            }
        }
    }

    if resp.dragged() && state.capture_drag_handle != CaptureDragHandle::None {
        if let (Some(start), Some(cur), Some(orig)) = (
            state.capture_drag_start,
            ctx.input(|i| i.pointer.interact_pos()),
            state.capture_drag_orig_rect,
        ) {
            let delta = cur - start;
            let mut next = orig;
            match state.capture_drag_handle {
                CaptureDragHandle::Move => {
                    next = orig.translate(delta);
                    let dx = (screen.min.x - next.min.x)
                        .max(0.0)
                        .min(screen.max.x - next.max.x);
                    if next.min.x < screen.min.x {
                        next = next.translate(vec2(screen.min.x - next.min.x, 0.0));
                    }
                    if next.max.x > screen.max.x {
                        next = next.translate(vec2(screen.max.x - next.max.x, 0.0));
                    }
                    if next.min.y < screen.min.y {
                        next = next.translate(vec2(0.0, screen.min.y - next.min.y));
                    }
                    if next.max.y > screen.max.y {
                        next = next.translate(vec2(0.0, screen.max.y - next.max.y));
                    }
                    let _ = dx;
                }
                CaptureDragHandle::NewRect => {
                    next = Rect::from_two_pos(start, cur).intersect(screen);
                    if next.width() < MIN_SEL_SIZE {
                        next.max.x = (next.min.x + MIN_SEL_SIZE).min(screen.max.x);
                    }
                    if next.height() < MIN_SEL_SIZE {
                        next.max.y = (next.min.y + MIN_SEL_SIZE).min(screen.max.y);
                    }
                }
                CaptureDragHandle::TopLeft => {
                    next.min.x = (orig.min.x + delta.x).clamp(screen.min.x, orig.max.x - MIN_SEL_SIZE);
                    next.min.y = (orig.min.y + delta.y).clamp(screen.min.y, orig.max.y - MIN_SEL_SIZE);
                }
                CaptureDragHandle::TopRight => {
                    next.max.x = (orig.max.x + delta.x).clamp(orig.min.x + MIN_SEL_SIZE, screen.max.x);
                    next.min.y = (orig.min.y + delta.y).clamp(screen.min.y, orig.max.y - MIN_SEL_SIZE);
                }
                CaptureDragHandle::BottomLeft => {
                    next.min.x = (orig.min.x + delta.x).clamp(screen.min.x, orig.max.x - MIN_SEL_SIZE);
                    next.max.y = (orig.max.y + delta.y).clamp(orig.min.y + MIN_SEL_SIZE, screen.max.y);
                }
                CaptureDragHandle::BottomRight => {
                    next.max.x = (orig.max.x + delta.x).clamp(orig.min.x + MIN_SEL_SIZE, screen.max.x);
                    next.max.y = (orig.max.y + delta.y).clamp(orig.min.y + MIN_SEL_SIZE, screen.max.y);
                }
                CaptureDragHandle::Top => {
                    next.min.y = (orig.min.y + delta.y).clamp(screen.min.y, orig.max.y - MIN_SEL_SIZE);
                }
                CaptureDragHandle::Bottom => {
                    next.max.y = (orig.max.y + delta.y).clamp(orig.min.y + MIN_SEL_SIZE, screen.max.y);
                }
                CaptureDragHandle::Left => {
                    next.min.x = (orig.min.x + delta.x).clamp(screen.min.x, orig.max.x - MIN_SEL_SIZE);
                }
                CaptureDragHandle::Right => {
                    next.max.x = (orig.max.x + delta.x).clamp(orig.min.x + MIN_SEL_SIZE, screen.max.x);
                }
                CaptureDragHandle::None => {}
            }
            state.capture_selection_rect = Some(next);
        }
    }

    if resp.drag_stopped() {
        state.capture_drag_handle = CaptureDragHandle::None;
        state.capture_drag_start = None;
        state.capture_drag_orig_rect = None;
    }
}

fn render_window_picker(ctx: &Context, state: &mut ShellUiState, screen_w: f32, screen_h: f32) {
    let windows: Vec<(String, String, String)> = state
        .wm
        .state
        .lock()
        .map(|g| {
            g.windows
                .iter()
                .filter(|w| !w.app_id.to_lowercase().contains("rocketde"))
                .map(|w| (w.id.clone(), w.app_id.clone(), w.title.clone()))
                .collect()
        })
        .unwrap_or_default();

    if state.capture_selected_window_id.is_none() {
        if let Some((first_id, _, _)) = windows.first() {
            state.capture_selected_window_id = Some(first_id.clone());
        }
    }

    let accent = Color32::from_rgb(
        state.config.theme.accent_rgb[0],
        state.config.theme.accent_rgb[1],
        state.config.theme.accent_rgb[2],
    );

    egui::Area::new(egui::Id::unique("rocketde_capture_win_picker"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, -24.0))
        .show(ctx, |ui| {
            Frame::NONE
                .fill(Color32::from_rgba_unmultiplied(24, 26, 32, 238))
                .corner_radius(CornerRadius::same(16))
                .inner_margin(Margin::same(14))
                .stroke(Stroke::new(
                    1.0,
                    Color32::from_rgba_unmultiplied(255, 255, 255, 45),
                ))
                .show(ui, |ui| {
                    ui.set_max_width((screen_w - 80.0).min(540.0));
                    ui.set_max_height((screen_h - 200.0).max(140.0));
                    ui.label(
                        RichText::new("Escolha a janela para capturar")
                            .size(13.5)
                            .strong()
                            .color(Color32::WHITE),
                    );
                    ui.add_space(6.0);
                    if windows.is_empty() {
                        ui.label(
                            RichText::new("Nenhuma janela aberta detectada.")
                                .size(12.0)
                                .color(Color32::from_rgb(170, 178, 195)),
                        );
                    } else {
                        egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                            for (id, app_id, title) in &windows {
                                let selected =
                                    state.capture_selected_window_id.as_deref() == Some(id.as_str());
                                let label = if title.is_empty() {
                                    app_id.clone()
                                } else {
                                    format!("{app_id} — {title}")
                                };
                                let btn = egui::Button::new(
                                    RichText::new(label).size(12.0).color(Color32::WHITE),
                                )
                                .fill(if selected {
                                    accent
                                } else {
                                    Color32::from_rgba_unmultiplied(255, 255, 255, 16)
                                })
                                .corner_radius(CornerRadius::same(10))
                                .min_size(vec2(380.0, 28.0));
                                if ui.add(btn).clicked() {
                                    state.capture_selected_window_id = Some(id.clone());
                                }
                            }
                        });
                    }
                });
        });
}

fn seg_button(ui: &mut egui::Ui, label: &str, active: bool, accent: Color32) -> egui::Response {
    let fill = if active {
        accent
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 18)
    };
    ui.add(
        egui::Button::new(RichText::new(label).size(12.0).color(Color32::WHITE))
            .fill(fill)
            .corner_radius(CornerRadius::same(10))
            .min_size(vec2(0.0, 28.0)),
    )
}

fn icon_seg_button(
    ui: &mut egui::Ui,
    icon: IconKind,
    label: &str,
    active: bool,
    accent: Color32,
) -> bool {
    let fill = if active {
        accent
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 18)
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(72.0, 28.0), Sense::click());
    ui.painter().rect_filled(rect, CornerRadius::same(10), fill);
    draw_icon(
        ui.painter(),
        pos2(rect.min.x + 15.0, rect.center().y),
        13.0,
        icon,
        Color32::WHITE,
    );
    ui.painter().text(
        pos2(rect.min.x + 28.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(12.0),
        Color32::WHITE,
    );
    resp.clicked()
}

fn mini_toggle(
    ui: &mut egui::Ui,
    icon: IconKind,
    _label: &str,
    active: bool,
    accent: Color32,
) -> egui::Response {
    let fill = if active {
        accent
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 18)
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
    ui.painter().rect_filled(rect, CornerRadius::same(10), fill);
    draw_icon(ui.painter(), rect.center(), 13.5, icon, Color32::WHITE);
    resp
}

fn toolbar_divider(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(vec2(4.0, 20.0), Sense::hover());
    ui.painter().line_segment(
        [pos2(r.center().x, r.min.y), pos2(r.center().x, r.max.y)],
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 40)),
    );
}

fn restore_wm_focus(state: &ShellUiState) {
    if let Ok(guard) = state.wm.state.lock() {
        if let Some(ref w) = guard.focused_window {
            if !w.id.is_empty() {
                crate::wm::pila::PilaConnector::focus_window(&w.id);
            }
        }
    }
}
