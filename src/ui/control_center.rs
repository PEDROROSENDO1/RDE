use crate::ui::icons::{IconKind, draw_icon};
use crate::ui::{CaptureMode, PendingCaptureAction, ShellUiState};
use egui::{
    Color32, Context, CornerRadius, Frame, Margin, Pos2, Rect, RichText, Sense, Stroke, Vec2,
};

pub fn render_control_center(ctx: &Context, state: &mut ShellUiState) {
    if !state.control_center_open {
        state.control_center_rect = None;
        state.cc_wifi_selected_ssid = None;
        return;
    }

    let screen_w = ctx.content_rect().width().max(800.0);
    let panel_w = 368.0;
    let pos = egui::pos2(screen_w - panel_w - 8.0, 4.0);
    let approx_cc_rect = state
        .control_center_rect
        .unwrap_or_else(|| Rect::from_min_size(pos, Vec2::new(panel_w, 500.0)));

    // Close Control Center automatically when clicking outside its card and restore app focus.
    if ctx.input(|i| i.pointer.any_pressed()) {
        if let Some(click_pos) = ctx.input(|i| i.pointer.interact_pos()) {
            if !approx_cc_rect.contains(click_pos) {
                state.control_center_open = false;
                state.control_center_rect = None;
                state.cc_wifi_selected_ssid = None;
                state.cc_wifi_password.clear();
                if let Ok(guard) = state.wm.state.lock() {
                    if let Some(ref w) = guard.focused_window {
                        if !w.id.is_empty() {
                            crate::wm::pila::PilaConnector::focus_window(&w.id);
                        }
                    }
                }
                return;
            }
        }
    }

    let t = &state.config.theme;
    let accent = Color32::from_rgb(t.accent_rgb[0], t.accent_rgb[1], t.accent_rgb[2]);
    let card_bg = Color32::from_rgba_unmultiplied(255, 255, 255, 14);
    let card_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 22));

    let hw_snap = state.hw.state.lock().map(|g| g.clone()).unwrap_or_default();
    let sys_snap = state
        .sys
        .snapshot
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    let media_snap = state
        .media
        .info
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();

    let cc_frame = Frame::NONE
        .fill(Color32::from_rgba_unmultiplied(
            t.panel_bg_rgba[0],
            t.panel_bg_rgba[1],
            t.panel_bg_rgba[2],
            246,
        ))
        .corner_radius(CornerRadius::same(18))
        .inner_margin(Margin::same(14))
        .stroke(Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 44),
        ));

    let win_resp = egui::Window::new("rocketde_control_center")
        .order(egui::Order::Foreground)
        .title_bar(false)
        .resizable(false)
        .fixed_pos(pos)
        .fixed_size(egui::vec2(panel_w, 0.0))
        .frame(cc_frame)
        .show(ctx, |ui| {
            ui.set_min_width(panel_w - 28.0);

            // Header
            ui.horizontal(|ui| {
                let (badge_r, _) = ui.allocate_exact_size(Vec2::new(24.0, 24.0), Sense::hover());
                ui.painter().rect_filled(
                    badge_r,
                    CornerRadius::same(7),
                    Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 44),
                );
                draw_icon(
                    ui.painter(),
                    badge_r.center(),
                    13.5,
                    IconKind::ControlCenter,
                    Color32::WHITE,
                );
                ui.add_space(2.0);
                ui.label(
                    RichText::new("Control Center")
                        .size(14.5)
                        .strong()
                        .color(Color32::WHITE),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (close_r, close_resp) =
                        ui.allocate_exact_size(Vec2::new(24.0, 24.0), Sense::click());
                    let bg = if close_resp.hovered() {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 42)
                    } else {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 18)
                    };
                    ui.painter().rect_filled(close_r, CornerRadius::same(12), bg);
                    draw_icon(
                        ui.painter(),
                        close_r.center(),
                        10.0,
                        IconKind::Close,
                        Color32::WHITE,
                    );
                    if close_resp.clicked() {
                        state.control_center_open = false;
                    }

                    ui.add_space(4.0);

                    if state.recording_active {
                        let secs = state
                            .recording_started_at
                            .map(|t| t.elapsed().as_secs())
                            .unwrap_or(0);
                        let label = format!("⏹ {:02}:{:02}", secs / 60, secs % 60);
                        if pill_action_button(
                            ui,
                            &label,
                            Color32::from_rgb(215, 45, 45),
                            Color32::WHITE,
                        )
                        .on_hover_text("Parar gravação de tela")
                        .clicked()
                        {
                            state.control_center_open = false;
                            state.pending_capture_action =
                                Some(PendingCaptureAction::StopRecording);
                        }
                    } else {
                        let (cap_r, cap_resp) =
                            ui.allocate_exact_size(Vec2::new(28.0, 24.0), Sense::click());
                        let cap_bg = if cap_resp.hovered() {
                            Color32::from_rgba_unmultiplied(255, 255, 255, 42)
                        } else {
                            Color32::from_rgba_unmultiplied(255, 255, 255, 18)
                        };
                        ui.painter()
                            .rect_filled(cap_r, CornerRadius::same(12), cap_bg);
                        draw_icon(
                            ui.painter(),
                            cap_r.center(),
                            13.0,
                            IconKind::Camera,
                            Color32::WHITE,
                        );
                        if cap_resp
                            .on_hover_text("Capturar ou Gravar tela")
                            .clicked()
                        {
                            state.close_overlays();
                            state.pending_capture_action =
                                Some(PendingCaptureAction::OpenOverlay(CaptureMode::Screenshot));
                            ctx.request_repaint();
                        }
                    }
                });
            });

            ui.add_space(10.0);

            // 2x2 Connectivity & Mode Tiles
            let wifi_subtitle = if !hw_snap.wifi_enabled {
                "Desativado"
            } else if !hw_snap.wifi_ssid.is_empty() {
                &hw_snap.wifi_ssid
            } else {
                &sys_snap.network_name
            };

            ui.columns(2, |cols| {
                let wifi_action = expandable_toggle_pill(
                    &mut cols[0],
                    if hw_snap.wifi_enabled {
                        IconKind::Wifi
                    } else {
                        IconKind::WifiOff
                    },
                    "Wi-Fi",
                    wifi_subtitle,
                    hw_snap.wifi_enabled,
                    state.cc_wifi_expanded,
                    "Gerenciar redes Wi-Fi",
                    accent,
                    card_bg,
                    card_stroke,
                );
                match wifi_action {
                    ExpandablePillAction::ToggleRadio => state.hw.toggle_wifi(),
                    ExpandablePillAction::ToggleExpand => {
                        state.cc_wifi_expanded = !state.cc_wifi_expanded;
                        if state.cc_wifi_expanded {
                            state.cc_bt_expanded = false;
                            if hw_snap.wifi_enabled {
                                state.hw.scan_wifi_networks(true);
                            }
                        } else {
                            state.cc_wifi_selected_ssid = None;
                            state.cc_wifi_password.clear();
                        }
                    }
                    ExpandablePillAction::None => {}
                }

                let bt_subtitle = if !hw_snap.bluetooth_enabled {
                    "Desligado"
                } else if !hw_snap.bluetooth_connected_name.is_empty() {
                    &hw_snap.bluetooth_connected_name
                } else {
                    "Ativo"
                };
                let bt_action = expandable_toggle_pill(
                    &mut cols[1],
                    IconKind::Bluetooth,
                    "Bluetooth",
                    bt_subtitle,
                    hw_snap.bluetooth_enabled,
                    state.cc_bt_expanded,
                    "Gerenciar dispositivos Bluetooth",
                    accent,
                    card_bg,
                    card_stroke,
                );
                match bt_action {
                    ExpandablePillAction::ToggleRadio => state.hw.toggle_bluetooth(),
                    ExpandablePillAction::ToggleExpand => {
                        state.cc_bt_expanded = !state.cc_bt_expanded;
                        if state.cc_bt_expanded {
                            state.cc_wifi_expanded = false;
                            state.cc_wifi_selected_ssid = None;
                            state.cc_wifi_password.clear();
                            if hw_snap.bluetooth_enabled {
                                state.hw.scan_bluetooth_devices(true);
                            }
                        }
                    }
                    ExpandablePillAction::None => {}
                }
            });

            ui.add_space(6.0);

            ui.columns(2, |cols| {
                if toggle_pill(
                    &mut cols[0],
                    IconKind::Moon,
                    "Foco / DND",
                    if hw_snap.dnd_enabled {
                        "Silencioso"
                    } else {
                        "Notificações"
                    },
                    hw_snap.dnd_enabled,
                    Color32::from_rgb(142, 68, 173),
                    card_bg,
                    card_stroke,
                ) {
                    state.hw.toggle_dnd();
                }

                if toggle_pill(
                    &mut cols[1],
                    IconKind::Bolt,
                    "Energia",
                    &hw_snap.power_profile_name,
                    hw_snap.power_saver,
                    Color32::from_rgb(243, 156, 18),
                    card_bg,
                    card_stroke,
                ) {
                    state.hw.toggle_power_saver();
                }
            });

            if state.cc_wifi_expanded {
                ui.add_space(8.0);
                render_wifi_manager_card(ctx, ui, state, &hw_snap, accent, card_bg, card_stroke);
            } else if state.cc_bt_expanded {
                ui.add_space(8.0);
                render_bluetooth_manager_card(ui, state, &hw_snap, accent, card_bg, card_stroke);
            }

            ui.add_space(8.0);

            // Brightness Slider Card
            let br_badge = if hw_snap.backlight_device.is_empty() {
                None
            } else {
                Some(hw_snap.backlight_device.as_str())
            };
            if let Some(next_br) = render_slider_card(
                ui,
                "cc_brightness_slider",
                card_bg,
                card_stroke,
                IconKind::Sun,
                "Brilho",
                hw_snap.brightness_percent,
                2.0..=100.0,
                accent,
                false,
                br_badge,
                None::<(&str, fn())>,
            ) {
                state.hw.set_brightness(next_br);
            }

            ui.add_space(6.0);

            // Volume Slider Card
            let vol_kind = if hw_snap.muted || hw_snap.volume_percent <= 0.5 {
                IconKind::VolumeMute
            } else if hw_snap.volume_percent < 45.0 {
                IconKind::VolumeLow
            } else {
                IconKind::VolumeHigh
            };
            let mut toggle_vol_mute = false;
            if let Some(next_vol) = render_slider_card(
                ui,
                "cc_volume_slider",
                card_bg,
                card_stroke,
                vol_kind,
                "Volume",
                hw_snap.volume_percent,
                0.0..=150.0,
                accent,
                hw_snap.muted,
                None,
                Some((
                    if hw_snap.muted { "Mudo" } else { "Silenciar" },
                    || {
                        toggle_vol_mute = true;
                    },
                )),
            ) {
                state.hw.set_volume(next_vol);
            }
            if toggle_vol_mute {
                state.hw.toggle_mute();
            }

            ui.add_space(6.0);

            // Microphone Slider Card
            let mic_kind = if hw_snap.mic_muted || hw_snap.mic_volume_percent <= 0.5 {
                IconKind::MicMute
            } else {
                IconKind::Mic
            };
            let mut toggle_mic_mute = false;
            if let Some(next_mic) = render_slider_card(
                ui,
                "cc_mic_slider",
                card_bg,
                card_stroke,
                mic_kind,
                "Microfone",
                hw_snap.mic_volume_percent,
                0.0..=100.0,
                Color32::from_rgb(48, 209, 88),
                hw_snap.mic_muted,
                None,
                Some((
                    if hw_snap.mic_muted { "Mudo" } else { "Silenciar" },
                    || {
                        toggle_mic_mute = true;
                    },
                )),
            ) {
                state.hw.set_mic_volume(next_mic);
            }
            if toggle_mic_mute {
                state.hw.toggle_mic_mute();
            }

            ui.add_space(6.0);

            // Media Player Card
            Frame::NONE
                .fill(card_bg)
                .corner_radius(CornerRadius::same(13))
                .inner_margin(Margin::symmetric(12, 10))
                .stroke(card_stroke)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (badge_r, _) =
                            ui.allocate_exact_size(Vec2::new(34.0, 34.0), Sense::hover());
                        let badge_bg = if media_snap.available {
                            Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 48)
                        } else {
                            Color32::from_rgba_unmultiplied(255, 255, 255, 16)
                        };
                        ui.painter()
                            .rect_filled(badge_r, CornerRadius::same(9), badge_bg);
                        draw_icon(
                            ui.painter(),
                            badge_r.center(),
                            16.0,
                            IconKind::MusicNote,
                            if media_snap.available {
                                accent
                            } else {
                                Color32::from_rgb(160, 170, 190)
                            },
                        );

                        ui.add_space(4.0);

                        let title_raw = if media_snap.available && !media_snap.title.is_empty() {
                            media_snap.title.as_str()
                        } else {
                            "Nenhuma mídia ativa"
                        };
                        let title = truncate_text(title_raw, 22);
                        let artist = if media_snap.available && !media_snap.artist.is_empty() {
                            truncate_text(&media_snap.artist, 24)
                        } else {
                            "Player MPRIS".to_string()
                        };

                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(title)
                                    .strong()
                                    .size(12.5)
                                    .color(Color32::WHITE),
                            );
                            ui.label(
                                RichText::new(artist)
                                    .size(10.5)
                                    .color(Color32::from_rgb(155, 164, 182)),
                            );
                        });

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if transport_button(ui, IconKind::Next, false, accent) {
                                state.media.next();
                            }
                            if transport_button(
                                ui,
                                if media_snap.playing {
                                    IconKind::Pause
                                } else {
                                    IconKind::Play
                                },
                                media_snap.playing,
                                accent,
                            ) {
                                state.media.play_pause();
                            }
                            if transport_button(ui, IconKind::Prev, false, accent) {
                                state.media.previous();
                            }
                        });
                    });
                });

            ui.add_space(6.0);

            // System Telemetry Card (2-column CPU & RAM with divider and rounded bars)
            Frame::NONE
                .fill(card_bg)
                .corner_radius(CornerRadius::same(13))
                .inner_margin(Margin::symmetric(12, 10))
                .stroke(card_stroke)
                .show(ui, |ui| {
                    let card_inner = ui.max_rect();
                    ui.columns(2, |cols| {
                        render_telemetry_col(
                            &mut cols[0],
                            "CPU",
                            &format!("{:.0}°C", sys_snap.cpu_temp_c),
                            sys_snap.cpu_usage,
                            &format!("{:.1}%", sys_snap.cpu_usage),
                            accent,
                        );
                        render_telemetry_col(
                            &mut cols[1],
                            "RAM",
                            &format!("{:.1}/{:.1} GB", sys_snap.ram_used_gb, sys_snap.ram_total_gb),
                            sys_snap.ram_percent,
                            &format!("{:.0}%", sys_snap.ram_percent),
                            Color32::from_rgb(48, 209, 88),
                        );
                    });
                    let mid_x = card_inner.center().x;
                    ui.painter().line_segment(
                        [
                            Pos2::new(mid_x, card_inner.min.y + 4.0),
                            Pos2::new(mid_x, card_inner.max.y - 4.0),
                        ],
                        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 16)),
                    );
                });
        });

    state.control_center_rect = win_resp.map(|r| r.response.rect);
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{}…", head.trim_end())
    } else {
        head
    }
}

