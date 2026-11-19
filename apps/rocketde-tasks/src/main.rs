mod config;
mod systemd;
mod telemetry;
mod ui;

use config::TasksConfig;
use egui::{
    Align, Color32, Context, CornerRadius, Event, Frame, Key, Layout, Margin, Modifiers,
    PointerButton, Pos2, RawInput, Rect, RichText, Sense, Stroke, Vec2,
};
use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use rocketde_core::config::RocketConfig;
use rocketde_ui_common::clipboard::{handle_egui_clipboard_output, push_clipboard_events};
use rocketde_ui_common::header_bar::{
    ResizeDir, WindowAction, detect_window_resize, header_bar_drag_area, window_controls_right,
};
use rocketde_ui_common::icons::{IconKind, draw_icon};
use rocketde_ui_common::theme::apply_macos_theme;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers as WlModifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        xdg::{
            XdgShell,
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{Shm, ShmHandler},
};
use std::collections::HashSet;
use std::num::NonZeroU32;
use std::os::fd::AsFd;
use std::os::unix::io::AsRawFd;
use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use systemd::{
    ActionResult, ServiceAction, ServiceScope, SystemdService, fetch_service_logs, kill_process,
    load_systemd_services, run_service_action,
};
use telemetry::{SystemTelemetry, TelemetryCollector};
use ui::{
    ServiceFilter, SortCol, TasksPage, UiAction, format_bytes, pill_btn, render_apps_page,
    render_compute_page, render_memory_page, render_overview_page, render_processes_page,
    render_services_page, render_settings_page, render_storage_net_page, subtle_btn,
};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface},
};

#[derive(Clone)]
enum PendingPrivileged {
    KillPid { pid: u32, signal: i32, label: String },
    KillGroup { pids: Vec<u32>, signal: i32, app_name: String },
    ServiceOp { unit: String, scope: ServiceScope, action: ServiceAction },
    FetchLogs { unit: String, scope: ServiceScope },
}

enum BgMessage {
    ServicesLoaded(ServiceScope, Result<Vec<SystemdService>, String>),
    ActionDone {
        res: ActionResult,
        retry: Option<PendingPrivileged>,
        refresh_services: Option<ServiceScope>,
    },
    LogsDone {
        unit: String,
        scope: ServiceScope,
        res: ActionResult,
    },
}

struct TasksApp {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    _compositor: CompositorState,
    shm: Shm,
    _window: Window,

    device: wgpu::Device,
    queue: wgpu::Queue,
    wgpu_surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    egui_ctx: Context,
    renderer: Renderer,

    width: u32,
    height: u32,
    scale_factor: f32,
    configured: bool,
    dirty: bool,
    repaint_delay: Option<Duration>,
    pending_events: Vec<Event>,
    modifiers: Modifiers,
    keyboard: Option<wayland_client::protocol::wl_keyboard::WlKeyboard>,
    pointer: Option<wayland_client::protocol::wl_pointer::WlPointer>,
    seat: Option<wl_seat::WlSeat>,
    last_pointer_serial: u32,
    is_maximized: bool,
    pending_window_action: WindowAction,
    start_instant: Instant,
    exit: bool,

    rocket_cfg: RocketConfig,
    cfg: TasksConfig,
    collector: TelemetryCollector,
    telemetry_snap: SystemTelemetry,
    current_uid: u32,

    page: TasksPage,
    search_query: String,
    expanded_apps: HashSet<String>,
    sort_col: SortCol,
    sort_desc: bool,
    selected_pid: Option<u32>,

    service_scope: ServiceScope,
    service_filter: ServiceFilter,
    services: Vec<SystemdService>,
    services_loading: bool,
    services_err: Option<String>,

    bg_tx: mpsc::Sender<BgMessage>,
    bg_rx: mpsc::Receiver<BgMessage>,
    wake_fn: std::sync::Arc<dyn Fn() + Send + Sync>,

    sudo_password: Option<String>,
    sudo_modal_open: bool,
    sudo_input: String,
    sudo_error: Option<String>,
    pending_privileged: Option<PendingPrivileged>,

    confirm_kill: Option<PendingPrivileged>,
    logs_modal: Option<(String, String)>,
    status_toast: Option<(String, Instant)>,
}

impl TasksApp {
    fn resize(&mut self, w: u32, h: u32) {
        self.width = w.max(640);
        self.height = h.max(420);
        let phys_w = ((self.width as f32) * self.scale_factor).round().max(1.0) as u32;
        let phys_h = ((self.height as f32) * self.scale_factor).round().max(1.0) as u32;
        self.surface_config.width = phys_w;
        self.surface_config.height = phys_h;
        self.wgpu_surface.configure(&self.device, &self.surface_config);
        self.configured = true;
        self.dirty = true;
    }

    fn set_toast(&mut self, msg: impl Into<String>) {
        self.status_toast = Some((msg.into(), Instant::now()));
        self.dirty = true;
    }

    fn request_services_refresh(&mut self) {
        self.services_loading = true;
        self.services_err = None;
        let scope = self.service_scope;
        let tx = self.bg_tx.clone();
        let wake = self.wake_fn.clone();
        std::thread::spawn(move || {
            let res = load_systemd_services(scope);
            let _ = tx.send(BgMessage::ServicesLoaded(scope, res));
            wake();
        });
    }

