use egui::{Color32, Pos2, Rect, Sense, Stroke, Ui, Vec2, pos2};
use rocketde_core::design::ThemeConfig;

/// Window-level edge for client-side resizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeDir {
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Window-level action requested by the unified HeaderBar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    None,
    Drag,
    Resize(ResizeDir),
    Minimize,
    ToggleMaximize,
    Close,
}

/// Detects pointer hover/press along the window border (`6.0px` margin),
/// updates the resize cursor icon, and returns `Some(ResizeDir)` when a
/// primary-button resize drag starts.
pub fn detect_window_resize(ctx: &egui::Context, is_maximized: bool) -> Option<ResizeDir> {
    if is_maximized {
        return None;
    }
    let rect = ctx.content_rect();
    let pos = ctx.input(|i| i.pointer.hover_pos().or_else(|| i.pointer.interact_pos()))?;
    let margin = 4.0;
    let left = (pos.x - rect.min.x) <= margin;
    let right = (rect.max.x - pos.x) <= margin;
    let top = (pos.y - rect.min.y) <= margin;
    let bottom = (rect.max.y - pos.y) <= margin;

    let (dir, cursor) = match (left, right, top, bottom) {
        (true, false, true, false) => (ResizeDir::TopLeft, egui::CursorIcon::ResizeNwSe),
        (false, true, false, true) => (ResizeDir::BottomRight, egui::CursorIcon::ResizeNwSe),
        (false, true, true, false) => (ResizeDir::TopRight, egui::CursorIcon::ResizeNeSw),
        (true, false, false, true) => (ResizeDir::BottomLeft, egui::CursorIcon::ResizeNeSw),
        (true, false, false, false) => (ResizeDir::Left, egui::CursorIcon::ResizeHorizontal),
        (false, true, false, false) => (ResizeDir::Right, egui::CursorIcon::ResizeHorizontal),
        (false, false, true, false) => (ResizeDir::Top, egui::CursorIcon::ResizeVertical),
        (false, false, false, true) => (ResizeDir::Bottom, egui::CursorIcon::ResizeVertical),
        _ => return None,
    };

    ctx.set_cursor_icon(cursor);
    if ctx.input(|i| i.pointer.primary_pressed()) {
        Some(dir)
    } else {
        None
    }
}

/// Interprets a `click_and_drag` response on a header-bar region as a
/// `WindowAction` (`ToggleMaximize` on double-click, `Drag` on primary drag).
/// Only triggers when `resp.hovered()` is true so child buttons/inputs in the
/// header bar never accidentally start a window move.
pub fn header_bar_drag_from_response(resp: &egui::Response, ctx: &egui::Context) -> WindowAction {
    if !resp.hovered() {
        return WindowAction::None;
    }
    if resp.double_clicked_by(egui::PointerButton::Primary) {
        WindowAction::ToggleMaximize
    } else if resp.drag_started_by(egui::PointerButton::Primary)
        || (resp.is_pointer_button_down_on()
            && ctx.input(|i| i.pointer.primary_down() && i.pointer.delta() != Vec2::ZERO))
    {
        WindowAction::Drag
    } else {
        WindowAction::None
    }
}

/// Registers drag & double-click-to-maximize on the empty background of a HeaderBar.
/// Call this at the start of the top panel before adding interactive child widgets.
pub fn header_bar_drag_area(ui: &mut Ui, bar_rect: Rect) -> WindowAction {
    let resp = ui.interact(
        bar_rect,
        ui.scope_id().with("rocketde_header_bar_drag"),
        Sense::click_and_drag(),
    );
    header_bar_drag_from_response(&resp, ui.ctx())
}