pub(crate) fn pill_action_button(
    ui: &mut egui::Ui,
    label: &str,
    bg: Color32,
    fg: Color32,
) -> egui::Response {
    let btn = egui::Button::new(RichText::new(label).size(11.0).strong().color(fg))
        .fill(bg)
        .stroke(Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 22),
        ))
        .corner_radius(CornerRadius::same(10))
        .min_size(Vec2::new(0.0, 22.0));
    ui.add(btn)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_slider_card(
    ui: &mut egui::Ui,
    id_salt: &str,
    card_bg: Color32,
    card_stroke: Stroke,
    icon: IconKind,
    label: &str,
    value: f32,
    range: std::ops::RangeInclusive<f32>,
    accent: Color32,
    muted: bool,
    right_info: Option<&str>,
    mut action_btn: Option<(&str, impl FnMut())>,
) -> Option<f32> {
    let mut new_val = None;

    Frame::NONE
        .fill(card_bg)
        .corner_radius(CornerRadius::same(13))
        .inner_margin(Margin::symmetric(12, 10))
        .stroke(card_stroke)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (icon_r, _) = ui.allocate_exact_size(Vec2::new(18.0, 18.0), Sense::hover());
                let icon_col = if muted {
                    Color32::from_rgb(155, 162, 178)
                } else {
                    Color32::WHITE
                };
                draw_icon(ui.painter(), icon_r.center(), 13.5, icon, icon_col);
                ui.label(
                    RichText::new(label)
                        .size(12.5)
                        .strong()
                        .color(Color32::WHITE),
                );
                let pct_col = if !muted && *range.end() > 100.0 && value > 100.0 {
                    Color32::from_rgb(255, 180, 60)
                } else {
                    Color32::from_rgb(165, 174, 192)
                };
                ui.label(
                    RichText::new(format!("{:.0}%", value))
                        .size(11.5)
                        .color(pct_col),
                );

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some((btn_label, ref mut on_click)) = action_btn {
                        let btn_bg = if muted {
                            Color32::from_rgba_unmultiplied(255, 69, 58, 175)
                        } else {
                            Color32::from_rgba_unmultiplied(255, 255, 255, 20)
                        };
                        if pill_action_button(ui, btn_label, btn_bg, Color32::WHITE).clicked() {
                            on_click();
                        }
                    } else if let Some(info) = right_info {
                        ui.label(
                            RichText::new(info)
                                .size(10.5)
                                .color(Color32::from_rgb(145, 154, 172)),
                        );
                    }
                });
            });

            ui.add_space(7.0);

            let min = *range.start();
            let max = *range.end();
            let span = (max - min).max(1.0);
            let frac = ((value - min) / span).clamp(0.0, 1.0);

            let slider_w = ui.available_width().max(120.0);
            let slider_h = 20.0;
            let (hit_rect, resp) =
                ui.allocate_exact_size(Vec2::new(slider_w, slider_h), Sense::click_and_drag());
            let _ = id_salt;

            let track_h = 10.0;
            let track_rect = Rect::from_center_size(
                hit_rect.center(),
                Vec2::new(hit_rect.width(), track_h),
            );
            ui.painter().rect(
                track_rect,
                CornerRadius::same(5),
                Color32::from_rgba_unmultiplied(255, 255, 255, 22),
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 18)),
                egui::StrokeKind::Inside,
            );

            let fill_col = if muted {
                Color32::from_rgba_unmultiplied(150, 158, 175, 150)
            } else if max > 100.0 && value > 100.0 {
                Color32::from_rgb(255, 159, 10)
            } else {
                accent
            };

            let knob_r = if resp.hovered() || resp.dragged() {
                8.5
            } else {
                7.5
            };
            let usable_w = (track_rect.width() - knob_r * 2.0).max(1.0);
            let knob_x = track_rect.min.x + knob_r + usable_w * frac;

            let fill_w = (knob_x - track_rect.min.x + 2.0).clamp(track_h, track_rect.width());
            let fill_rect =
                Rect::from_min_size(track_rect.min, Vec2::new(fill_w, track_rect.height()));
            ui.painter()
                .rect_filled(fill_rect, CornerRadius::same(5), fill_col);

            if max > 100.0 {
                let mark_frac = (100.0 - min) / span;
                let mark_x = track_rect.min.x + knob_r + usable_w * mark_frac;
                ui.painter().line_segment(
                    [
                        Pos2::new(mark_x, track_rect.min.y - 2.0),
                        Pos2::new(mark_x, track_rect.max.y + 2.0),
                    ],
                    Stroke::new(1.2, Color32::from_rgba_unmultiplied(255, 255, 255, 110)),
                );
            }

            // Crisp white thumb with subtle outer ring
            ui.painter().circle_filled(
                Pos2::new(knob_x, track_rect.center().y),
                knob_r + 1.5,
                Color32::from_rgba_unmultiplied(0, 0, 0, 55),
            );
            ui.painter().circle_filled(
                Pos2::new(knob_x, track_rect.center().y),
                knob_r,
                Color32::WHITE,
            );

            if (resp.clicked() || resp.dragged())
                && let Some(pointer_pos) = resp.interact_pointer_pos()
            {
                let next_frac =
                    ((pointer_pos.x - (track_rect.min.x + knob_r)) / usable_w).clamp(0.0, 1.0);
                let computed = min + next_frac * span;
                if (computed - value).abs() >= 0.25 {
                    new_val = Some(computed);
                }
            }
        });

    new_val
}

