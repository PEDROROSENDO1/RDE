use egui::{
    Align, Color32, Context, CornerRadius, Event, FontId, Frame, Key, Layout, Margin, Modifiers,
    PointerButton, Pos2, RawInput, Rect, RichText, Sense, Stroke, Vec2,
};
use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use rocketde_core::config::{OverviewPreviewMode, RocketConfig};
use rocketde_core::hardware::HardwareController;
use rocketde_core::ipc::{IpcCommand, send_command_to_daemon};
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
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::os::fd::AsFd;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::process::Command;
use std::ptr::NonNull;
use std::time::{Duration, Instant};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    Appearance,
    Desktop,
    TopBar,
    Dock,
    OverviewLauncher,
    Capture,
    Hardware,
    About,
}

impl SettingsPage {
    const ALL: [Self; 8] = [
        Self::Appearance,
        Self::Desktop,
        Self::TopBar,
        Self::Dock,
        Self::OverviewLauncher,
        Self::Capture,
        Self::Hardware,
        Self::About,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Aparência & Tema",
            Self::Desktop => "Área de Trabalho",
            Self::TopBar => "Barra Superior",
            Self::Dock => "Dock & Favoritos",
            Self::OverviewLauncher => "Overview & Launchpad",
            Self::Capture => "Captura & Gravação",
            Self::Hardware => "Áudio, Brilho & Energia",
            Self::About => "Sobre o RocketDE",
        }
    }

    fn subtitle(self) -> &'static str {
        match self {
            Self::Appearance => "Cor de destaque, transparência e estilo",
            Self::Desktop => "Papel de parede em ~/.rocket/backgrounds/ (preencher tela)",
            Self::TopBar => "Altura, relógio, menus globais e workspaces",
            Self::Dock => "Tamanho, ampliação e apps fixados",
            Self::OverviewLauncher => "Animações, preview de janelas e grade",
            Self::Capture => "FPS, VA-API, clipboard e pastas",
            Self::Hardware => "Volume até 150%, brilho, Wi-Fi e energia",
            Self::About => "Versão, caminho da config e padrões",
        }
    }

    fn keywords(self) -> &'static str {
        match self {
            Self::Appearance => "cor accent tema blur opacidade fonte vidro transparencia",
            Self::Desktop => "wallpaper fundo papel de parede imagem",
            Self::TopBar => "barra relogio formato clock menu workspace",
            Self::Dock => "dock icones tamanho zoom ampliacao auto ocultar fixados",
            Self::OverviewLauncher => "overview launchpad grade animacao janela",
            Self::Capture => "screenshot gravacao tela video fps vaapi clipboard pasta",
            Self::Hardware => "volume som microfone brilho tela wifi bluetooth energia bateria",
            Self::About => "versao config resetar padroes fabrica",
        }
    }

    fn icon(self) -> IconKind {
        match self {
            Self::Appearance => IconKind::ControlCenter,
            Self::Desktop => IconKind::OpenApp,
            Self::TopBar => IconKind::Sun,
            Self::Dock => IconKind::Rocket,
            Self::OverviewLauncher => IconKind::Star,
            Self::Capture => IconKind::Camera,
            Self::Hardware => IconKind::VolumeHigh,
            Self::About => IconKind::Terminal,
        }
    }
}

struct SettingsApp {
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
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    seat: Option<wl_seat::WlSeat>,
    last_pointer_serial: u32,
    is_maximized: bool,
    pending_window_action: WindowAction,
    start_instant: Instant,
    exit: bool,

    // Settings UI state
    page: SettingsPage,
    search_query: String,
    cfg: RocketConfig,
    hw: HardwareController,
    screenshots_dir_buf: String,
    recordings_dir_buf: String,
    status_toast: Option<(String, Instant)>,
    pending_save: bool,
    last_change_at: Option<Instant>,
    reset_confirm: bool,
    backgrounds: Vec<PathBuf>,
    icon_packs: Vec<String>,
    wallpaper_thumbs: HashMap<PathBuf, Option<egui::TextureHandle>>,
    thumb_req_tx: std::sync::mpsc::Sender<PathBuf>,
    thumb_res_rx: std::sync::mpsc::Receiver<(PathBuf, Option<egui::ColorImage>)>,
}

impl SettingsApp {
    fn sync_dir_buffers(&mut self) {
        self.screenshots_dir_buf = self.cfg.capture.screenshots_dir.clone().unwrap_or_default();
        self.recordings_dir_buf = self.cfg.capture.recordings_dir.clone().unwrap_or_default();
    }

    fn mark_changed(&mut self) {
        self.pending_save = true;
        self.last_change_at = Some(Instant::now());
        apply_macos_theme(&self.egui_ctx, &self.cfg);
    }

    fn save_and_reload(&mut self, toast_msg: &str) {
        self.cfg.capture.screenshots_dir = {
            let t = self.screenshots_dir_buf.trim();
            if t.is_empty() { None } else { Some(t.to_string()) }
        };
        self.cfg.capture.recordings_dir = {
            let t = self.recordings_dir_buf.trim();
            if t.is_empty() { None } else { Some(t.to_string()) }
        };
        let msg = match self.cfg.try_save() {
            Ok(()) => {
                let ok = send_command_to_daemon(&IpcCommand::ReloadConfig).unwrap_or(false);
                if ok {
                    toast_msg.to_string()
                } else {
                    format!("{toast_msg} (salvo em disco)")
                }
            }
            Err(err) => {
                eprintln!("[rocketde-settings] Save failed: {err}");
                format!("Erro ao salvar: {err}")
            }
        };
        self.status_toast = Some((msg, Instant::now()));
        self.pending_save = false;
        self.last_change_at = None;
        self.dirty = true;
    }

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

