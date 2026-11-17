use egui::{
    Align, Color32, Context, CornerRadius, Event, FontId, Frame, Key, Layout, Margin, Modifiers,
    PointerButton, Pos2, RawInput, Rect, RichText, Sense, Stroke, Vec2,
};
use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use rocketde::config::{OverviewPreviewMode, RocketConfig};
use rocketde::ipc::{IpcCommand, send_command_to_daemon};
use rocketde::system::audio_brightness::HardwareController;
use rocketde::ui::apply_macos_theme;
use rocketde::ui::icons::{IconKind, draw_icon};
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

    fn badge_color(self) -> Color32 {
        match self {
            Self::Appearance => Color32::from_rgb(10, 132, 255),
            Self::Desktop => Color32::from_rgb(50, 173, 230),
            Self::TopBar => Color32::from_rgb(94, 92, 230),
            Self::Dock => Color32::from_rgb(255, 94, 58),
            Self::OverviewLauncher => Color32::from_rgb(175, 82, 222),
            Self::Capture => Color32::from_rgb(255, 69, 58),
            Self::Hardware => Color32::from_rgb(48, 209, 88),
            Self::About => Color32::from_rgb(142, 142, 147),
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
    configured: bool,
    dirty: bool,
    repaint_delay: Option<Duration>,
    pending_events: Vec<Event>,
    modifiers: Modifiers,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
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
    backgrounds: Vec<PathBuf>,
    wallpaper_thumbs: HashMap<PathBuf, Option<egui::TextureHandle>>,
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
        self.cfg.save();
        let ok = send_command_to_daemon(&IpcCommand::ReloadConfig).unwrap_or(false);
        let msg = if ok {
            toast_msg.to_string()
        } else {
            format!("{toast_msg} (salvo em disco)")
        };
        self.status_toast = Some((msg, Instant::now()));
        self.pending_save = false;
        self.last_change_at = None;
        self.dirty = true;
    }

    fn resize(&mut self, w: u32, h: u32) {
        self.width = w.max(320);
        self.height = h.max(240);
        self.surface_config.width = self.width;
        self.surface_config.height = self.height;
        self.wgpu_surface.configure(&self.device, &self.surface_config);
        self.configured = true;
        self.dirty = true;
    }

    fn render_frame(&mut self) {
        if !self.configured {
            return;
        }

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
            size_in_pixels: [self.width, self.height],
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
                            r: 0.075,
                            g: 0.082,
                            b: 0.118,
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
        let accent = Color32::from_rgb(
            self.cfg.theme.accent_rgb[0],
            self.cfg.theme.accent_rgb[1],
            self.cfg.theme.accent_rgb[2],
        );
        let sidebar_bg = Color32::from_rgb(22, 24, 34);
        let content_bg = Color32::from_rgb(26, 29, 41);
        let card_bg = Color32::from_rgba_unmultiplied(255, 255, 255, 13);
        let card_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 22));

        // Left macOS / GNOME-style navigation sidebar
        egui::Panel::left("settings_sidebar")
            .exact_size(244.0)
            .frame(
                Frame::NONE
                    .fill(sidebar_bg)
                    .inner_margin(Margin::symmetric(12, 14)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (logo_r, _) =
                        ui.allocate_exact_size(Vec2::new(28.0, 28.0), Sense::hover());
                    ui.painter()
                        .rect_filled(logo_r, CornerRadius::same(8), accent);
                    draw_icon(
                        ui.painter(),
                        logo_r.center(),
                        15.0,
                        IconKind::Rocket,
                        Color32::WHITE,
                    );
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Configurações")
                                .size(15.0)
                                .strong()
                                .color(Color32::WHITE),
                        );
                        ui.label(
                            RichText::new("RocketDE Shell")
                                .size(11.0)
                                .color(Color32::from_rgb(150, 160, 180)),
                        );
                    });
                });

                ui.add_space(12.0);

                // Search input
                Frame::NONE
                    .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 16))
                    .corner_radius(CornerRadius::same(9))
                    .inner_margin(Margin::symmetric(10, 6))
                    .stroke(card_stroke)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (sr, _) =
                                ui.allocate_exact_size(Vec2::new(14.0, 14.0), Sense::hover());
                            draw_icon(
                                ui.painter(),
                                sr.center(),
                                12.0,
                                IconKind::Search,
                                Color32::from_rgb(160, 170, 188),
                            );
                            ui.add(
                                egui::TextEdit::singleline(&mut self.search_query)
                                    .frame(Frame::NONE)
                                    .hint_text("Buscar ajuste...")
                                    .desired_width(f32::INFINITY),
                            );
                        });
                    });

                ui.add_space(12.0);

                let q = self.search_query.trim().to_lowercase();
                for page in SettingsPage::ALL {
                    if !q.is_empty()
                        && !page.title().to_lowercase().contains(&q)
                        && !page.subtitle().to_lowercase().contains(&q)
                    {
                        continue;
                    }
                    let selected = self.page == page;
                    let (row_r, row_resp) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 38.0),
                        Sense::click(),
                    );
                    let row_bg = if selected {
                        Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 64)
                    } else if row_resp.hovered() {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 16)
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter()
                        .rect_filled(row_r, CornerRadius::same(9), row_bg);
                    if selected {
                        ui.painter().rect_stroke(
                            row_r,
                            CornerRadius::same(9),
                            Stroke::new(
                                1.0,
                                Color32::from_rgba_unmultiplied(
                                    accent.r(),
                                    accent.g(),
                                    accent.b(),
                                    135,
                                ),
                            ),
                            egui::StrokeKind::Inside,
                        );
                    }

                    let badge_r = Rect::from_center_size(
                        Pos2::new(row_r.min.x + 20.0, row_r.center().y),
                        Vec2::new(24.0, 24.0),
                    );
                    ui.painter().rect_filled(
                        badge_r,
                        CornerRadius::same(6),
                        page.badge_color(),
                    );
                    draw_icon(
                        ui.painter(),
                        badge_r.center(),
                        13.0,
                        page.icon(),
                        Color32::WHITE,
                    );

                    ui.painter().text(
                        Pos2::new(row_r.min.x + 40.0, row_r.center().y),
                        egui::Align2::LEFT_CENTER,
                        page.title(),
                        FontId::proportional(13.0),
                        if selected {
                            Color32::WHITE
                        } else {
                            Color32::from_rgb(220, 226, 238)
                        },
                    );

                    if row_resp.clicked() {
                        self.page = page;
                    }
                    ui.add_space(3.0);
                }

                // Bottom status & manual save button
                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    if let Some((ref msg, created)) = self.status_toast {
                        if created.elapsed() < Duration::from_secs(3) {
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(msg)
                                    .size(11.0)
                                    .color(Color32::from_rgb(120, 225, 150)),
                            );
                            ctx.request_repaint_after(Duration::from_millis(250));
                        } else {
                            self.status_toast = None;
                        }
                    }

                    let save_btn = egui::Button::new(
                        RichText::new("Salvar & Aplicar")
                            .size(12.5)
                            .strong()
                            .color(Color32::WHITE),
                    )
                    .fill(accent)
                    .corner_radius(CornerRadius::same(9))
                    .min_size(Vec2::new(ui.available_width(), 32.0));
                    if ui.add(save_btn).clicked() {
                        self.save_and_reload("Configurações aplicadas!");
                    }
                });
            });

        // Right main content area
        egui::CentralPanel::default()
            .frame(
                Frame::NONE
                    .fill(content_bg)
                    .inner_margin(Margin::symmetric(24, 20)),
            )
            .show(ui, |ui| {
                // Page header
                ui.horizontal(|ui| {
                    let (hdr_badge, _) =
                        ui.allocate_exact_size(Vec2::new(36.0, 36.0), Sense::hover());
                    ui.painter().rect_filled(
                        hdr_badge,
                        CornerRadius::same(10),
                        self.page.badge_color(),
                    );
                    draw_icon(
                        ui.painter(),
                        hdr_badge.center(),
                        18.0,
                        self.page.icon(),
                        Color32::WHITE,
                    );
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(self.page.title())
                                .size(19.0)
                                .strong()
                                .color(Color32::WHITE),
                        );
                        ui.label(
                            RichText::new(self.page.subtitle())
                                .size(12.0)
                                .color(Color32::from_rgb(160, 170, 188)),
                        );
                    });
                });

                ui.add_space(16.0);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.page {
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
                    });
            });
    }

    fn ensure_wallpaper_thumb(&mut self, path: &PathBuf) -> Option<egui::TextureId> {
        if !self.wallpaper_thumbs.contains_key(path) {
            let tex = image::open(path).ok().map(|img| {
                let thumb = img
                    .resize_exact(320, 200, image::imageops::FilterType::Triangle)
                    .to_rgba8();
                let color_img = egui::ColorImage::from_rgba_unmultiplied(
                    [thumb.width() as usize, thumb.height() as usize],
                    thumb.as_raw(),
                );
                self.egui_ctx.load_texture(
                    format!("wp_thumb_{}", path.display()),
                    color_img,
                    egui::TextureOptions::LINEAR,
                )
            });
            self.wallpaper_thumbs.insert(path.clone(), tex);
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
                    let current_name = active_path
                        .as_deref()
                        .and_then(|p| std::path::Path::new(p).file_name())
                        .and_then(|n| n.to_str())
                        .unwrap_or("Fundo padrão do sistema");
                    ui.label(
                        RichText::new(current_name)
                            .size(14.5)
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
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Recarregar pasta").clicked() {
                            self.backgrounds = RocketConfig::list_backgrounds();
                            self.wallpaper_thumbs.clear();
                        }
                        if ui.button("Abrir pasta").clicked() {
                            let _ = std::fs::create_dir_all(&bg_dir);
                            let _ = Command::new("xdg-open").arg(&bg_dir).spawn();
                        }
                        if active_path.is_some() && ui.button("Remover").clicked() {
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

                let cols = 3usize;
                let spacing = 12.0;
                let avail_w = ui.available_width();
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
                            ui.painter().text(
                                egui::pos2(card_rect.min.x + 8.0, thumb_rect.max.y + 13.0),
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
            ui.separator();
            let mut dock_a = self.cfg.theme.dock_bg_rgba[3] as f32;
            if slider_row(ui, "Opacidade da Dock", &mut dock_a, 80.0..=255.0, accent, |v| {
                format!("{:.0}%", v / 255.0 * 100.0)
            }) {
                self.cfg.theme.dock_bg_rgba[3] = dock_a.round() as u8;
                changed = true;
            }
            ui.separator();
            let mut panel_a = self.cfg.theme.panel_bg_rgba[3] as f32;
            if slider_row(ui, "Opacidade dos Painéis & Popups", &mut panel_a, 120.0..=255.0, accent, |v| {
                format!("{:.0}%", v / 255.0 * 100.0)
            }) {
                self.cfg.theme.panel_bg_rgba[3] = panel_a.round() as u8;
                changed = true;
            }
        });

        if changed {
            self.mark_changed();
        }
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
            ui.separator();
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
            ui.separator();
            if toggle_row(
                ui,
                "Player de Mídia MPRIS",
                "Exibe faixa atual na barra superior",
                &mut self.cfg.top_bar.show_media,
                accent,
            ) {
                changed = true;
            }
            ui.separator();
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
            ui.add_space(6.0);
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.cfg.top_bar.clock_format)
                        .desired_width(f32::INFINITY),
                )
                .changed()
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
            ui.separator();
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
            ui.separator();
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
            ui.separator();
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
                    ui.separator();
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
            ui.separator();
            if toggle_row(
                ui,
                "Animações do Overview",
                "Transições suaves ao abrir, fechar e trocar de área",
                &mut self.cfg.overview.animations,
                accent,
            ) {
                changed = true;
            }
            ui.separator();
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
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Dica de Atalho")
                        .size(13.0)
                        .color(Color32::WHITE),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut self.cfg.launcher.hotkey_hint)
                                .desired_width(180.0),
                        )
                        .changed()
                    {
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
            ui.separator();
            if toggle_row(
                ui,
                "Incluir Cursor na Captura",
                "Mostra o ponteiro do mouse em fotos e vídeos",
                &mut self.cfg.capture.include_cursor,
                accent,
            ) {
                changed = true;
            }
            ui.separator();
            if toggle_row(
                ui,
                "Copiar Screenshot para Clipboard",
                "Copia automaticamente via Wayland clipboard",
                &mut self.cfg.capture.copy_to_clipboard,
                accent,
            ) {
                changed = true;
            }
            ui.separator();
            if toggle_row(
                ui,
                "Salvar Screenshot em Disco",
                "Grava PNG automaticamente na pasta configurada",
                &mut self.cfg.capture.save_to_disk,
                accent,
            ) {
                changed = true;
            }
            ui.separator();
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
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.screenshots_dir_buf)
                        .hint_text("~/Pictures/Screenshots")
                        .desired_width(f32::INFINITY),
                )
                .changed()
            {
                changed = true;
            }
            ui.add_space(6.0);
            ui.label(RichText::new("Pasta de Gravações").size(12.0).color(Color32::from_rgb(180, 190, 208)));
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.recordings_dir_buf)
                        .hint_text("~/Videos/Screencasts")
                        .desired_width(f32::INFINITY),
                )
                .changed()
            {
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
            ui.separator();
            let mut mic = hw_snap.mic_volume_percent;
            if slider_row(ui, "Ganho do Microfone", &mut mic, 0.0..=100.0, Color32::from_rgb(48, 209, 88), |v| {
                format!("{:.0}%", v)
            }) {
                self.hw.set_mic_volume(mic);
            }
            ui.separator();
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
            ui.separator();
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
            ui.separator();
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
            ui.separator();
            about_row(ui, "Compositor", "Pila WM (wlr-layer-shell)");
            ui.separator();
            about_row(
                ui,
                "Arquivo de Configuração",
                &RocketConfig::config_path().display().to_string(),
            );
        });

        ui.add_space(14.0);

        ui.horizontal(|ui| {
            if pill_btn_col(
                ui,
                "Restaurar Padrões de Fábrica",
                Color32::from_rgba_unmultiplied(255, 69, 58, 185),
            )
            .clicked()
            {
                self.cfg = RocketConfig::default();
                self.sync_dir_buffers();
                apply_macos_theme(&self.egui_ctx, &self.cfg);
                self.save_and_reload("Padrões restaurados!");
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
        RichText::new(title)
            .size(12.0)
            .strong()
            .color(Color32::from_rgb(175, 185, 205)),
    );
    ui.add_space(5.0);
    Frame::NONE
        .fill(card_bg)
        .corner_radius(CornerRadius::same(13))
        .inner_margin(Margin::symmetric(14, 12))
        .stroke(card_stroke)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
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
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new(title).size(13.0).color(Color32::WHITE));
            if !subtitle.is_empty() {
                ui.label(
                    RichText::new(subtitle)
                        .size(11.0)
                        .color(Color32::from_rgb(152, 162, 180)),
                );
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(42.0, 24.0), Sense::click());
            let bg = if *value {
                accent
            } else {
                Color32::from_rgba_unmultiplied(255, 255, 255, 30)
            };
            ui.painter().rect_filled(rect, CornerRadius::same(12), bg);
            let knob_x = if *value {
                rect.max.x - 12.0
            } else {
                rect.min.x + 12.0
            };
            ui.painter()
                .circle_filled(Pos2::new(knob_x, rect.center().y), 9.0, Color32::WHITE);
            if resp.clicked() {
                *value = !*value;
                changed = true;
            }
        });
    });
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