enum ExpandablePillAction {
    None,
    ToggleRadio,
    ToggleExpand,
}

#[allow(clippy::too_many_arguments)]
fn expandable_toggle_pill(
    ui: &mut egui::Ui,
    icon: IconKind,
    title: &str,
    subtitle: &str,
    active: bool,
    expanded: bool,
    hover_tip: &str,
    active_col: Color32,
    inactive_col: Color32,
    card_stroke: Stroke,
) -> ExpandablePillAction {
    let fill = if active {
        Color32::from_rgba_unmultiplied(active_col.r(), active_col.g(), active_col.b(), 220)
    } else {
        inactive_col
    };
    let stroke = if active {
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 48))
    } else {
        card_stroke
    };
    let mut action = ExpandablePillAction::None;

    Frame::NONE
        .fill(fill)
        .corner_radius(CornerRadius::same(13))
        .inner_margin(Margin::symmetric(10, 9))
        .stroke(stroke)
        .show(ui, |ui| {
            ui.set_min_width(142.0);
            ui.horizontal(|ui| {
                let main_resp = ui
                    .horizontal(|ui| {
                        let (badge_r, _) =
                            ui.allocate_exact_size(Vec2::new(30.0, 30.0), Sense::hover());
                        let badge_bg = if active {
                            Color32::from_rgba_unmultiplied(255, 255, 255, 56)
                        } else {
                            Color32::from_rgba_unmultiplied(255, 255, 255, 22)
                        };
                        ui.painter()
                            .circle_filled(badge_r.center(), 15.0, badge_bg);
                        draw_icon(
                            ui.painter(),
                            badge_r.center(),
                            14.0,
                            icon,
                            Color32::WHITE,
                        );
                        ui.add_space(2.0);
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(title)
                                    .strong()
                                    .size(12.5)
                                    .color(Color32::WHITE),
                            );
                            let sub_trunc = truncate_text(subtitle, 11);
                            ui.label(
                                RichText::new(sub_trunc)
                                    .size(10.5)
                                    .color(Color32::from_rgba_unmultiplied(240, 244, 255, 205)),
                            );
                        });
                    })
                    .response
                    .interact(Sense::click());

                if main_resp.clicked() {
                    action = ExpandablePillAction::ToggleRadio;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (chev_r, chev_resp) =
                        ui.allocate_exact_size(Vec2::new(24.0, 24.0), Sense::click());
                    let chev_bg = if expanded || chev_resp.hovered() {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 48)
                    } else {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 20)
                    };
                    ui.painter().circle_filled(chev_r.center(), 12.0, chev_bg);
                    draw_icon(
                        ui.painter(),
                        chev_r.center(),
                        10.5,
                        if expanded {
                            IconKind::ChevronLeft
                        } else {
                            IconKind::ChevronRight
                        },
                        Color32::WHITE,
                    );
                    if chev_resp.on_hover_text(hover_tip).clicked() {
                        action = ExpandablePillAction::ToggleExpand;
                    }
                });
            });
        });

    action
}