/// Draws the 3 circular window control buttons on the RIGHT side of the HeaderBar
/// in order: Minimize, Maximize, Close (left-to-right).
pub fn window_controls_right(
    ui: &mut Ui,
    t: &ThemeConfig,
    is_maximized: bool,
) -> WindowAction {
    let mut action = WindowAction::None;

    // Fixed-width left-to-right group so order is ALWAYS: Yellow (Min), Green (Max), Red (Close).
    ui.allocate_ui_with_layout(
        Vec2::new(72.0, 24.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = t.spacing.xs.max(4.0);

            if traffic_dot(
                ui,
                "win_min",
                Color32::from_rgb(
                    t.surface.warning_rgb[0],
                    t.surface.warning_rgb[1],
                    t.surface.warning_rgb[2],
                ),
                DotKind::Minimize,
            ) {
                action = WindowAction::Minimize;
            }
            if traffic_dot(
                ui,
                "win_max",
                Color32::from_rgb(
                    t.surface.success_rgb[0],
                    t.surface.success_rgb[1],
                    t.surface.success_rgb[2],
                ),
                if is_maximized {
                    DotKind::Restore
                } else {
                    DotKind::Maximize
                },
            ) {
                action = WindowAction::ToggleMaximize;
            }
            if traffic_dot(
                ui,
                "win_close",
                Color32::from_rgb(
                    t.surface.danger_rgb[0],
                    t.surface.danger_rgb[1],
                    t.surface.danger_rgb[2],
                ),
                DotKind::Close,
            ) {
                action = WindowAction::Close;
            }
        },
    );

    action
}

#[derive(Clone, Copy)]
enum DotKind {
    Minimize,
    Maximize,
    Restore,
    Close,
}