    fn render_frame(&mut self) {
        if !self.configured {
            return;
        }
        while let Ok((path, img_opt)) = self.thumb_res_rx.try_recv() {
            let tex = img_opt.map(|color_img| {
                self.egui_ctx.load_texture(
                    format!("wp_thumb_{}", path.display()),
                    color_img,
                    egui::TextureOptions::LINEAR,
                )
            });
            self.wallpaper_thumbs.insert(path, tex);
        }
        self.egui_ctx.set_pixels_per_point(self.scale_factor);

        if self.pending_save
            && self
                .last_change_at
                .is_some_and(|t| t.elapsed() >= Duration::from_millis(220))
        {
            self.save_and_reload("Aplicado ao vivo");
        }

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
                    self._window.resize(seat, self.last_pointer_serial, to_xdg_edge(dir));
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
                label: Some("rocketde_settings_encoder"),
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
                label: Some("rocketde_settings_pass"),
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
            self.cfg.theme.accent_rgb[0],
            self.cfg.theme.accent_rgb[1],
            self.cfg.theme.accent_rgb[2],
        );
        let sidebar_bg = Color32::from_rgb(20, 22, 30);
        let toolbar_bg = Color32::from_rgb(24, 26, 36);
        let content_bg = Color32::from_rgb(18, 20, 27);
        let status_bg = Color32::from_rgb(22, 24, 33);
        let card_bg = Color32::from_rgb(24, 27, 38);
        let divider_col = Color32::from_rgba_unmultiplied(255, 255, 255, 14);
        let card_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 18));

        // 1. Left Sidebar (matching rocketde-files)
        egui::Panel::left("settings_sidebar")
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
                    (self.cfg.theme.spacing.header_height.clamp(32.0, 64.0) - 20.0).max(2.0),
                );
                ui.label(
                    RichText::new("CATEGORIAS")
                        .size(10.5)
                        .strong()
                        .color(Color32::from_rgb(122, 132, 152)),
                );
                ui.add_space(6.0);

                let q = self.search_query.trim().to_lowercase();
                let mut shown_pages = 0usize;
                for page in SettingsPage::ALL {
                    if !q.is_empty()
                        && !page.title().to_lowercase().contains(&q)
                        && !page.subtitle().to_lowercase().contains(&q)
                        && !page.keywords().contains(&q)
                    {
                        continue;
                    }
                    shown_pages += 1;
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
                        FontId::proportional(13.0),
                        if selected {
                            Color32::WHITE
                        } else {
                            Color32::from_rgb(215, 222, 236)
                        },
                    );

                    if row_resp.clicked() {
                        self.page = page;
                        self.hw.set_detailed_active(page == SettingsPage::Hardware);
                    }
                    ui.add_space(1.5);
                }
                if shown_pages == 0 {
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("Nenhuma categoria encontrada.")
                            .size(11.5)
                            .color(Color32::from_rgb(138, 148, 168)),
                    );
                    if ui.small_button("Limpar busca").clicked() {
                        self.search_query.clear();
                    }
                }
            });

        // 2. Top Toolbar (matching rocketde-files)
        let header_h = self.cfg.theme.spacing.header_height.clamp(32.0, 64.0);
        egui::Panel::top("settings_toolbar")
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
                    draw_icon(
                        ui.painter(),
                        ico_r.center(),
                        14.0,
                        self.page.icon(),
                        accent,
                    );
                    ui.label(
                        RichText::new("Configurações")
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

                        let win_act = window_controls_right(ui, &self.cfg.theme, self.is_maximized);
                        if win_act != WindowAction::None {
                            self.pending_window_action = win_act;
                        }

                        ui.add_space(4.0);

                        if pill_btn_col(ui, "Salvar & Aplicar", accent).clicked() {
                            self.save_and_reload("Configurações aplicadas!");
                        }

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
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.search_query)
                                            .frame(Frame::NONE)
                                            .hint_text("Buscar ajuste...")
                                            .desired_width(130.0),
                                    );
                                });
                            });
                    });
                });
            });

        // 3. Bottom Status Bar (matching rocketde-files)
        egui::Panel::bottom("settings_status")
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
                    if let Some((ref msg, created)) = self.status_toast {
                        if created.elapsed() < Duration::from_secs(3) {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(
                                    RichText::new(msg)
                                        .size(11.5)
                                        .color(Color32::from_rgb(115, 225, 150)),
                                );
                            });
                            ctx.request_repaint_after(Duration::from_millis(250));
                        } else {
                            self.status_toast = None;
                        }
                    }
                });
            });

        // 4. Right main content area
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
                        SettingsPage::Appearance => {
                            self.render_appearance_page(ui, accent, card_bg, card_stroke)
                        }
                        SettingsPage::Desktop => {
                            self.render_desktop_page(ui, accent, card_bg, card_stroke)
                        }
                        SettingsPage::TopBar => {
                            self.render_topbar_page(ui, accent, card_bg, card_stroke)
                        }
                        SettingsPage::Dock => {
                            self.render_dock_page(ui, accent, card_bg, card_stroke)
                        }
                        SettingsPage::OverviewLauncher => {
                            self.render_overview_launcher_page(ui, accent, card_bg, card_stroke)
                        }
                        SettingsPage::Capture => {
                            self.render_capture_page(ui, accent, card_bg, card_stroke)
                        }
                        SettingsPage::Hardware => {
                            self.render_hardware_page(ui, accent, card_bg, card_stroke)
                        }
                        SettingsPage::About => {
                            self.render_about_page(ui, accent, card_bg, card_stroke)
                        }
                        }
                    });
            });
    }

    fn ensure_wallpaper_thumb(&mut self, path: &PathBuf) -> Option<egui::TextureId> {
        if !self.wallpaper_thumbs.contains_key(path) {
            self.wallpaper_thumbs.insert(path.clone(), None);
            let _ = self.thumb_req_tx.send(path.clone());
        }
        self.wallpaper_thumbs
            .get(path)
            .and_then(|o| o.as_ref())
            .map(|t| t.id())
    }

    fn render_desktop_page(
        &mut self,
        ui: &mut egui::Ui,
        accent: Color32,
        card_bg: Color32,
        card_stroke: Stroke,
    ) {
        let bg_dir = RocketConfig::backgrounds_dir();
        let active_path = self.cfg.desktop.wallpaper_path.clone();

        prefs_group(ui, "Papel de Parede Ativo", card_bg, card_stroke, |ui| {
            ui.horizontal(|ui| {
                let preview_size = egui::vec2(176.0, 110.0);
                let (preview_rect, _) =
                    ui.allocate_exact_size(preview_size, egui::Sense::hover());
                ui.painter().rect_filled(
                    preview_rect,
                    CornerRadius::same(10),
                    Color32::from_rgb(24, 28, 36),
                );
                if let Some(active) = active_path.as_ref().map(PathBuf::from) {
                    if let Some(tex_id) = self.ensure_wallpaper_thumb(&active) {
                        ui.painter().image(
                            tex_id,
                            preview_rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                }
                ui.painter().rect_stroke(
                    preview_rect,
                    CornerRadius::same(10),
                    Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 36)),
                    egui::StrokeKind::Inside,
                );

                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    let current_name = active_path
                        .as_deref()
                        .and_then(|p| std::path::Path::new(p).file_name())
                        .and_then(|n| n.to_str())
                        .unwrap_or("Fundo padrão do sistema");
                    ui.label(
                        RichText::new(current_name)
                            .size(14.0)
                            .strong()
                            .color(Color32::WHITE),
                    );
                    ui.label(
                        RichText::new(format!("Pasta: {}", bg_dir.display()))
                            .size(11.5)
                            .color(Color32::from_rgb(155, 165, 185)),
                    );
                    ui.label(
                        RichText::new("Modo: Esticado para preencher a tela inteira")
                            .size(11.5)
                            .color(Color32::from_rgb(135, 195, 235)),
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        if pill_btn_col(ui, "Recarregar pasta", Color32::from_rgba_unmultiplied(255, 255, 255, 18)).clicked() {
                            self.backgrounds = RocketConfig::list_backgrounds();
                            self.wallpaper_thumbs.clear();
                        }
                        if pill_btn_col(ui, "Abrir pasta", Color32::from_rgba_unmultiplied(255, 255, 255, 18)).clicked() {
                            let _ = std::fs::create_dir_all(&bg_dir);
                            let _ = Command::new("xdg-open").arg(&bg_dir).spawn();
                        }
                        if active_path.is_some()
                            && pill_btn_col(ui, "Remover", Color32::from_rgba_unmultiplied(255, 69, 58, 160)).clicked()
                        {
                            self.cfg.desktop.wallpaper_path = None;
                            self.save_and_reload("Papel de parede removido");
                        }
                    });
                });
            });
        });

        ui.add_space(12.0);

        prefs_group(
            ui,
            "Galeria (~/.rocket/backgrounds/)",
            card_bg,
            card_stroke,
            |ui| {
                let paths = self.backgrounds.clone();
                if paths.is_empty() {
                    ui.label(
                        RichText::new(format!(
                            "Nenhuma imagem encontrada em {}.",
                            bg_dir.display()
                        ))
                        .size(13.0)
                        .color(Color32::from_rgb(180, 188, 204)),
                    );
                    ui.label(
                        RichText::new(
                            "Adicione arquivos .png, .jpg, .jpeg, .webp, .bmp ou .gif nessa pasta e clique em \"Recarregar pasta\".",
                        )
                        .size(11.5)
                        .color(Color32::from_rgb(140, 150, 170)),
                    );
                    return;
                }

                let avail_w = ui.available_width();
                let spacing = 12.0;
                let cols = ((avail_w / 180.0).floor() as usize).clamp(1, 3);
                let card_w = ((avail_w - spacing * (cols as f32 - 1.0)) / cols as f32).max(120.0);
                let thumb_h = (card_w * 0.625).round();

                for row in paths.chunks(cols) {
                    ui.horizontal(|ui| {
                        for (idx, path) in row.iter().enumerate() {
                            if idx > 0 {
                                ui.add_space(spacing);
                            }
                            let path_str = path.to_string_lossy().to_string();
                            let is_selected = active_path.as_deref() == Some(path_str.as_str());
                            let tex_id = self.ensure_wallpaper_thumb(path);

                            let (card_rect, resp) = ui.allocate_exact_size(
                                egui::vec2(card_w, thumb_h + 26.0),
                                egui::Sense::click(),
                            );
                            let thumb_rect = egui::Rect::from_min_size(
                                card_rect.min,
                                egui::vec2(card_w, thumb_h),
                            );

                            let bg_col = if is_selected {
                                accent.linear_multiply(0.25)
                            } else if resp.hovered() {
                                Color32::from_rgba_unmultiplied(255, 255, 255, 20)
                            } else {
                                Color32::from_rgba_unmultiplied(255, 255, 255, 10)
                            };
                            ui.painter().rect_filled(card_rect, CornerRadius::same(10), bg_col);

                            if let Some(tex_id) = tex_id {
                                ui.painter().image(
                                    tex_id,
                                    thumb_rect.shrink(3.0),
                                    egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    ),
                                    Color32::WHITE,
                                );
                            } else {
                                ui.painter().rect_filled(
                                    thumb_rect.shrink(3.0),
                                    CornerRadius::same(8),
                                    Color32::from_rgb(30, 34, 42),
                                );
                            }

                            let border = if is_selected {
                                Stroke::new(2.0, accent)
                            } else if resp.hovered() {
                                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 60))
                            } else {
                                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 22))
                            };
                            ui.painter().rect_stroke(
                                card_rect,
                                CornerRadius::same(10),
                                border,
                                egui::StrokeKind::Inside,
                            );

                            let name = path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("imagem");
                            let name_clip = Rect::from_min_max(
                                Pos2::new(card_rect.min.x + 6.0, thumb_rect.max.y),
                                Pos2::new(card_rect.max.x - 6.0, card_rect.max.y),
                            );
                            ui.painter().with_clip_rect(name_clip).text(
                                Pos2::new(name_clip.min.x + 2.0, name_clip.center().y),
                                egui::Align2::LEFT_CENTER,
                                name,
                                FontId::proportional(11.5),
                                if is_selected {
                                    Color32::WHITE
                                } else {
                                    Color32::from_rgb(200, 208, 220)
                                },
                            );

                            if resp.clicked() {
                                self.cfg.desktop.wallpaper_path = Some(path_str);
                                self.save_and_reload("Papel de parede atualizado!");
                            }
                        }
                    });
                    ui.add_space(10.0);
                }
            },
        );
    }

    fn render_appearance_page(
        &mut self,
        ui: &mut egui::Ui,
        accent: Color32,
        card_bg: Color32,
        card_stroke: Stroke,
    ) {
        let mut changed = false;
        prefs_group(ui, "Cor de Destaque (Accent)", card_bg, card_stroke, |ui| {
            let presets: [(&str, [u8; 3]); 6] = [
                ("Azul macOS", [10, 132, 255]),
                ("Roxo", [175, 82, 222]),
                ("Verde", [48, 209, 88]),
                ("Laranja", [255, 149, 0]),
                ("Rosa", [255, 55, 95]),
                ("Ciano", [50, 173, 230]),
            ];
            ui.horizontal_wrapped(|ui| {
                for (name, rgb) in presets {
                    let col = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                    let selected = self.cfg.theme.accent_rgb == rgb;
                    let (r, resp) =
                        ui.allocate_exact_size(Vec2::new(30.0, 30.0), Sense::click());
                    ui.painter().circle_filled(r.center(), 12.0, col);
                    if selected {
                        ui.painter().circle_stroke(
                            r.center(),
                            15.0,
                            Stroke::new(2.0, Color32::WHITE),
                        );
                    }
                    if resp.on_hover_text(name).clicked() {
                        self.cfg.theme.accent_rgb = rgb;
                        changed = true;
                    }
                }
            });
        });

        ui.add_space(12.0);

        prefs_group(ui, "Transparência das Superfícies", card_bg, card_stroke, |ui| {
            let mut bar_a = self.cfg.theme.bar_bg_rgba[3] as f32;
            if slider_row(ui, "Opacidade da Barra Superior", &mut bar_a, 80.0..=255.0, accent, |v| {
                format!("{:.0}%", v / 255.0 * 100.0)
            }) {
                self.cfg.theme.bar_bg_rgba[3] = bar_a.round() as u8;
                changed = true;
            }
            card_divider(ui);
            let mut dock_a = self.cfg.theme.dock_bg_rgba[3] as f32;
            if slider_row(ui, "Opacidade da Dock", &mut dock_a, 80.0..=255.0, accent, |v| {
                format!("{:.0}%", v / 255.0 * 100.0)
            }) {
                self.cfg.theme.dock_bg_rgba[3] = dock_a.round() as u8;
                changed = true;
            }
            card_divider(ui);
            let mut panel_a = self.cfg.theme.panel_bg_rgba[3] as f32;
            if slider_row(ui, "Opacidade dos Painéis & Popups", &mut panel_a, 120.0..=255.0, accent, |v| {
                format!("{:.0}%", v / 255.0 * 100.0)
            }) {
                self.cfg.theme.panel_bg_rgba[3] = panel_a.round() as u8;
                changed = true;
            }
        });

        ui.add_space(12.0);

        prefs_group(ui, "Janelas & HeaderBar (theme.ron)", card_bg, card_stroke, |ui| {
            let mut hdr_h = self.cfg.theme.spacing.header_height;
            if slider_row(ui, "Altura da HeaderBar", &mut hdr_h, 34.0..=56.0, accent, |v| {
                format!("{:.0} px", v)
            }) {
                self.cfg.theme.spacing.header_height = hdr_h.round();
                changed = true;
            }
        });

        if changed {
            self.mark_changed();
        }

        ui.add_space(12.0);

        let icons_dir = RocketConfig::icons_dir();
        let active_pack = self.cfg.desktop.icon_pack.clone();
        let packs = self.icon_packs.clone();
        prefs_group(
            ui,
            "Pacote de Ícones (~/.rocket/icons/)",
            card_bg,
            card_stroke,
            |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "Ativo: {}",
                            active_pack.as_deref().unwrap_or("Padrão do sistema")
                        ))
                        .size(13.0)
                        .strong()
                        .color(Color32::WHITE),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        if pill_btn_col(
                            ui,
                            "Abrir pasta",
                            Color32::from_rgba_unmultiplied(255, 255, 255, 18),
                        )
                        .clicked()
                        {
                            let _ = std::fs::create_dir_all(&icons_dir);
                            let _ = Command::new("xdg-open").arg(&icons_dir).spawn();
                        }
                        if pill_btn_col(
                            ui,
                            "Recarregar pasta",
                            Color32::from_rgba_unmultiplied(255, 255, 255, 18),
                        )
                        .clicked()
                        {
                            self.icon_packs = RocketConfig::list_icon_packs();
                        }
                    });
                });
                ui.label(
                    RichText::new(format!("Pasta: {}", icons_dir.display()))
                        .size(11.5)
                        .color(Color32::from_rgb(155, 165, 185)),
                );
                ui.add_space(6.0);

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                    let sys_active = active_pack.is_none();
                    if pill_btn(ui, "Padrão do sistema", sys_active, accent).clicked()
                        && !sys_active
                    {
                        self.cfg.desktop.icon_pack = None;
                        self.save_and_reload("Pacote de ícones: Padrão do sistema");
                    }
                    for pack in &packs {
                        let is_active = active_pack.as_deref() == Some(pack.as_str());
                        if pill_btn(ui, pack, is_active, accent).clicked() && !is_active {
                            self.cfg.desktop.icon_pack = Some(pack.clone());
                            self.save_and_reload(&format!("Pacote de ícones: {pack}"));
                        }
                    }
                });

                if packs.is_empty() {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "Nenhuma subpasta encontrada em ~/.rocket/icons/. Coloque pastas de pacotes de ícones lá e clique em \"Recarregar pasta\".",
                        )
                        .size(11.5)
                        .color(Color32::from_rgb(140, 150, 170)),
                    );
                }
            },
        );
    }

    fn render_topbar_page(
        &mut self,
        ui: &mut egui::Ui,
        accent: Color32,
        card_bg: Color32,
        card_stroke: Stroke,
    ) {
        let mut changed = false;
        prefs_group(ui, "Dimensões & Áreas de Trabalho", card_bg, card_stroke, |ui| {
            let mut h = self.cfg.top_bar.height as f32;
            if slider_row(ui, "Altura da Barra", &mut h, 24.0..=44.0, accent, |v| {
                format!("{:.0} px", v)
            }) {
                self.cfg.top_bar.height = h.round() as u32;
                changed = true;
            }
            card_divider(ui);
            let mut ws = self.cfg.top_bar.workspace_count as f32;
            if slider_row(ui, "Quantidade de Workspaces", &mut ws, 2.0..=12.0, accent, |v| {
                format!("{:.0}", v)
            }) {
                self.cfg.top_bar.workspace_count = ws.round() as usize;
                changed = true;
            }
        });

        ui.add_space(12.0);

        prefs_group(ui, "Indicadores & Menus", card_bg, card_stroke, |ui| {
            if toggle_row(
                ui,
                "Menus Globais (Arquivo, Editar, Janela…)",
                "Envia atalhos reais para o app em foco",
                &mut self.cfg.top_bar.show_global_menu,
                accent,
            ) {
                changed = true;
            }
            card_divider(ui);
            if toggle_row(
                ui,
                "Player de Mídia MPRIS",
                "Exibe faixa atual na barra superior",
                &mut self.cfg.top_bar.show_media,
                accent,
            ) {
                changed = true;
            }
            card_divider(ui);
            if toggle_row(
                ui,
                "Telemetria CPU & RAM na Barra",
                "Mostra porcentagem de CPU e memória",
                &mut self.cfg.top_bar.show_cpu_ram,
                accent,
            ) {
                changed = true;
            }
        });

        ui.add_space(12.0);

        prefs_group(ui, "Formato do Relógio", card_bg, card_stroke, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (label, fmt) in [
                    ("Dia + Hora", "%a %d %b  %H:%M"),
                    ("Com Segundos", "%a %d %b   %H:%M:%S"),
                    ("Compacto", "%H:%M"),
                ] {
                    let active = self.cfg.top_bar.clock_format == fmt;
                    if pill_btn(ui, label, active, accent).clicked() {
                        self.cfg.top_bar.clock_format = fmt.to_string();
                        changed = true;
                    }
                }
            });
            ui.add_space(4.0);
            if styled_text_input(ui, &mut self.cfg.top_bar.clock_format, "%a %d %b  %H:%M", f32::INFINITY)
            {
                changed = true;
            }
        });

        if changed {
            self.mark_changed();
        }
    }

    fn render_dock_page(
        &mut self,
        ui: &mut egui::Ui,
        accent: Color32,
        card_bg: Color32,
        card_stroke: Stroke,
    ) {
        let mut changed = false;
        prefs_group(ui, "Comportamento & Tamanho", card_bg, card_stroke, |ui| {
            if toggle_row(
                ui,
                "Ativar Dock Inferior",
                "Oculta automaticamente quando janelas tocam a borda",
                &mut self.cfg.dock.enabled,
                accent,
            ) {
                changed = true;
            }
            card_divider(ui);
            if slider_row(
                ui,
                "Tamanho dos Ícones",
                &mut self.cfg.dock.icon_size,
                32.0..=72.0,
                accent,
                |v| format!("{:.0} px", v),
            ) {
                changed = true;
            }
            card_divider(ui);
            if slider_row(
                ui,
                "Ampliação no Hover",
                &mut self.cfg.dock.magnification,
                1.0..=1.8,
                accent,
                |v| format!("{:.2}x", v),
            ) {
                changed = true;
            }
            card_divider(ui);
            let mut mb = self.cfg.dock.margin_bottom as f32;
            if slider_row(ui, "Margem Inferior", &mut mb, 0.0..=32.0, accent, |v| {
                format!("{:.0} px", v)
            }) {
                self.cfg.dock.margin_bottom = mb.round() as i32;
                changed = true;
            }
        });

        ui.add_space(12.0);

        prefs_group(ui, "Aplicativos Fixados na Dock", card_bg, card_stroke, |ui| {
            let mut swap_pair: Option<(usize, usize)> = None;
            let mut remove_idx: Option<usize> = None;
            let len = self.cfg.dock.pinned_apps.len();
            for (idx, item) in self.cfg.dock.pinned_apps.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(&item.name)
                            .size(13.0)
                            .strong()
                            .color(Color32::WHITE),
                    );
                    ui.label(
                        RichText::new(&item.exec)
                            .size(11.0)
                            .color(Color32::from_rgb(150, 160, 178)),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if item.exec != "@launcher"
                            && pill_btn_col(
                                ui,
                                "Remover",
                                Color32::from_rgba_unmultiplied(255, 69, 58, 150),
                            )
                            .clicked()
                        {
                            remove_idx = Some(idx);
                        }
                        if idx + 1 < len && pill_btn(ui, "↓", false, accent).clicked() {
                            swap_pair = Some((idx, idx + 1));
                        }
                        if idx > 0 && pill_btn(ui, "↑", false, accent).clicked() {
                            swap_pair = Some((idx - 1, idx));
                        }
                    });
                });
                if idx + 1 < len {
                    card_divider(ui);
                }
            }
            if let Some((a, b)) = swap_pair {
                self.cfg.dock.pinned_apps.swap(a, b);
                changed = true;
            }
            if let Some(r) = remove_idx {
                self.cfg.dock.pinned_apps.remove(r);
                changed = true;
            }
        });

        if changed {
            self.mark_changed();
        }
    }

    fn render_overview_launcher_page(
        &mut self,
        ui: &mut egui::Ui,
        accent: Color32,
        card_bg: Color32,
        card_stroke: Stroke,
    ) {
        let mut changed = false;
        prefs_group(ui, "Overview (Desktops Virtuais)", card_bg, card_stroke, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Modo de Preview das Janelas")
                        .size(13.0)
                        .color(Color32::WHITE),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    for (label, mode) in [
                        ("Ícones", OverviewPreviewMode::Icons),
                        ("Ao Vivo", OverviewPreviewMode::Live),
                        ("Snapshot", OverviewPreviewMode::Snapshot),
                    ] {
                        let active = self.cfg.overview.preview_mode == mode;
                        if pill_btn(ui, label, active, accent).clicked() {
                            self.cfg.overview.preview_mode = mode;
                            changed = true;
                        }
                    }
                });
            });
            card_divider(ui);
            if toggle_row(
                ui,
                "Animações do Overview",
                "Transições suaves ao abrir, fechar e trocar de área",
                &mut self.cfg.overview.animations,
                accent,
            ) {
                changed = true;
            }
            card_divider(ui);
            if toggle_row(
                ui,
                "Blur do Compositor no Overview",
                "Pode consumir mais GPU em placas integradas",
                &mut self.cfg.overview.compositor_blur,
                accent,
            ) {
                changed = true;
            }
        });

        ui.add_space(12.0);

        prefs_group(ui, "Launchpad", card_bg, card_stroke, |ui| {
            let mut max_res = self.cfg.launcher.max_results as f32;
            if slider_row(ui, "Máximo de Resultados na Busca", &mut max_res, 10.0..=60.0, accent, |v| {
                format!("{:.0}", v)
            }) {
                self.cfg.launcher.max_results = max_res.round() as usize;
                changed = true;
            }
            card_divider(ui);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Dica de Atalho")
                        .size(13.0)
                        .color(Color32::WHITE),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if styled_text_input(ui, &mut self.cfg.launcher.hotkey_hint, "Super+Space", 180.0) {
                        changed = true;
                    }
                });
            });
        });

        if changed {
            self.mark_changed();
        }
    }

    fn render_capture_page(
        &mut self,
        ui: &mut egui::Ui,
        accent: Color32,
        card_bg: Color32,
        card_stroke: Stroke,
    ) {
        let mut changed = false;
        prefs_group(ui, "Gravação & Captura de Tela", card_bg, card_stroke, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("FPS Padrão de Gravação").size(13.0).color(Color32::WHITE));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    for fps in [60u32, 30, 24, 15] {
                        let active = self.cfg.capture.default_fps == fps;
                        if pill_btn(ui, &format!("{fps} FPS"), active, accent).clicked() {
                            self.cfg.capture.default_fps = fps;
                            changed = true;
                        }
                    }
                });
            });
            card_divider(ui);
            if toggle_row(
                ui,
                "Incluir Cursor na Captura",
                "Mostra o ponteiro do mouse em fotos e vídeos",
                &mut self.cfg.capture.include_cursor,
                accent,
            ) {
                changed = true;
            }
            card_divider(ui);
            if toggle_row(
                ui,
                "Copiar Screenshot para Clipboard",
                "Copia automaticamente via Wayland clipboard",
                &mut self.cfg.capture.copy_to_clipboard,
                accent,
            ) {
                changed = true;
            }
            card_divider(ui);
            if toggle_row(
                ui,
                "Salvar Screenshot em Disco",
                "Grava PNG automaticamente na pasta configurada",
                &mut self.cfg.capture.save_to_disk,
                accent,
            ) {
                changed = true;
            }
            card_divider(ui);
            if toggle_row(
                ui,
                "Aceleração por Hardware (VA-API)",
                "Usa codificação H.264 na GPU quando disponível",
                &mut self.cfg.capture.prefer_vaapi,
                accent,
            ) {
                changed = true;
            }
        });

        ui.add_space(12.0);

        prefs_group(ui, "Diretórios de Saída (vazio = padrão)", card_bg, card_stroke, |ui| {
            ui.label(RichText::new("Pasta de Screenshots").size(12.0).color(Color32::from_rgb(180, 190, 208)));
            if styled_text_input(ui, &mut self.screenshots_dir_buf, "~/Pictures/Screenshots", f32::INFINITY) {
                changed = true;
            }
            ui.add_space(4.0);
            ui.label(RichText::new("Pasta de Gravações").size(12.0).color(Color32::from_rgb(180, 190, 208)));
            if styled_text_input(ui, &mut self.recordings_dir_buf, "~/Videos/Screencasts", f32::INFINITY) {
                changed = true;
            }
        });

        if changed {
            self.mark_changed();
        }
    }

    fn render_hardware_page(
        &mut self,
        ui: &mut egui::Ui,
        accent: Color32,
        card_bg: Color32,
        card_stroke: Stroke,
    ) {
        let hw_snap = self.hw.state.lock().map(|g| g.clone()).unwrap_or_default();

        prefs_group(ui, "Áudio & Brilho (Tempo Real)", card_bg, card_stroke, |ui| {
            let mut vol = hw_snap.volume_percent;
            if slider_row(ui, "Volume do Alto-falante (até 150%)", &mut vol, 0.0..=150.0, accent, |v| {
                format!("{:.0}%", v)
            }) {
                self.hw.set_volume(vol);
            }
            let mut muted = hw_snap.muted;
            if toggle_row(
                ui,
                "Silenciar Alto-falante",
                if muted { "Áudio silenciado" } else { "Áudio ativo" },
                &mut muted,
                Color32::from_rgb(255, 69, 58),
            ) {
                self.hw.toggle_mute();
            }
            card_divider(ui);
            let mut mic = hw_snap.mic_volume_percent;
            if slider_row(ui, "Ganho do Microfone", &mut mic, 0.0..=100.0, Color32::from_rgb(48, 209, 88), |v| {
                format!("{:.0}%", v)
            }) {
                self.hw.set_mic_volume(mic);
            }
            let mut mic_muted = hw_snap.mic_muted;
            if toggle_row(
                ui,
                "Silenciar Microfone",
                if mic_muted { "Microfone silenciado" } else { "Microfone ativo" },
                &mut mic_muted,
                Color32::from_rgb(255, 69, 58),
            ) {
                self.hw.toggle_mic_mute();
            }
            card_divider(ui);
            let mut br = hw_snap.brightness_percent;
            if slider_row(ui, "Brilho da Tela", &mut br, 2.0..=100.0, accent, |v| {
                format!("{:.0}%", v)
            }) {
                self.hw.set_brightness(br);
            }
        });

        ui.add_space(12.0);

        prefs_group(ui, "Rádios & Perfil de Energia", card_bg, card_stroke, |ui| {
            let mut wifi = hw_snap.wifi_enabled;
            if toggle_row(ui, "Wi-Fi", &hw_snap.wifi_ssid, &mut wifi, accent) {
                self.hw.toggle_wifi();
            }
            card_divider(ui);
            let mut bt = hw_snap.bluetooth_enabled;
            if toggle_row(
                ui,
                "Bluetooth",
                if hw_snap.bluetooth_connected_name.is_empty() {
                    "Adaptador sem fio"
                } else {
                    &hw_snap.bluetooth_connected_name
                },
                &mut bt,
                accent,
            ) {
                self.hw.toggle_bluetooth();
            }
            card_divider(ui);
            let mut saver = hw_snap.power_saver;
            if toggle_row(
                ui,
                "Modo Economia de Energia",
                &format!("Perfil atual: {}", hw_snap.power_profile_name),
                &mut saver,
                Color32::from_rgb(243, 156, 18),
            ) {
                self.hw.toggle_power_saver();
            }
        });
    }

    fn render_about_page(
        &mut self,
        ui: &mut egui::Ui,
        accent: Color32,
        card_bg: Color32,
        card_stroke: Stroke,
    ) {
        prefs_group(ui, "Informações do Sistema", card_bg, card_stroke, |ui| {
            about_row(ui, "Ambiente", "RocketDE 0.1.0 (Wayland / egui + wgpu)");
            card_divider(ui);
            about_row(ui, "Compositor", "Pila WM (wlr-layer-shell)");
            card_divider(ui);
            about_row(
                ui,
                "Arquivo de Configuração",
                &RocketConfig::config_path().display().to_string(),
            );
        });

        ui.add_space(14.0);

        ui.horizontal(|ui| {
            if !self.reset_confirm {
                if pill_btn_col(
                    ui,
                    "Restaurar Padrões de Fábrica",
                    Color32::from_rgba_unmultiplied(255, 69, 58, 185),
                )
                .clicked()
                {
                    self.reset_confirm = true;
                }
            } else {
                ui.label(
                    RichText::new("Confirmar restauração de todos os padrões?")
                        .size(12.0)
                        .color(Color32::from_rgb(255, 160, 150)),
                );
                if pill_btn_col(
                    ui,
                    "Confirmar Reset",
                    Color32::from_rgba_unmultiplied(255, 69, 58, 220),
                )
                .clicked()
                {
                    self.reset_confirm = false;
                    self.cfg = RocketConfig::default();
                    self.sync_dir_buffers();
                    apply_macos_theme(&self.egui_ctx, &self.cfg);
                    self.save_and_reload("Padrões restaurados!");
                }
                if pill_btn_col(
                    ui,
                    "Cancelar",
                    Color32::from_rgba_unmultiplied(255, 255, 255, 22),
                )
                .clicked()
                {
                    self.reset_confirm = false;
                }
            }
            let _ = accent;
        });
    }
}