fn render_wifi_manager_card(
    ctx: &Context,
    ui: &mut egui::Ui,
    state: &mut ShellUiState,
    hw_snap: &crate::system::audio_brightness::AudioBrightnessState,
    accent: Color32,
    card_bg: Color32,
    card_stroke: Stroke,
) {
    Frame::NONE
        .fill(card_bg)
        .corner_radius(CornerRadius::same(13))
        .inner_margin(Margin::same(10))
        .stroke(card_stroke)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Redes Wi-Fi")
                        .size(12.5)
                        .strong()
                        .color(Color32::WHITE),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if hw_snap.wifi_scanning {
                        ui.label(
                            RichText::new("Buscando...")
                                .size(11.0)
                                .color(Color32::from_rgb(160, 170, 190)),
                        );
                    } else if pill_action_button(
                        ui,
                        "Atualizar",
                        Color32::from_rgba_unmultiplied(255, 255, 255, 20),
                        Color32::WHITE,
                    )
                    .clicked()
                    {
                        state.hw.scan_wifi_networks(true);
                    }
                });
            });

            if let Some(ref msg) = hw_snap.wifi_status_msg {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(msg)
                        .size(11.0)
                        .color(Color32::from_rgb(185, 200, 225)),
                );
            }

            if !hw_snap.wifi_enabled {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Wi-Fi desativado.")
                            .size(11.5)
                            .color(Color32::from_rgb(160, 170, 190)),
                    );
                    if pill_action_button(ui, "Ativar Wi-Fi", accent, Color32::WHITE).clicked() {
                        state.hw.toggle_wifi();
                    }
                });
                return;
            }

            ui.add_space(6.0);

            if let Some(target_ssid) = state.cc_wifi_selected_ssid.clone() {
                Frame::NONE
                    .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 16))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(8))
                    .stroke(card_stroke)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!("Senha para \"{}\"", target_ssid))
                                .size(11.5)
                                .strong()
                                .color(Color32::WHITE),
                        );
                        ui.add_space(4.0);
                        let edit = egui::TextEdit::singleline(&mut state.cc_wifi_password)
                            .password(true)
                            .hint_text("Digite a senha da rede")
                            .desired_width(f32::INFINITY);
                        let resp = ui.add(edit);
                        if state.cc_wifi_focus_requested {
                            resp.request_focus();
                            state.cc_wifi_focus_requested = false;
                        }
                        let submit_enter =
                            resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if pill_action_button(ui, "Conectar", accent, Color32::WHITE).clicked()
                                || submit_enter
                            {
                                let pwd = state.cc_wifi_password.clone();
                                state.hw.connect_wifi(target_ssid.clone(), Some(pwd));
                                state.cc_wifi_selected_ssid = None;
                                state.cc_wifi_password.clear();
                            }
                            if pill_action_button(
                                ui,
                                "Cancelar",
                                Color32::from_rgba_unmultiplied(255, 255, 255, 20),
                                Color32::WHITE,
                            )
                            .clicked()
                            {
                                state.cc_wifi_selected_ssid = None;
                                state.cc_wifi_password.clear();
                            }
                        });
                    });
                ui.add_space(6.0);
            }

            egui::ScrollArea::vertical()
                .id_salt("cc_wifi_list_scroll")
                .max_height(165.0)
                .show(ui, |ui| {
                    if hw_snap.wifi_networks.is_empty() && !hw_snap.wifi_scanning {
                        ui.label(
                            RichText::new("Nenhuma rede encontrada. Clique em Atualizar.")
                                .size(11.0)
                                .color(Color32::from_rgb(155, 165, 180)),
                        );
                    }

                    for net in &hw_snap.wifi_networks {
                        let is_connecting =
                            hw_snap.wifi_connecting_ssid.as_deref() == Some(net.ssid.as_str());
                        let row_bg = if net.active {
                            Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 52)
                        } else {
                            Color32::from_rgba_unmultiplied(255, 255, 255, 10)
                        };

                        Frame::NONE
                            .fill(row_bg)
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(Margin::symmetric(8, 6))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let (sig_r, _) = ui.allocate_exact_size(
                                        Vec2::new(14.0, 14.0),
                                        Sense::hover(),
                                    );
                                    draw_icon(
                                        ui.painter(),
                                        sig_r.center(),
                                        12.0,
                                        IconKind::Wifi,
                                        if net.active {
                                            accent
                                        } else {
                                            Color32::from_rgb(210, 218, 232)
                                        },
                                    );

                                    let secured =
                                        !net.security.is_empty() && net.security != "--";
                                    let ssid_trunc = truncate_text(&net.ssid, 15);
                                    ui.vertical(|ui| {
                                        ui.label(
                                            RichText::new(ssid_trunc)
                                                .size(12.0)
                                                .strong()
                                                .color(Color32::WHITE),
                                        );
                                        let meta = if is_connecting {
                                            "Conectando...".to_string()
                                        } else if net.active {
                                            format!("Conectado • {}%", net.signal)
                                        } else if net.saved {
                                            format!("Salva • {}%", net.signal)
                                        } else if secured {
                                            format!("Protegida • {}%", net.signal)
                                        } else {
                                            format!("Aberta • {}%", net.signal)
                                        };
                                        ui.label(
                                            RichText::new(meta)
                                                .size(10.0)
                                                .color(Color32::from_rgb(165, 174, 192)),
                                        );
                                    });

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let subtle =
                                                Color32::from_rgba_unmultiplied(255, 255, 255, 20);
                                            if net.active {
                                                if pill_action_button(
                                                    ui,
                                                    "Desconectar",
                                                    subtle,
                                                    Color32::WHITE,
                                                )
                                                .clicked()
                                                {
                                                    state.hw.disconnect_wifi(net.ssid.clone());
                                                }
                                                if pill_action_button(
                                                    ui,
                                                    "Esquecer",
                                                    subtle,
                                                    Color32::WHITE,
                                                )
                                                .clicked()
                                                {
                                                    state.hw.forget_wifi(net.ssid.clone());
                                                }
                                            } else if net.saved {
                                                if pill_action_button(
                                                    ui,
                                                    "Conectar",
                                                    accent,
                                                    Color32::WHITE,
                                                )
                                                .clicked()
                                                {
                                                    state.hw.connect_wifi(net.ssid.clone(), None);
                                                }
                                                if pill_action_button(
                                                    ui,
                                                    "Editar",
                                                    subtle,
                                                    Color32::WHITE,
                                                )
                                                .clicked()
                                                {
                                                    state.cc_wifi_selected_ssid =
                                                        Some(net.ssid.clone());
                                                    state.cc_wifi_password.clear();
                                                    state.cc_wifi_focus_requested = true;
                                                }
                                                if pill_action_button(
                                                    ui,
                                                    "Esquecer",
                                                    subtle,
                                                    Color32::WHITE,
                                                )
                                                .clicked()
                                                {
                                                    state.hw.forget_wifi(net.ssid.clone());
                                                }
                                            } else if pill_action_button(
                                                ui,
                                                "Conectar",
                                                accent,
                                                Color32::WHITE,
                                            )
                                            .clicked()
                                            {
                                                if secured {
                                                    state.cc_wifi_selected_ssid =
                                                        Some(net.ssid.clone());
                                                    state.cc_wifi_password.clear();
                                                    state.cc_wifi_focus_requested = true;
                                                } else {
                                                    state.hw.connect_wifi(net.ssid.clone(), None);
                                                }
                                            }
                                        },
                                    );
                                });
                            });
                        ui.add_space(4.0);
                    }
                });
            let _ = ctx;
        });
}