    fn dispatch_privileged(&mut self, op: PendingPrivileged) {
        let pass = self.sudo_password.clone();
        let tx = self.bg_tx.clone();
        let wake = self.wake_fn.clone();

        std::thread::spawn(move || {
            match &op {
                PendingPrivileged::KillPid { pid, signal, .. } => {
                    let res = kill_process(*pid, *signal, pass.as_deref());
                    let _ = tx.send(BgMessage::ActionDone {
                        res,
                        retry: Some(op),
                        refresh_services: None,
                    });
                }
                PendingPrivileged::KillGroup { pids, signal, app_name } => {
                    let mut last_res = ActionResult::Ok(format!(
                        "{app_name} encerrado ({} processos)",
                        pids.len()
                    ));
                    for &pid in pids {
                        match kill_process(pid, *signal, pass.as_deref()) {
                            ActionResult::NeedsPassword => {
                                last_res = ActionResult::NeedsPassword;
                                break;
                            }
                            ActionResult::Err(e) => {
                                last_res = ActionResult::Err(e);
                            }
                            ActionResult::Ok(_) => {}
                        }
                    }
                    let _ = tx.send(BgMessage::ActionDone {
                        res: last_res,
                        retry: Some(op),
                        refresh_services: None,
                    });
                }
                PendingPrivileged::ServiceOp { unit, scope, action } => {
                    let res = run_service_action(unit, *scope, *action, pass.as_deref());
                    let sc = *scope;
                    let _ = tx.send(BgMessage::ActionDone {
                        res,
                        retry: Some(op),
                        refresh_services: Some(sc),
                    });
                }
                PendingPrivileged::FetchLogs { unit, scope } => {
                    let res = fetch_service_logs(unit, *scope, pass.as_deref());
                    let _ = tx.send(BgMessage::LogsDone {
                        unit: unit.clone(),
                        scope: *scope,
                        res,
                    });
                }
            }
            wake();
        });
    }

    fn process_bg_messages(&mut self) {
        while let Ok(msg) = self.bg_rx.try_recv() {
            match msg {
                BgMessage::ServicesLoaded(scope, res) => {
                    if scope == self.service_scope {
                        self.services_loading = false;
                        match res {
                            Ok(list) => {
                                self.services = list;
                                self.services_err = None;
                            }
                            Err(e) => {
                                self.services_err = Some(e);
                            }
                        }
                    }
                }
                BgMessage::ActionDone {
                    res,
                    retry,
                    refresh_services,
                } => match res {
                    ActionResult::Ok(msg) => {
                        self.set_toast(msg);
                        if !self.cfg.remember_sudo_session {
                            self.sudo_password = None;
                        }
                        if let Some(sc) = refresh_services {
                            if sc == self.service_scope {
                                self.request_services_refresh();
                            }
                        }
                    }
                    ActionResult::NeedsPassword => {
                        self.sudo_password = None;
                        self.sudo_error = if self.sudo_modal_open {
                            Some("Senha incorreta ou necessária para continuar.".to_string())
                        } else {
                            None
                        };
                        self.pending_privileged = retry;
                        self.sudo_modal_open = true;
                    }
                    ActionResult::Err(err) => {
                        self.set_toast(format!("Erro: {err}"));
                    }
                },
                BgMessage::LogsDone { unit, scope, res } => match res {
                    ActionResult::Ok(txt) => {
                        if !self.cfg.remember_sudo_session {
                            self.sudo_password = None;
                        }
                        let content = if txt.trim().is_empty() {
                            "(Sem registros recentes no journal para este serviço)".to_string()
                        } else {
                            txt
                        };
                        self.logs_modal = Some((unit, content));
                    }
                    ActionResult::NeedsPassword => {
                        self.sudo_password = None;
                        self.sudo_error = None;
                        self.pending_privileged = Some(PendingPrivileged::FetchLogs { unit, scope });
                        self.sudo_modal_open = true;
                    }
                    ActionResult::Err(err) => {
                        self.set_toast(format!("Erro ao ler logs: {err}"));
                    }
                },
            }
        }
    }