fn prefs_group(
    ui: &mut egui::Ui,
    title: &str,
    card_bg: Color32,
    card_stroke: Stroke,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    ui.label(
        RichText::new(title.to_uppercase())
            .size(10.5)
            .strong()
            .color(Color32::from_rgb(122, 132, 152)),
    );
    ui.add_space(5.0);
    Frame::NONE
        .fill(card_bg)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(16, 12))
        .stroke(card_stroke)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            add_contents(ui);
        });
}

fn toggle_row(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    value: &mut bool,
    accent: Color32,
) -> bool {
    let mut changed = false;
    let draw_switch = |ui: &mut egui::Ui, value: &mut bool, changed: &mut bool| {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(40.0, 22.0), Sense::click());
        let bg = if *value {
            accent
        } else {
            Color32::from_rgba_unmultiplied(255, 255, 255, 30)
        };
        ui.painter().rect_filled(rect, CornerRadius::same(11), bg);
        let knob_x = if *value {
            rect.max.x - 11.0
        } else {
            rect.min.x + 11.0
        };
        ui.painter()
            .circle_filled(Pos2::new(knob_x, rect.center().y), 8.0, Color32::WHITE);
        if resp.clicked() {
            *value = !*value;
            *changed = true;
        }
    };

    if subtitle.is_empty() {
        ui.horizontal_centered(|ui| {
            ui.label(RichText::new(title).size(13.0).color(Color32::WHITE));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                draw_switch(ui, value, &mut changed);
            });
        });
    } else {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(RichText::new(title).size(13.0).color(Color32::WHITE));
                ui.label(
                    RichText::new(subtitle)
                        .size(11.0)
                        .color(Color32::from_rgb(148, 158, 178)),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                draw_switch(ui, value, &mut changed);
            });
        });
    }
    changed
}