fn render_bluetooth_manager_card(
    ui: &mut egui::Ui,
    state: &mut ShellUiState,
    hw_snap: &crate::system::audio_brightness::AudioBrightnessState,
    accent: Color32,
    card_bg: Color32,
    card_stroke: Stroke,
) {
    Frame::NONE
        .fill(card_bg)
        .corner_radius(CornerRadius::same(13))
        .inner_margin(Margin::same(10))
        .stroke(card_stroke)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Dispositivos Bluetooth")
                        .size(12.5)
                        .strong()
                        .color(Color32::WHITE),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if hw_snap.bluetooth_scanning {
                        ui.label(
                            RichText::new("Buscando...")
                                .size(11.0)
                                .color(Color32::from_rgb(160, 170, 190)),
                        );
                    } else if pill_action_button(
                        ui,
                        "Buscar",
                        Color32::from_rgba_unmultiplied(255, 255, 255, 20),
                        Color32::WHITE,
                    )
                    .clicked()
                    {
                        state.hw.scan_bluetooth_devices(true);
                    }
                });
            });

            if let Some(ref msg) = hw_snap.bluetooth_status_msg {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(msg)
                        .size(11.0)
                        .color(Color32::from_rgb(185, 200, 225)),
                );
            }

            if !hw_snap.bluetooth_enabled {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Bluetooth desativado.")
                            .size(11.5)
                            .color(Color32::from_rgb(160, 170, 190)),
                    );
                    if pill_action_button(ui, "Ativar Bluetooth", accent, Color32::WHITE).clicked() {
                        state.hw.toggle_bluetooth();
                    }
                });
                return;
            }

            ui.add_space(6.0);

            egui::ScrollArea::vertical()
                .id_salt("cc_bt_list_scroll")
                .max_height(165.0)
                .show(ui, |ui| {
                    if hw_snap.bluetooth_devices.is_empty() && !hw_snap.bluetooth_scanning {
                        ui.label(
                            RichText::new("Nenhum dispositivo encontrado. Clique em Buscar.")
                                .size(11.0)
                                .color(Color32::from_rgb(155, 165, 180)),
                        );
                    }

                    for dev in &hw_snap.bluetooth_devices {
                        let is_connecting = hw_snap.bluetooth_connecting_mac.as_deref()
                            == Some(dev.mac.as_str());
                        let row_bg = if dev.connected {
                            Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 52)
                        } else {
                            Color32::from_rgba_unmultiplied(255, 255, 255, 10)
                        };

                        Frame::NONE
                            .fill(row_bg)
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(Margin::symmetric(8, 6))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let (ic_r, _) = ui.allocate_exact_size(
                                        Vec2::new(14.0, 14.0),
                                        Sense::hover(),
                                    );
                                    draw_icon(
                                        ui.painter(),
                                        ic_r.center(),
                                        12.0,
                                        IconKind::Bluetooth,
                                        if dev.connected {
                                            accent
                                        } else {
                                            Color32::from_rgb(210, 218, 232)
                                        },
                                    );

                                    let name_trunc = truncate_text(&dev.name, 16);
                                    ui.vertical(|ui| {
                                        ui.label(
                                            RichText::new(name_trunc)
                                                .size(12.0)
                                                .strong()
                                                .color(Color32::WHITE),
                                        );
                                        let meta = if is_connecting {
                                            "Conectando...".to_string()
                                        } else if dev.connected {
                                            "Conectado".to_string()
                                        } else if dev.paired {
                                            "Pareado".to_string()
                                        } else {
                                            dev.mac.clone()
                                        };
                                        ui.label(
                                            RichText::new(meta)
                                                .size(10.0)
                                                .color(Color32::from_rgb(165, 174, 192)),
                                        );
                                    });

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let subtle =
                                                Color32::from_rgba_unmultiplied(255, 255, 255, 20);
                                            if dev.connected {
                                                if pill_action_button(
                                                    ui,
                                                    "Desconectar",
                                                    subtle,
                                                    Color32::WHITE,
                                                )
                                                .clicked()
                                                {
                                                    state.hw.disconnect_bluetooth(dev.mac.clone());
                                                }
                                                if pill_action_button(
                                                    ui,
                                                    "Esquecer",
                                                    subtle,
                                                    Color32::WHITE,
                                                )
                                                .clicked()
                                                {
                                                    state.hw.forget_bluetooth(dev.mac.clone());
                                                }
                                            } else {
                                                if pill_action_button(
                                                    ui,
                                                    "Conectar",
                                                    accent,
                                                    Color32::WHITE,
                                                )
                                                .clicked()
                                                {
                                                    state.hw.connect_bluetooth(dev.mac.clone());
                                                }
                                                if dev.paired
                                                    && pill_action_button(
                                                        ui,
                                                        "Esquecer",
                                                        subtle,
                                                        Color32::WHITE,
                                                    )
                                                    .clicked()
                                                {
                                                    state.hw.forget_bluetooth(dev.mac.clone());
                                                }
                                            }
                                        },
                                    );
                                });
                            });
                        ui.add_space(4.0);
                    }
                });
        });
}