    fn render_frame(&mut self) {
        if !self.configured {
            return;
        }
        self.process_bg_messages();
        if let Ok(guard) = self.collector.data.lock() {
            self.telemetry_snap = guard.clone();
        }
        self.egui_ctx.set_pixels_per_point(self.scale_factor);

        let elapsed = self.start_instant.elapsed().as_secs_f64();
        let raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(self.width as f32, self.height as f32),
            )),
            time: Some(elapsed),
            events: std::mem::take(&mut self.pending_events),
            ..Default::default()
        };

        let egui_ctx = self.egui_ctx.clone();
        let full_output = egui_ctx.run_ui(raw_input, |ui| {
            self.draw_ui(ui);
        });
        handle_egui_clipboard_output(&full_output.platform_output.commands);

        match std::mem::replace(&mut self.pending_window_action, WindowAction::None) {
            WindowAction::Drag => {
                if let Some(seat) = &self.seat {
                    self._window.move_(seat, self.last_pointer_serial);
                }
            }
            WindowAction::Resize(dir) => {
                if let Some(seat) = &self.seat {
                    self._window
                        .resize(seat, self.last_pointer_serial, to_xdg_edge(dir));
                }
            }
            WindowAction::Minimize => {
                self._window.set_minimized();
            }
            WindowAction::ToggleMaximize => {
                if self.is_maximized {
                    self._window.unset_maximized();
                } else {
                    self._window.set_maximized();
                }
            }
            WindowAction::Close => {
                self.exit = true;
            }
            WindowAction::None => {}
        }

        let min_delay = full_output
            .viewport_output
            .values()
            .map(|v| v.repaint_delay)
            .min();
        let needs_repaint = min_delay.map(|d| d.is_zero()).unwrap_or(false);
        self.repaint_delay = min_delay.filter(|d| !d.is_zero() && *d < Duration::from_secs(60));
        self.dirty = needs_repaint;

        let frame_tex = match self.wgpu_surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(tex) => tex,
            wgpu::CurrentSurfaceTexture::Suboptimal(tex) => {
                self.wgpu_surface.configure(&self.device, &self.surface_config);
                tex
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.wgpu_surface.configure(&self.device, &self.surface_config);
                self.dirty = true;
                return;
            }
            _ => return,
        };

        let view = frame_tex
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        for (id, deltas) in &full_output.textures_delta.set {
            for image_delta in deltas {
                self.renderer
                    .update_texture(&self.device, &self.queue, *id, image_delta);
            }
        }

        let paint_jobs = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [self.surface_config.width, self.surface_config.height],
            pixels_per_point: full_output.pixels_per_point,
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rocketde_tasks_encoder"),
            });
        let user_cmd_bufs = self.renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &paint_jobs,
            &screen_descriptor,
        );
        {
            let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rocketde_tasks_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.07,
                            g: 0.078,
                            b: 0.105,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let mut static_pass = render_pass.forget_lifetime();
            self.renderer
                .render(&mut static_pass, &paint_jobs, &screen_descriptor);
        }

        for id in &full_output.textures_delta.free {
            self.renderer.free_texture(id);
        }

        let mut all_cmds = user_cmd_bufs;
        all_cmds.push(encoder.finish());
        self.queue.submit(all_cmds);
        self.queue.present(frame_tex);
    }

    fn draw_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if let Some(dir) = detect_window_resize(&ctx, self.is_maximized) {
            self.pending_window_action = WindowAction::Resize(dir);
        }

        let accent = Color32::from_rgb(
            self.rocket_cfg.theme.accent_rgb[0],
            self.rocket_cfg.theme.accent_rgb[1],
            self.rocket_cfg.theme.accent_rgb[2],
        );
        let sidebar_bg = Color32::from_rgb(20, 22, 30);
        let toolbar_bg = Color32::from_rgb(24, 26, 36);
        let content_bg = Color32::from_rgb(18, 20, 27);
        let status_bg = Color32::from_rgb(22, 24, 33);
        let card_bg = Color32::from_rgb(24, 27, 38);
        let divider_col = Color32::from_rgba_unmultiplied(255, 255, 255, 14);
        let card_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 18));

        let mut actions: Vec<UiAction> = Vec::new();
        let mut jump_page: Option<TasksPage> = None;

        // 1. Left Sidebar (matching rocketde-settings & rocketde-files)
        egui::Panel::left("tasks_sidebar")
            .exact_size(220.0)
            .frame(
                Frame::NONE
                    .fill(sidebar_bg)
                    .inner_margin(Margin::symmetric(10, 12)),
            )
            .show(ui, |ui| {
                let panel_rect = ui.max_rect();
                ui.painter().line_segment(
                    [
                        Pos2::new(panel_rect.max.x + 10.0, panel_rect.min.y - 12.0),
                        Pos2::new(panel_rect.max.x + 10.0, panel_rect.max.y + 12.0),
                    ],
                    Stroke::new(1.0, divider_col),
                );

                ui.add_space(
                    (self.rocket_cfg.theme.spacing.header_height.clamp(32.0, 64.0) - 20.0).max(2.0),
                );
                ui.label(
                    RichText::new("MONITORAMENTO")
                        .size(10.5)
                        .strong()
                        .color(Color32::from_rgb(122, 132, 152)),
                );
                ui.add_space(6.0);

                for page in TasksPage::ALL {
                    let selected = self.page == page;
                    let (row_r, row_resp) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 30.0),
                        Sense::click(),
                    );
                    let row_bg = if selected {
                        Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 56)
                    } else if row_resp.hovered() {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 12)
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter()
                        .rect_filled(row_r, CornerRadius::same(7), row_bg);

                    let icon_col = if selected {
                        accent
                    } else {
                        Color32::from_rgb(185, 195, 215)
                    };
                    draw_icon(
                        ui.painter(),
                        Pos2::new(row_r.min.x + 15.0, row_r.center().y),
                        13.5,
                        page.icon(),
                        icon_col,
                    );

                    ui.painter().text(
                        Pos2::new(row_r.min.x + 32.0, row_r.center().y),
                        egui::Align2::LEFT_CENTER,
                        page.title(),
                        egui::FontId::proportional(13.0),
                        if selected {
                            Color32::WHITE
                        } else {
                            Color32::from_rgb(215, 222, 236)
                        },
                    );

                    if row_resp.clicked() {
                        self.page = page;
                        if page == TasksPage::Services && self.services.is_empty() && !self.services_loading {
                            self.request_services_refresh();
                        }
                    }
                    ui.add_space(1.5);
                }

                // Bottom sidebar pause/resume button
                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    let paused = self.collector.is_paused();
                    let (r, resp) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 32.0),
                        Sense::click(),
                    );
                    let bg = if resp.hovered() {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 22)
                    } else {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 11)
                    };
                    ui.painter().rect(
                        r,
                        CornerRadius::same(8),
                        bg,
                        Stroke::new(1.0, divider_col),
                        egui::StrokeKind::Inside,
                    );
                    draw_icon(
                        ui.painter(),
                        Pos2::new(r.min.x + 16.0, r.center().y),
                        13.0,
                        if paused { IconKind::Play } else { IconKind::Pause },
                        if paused { Color32::from_rgb(48, 209, 88) } else { Color32::from_rgb(200, 210, 228) },
                    );
                    ui.painter().text(
                        Pos2::new(r.min.x + 32.0, r.center().y),
                        egui::Align2::LEFT_CENTER,
                        if paused { "Retomar Coleta" } else { "Pausar Coleta" },
                        egui::FontId::proportional(12.5),
                        Color32::from_rgb(225, 232, 244),
                    );
                    if resp.clicked() {
                        self.collector.set_paused(!paused);
                    }
                });
            });

        // 2. Top Toolbar
        let header_h = self
            .rocket_cfg
            .theme
            .spacing
            .header_height
            .clamp(32.0, 64.0);
        egui::Panel::top("tasks_toolbar")
            .exact_size(header_h)
            .frame(
                Frame::NONE
                    .fill(toolbar_bg)
                    .inner_margin(Margin::symmetric(14, 5)),
            )
            .show(ui, |ui| {
                ui.style_mut().interaction.selectable_labels = false;
                let pr = ui.max_rect();
                let drag_action = header_bar_drag_area(
                    ui,
                    Rect::from_min_max(
                        Pos2::new(pr.min.x - 14.0, pr.min.y - 5.0),
                        Pos2::new(pr.max.x + 14.0, pr.max.y + 5.0),
                    ),
                );
                if drag_action != WindowAction::None {
                    self.pending_window_action = drag_action;
                }
                ui.painter().line_segment(
                    [
                        Pos2::new(pr.min.x - 14.0, pr.max.y + 5.0),
                        Pos2::new(pr.max.x + 14.0, pr.max.y + 5.0),
                    ],
                    Stroke::new(1.0, divider_col),
                );

                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let (ico_r, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                    draw_icon(ui.painter(), ico_r.center(), 14.0, self.page.icon(), accent);
                    ui.label(
                        RichText::new("Gerenciador de Tarefas")
                            .size(13.0)
                            .color(Color32::from_rgb(150, 160, 180)),
                    );
                    ui.label(
                        RichText::new("›")
                            .size(13.0)
                            .color(Color32::from_rgb(100, 110, 130)),
                    );
                    ui.label(
                        RichText::new(self.page.title())
                            .size(13.5)
                            .strong()
                            .color(Color32::WHITE),
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;

                        let win_act =
                            window_controls_right(ui, &self.rocket_cfg.theme, self.is_maximized);
                        if win_act != WindowAction::None {
                            self.pending_window_action = win_act;
                        }

                        ui.add_space(4.0);

                        Frame::NONE
                            .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 10))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(Margin::symmetric(8, 4))
                            .stroke(card_stroke)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 5.0;
                                    let (sr, _) = ui.allocate_exact_size(
                                        Vec2::new(13.0, 13.0),
                                        Sense::hover(),
                                    );
                                    draw_icon(
                                        ui.painter(),
                                        sr.center(),
                                        12.0,
                                        IconKind::Search,
                                        Color32::from_rgb(145, 155, 175),
                                    );
                                    let search_resp = ui.add(
                                        egui::TextEdit::singleline(&mut self.search_query)
                                            .frame(Frame::NONE)
                                            .hint_text("Filtrar (Ctrl+F)...")
                                            .desired_width(160.0),
                                    );
                                    if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(Key::F)) {
                                        search_resp.request_focus();
                                    }
                                    if ctx.input(|i| i.key_pressed(Key::Escape)) {
                                        if self.logs_modal.is_some() {
                                            self.logs_modal = None;
                                        } else if self.confirm_kill.is_some() {
                                            self.confirm_kill = None;
                                        } else if self.sudo_modal_open {
                                            self.sudo_modal_open = false;
                                            self.sudo_input.clear();
                                            self.pending_privileged = None;
                                        } else if !self.search_query.is_empty() {
                                            self.search_query.clear();
                                        }
                                    }
                                });
                            });
                    });
                });
            });

        // 3. Bottom Status Bar
        egui::Panel::bottom("tasks_status")
            .exact_size(28.0)
            .frame(
                Frame::NONE
                    .fill(status_bg)
                    .inner_margin(Margin::symmetric(14, 5)),
            )
            .show(ui, |ui| {
                let sr = ui.max_rect();
                ui.painter().line_segment(
                    [
                        Pos2::new(sr.min.x - 14.0, sr.min.y - 5.0),
                        Pos2::new(sr.max.x + 14.0, sr.min.y - 5.0),
                    ],
                    Stroke::new(1.0, divider_col),
                );
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(self.page.subtitle())
                            .size(11.5)
                            .color(Color32::from_rgb(155, 165, 185)),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some((ref msg, created)) = self.status_toast {
                            if created.elapsed() < Duration::from_secs(4) {
                                ui.label(
                                    RichText::new(msg)
                                        .size(11.5)
                                        .color(Color32::from_rgb(115, 225, 150)),
                                );
                                ctx.request_repaint_after(Duration::from_millis(250));
                                return;
                            } else {
                                self.status_toast = None;
                            }
                        }
                        let iec = self.cfg.show_iec_units;
                        ui.label(
                            RichText::new(format!(
                                "CPU {:.1}% · RAM Real {} / {} · {} processos",
                                self.telemetry_snap.cpu.total_pct,
                                format_bytes(self.telemetry_snap.mem.real_used_bytes, iec),
                                format_bytes(self.telemetry_snap.mem.total_bytes, iec),
                                self.telemetry_snap.processes.len()
                            ))
                            .size(11.5)
                            .color(Color32::from_rgb(145, 155, 175)),
                        );
                    });
                });
            });

        // 4. Main Content Area
        egui::CentralPanel::default()
            .frame(
                Frame::NONE
                    .fill(content_bg)
                    .inner_margin(Margin::symmetric(20, 16)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width((ui.available_width() - 4.0).max(200.0));
                        match self.page {
                            TasksPage::Overview => render_overview_page(
                                ui,
                                &self.telemetry_snap,
                                &self.cfg,
                                accent,
                                card_bg,
                                card_stroke,
                                &mut jump_page,
                            ),
                            TasksPage::Apps => render_apps_page(
                                ui,
                                &self.telemetry_snap,
                                &self.cfg,
                                &self.search_query,
                                &mut self.expanded_apps,
                                accent,
                                card_bg,
                                card_stroke,
                                &mut actions,
                            ),
                            TasksPage::Processes => render_processes_page(
                                ui,
                                &self.telemetry_snap,
                                &self.cfg,
                                &self.search_query,
                                &mut self.sort_col,
                                &mut self.sort_desc,
                                &mut self.selected_pid,
                                self.current_uid,
                                accent,
                                card_bg,
                                card_stroke,
                                &mut actions,
                            ),
                            TasksPage::Compute => render_compute_page(
                                ui,
                                &self.telemetry_snap,
                                &self.cfg,
                                accent,
                                card_bg,
                                card_stroke,
                            ),
                            TasksPage::Memory => render_memory_page(
                                ui,
                                &self.telemetry_snap,
                                &self.cfg,
                                accent,
                                card_bg,
                                card_stroke,
                            ),
                            TasksPage::StorageNet => render_storage_net_page(
                                ui,
                                &self.telemetry_snap,
                                &self.cfg,
                                accent,
                                card_bg,
                                card_stroke,
                            ),
                            TasksPage::Services => render_services_page(
                                ui,
                                &self.services,
                                &mut self.service_scope,
                                &mut self.service_filter,
                                &self.search_query,
                                self.services_loading,
                                self.services_err.as_deref(),
                                accent,
                                card_bg,
                                card_stroke,
                                &mut actions,
                            ),
                            TasksPage::Settings => render_settings_page(
                                ui,
                                &mut self.cfg,
                                self.sudo_password.is_some(),
                                accent,
                                card_bg,
                                card_stroke,
                                &mut actions,
                            ),
                        }
                    });
            });

        if let Some(p) = jump_page {
            self.page = p;
        }

        for act in actions {
            match act {
                UiAction::KillPid { pid, signal, label } => {
                    let op = PendingPrivileged::KillPid { pid, signal, label };
                    if self.cfg.confirm_before_kill && (signal == libc::SIGTERM || signal == libc::SIGKILL) {
                        self.confirm_kill = Some(op);
                    } else {
                        self.dispatch_privileged(op);
                    }
                }
                UiAction::KillGroup { pids, signal, app_name } => {
                    let op = PendingPrivileged::KillGroup { pids, signal, app_name };
                    if self.cfg.confirm_before_kill {
                        self.confirm_kill = Some(op);
                    } else {
                        self.dispatch_privileged(op);
                    }
                }
                UiAction::ServiceOp { unit, scope, action } => {
                    self.dispatch_privileged(PendingPrivileged::ServiceOp { unit, scope, action });
                }
                UiAction::FetchLogs { unit, scope } => {
                    self.dispatch_privileged(PendingPrivileged::FetchLogs { unit, scope });
                }
                UiAction::RefreshServices => {
                    self.request_services_refresh();
                }
                UiAction::SaveConfig => {
                    self.collector.set_refresh_ms(self.cfg.refresh_ms);
                    self.collector.set_history_cap(self.cfg.history_samples);
                    self.cfg.save();
                    self.set_toast("Configurações salvas");
                }
                UiAction::ClearSudoCache => {
                    self.sudo_password = None;
                    self.set_toast("Senha sudo removida da memória");
                }
            }
        }

        // Modals
        self.draw_modals(&ctx, accent, card_bg, card_stroke);
    }

    fn draw_modals(
        &mut self,
        ctx: &Context,
        accent: Color32,
        card_bg: Color32,
        card_stroke: Stroke,
    ) {
        if let Some(op) = self.confirm_kill.clone() {
            let desc = match &op {
                PendingPrivileged::KillPid { label, signal, .. } => {
                    let sig_name = if *signal == libc::SIGKILL { "SIGKILL (forçar)" } else { "SIGTERM" };
                    format!("Deseja enviar {sig_name} para {label}?")
                }
                PendingPrivileged::KillGroup { app_name, pids, .. } => {
                    format!("Deseja encerrar todos os {} processos de \"{app_name}\"?", pids.len())
                }
                _ => String::new(),
            };
            egui::Window::new("Confirmar Encerramento")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .frame(
                    Frame::NONE
                        .fill(card_bg)
                        .stroke(card_stroke)
                        .corner_radius(CornerRadius::same(12))
                        .inner_margin(Margin::same(18)),
                )
                .show(ctx, |ui| {
                    ui.label(RichText::new(desc).size(13.0).color(Color32::WHITE));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if subtle_btn(ui, "Cancelar").clicked() {
                            self.confirm_kill = None;
                        }
                        if pill_btn(ui, "Confirmar", Color32::from_rgb(255, 69, 58)).clicked() {
                            self.confirm_kill = None;
                            self.dispatch_privileged(op);
                        }
                    });
                });
        }

        if self.sudo_modal_open {
            egui::Window::new("Autenticação Necessária (sudo)")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .frame(
                    Frame::NONE
                        .fill(card_bg)
                        .stroke(card_stroke)
                        .corner_radius(CornerRadius::same(12))
                        .inner_margin(Margin::same(18)),
                )
                .show(ctx, |ui| {
                    ui.set_width(340.0);
                    ui.label(
                        RichText::new("Esta ação requer privilégios administrativos. Digite sua senha sudo:")
                            .size(12.5)
                            .color(Color32::from_rgb(210, 220, 236)),
                    );
                    ui.add_space(8.0);
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.sudo_input)
                            .password(true)
                            .desired_width(320.0)
                            .hint_text("Senha do usuário..."),
                    );
                    resp.request_focus();
                    if let Some(ref err) = self.sudo_error {
                        ui.add_space(4.0);
                        ui.label(RichText::new(err).size(11.5).color(Color32::from_rgb(255, 69, 58)));
                    }
                    ui.add_space(12.0);
                    let submit = ui.input(|i| i.key_pressed(Key::Enter));
                    ui.horizontal(|ui| {
                        if subtle_btn(ui, "Cancelar").clicked() {
                            self.sudo_modal_open = false;
                            self.sudo_input.clear();
                            self.sudo_error = None;
                            self.pending_privileged = None;
                        }
                        if pill_btn(ui, "Autenticar", accent).clicked() || submit {
                            let pw = std::mem::take(&mut self.sudo_input);
                            self.sudo_password = Some(pw);
                            self.sudo_modal_open = false;
                            self.sudo_error = None;
                            if let Some(op) = self.pending_privileged.take() {
                                self.dispatch_privileged(op);
                            }
                        }
                    });
                });
        }

        if let Some((unit, content)) = self.logs_modal.clone() {
            egui::Window::new(format!("Logs: {unit}"))
                .collapsible(false)
                .resizable(true)
                .default_size(Vec2::new(720.0, 440.0))
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .frame(
                    Frame::NONE
                        .fill(card_bg)
                        .stroke(card_stroke)
                        .corner_radius(CornerRadius::same(12))
                        .inner_margin(Margin::same(16)),
                )
                .show(ctx, |ui| {
                    egui::ScrollArea::both()
                        .max_height(360.0)
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(&content)
                                    .size(11.5)
                                    .monospace()
                                    .color(Color32::from_rgb(215, 224, 238)),
                            );
                        });
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if subtle_btn(ui, "Copiar Logs").clicked() {
                            ctx.copy_text(content.clone());
                            self.set_toast("Logs copiados para a área de transferência");
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if pill_btn(ui, "Fechar", accent).clicked() {
                                self.logs_modal = None;
                            }
                        });
                    });
                });
        }
    }
}