fn traffic_dot(ui: &mut Ui, _id_salt: &str, base_col: Color32, kind: DotKind) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::click());
    let hovered = resp.hovered();
    let radius = if hovered { 7.0 } else { 6.5 };
    let center = rect.center();

    let fill = if resp.is_pointer_button_down_on() {
        base_col.linear_multiply(0.8)
    } else if hovered {
        base_col
    } else {
        base_col.gamma_multiply(0.92)
    };

    let painter = ui.painter();
    painter.circle_filled(center, radius, fill);
    painter.circle_stroke(
        center,
        radius,
        Stroke::new(0.8, Color32::from_rgba_unmultiplied(0, 0, 0, 55)),
    );

    if hovered {
        let glyph_col = Color32::from_rgba_unmultiplied(20, 20, 24, 210);
        let stroke = Stroke::new(1.3, glyph_col);
        match kind {
            DotKind::Close => {
                let d = 2.6;
                painter.line_segment(
                    [pos2(center.x - d, center.y - d), pos2(center.x + d, center.y + d)],
                    stroke,
                );
                painter.line_segment(
                    [pos2(center.x + d, center.y - d), pos2(center.x - d, center.y + d)],
                    stroke,
                );
            }
            DotKind::Minimize => {
                painter.line_segment(
                    [pos2(center.x - 3.0, center.y), pos2(center.x + 3.0, center.y)],
                    stroke,
                );
            }
            DotKind::Maximize => {
                let r = Rect::from_center_size(center, Vec2::splat(5.2));
                painter.rect_stroke(r, 1.0, stroke, egui::StrokeKind::Inside);
            }
            DotKind::Restore => {
                let back = Rect::from_center_size(Pos2::new(center.x + 1.0, center.y - 1.0), Vec2::splat(4.0));
                let front = Rect::from_center_size(Pos2::new(center.x - 1.0, center.y + 1.0), Vec2::splat(4.0));
                painter.rect_stroke(back, 0.8, stroke, egui::StrokeKind::Inside);
                painter.rect_stroke(front, 0.8, stroke, egui::StrokeKind::Inside);
            }
        }
    }

    resp.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_bar_drag_works_in_top_panel() {
        let ctx = egui::Context::default();
        let mut last_action;

        let mut t = 0.0_f64;
        let mut run_frame = |ctx: &egui::Context, events: Vec<egui::Event>| -> WindowAction {
            t += 0.016;
            let mut action = WindowAction::None;
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
                time: Some(t),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| {
                egui::Panel::left("left_p")
                    .exact_size(220.0)
                    .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(10, 12)))
                    .show(ui, |ui| {
                        let pr = ui.max_rect();
                        let top_drag = Rect::from_min_size(
                            pos2(pr.min.x - 10.0, pr.min.y - 12.0),
                            Vec2::new(220.0, 44.0),
                        );
                        let act = header_bar_drag_area(ui, top_drag);
                        if act != WindowAction::None {
                            action = act;
                        }
                    });
                egui::Panel::top("top_p")
                    .exact_size(44.0)
                    .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(12, 5)))
                    .show(ui, |ui| {
                        let pr = ui.max_rect();
                        let act = header_bar_drag_area(
                            ui,
                            Rect::from_min_max(
                                pos2(pr.min.x - 12.0, pr.min.y - 5.0),
                                pos2(pr.max.x + 12.0, pr.max.y + 5.0),
                            ),
                        );
                        if act != WindowAction::None {
                            action = act;
                        }
                        ui.horizontal(|ui| {
                            let _ = ui.button("Back");
                            ui.label("Title");
                        });
                    });
            });
            out.textures_delta.clear();
            action
        };

        // Frame 1: initial layout
        let _ = run_frame(&ctx, vec![]);
        // Frame 2: pointer move to (400, 20)
        let _ = run_frame(&ctx, vec![egui::Event::PointerMoved(pos2(400.0, 20.0))]);
        // Frame 3: primary press at (400, 20)
        last_action = run_frame(
            &ctx,
            vec![egui::Event::PointerButton {
                pos: pos2(400.0, 20.0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert_eq!(last_action, WindowAction::None);

        // Frame 4: small move to (403, 20) should immediately trigger Drag
        last_action = run_frame(&ctx, vec![egui::Event::PointerMoved(pos2(403.0, 20.0))]);
        assert_eq!(last_action, WindowAction::Drag, "small move in top panel");

        // Now test left panel top_drag area at (100, 20)
        let _ = run_frame(
            &ctx,
            vec![egui::Event::PointerButton {
                pos: pos2(403.0, 20.0),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let _ = run_frame(&ctx, vec![egui::Event::PointerMoved(pos2(100.0, 20.0))]);
        let _ = run_frame(
            &ctx,
            vec![egui::Event::PointerButton {
                pos: pos2(100.0, 20.0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        last_action = run_frame(&ctx, vec![egui::Event::PointerMoved(pos2(112.0, 20.0))]);
        assert_eq!(last_action, WindowAction::Drag, "move in left panel top_drag");
    }

    #[test]
    fn header_bar_drag_with_scroll_area_tabs() {
        let ctx = egui::Context::default();
        let mut t = 0.0_f64;
        let mut run_frame = |events: Vec<egui::Event>| -> WindowAction {
            t += 0.016;
            let mut action = WindowAction::None;
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0))),
                time: Some(t),
                events,
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| {
                egui::Panel::top("editor_header")
                    .exact_size(40.0)
                    .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(10, 0)))
                    .show(ui, |ui| {
                        let bar_rect = ui.max_rect();
                        let full = Rect::from_min_max(
                            pos2(bar_rect.min.x - 10.0, bar_rect.min.y),
                            pos2(bar_rect.max.x + 10.0, bar_rect.max.y),
                        );
                        let act = header_bar_drag_area(ui, full);
                        if act != WindowAction::None {
                            action = act;
                        }
                        ui.horizontal_centered(|ui| {
                            let tabs_max_w = (ui.available_width() - 114.0).max(120.0);
                            ui.allocate_ui_with_layout(
                                Vec2::new(tabs_max_w, 34.0),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    egui::ScrollArea::horizontal()
                                        .id_salt("tabs_scroll")
                                        .scroll_source(egui::scroll_area::ScrollSource::MOUSE_WHEEL)
                                        .auto_shrink([true, false])
                                        .show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                let _ = ui.button("Tab 1");
                                            });
                                        });
                                },
                            );
                        });
                    });
            });
            out.textures_delta.clear();
            action
        };

        let _ = run_frame(vec![]);
        let _ = run_frame(vec![egui::Event::PointerMoved(pos2(400.0, 20.0))]);
        let _ = run_frame(vec![egui::Event::PointerButton {
            pos: pos2(400.0, 20.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        let act = run_frame(vec![egui::Event::PointerMoved(pos2(415.0, 20.0))]);
        assert_eq!(act, WindowAction::Drag, "drag in empty area next to tabs");
    }
}