fn slider_row(
    ui: &mut egui::Ui,
    title: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    accent: Color32,
    fmt: impl FnOnce(f32) -> String,
) -> bool {
    let mut changed = false;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).size(13.0).color(Color32::WHITE));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(fmt(*value))
                        .size(12.0)
                        .strong()
                        .color(Color32::from_rgb(185, 195, 214)),
                );
            });
        });

        let min = *range.start();
        let max = *range.end();
        let span = (max - min).max(1.0);
        let frac = ((*value - min) / span).clamp(0.0, 1.0);
        let w = ui.available_width().max(160.0);
        let (hit_rect, resp) = ui.allocate_exact_size(Vec2::new(w, 20.0), Sense::click_and_drag());
        let track_rect = Rect::from_center_size(hit_rect.center(), Vec2::new(w, 8.0));
        ui.painter().rect_filled(
            track_rect,
            CornerRadius::same(4),
            Color32::from_rgba_unmultiplied(255, 255, 255, 24),
        );
        let knob_r = if resp.hovered() || resp.dragged() { 8.0 } else { 7.0 };
        let usable_w = (track_rect.width() - knob_r * 2.0).max(1.0);
        let knob_x = track_rect.min.x + knob_r + usable_w * frac;
        let fill_w = (knob_x - track_rect.min.x + 2.0).clamp(8.0, track_rect.width());
        ui.painter().rect_filled(
            Rect::from_min_size(track_rect.min, Vec2::new(fill_w, 8.0)),
            CornerRadius::same(4),
            accent,
        );
        ui.painter().circle_filled(
            Pos2::new(knob_x, track_rect.center().y),
            knob_r,
            Color32::WHITE,
        );
        if (resp.clicked() || resp.dragged())
            && let Some(p) = resp.interact_pointer_pos()
        {
            let next_frac = ((p.x - (track_rect.min.x + knob_r)) / usable_w).clamp(0.0, 1.0);
            let next_val = min + next_frac * span;
            if (next_val - *value).abs() >= 0.1 {
                *value = next_val;
                changed = true;
            }
        }
    });
    changed
}