fn main() {
    let conn = Connection::connect_to_env().expect("Falha ao conectar ao Wayland");
    let (globals, mut event_queue) =
        registry_queue_init(&conn).expect("Falha ao inicializar fila Wayland");
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor indisponível");
    let xdg_shell = XdgShell::bind(&globals, &qh).expect("xdg_wm_base indisponível");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm indisponível");

    let wl_surface = compositor.create_surface(&qh);
    let window = xdg_shell.create_window(wl_surface, WindowDecorations::RequestClient, &qh);
    window.set_title("Gerenciador de Tarefas do RocketDE");
    window.set_app_id("rocketde-tasks");
    window.set_min_size(Some((820, 560)));
    window.commit();

    let mut instance_desc = wgpu::InstanceDescriptor::new_without_display_handle();
    instance_desc.backends = wgpu::Backends::VULKAN;
    let mut instance = wgpu::Instance::new(instance_desc);

    let raw_display_ptr = NonNull::new(conn.backend().display_ptr() as *mut _).unwrap();
    let raw_surface_ptr = NonNull::new(window.wl_surface().id().as_ptr() as *mut _).unwrap();
    let create_surface = |inst: &wgpu::Instance| unsafe {
        inst.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                raw_display_ptr,
            ))),
            raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(
                raw_surface_ptr,
            )),
        })
        .expect("Falha ao criar superfície wgpu")
    };

    let mut wgpu_surface = create_surface(&instance);
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        force_fallback_adapter: false,
        compatible_surface: Some(&wgpu_surface),
        apply_limit_buckets: false,
    })) {
        Ok(a) => a,
        Err(_) => {
            drop(wgpu_surface);
            let mut gl_desc = wgpu::InstanceDescriptor::new_without_display_handle();
            gl_desc.backends = wgpu::Backends::GL;
            instance = wgpu::Instance::new(gl_desc);
            wgpu_surface = create_surface(&instance);
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: false,
                compatible_surface: Some(&wgpu_surface),
                apply_limit_buckets: false,
            }))
            .expect("Nenhum adaptador gráfico encontrado")
        }
    };

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("rocketde_tasks_device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
    }))
    .expect("Erro ao requisitar device wgpu");

    let caps = wgpu_surface.get_capabilities(&adapter);
    let format = caps
        .formats
        .iter()
        .copied()
        .find(|f| {
            matches!(
                f,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
            )
        })
        .unwrap_or(wgpu::TextureFormat::Bgra8Unorm);
    let alpha_mode = caps
        .alpha_modes
        .first()
        .copied()
        .unwrap_or(wgpu::CompositeAlphaMode::Auto);

    let init_w = 1080u32;
    let init_h = 700u32;
    let surface_config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space: wgpu::SurfaceColorSpace::Auto,
        width: init_w,
        height: init_h,
        present_mode: wgpu::PresentMode::Fifo,
        alpha_mode,
        view_formats: vec![],
        desired_maximum_frame_latency: 1,
    };

    let rocket_cfg = RocketConfig::load_or_create();
    let cfg = TasksConfig::load_or_create();
    let egui_ctx = Context::default();
    apply_macos_theme(&egui_ctx, &rocket_cfg);
    let renderer = Renderer::new(&device, format, RendererOptions::default());

    let mut wake_fds = [-1i32; 2];
    unsafe {
        libc::pipe2(wake_fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let wake_rd = wake_fds[0];
    let wake_wr = wake_fds[1];
    let wake_fn: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(move || {
        if wake_wr >= 0 {
            let b: u8 = 1;
            unsafe {
                libc::write(wake_wr, (&b as *const u8).cast::<libc::c_void>(), 1);
            }
        }
    });

    let collector = TelemetryCollector::start(cfg.refresh_ms, cfg.history_samples, {
        let w = wake_fn.clone();
        move || w()
    });
    let (bg_tx, bg_rx) = mpsc::channel();
    let current_uid = unsafe { libc::getuid() };

    let mut app = TasksApp {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        _compositor: compositor,
        shm,
        _window: window,
        device,
        queue,
        wgpu_surface,
        surface_config,
        egui_ctx,
        renderer,
        width: init_w,
        height: init_h,
        scale_factor: 1.0,
        configured: false,
        dirty: true,
        repaint_delay: None,
        pending_events: Vec::with_capacity(32),
        modifiers: Modifiers::NONE,
        keyboard: None,
        pointer: None,
        seat: None,
        last_pointer_serial: 0,
        is_maximized: false,
        pending_window_action: WindowAction::None,
        start_instant: Instant::now(),
        exit: false,
        rocket_cfg,
        cfg,
        collector,
        telemetry_snap: SystemTelemetry::default(),
        current_uid,
        page: TasksPage::Overview,
        search_query: String::new(),
        expanded_apps: HashSet::new(),
        sort_col: SortCol::Ram,
        sort_desc: true,
        selected_pid: None,
        service_scope: ServiceScope::System,
        service_filter: ServiceFilter::All,
        services: Vec::new(),
        services_loading: false,
        services_err: None,
        bg_tx,
        bg_rx,
        wake_fn,
        sudo_password: None,
        sudo_modal_open: false,
        sudo_input: String::new(),
        sudo_error: None,
        pending_privileged: None,
        confirm_kill: None,
        logs_modal: None,
        status_toast: None,
    };

    let wayland_fd = conn.as_fd();
    loop {
        let _ = conn.flush();
        if let Some(guard) = conn.prepare_read() {
            let mut pfds = [
                libc::pollfd {
                    fd: wayland_fd.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: wake_rd,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let nfds = if wake_rd >= 0 { 2 } else { 1 };
            let timeout_ms = if app.dirty || !app.pending_events.is_empty() {
                16
            } else if let Some(d) = app.repaint_delay {
                d.as_millis().clamp(16, 1000) as i32
            } else {
                -1
            };
            let ret = unsafe { libc::poll(pfds.as_mut_ptr(), nfds, timeout_ms) };
            if ret > 0 && (pfds[0].revents & libc::POLLIN) != 0 {
                let _ = guard.read();
            } else {
                drop(guard);
                if ret == 0 && app.repaint_delay.is_some() {
                    app.dirty = true;
                }
            }
            if wake_rd >= 0 && ret > 0 && (pfds[1].revents & libc::POLLIN) != 0 {
                let mut buf = [0u8; 64];
                while unsafe {
                    libc::read(wake_rd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len())
                } > 0
                {}
                app.dirty = true;
            }
        }
        let _ = event_queue.dispatch_pending(&mut app);
        if app.exit {
            break;
        }
        if app.dirty || !app.pending_events.is_empty() {
            app.render_frame();
        }
    }
}

impl CompositorHandler for TasksApp {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        let factor = new_factor.max(1);
        if (self.scale_factor - factor as f32).abs() > f32::EPSILON {
            self.scale_factor = factor as f32;
            surface.set_buffer_scale(factor);
            if self.configured {
                self.resize(self.width, self.height);
            }
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.dirty = true;
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for TasksApp {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output_state }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for TasksApp {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &Window,
        configure: WindowConfigure,
        _: u32,
    ) {
        let w = configure.new_size.0.map(NonZeroU32::get).unwrap_or(1080);
        let h = configure.new_size.1.map(NonZeroU32::get).unwrap_or(700);
        self.is_maximized = configure.is_maximized();
        self.resize(w, h);
        self.render_frame();
    }
}

impl SeatHandler for TasksApp {
    fn seat_state(&mut self) -> &mut SeatState { &mut self.seat_state }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        self.seat = Some(seat.clone());
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            if let Ok(kb) = self.seat_state.get_keyboard(qh, &seat, None) {
                self.keyboard = Some(kb);
            }
        }
        if cap == Capability::Pointer && self.pointer.is_none() {
            if let Ok(ptr) = self.seat_state.get_pointer(qh, &seat) {
                self.pointer = Some(ptr);
            }
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard {
            if let Some(kb) = self.keyboard.take() { kb.release(); }
        }
        if cap == Capability::Pointer {
            if let Some(ptr) = self.pointer.take() { ptr.release(); }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for TasksApp {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.push_key(event, true);
    }
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.push_key(event, true);
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.push_key(event, false);
    }
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, mods: WlModifiers, _: RawModifiers, _: u32) {
        self.modifiers = Modifiers {
            alt: mods.alt,
            ctrl: mods.ctrl,
            shift: mods.shift,
            mac_cmd: mods.logo,
            command: mods.ctrl || mods.logo,
        };
    }
}

impl TasksApp {
    fn push_key(&mut self, event: KeyEvent, pressed: bool) {
        if let Some(key) = translate_keysym(event.keysym) {
            self.pending_events.push(Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: self.modifiers,
            });
            push_clipboard_events(key, pressed, self.modifiers, &mut self.pending_events);
        }
        if pressed && !self.modifiers.ctrl && !self.modifiers.alt && !self.modifiers.mac_cmd {
            if let Some(txt) = &event.utf8 {
                let filtered: String = txt.chars().filter(|c| !c.is_control()).collect();
                if !filtered.is_empty() {
                    self.pending_events.push(Event::Text(filtered));
                }
            }
        }
        self.dirty = true;
    }
}

impl PointerHandler for TasksApp {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for ev in events {
            let pos = Pos2::new(ev.position.0 as f32, ev.position.1 as f32);
            match ev.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    self.pending_events.push(Event::PointerMoved(pos));
                }
                PointerEventKind::Leave { .. } => {
                    self.pending_events.push(Event::PointerGone);
                }
                PointerEventKind::Press { button, serial, .. } => {
                    self.last_pointer_serial = serial;
                    if let Some(btn) = translate_btn(button) {
                        self.pending_events.push(Event::PointerButton {
                            pos,
                            button: btn,
                            pressed: true,
                            modifiers: self.modifiers,
                        });
                    }
                }
                PointerEventKind::Release { button, .. } => {
                    if let Some(btn) = translate_btn(button) {
                        self.pending_events.push(Event::PointerButton {
                            pos,
                            button: btn,
                            pressed: false,
                            modifiers: self.modifiers,
                        });
                    }
                }
                PointerEventKind::Axis { horizontal, vertical, .. } => {
                    self.pending_events.push(Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: Vec2::new(-horizontal.absolute as f32, -vertical.absolute as f32),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Unknown,
                        modifiers: self.modifiers,
                    });
                }
            }
            self.dirty = true;
        }
    }
}