fn render_telemetry_col(
    ui: &mut egui::Ui,
    title: &str,
    sub_label: &str,
    percent: f32,
    pct_label: &str,
    bar_col: Color32,
) {
    let clamped = (percent / 100.0).clamp(0.0, 1.0);
    let fill_col = if percent >= 85.0 {
        Color32::from_rgb(255, 69, 58)
    } else if percent >= 65.0 {
        Color32::from_rgb(255, 159, 10)
    } else {
        bar_col
    };

    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(title)
                    .size(11.5)
                    .strong()
                    .color(Color32::from_rgb(175, 184, 202)),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(sub_label)
                        .size(10.5)
                        .color(Color32::from_rgb(150, 158, 176)),
                );
            });
        });

        ui.add_space(3.0);
        ui.label(
            RichText::new(pct_label)
                .size(17.0)
                .strong()
                .color(Color32::WHITE),
        );
        ui.add_space(5.0);

        let (bar_rect, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width().max(40.0), 6.0),
            Sense::hover(),
        );
        ui.painter().rect_filled(
            bar_rect,
            CornerRadius::same(3),
            Color32::from_rgba_unmultiplied(255, 255, 255, 22),
        );
        if clamped > 0.0 {
            let fill_rect = Rect::from_min_size(
                bar_rect.min,
                Vec2::new((bar_rect.width() * clamped).max(6.0), bar_rect.height()),
            );
            ui.painter()
                .rect_filled(fill_rect, CornerRadius::same(3), fill_col);
        }
    });
}