fn pill_btn_col(ui: &mut egui::Ui, label: &str, bg: Color32) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(11.5).strong().color(Color32::WHITE))
            .fill(bg)
            .corner_radius(CornerRadius::same(8))
            .min_size(Vec2::new(0.0, 24.0)),
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
    let window = xdg_shell.create_window(wl_surface, WindowDecorations::RequestServer, &qh);
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
    let hw = HardwareController::new(|| {});
    hw.set_detailed_active(true);

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
        configured: false,
        dirty: true,
        repaint_delay: None,
        pending_events: Vec::with_capacity(32),
        modifiers: Modifiers::NONE,
        keyboard: None,
        pointer: None,
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
        wallpaper_thumbs: HashMap::new(),
    };

    let wayland_fd = conn.as_fd();
    loop {
        let _ = conn.flush();
        if let Some(guard) = conn.prepare_read() {
            let mut pfd = libc::pollfd {
                fd: wayland_fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let timeout_ms = if app.dirty || !app.pending_events.is_empty() {
                16
            } else if app.pending_save {
                60
            } else if let Some(d) = app.repaint_delay {
                d.as_millis().clamp(16, 1000) as i32
            } else {
                500
            };
            let ret = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
            if ret > 0 && (pfd.revents & libc::POLLIN) != 0 {
                let _ = guard.read();
            } else {
                drop(guard);
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
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
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
        self.resize(w, h);
        self.render_frame();
    }
}

impl SeatHandler for SettingsApp {
    fn seat_state(&mut self) -> &mut SeatState { &mut self.seat_state }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
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
                PointerEventKind::Press { button, .. } => {
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

fn translate_btn(b: u32) -> Option<PointerButton> {
    match b {
        0x110 => Some(PointerButton::Primary),
        0x111 => Some(PointerButton::Secondary),
        0x112 => Some(PointerButton::Middle),
        _ => None,
    }
}

fn translate_keysym(sym: Keysym) -> Option<Key> {
    match sym {
        Keysym::Escape => Some(Key::Escape),
        Keysym::Return | Keysym::KP_Enter => Some(Key::Enter),
        Keysym::BackSpace => Some(Key::Backspace),
        Keysym::Tab | Keysym::ISO_Left_Tab => Some(Key::Tab),
        Keysym::space => Some(Key::Space),
        Keysym::Up => Some(Key::ArrowUp),
        Keysym::Down => Some(Key::ArrowDown),
        Keysym::Left => Some(Key::ArrowLeft),
        Keysym::Right => Some(Key::ArrowRight),
        Keysym::Home => Some(Key::Home),
        Keysym::End => Some(Key::End),
        Keysym::Delete => Some(Key::Delete),
        _ => None,
    }
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