fn pill_btn(ui: &mut egui::Ui, label: &str, active: bool, accent: Color32) -> egui::Response {
    let bg = if active {
        accent
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 20)
    };
    pill_btn_col(ui, label, bg)
}

fn card_divider(ui: &mut egui::Ui) {
    ui.add_space(2.0);
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().line_segment(
        [r.left_center(), r.right_center()],
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 12)),
    );
    ui.add_space(2.0);
}

fn styled_text_input(ui: &mut egui::Ui, buf: &mut String, hint: &str, width: f32) -> bool {
    let mut changed = false;
    Frame::NONE
        .fill(Color32::from_rgb(14, 16, 22))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 6))
        .stroke(Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 24),
        ))
        .show(ui, |ui| {
            let w = if width.is_finite() {
                width
            } else {
                ui.available_width()
            };
            if ui
                .add(
                    egui::TextEdit::singleline(buf)
                        .frame(Frame::NONE)
                        .hint_text(hint)
                        .desired_width(w),
                )
                .changed()
            {
                changed = true;
            }
        });
    changed
}

fn pill_btn_col(ui: &mut egui::Ui, label: &str, bg: Color32) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(11.5).strong().color(Color32::WHITE))
            .fill(bg)
            .corner_radius(CornerRadius::same(8))
            .min_size(Vec2::new(0.0, 28.0)),
    )
}