fn to_xdg_edge(
    dir: ResizeDir,
) -> smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge {
    use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge;
    match dir {
        ResizeDir::Top => ResizeEdge::Top,
        ResizeDir::Bottom => ResizeEdge::Bottom,
        ResizeDir::Left => ResizeEdge::Left,
        ResizeDir::Right => ResizeEdge::Right,
        ResizeDir::TopLeft => ResizeEdge::TopLeft,
        ResizeDir::TopRight => ResizeEdge::TopRight,
        ResizeDir::BottomLeft => ResizeEdge::BottomLeft,
        ResizeDir::BottomRight => ResizeEdge::BottomRight,
    }
}

fn translate_btn(b: u32) -> Option<PointerButton> {
    match b {
        0x110 => Some(PointerButton::Primary),
        0x111 => Some(PointerButton::Secondary),
        0x112 => Some(PointerButton::Middle),
        _ => None,
    }
}

fn translate_keysym(sym: Keysym) -> Option<Key> {
    rocketde_ui_common::keysym::translate_keysym_raw(sym.raw())
}

impl ShmHandler for TasksApp {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}

impl ProvidesRegistryState for TasksApp {
    fn registry(&mut self) -> &mut RegistryState { &mut self.registry_state }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(TasksApp);
delegate_registry!(TasksApp);