fn transport_button(ui: &mut egui::Ui, kind: IconKind, highlighted: bool, accent: Color32) -> bool {
    let size = if highlighted { 30.0 } else { 26.0 };
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(size, size), Sense::click());
    let bg = if highlighted {
        accent
    } else if resp.hovered() {
        Color32::from_rgba_unmultiplied(255, 255, 255, 34)
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 16)
    };
    ui.painter()
        .circle_filled(rect.center(), size * 0.5, bg);
    draw_icon(ui.painter(), rect.center(), 12.0, kind, Color32::WHITE);
    resp.clicked()
}

#[allow(clippy::too_many_arguments)]
fn toggle_pill(
    ui: &mut egui::Ui,
    icon: IconKind,
    title: &str,
    subtitle: &str,
    active: bool,
    active_col: Color32,
    inactive_col: Color32,
    card_stroke: Stroke,
) -> bool {
    let fill = if active {
        Color32::from_rgba_unmultiplied(active_col.r(), active_col.g(), active_col.b(), 220)
    } else {
        inactive_col
    };
    let stroke = if active {
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 48))
    } else {
        card_stroke
    };
    let mut clicked = false;

    Frame::NONE
        .fill(fill)
        .corner_radius(CornerRadius::same(13))
        .inner_margin(Margin::symmetric(10, 9))
        .stroke(stroke)
        .show(ui, |ui| {
            ui.set_min_width(142.0);
            let resp = ui
                .horizontal(|ui| {
                    let (badge_r, _) =
                        ui.allocate_exact_size(Vec2::new(30.0, 30.0), Sense::hover());
                    let badge_bg = if active {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 56)
                    } else {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 22)
                    };
                    ui.painter()
                        .circle_filled(badge_r.center(), 15.0, badge_bg);
                    draw_icon(
                        ui.painter(),
                        badge_r.center(),
                        14.0,
                        icon,
                        Color32::WHITE,
                    );
                    ui.add_space(2.0);
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(title)
                                .strong()
                                .size(12.5)
                                .color(Color32::WHITE),
                        );
                        let sub_trunc = truncate_text(subtitle, 16);
                        ui.label(
                            RichText::new(sub_trunc)
                                .size(10.5)
                                .color(Color32::from_rgba_unmultiplied(240, 244, 255, 205)),
                        );
                    });
                })
                .response
                .interact(egui::Sense::click());

            if resp.clicked() {
                clicked = true;
            }
        });

    clicked
}