fn about_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(label)
                .size(12.5)
                .color(Color32::from_rgb(165, 175, 194)),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(value)
                    .size(12.5)
                    .strong()
                    .color(Color32::WHITE),
            );
        });
    });
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
    window.set_title("Configurações do RocketDE");
    window.set_app_id("rocketde-settings");
    window.set_min_size(Some((760, 520)));
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
        label: Some("rocketde_settings_device"),
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

    let init_w = 920u32;
    let init_h = 620u32;
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

    let cfg = RocketConfig::load_or_create();
    let egui_ctx = Context::default();
    apply_macos_theme(&egui_ctx, &cfg);
    let renderer = Renderer::new(&device, format, RendererOptions::default());
    let mut wake_fds = [-1i32; 2];
    unsafe {
        libc::pipe2(wake_fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let wake_rd = wake_fds[0];
    let wake_wr = wake_fds[1];
    let wake_fn = std::sync::Arc::new(move || {
        if wake_wr >= 0 {
            let b: u8 = 1;
            unsafe {
                libc::write(wake_wr, (&b as *const u8).cast::<libc::c_void>(), 1);
            }
        }
    });

    let (thumb_req_tx, thumb_req_rx) = std::sync::mpsc::channel::<PathBuf>();
    let (thumb_res_tx, thumb_res_rx) = std::sync::mpsc::channel::<(PathBuf, Option<egui::ColorImage>)>();
    {
        let wake_thumb = wake_fn.clone();
        std::thread::spawn(move || {
            while let Ok(path) = thumb_req_rx.recv() {
                let color_img = image::open(&path).ok().map(|img| {
                    let thumb = img
                        .resize_to_fill(320, 200, image::imageops::FilterType::Triangle)
                        .to_rgba8();
                    egui::ColorImage::from_rgba_unmultiplied(
                        [thumb.width() as usize, thumb.height() as usize],
                        thumb.as_raw(),
                    )
                });
                let _ = thumb_res_tx.send((path, color_img));
                wake_thumb();
            }
        });
    }

    let hw = HardwareController::new({
        let wake_hw = wake_fn.clone();
        move || wake_hw()
    });

    let mut app = SettingsApp {
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
        page: SettingsPage::Appearance,
        search_query: String::new(),
        screenshots_dir_buf: cfg.capture.screenshots_dir.clone().unwrap_or_default(),
        recordings_dir_buf: cfg.capture.recordings_dir.clone().unwrap_or_default(),
        cfg,
        hw,
        status_toast: None,
        pending_save: false,
        last_change_at: None,
        backgrounds: RocketConfig::list_backgrounds(),
        icon_packs: RocketConfig::list_icon_packs(),
        reset_confirm: false,
        wallpaper_thumbs: HashMap::new(),
        thumb_req_tx,
        thumb_res_rx,
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
            } else if app.pending_save {
                60
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
                while unsafe { libc::read(wake_rd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len()) } > 0 {}
                app.dirty = true;
            }
        }
        let _ = event_queue.dispatch_pending(&mut app);
        if app.exit {
            break;
        }
        if app.dirty || !app.pending_events.is_empty() || app.pending_save {
            app.render_frame();
        }
    }
}

impl CompositorHandler for SettingsApp {
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

impl OutputHandler for SettingsApp {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output_state }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for SettingsApp {
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
        let w = configure.new_size.0.map(NonZeroU32::get).unwrap_or(920);
        let h = configure.new_size.1.map(NonZeroU32::get).unwrap_or(620);
        self.is_maximized = configure.is_maximized();
        self.resize(w, h);
        self.render_frame();
    }
}

impl SeatHandler for SettingsApp {
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

impl KeyboardHandler for SettingsApp {
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

impl SettingsApp {
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

impl PointerHandler for SettingsApp {
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

impl ShmHandler for SettingsApp {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}

impl ProvidesRegistryState for SettingsApp {
    fn registry(&mut self) -> &mut RegistryState { &mut self.registry_state }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(SettingsApp);
delegate_registry!(SettingsApp);
