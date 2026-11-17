use chrono::{DateTime, Local};
use egui::{
    Align, Color32, Context, CornerRadius, Event, FontId, Frame, Key, Layout, Margin, Modifiers,
    PointerButton, Pos2, RawInput, Rect, RichText, Sense, Stroke, Vec2,
};
use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use rocketde::config::RocketConfig;
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
use std::fs;
use std::num::NonZeroU32;
use std::os::fd::AsFd;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::ptr::NonNull;
use std::time::{Duration, Instant, SystemTime};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Grid,
    List,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    Dir,
    Image,
    Video,
    Audio,
    Archive,
    Code,
    Document,
    Other,
}

impl FileKind {
    fn from_path(path: &Path, is_dir: bool) -> Self {
        if is_dir {
            return Self::Dir;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "svg" | "bmp" | "ico" => Self::Image,
            "mp4" | "mkv" | "webm" | "avi" | "mov" => Self::Video,
            "mp3" | "flac" | "ogg" | "wav" | "m4a" | "opus" => Self::Audio,
            "zip" | "tar" | "gz" | "xz" | "zst" | "7z" | "rar" => Self::Archive,
            "rs" | "py" | "js" | "ts" | "tsx" | "c" | "cpp" | "h" | "toml" | "json" | "yaml"
            | "yml" | "sh" | "html" | "css" => Self::Code,
            "pdf" | "md" | "txt" | "doc" | "docx" | "odt" | "csv" => Self::Document,
            _ => Self::Other,
        }
    }

    fn badge_label(self) -> &'static str {
        match self {
            Self::Dir => "DIR",
            Self::Image => "IMG",
            Self::Video => "VID",
            Self::Audio => "AUD",
            Self::Archive => "ZIP",
            Self::Code => "DEV",
            Self::Document => "DOC",
            Self::Other => "ARQ",
        }
    }

    fn badge_color(self, accent: Color32) -> Color32 {
        match self {
            Self::Dir => accent,
            Self::Image => Color32::from_rgb(175, 82, 222),
            Self::Video => Color32::from_rgb(255, 69, 58),
            Self::Audio => Color32::from_rgb(48, 209, 88),
            Self::Archive => Color32::from_rgb(255, 159, 10),
            Self::Code => Color32::from_rgb(50, 173, 230),
            Self::Document => Color32::from_rgb(94, 92, 230),
            Self::Other => Color32::from_rgb(120, 130, 150),
        }
    }
}

#[derive(Debug, Clone)]
struct FileEntry {
    name: String,
    path: PathBuf,
    is_dir: bool,
    is_hidden: bool,
    size_bytes: u64,
    modified_str: String,
    kind: FileKind,
}

struct FilesApp {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    _compositor: CompositorState,
    shm: Shm,
    window: Window,

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

    // File manager state
    cfg: RocketConfig,
    current_dir: PathBuf,
    entries: Vec<FileEntry>,
    history_back: Vec<PathBuf>,
    history_fwd: Vec<PathBuf>,
    selected_path: Option<PathBuf>,
    view_mode: ViewMode,
    show_hidden: bool,
    search_query: String,
    editing_path: bool,
    path_input: String,
    new_folder_mode: bool,
    new_folder_name: String,
    rename_target: Option<PathBuf>,
    rename_buf: String,
    delete_confirm_target: Option<PathBuf>,
    context_menu: Option<(FileEntry, Pos2)>,
    toast: Option<(String, Instant)>,
    places: Vec<(&'static str, PathBuf, IconKind)>,
}

impl FilesApp {
    fn navigate_to(&mut self, dest: PathBuf, push_history: bool) {
        if !dest.is_dir() {
            self.set_toast(format!("Pasta inacessível: {}", dest.display()));
            return;
        }
        if push_history && self.current_dir != dest {
            self.history_back.push(self.current_dir.clone());
            self.history_fwd.clear();
        }
        self.current_dir = dest;
        self.selected_path = None;
        self.search_query.clear();
        self.editing_path = false;
        self.new_folder_mode = false;
        self.rename_target = None;
        self.delete_confirm_target = None;
        self.context_menu = None;
        self.refresh_entries();
        let title = self
            .current_dir
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("/");
        self.window.set_title(&format!("Arquivos — {}", title));
        self.dirty = true;
    }

    fn go_back(&mut self) {
        if let Some(prev) = self.history_back.pop() {
            self.history_fwd.push(self.current_dir.clone());
            self.navigate_to(prev, false);
        }
    }

    fn go_forward(&mut self) {
        if let Some(next) = self.history_fwd.pop() {
            self.history_back.push(self.current_dir.clone());
            self.navigate_to(next, false);
        }
    }

    fn go_up(&mut self) {
        if let Some(parent) = self.current_dir.parent() {
            self.navigate_to(parent.to_path_buf(), true);
        }
    }

    fn refresh_entries(&mut self) {
        let mut list = Vec::new();
        match fs::read_dir(&self.current_dir) {
            Ok(rd) => {
                for item in rd.flatten() {
                    let path = item.path();
                    let name = item.file_name().to_string_lossy().to_string();
                    let is_hidden = name.starts_with('.');
                    let meta = item.metadata().ok();
                    let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or_else(|| path.is_dir());
                    let size_bytes = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                    let modified_str = meta
                        .and_then(|m| m.modified().ok())
                        .map(format_system_time)
                        .unwrap_or_else(|| "—".to_string());
                    let kind = FileKind::from_path(&path, is_dir);
                    list.push(FileEntry {
                        name,
                        path,
                        is_dir,
                        is_hidden,
                        size_bytes,
                        modified_str,
                        kind,
                    });
                }
            }
            Err(e) => {
                self.set_toast(format!("Erro ao ler pasta: {e}"));
            }
        }
        list.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.entries = list;
        self.dirty = true;
    }

    fn set_toast(&mut self, msg: String) {
        self.toast = Some((msg, Instant::now()));
        self.dirty = true;
    }

    fn open_entry(&mut self, entry: &FileEntry) {
        if entry.is_dir {
            self.navigate_to(entry.path.clone(), true);
        } else {
            let p = entry.path.clone();
            std::thread::spawn(move || {
                let _ = Command::new("xdg-open")
                    .arg(&p)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
            });
            self.set_toast(format!("Abrindo {}", entry.name));
        }
    }

    fn open_in_terminal(&mut self, dir: &Path) {
        let d = dir.to_path_buf();
        std::thread::spawn(move || {
            let ok = Command::new("foot")
                .arg("--working-directory")
                .arg(&d)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .is_ok();
            if !ok {
                let _ = Command::new("alacritty")
                    .arg("--working-directory")
                    .arg(&d)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
            }
        });
        self.set_toast("Abrindo terminal na pasta".to_string());
    }

    fn resize(&mut self, w: u32, h: u32) {
        self.width = w.max(360);
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
                label: Some("rocketde_files_encoder"),
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
                label: Some("rocketde_files_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.08,
                            g: 0.085,
                            b: 0.12,
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
        let sidebar_bg = Color32::from_rgb(21, 23, 33);
        let toolbar_bg = Color32::from_rgb(24, 27, 38);
        let content_bg = Color32::from_rgb(27, 30, 42);
        let card_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 20));

        // Keyboard shortcuts
        if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(Key::H)) {
            self.show_hidden = !self.show_hidden;
        }
        if !self.editing_path && !self.new_folder_mode && self.rename_target.is_none() {
            if ctx.input(|i| i.key_pressed(Key::Backspace)) {
                self.go_up();
            }
            if ctx.input(|i| i.key_pressed(Key::F5)) {
                self.refresh_entries();
            }
        }

        // Left Sidebar (Finder / Nautilus Places)
        egui::Panel::left("files_sidebar")
            .exact_size(210.0)
            .frame(
                Frame::NONE
                    .fill(sidebar_bg)
                    .inner_margin(Margin::symmetric(12, 14)),
            )
            .show(ui, |ui| {
                ui.label(
                    RichText::new("LOCAIS")
                        .size(11.0)
                        .strong()
                        .color(Color32::from_rgb(138, 148, 168)),
                );
                ui.add_space(6.0);

                let places = self.places.clone();
                for (label, path, icon) in places {
                    let active = self.current_dir == path;
                    let (row_r, resp) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 32.0),
                        Sense::click(),
                    );
                    let bg = if active {
                        Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 64)
                    } else if resp.hovered() {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 16)
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(row_r, CornerRadius::same(8), bg);
                    draw_icon(
                        ui.painter(),
                        Pos2::new(row_r.min.x + 16.0, row_r.center().y),
                        13.5,
                        icon,
                        if active { accent } else { Color32::from_rgb(200, 210, 228) },
                    );
                    ui.painter().text(
                        Pos2::new(row_r.min.x + 34.0, row_r.center().y),
                        egui::Align2::LEFT_CENTER,
                        label,
                        FontId::proportional(13.0),
                        if active {
                            Color32::WHITE
                        } else {
                            Color32::from_rgb(220, 226, 238)
                        },
                    );
                    if resp.clicked() {
                        self.navigate_to(path, true);
                    }
                    ui.add_space(2.0);
                }

                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    if pill_btn(
                        ui,
                        "Abrir no Terminal",
                        Color32::from_rgba_unmultiplied(255, 255, 255, 18),
                    )
                    .clicked()
                    {
                        let cur = self.current_dir.clone();
                        self.open_in_terminal(&cur);
                    }
                });
            });

        // Top Toolbar (Navigation + Breadcrumbs + Search + Actions)
        egui::Panel::top("files_toolbar")
            .exact_size(48.0)
            .frame(
                Frame::NONE
                    .fill(toolbar_bg)
                    .inner_margin(Margin::symmetric(14, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if nav_icon_btn(ui, IconKind::ChevronLeft, !self.history_back.is_empty()) {
                        self.go_back();
                    }
                    if nav_icon_btn(ui, IconKind::ChevronRight, !self.history_fwd.is_empty()) {
                        self.go_forward();
                    }
                    if pill_btn(
                        ui,
                        "↑",
                        Color32::from_rgba_unmultiplied(255, 255, 255, 18),
                    )
                    .on_hover_text("Pasta acima (Backspace)")
                    .clicked()
                    {
                        self.go_up();
                    }

                    ui.add_space(6.0);

                    // Breadcrumb or Path Edit
                    if self.editing_path {
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.path_input)
                                .desired_width(340.0),
                        );
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            let target = PathBuf::from(self.path_input.trim());
                            self.navigate_to(target, true);
                        }
                        if pill_btn(ui, "Ir", accent).clicked() {
                            let target = PathBuf::from(self.path_input.trim());
                            self.navigate_to(target, true);
                        }
                        if pill_btn(
                            ui,
                            "✕",
                            Color32::from_rgba_unmultiplied(255, 255, 255, 18),
                        )
                        .clicked()
                        {
                            self.editing_path = false;
                        }
                    } else {
                        Frame::NONE
                            .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 12))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(Margin::symmetric(8, 4))
                            .stroke(card_stroke)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let ancestors: Vec<PathBuf> = self
                                        .current_dir
                                        .ancestors()
                                        .map(|p| p.to_path_buf())
                                        .collect::<Vec<_>>()
                                        .into_iter()
                                        .rev()
                                        .collect();
                                    let start_idx = ancestors.len().saturating_sub(4);
                                    for (i, anc) in ancestors[start_idx..].iter().enumerate() {
                                        let label = anc
                                            .file_name()
                                            .and_then(|s| s.to_str())
                                            .filter(|s| !s.is_empty())
                                            .unwrap_or("/");
                                        let is_last = start_idx + i + 1 == ancestors.len();
                                        let col = if is_last {
                                            Color32::WHITE
                                        } else {
                                            Color32::from_rgb(165, 175, 194)
                                        };
                                        if ui
                                            .add(
                                                egui::Button::new(
                                                    RichText::new(label).size(12.0).color(col),
                                                )
                                                .fill(Color32::TRANSPARENT),
                                            )
                                            .clicked()
                                        {
                                            self.navigate_to(anc.clone(), true);
                                        }
                                        if !is_last {
                                            ui.label(
                                                RichText::new("/")
                                                    .size(11.5)
                                                    .color(Color32::from_rgb(110, 120, 138)),
                                            );
                                        }
                                    }
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new("✎")
                                                    .size(11.5)
                                                    .color(Color32::from_rgb(155, 165, 184)),
                                            )
                                            .fill(Color32::TRANSPARENT),
                                        )
                                        .on_hover_text("Editar caminho")
                                        .clicked()
                                    {
                                        self.path_input = self.current_dir.display().to_string();
                                        self.editing_path = true;
                                    }
                                });
                            });
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let view_lbl = match self.view_mode {
                            ViewMode::Grid => "Lista",
                            ViewMode::List => "Grade",
                        };
                        if pill_btn(
                            ui,
                            view_lbl,
                            Color32::from_rgba_unmultiplied(255, 255, 255, 18),
                        )
                        .clicked()
                        {
                            self.view_mode = match self.view_mode {
                                ViewMode::Grid => ViewMode::List,
                                ViewMode::List => ViewMode::Grid,
                            };
                        }

                        if pill_btn(
                            ui,
                            "Ocultos",
                            if self.show_hidden {
                                accent
                            } else {
                                Color32::from_rgba_unmultiplied(255, 255, 255, 18)
                            },
                        )
                        .on_hover_text("Mostrar arquivos ocultos (Ctrl+H)")
                        .clicked()
                        {
                            self.show_hidden = !self.show_hidden;
                        }

                        if pill_btn(ui, "+ Pasta", accent).clicked() {
                            self.new_folder_mode = !self.new_folder_mode;
                            self.new_folder_name.clear();
                        }

                        Frame::NONE
                            .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 14))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(Margin::symmetric(8, 4))
                            .stroke(card_stroke)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.search_query)
                                        .frame(Frame::NONE)
                                        .hint_text("Filtrar...")
                                        .desired_width(135.0),
                                );
                            });
                    });
                });
            });

        // Bottom Status Bar
        egui::Panel::bottom("files_status")
            .exact_size(30.0)
            .frame(
                Frame::NONE
                    .fill(toolbar_bg)
                    .inner_margin(Margin::symmetric(14, 6)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let total = self.entries.len();
                    let hidden_cnt = self.entries.iter().filter(|e| e.is_hidden).count();
                    let status_txt = if let Some(ref sel) = self.selected_path {
                        if let Some(ent) = self.entries.iter().find(|e| &e.path == sel) {
                            if ent.is_dir {
                                format!("{} • Pasta • {}", ent.name, ent.modified_str)
                            } else {
                                format!(
                                    "{} • {} • {}",
                                    ent.name,
                                    format_bytes(ent.size_bytes),
                                    ent.modified_str
                                )
                            }
                        } else {
                            format!("{total} itens ({hidden_cnt} ocultos)")
                        }
                    } else {
                        format!("{total} itens ({hidden_cnt} ocultos)")
                    };
                    ui.label(
                        RichText::new(status_txt)
                            .size(11.5)
                            .color(Color32::from_rgb(165, 175, 194)),
                    );

                    if let Some((ref msg, created)) = self.toast {
                        if created.elapsed() < Duration::from_secs(3) {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(
                                    RichText::new(msg)
                                        .size(11.5)
                                        .color(Color32::from_rgb(120, 225, 150)),
                                );
                            });
                            ctx.request_repaint_after(Duration::from_millis(250));
                        } else {
                            self.toast = None;
                        }
                    }
                });
            });

        // Central Files Area
        egui::CentralPanel::default()
            .frame(
                Frame::NONE
                    .fill(content_bg)
                    .inner_margin(Margin::symmetric(16, 14)),
            )
            .show(ui, |ui| {
                // Inline Action Bars (New Folder / Rename / Delete Confirm)
                if self.new_folder_mode {
                    inline_action_bar(ui, card_stroke, |ui| {
                        ui.label(RichText::new("Nova pasta:").size(12.5).color(Color32::WHITE));
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.new_folder_name)
                                .hint_text("Nome da pasta")
                                .desired_width(220.0),
                        );
                        let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if pill_btn(ui, "Criar", accent).clicked() || submit {
                            let name = self.new_folder_name.trim().to_string();
                            if !name.is_empty() {
                                let target = self.current_dir.join(&name);
                                match fs::create_dir(&target) {
                                    Ok(_) => {
                                        self.new_folder_mode = false;
                                        self.refresh_entries();
                                        self.set_toast(format!("Pasta '{}' criada", name));
                                    }
                                    Err(e) => self.set_toast(format!("Erro: {e}")),
                                }
                            }
                        }
                        if pill_btn(ui, "Cancelar", Color32::from_rgba_unmultiplied(255, 255, 255, 20)).clicked() {
                            self.new_folder_mode = false;
                        }
                    });
                    ui.add_space(8.0);
                }

                if let Some(target) = self.rename_target.clone() {
                    inline_action_bar(ui, card_stroke, |ui| {
                        ui.label(RichText::new("Renomear para:").size(12.5).color(Color32::WHITE));
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.rename_buf)
                                .desired_width(220.0),
                        );
                        let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if pill_btn(ui, "Salvar", accent).clicked() || submit {
                            let new_name = self.rename_buf.trim().to_string();
                            if !new_name.is_empty()
                                && let Some(parent) = target.parent()
                            {
                                let dest = parent.join(&new_name);
                                match fs::rename(&target, &dest) {
                                    Ok(_) => {
                                        self.rename_target = None;
                                        self.refresh_entries();
                                        self.set_toast(format!("Renomeado para '{}'", new_name));
                                    }
                                    Err(e) => self.set_toast(format!("Erro ao renomear: {e}")),
                                }
                            }
                        }
                        if pill_btn(ui, "Cancelar", Color32::from_rgba_unmultiplied(255, 255, 255, 20)).clicked() {
                            self.rename_target = None;
                        }
                    });
                    ui.add_space(8.0);
                }

                if let Some(target) = self.delete_confirm_target.clone() {
                    let name = target
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("item")
                        .to_string();
                    inline_action_bar(ui, card_stroke, |ui| {
                        ui.label(
                            RichText::new(format!("Excluir '{}' permanentemente?", name))
                                .size(12.5)
                                .strong()
                                .color(Color32::from_rgb(255, 130, 120)),
                        );
                        if pill_btn(ui, "Excluir", Color32::from_rgb(215, 55, 50)).clicked() {
                            let res = if target.is_dir() {
                                fs::remove_dir_all(&target)
                            } else {
                                fs::remove_file(&target)
                            };
                            match res {
                                Ok(_) => {
                                    self.delete_confirm_target = None;
                                    self.refresh_entries();
                                    self.set_toast(format!("'{}' excluído", name));
                                }
                                Err(e) => self.set_toast(format!("Erro ao excluir: {e}")),
                            }
                        }
                        if pill_btn(ui, "Cancelar", Color32::from_rgba_unmultiplied(255, 255, 255, 20)).clicked() {
                            self.delete_confirm_target = None;
                        }
                    });
                    ui.add_space(8.0);
                }

                let q = self.search_query.trim().to_lowercase();
                let visible: Vec<FileEntry> = self
                    .entries
                    .iter()
                    .filter(|e| (self.show_hidden || !e.is_hidden) && (q.is_empty() || e.name.to_lowercase().contains(&q)))
                    .cloned()
                    .collect();

                if visible.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new("Pasta vazia")
                                .size(15.0)
                                .color(Color32::from_rgb(140, 150, 170)),
                        );
                    });
                    return;
                }

                let mut to_open: Option<FileEntry> = None;
                let mut to_select: Option<PathBuf> = None;
                let mut to_context: Option<(FileEntry, Pos2)> = None;

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.view_mode {
                        ViewMode::Grid => {
                            let avail_w = ui.available_width().max(240.0);
                            let cols = ((avail_w / 128.0).floor() as usize).max(2);
                            let card_w = (avail_w - (cols as f32 - 1.0) * 10.0) / cols as f32;
                            let card_h = 108.0;

                            for chunk in visible.chunks(cols) {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 10.0;
                                    for entry in chunk {
                                        let selected =
                                            self.selected_path.as_ref() == Some(&entry.path);
                                        let (rect, resp) = ui.allocate_exact_size(
                                            Vec2::new(card_w, card_h),
                                            Sense::click(),
                                        );
                                        let bg = if selected {
                                            Color32::from_rgba_unmultiplied(
                                                accent.r(),
                                                accent.g(),
                                                accent.b(),
                                                64,
                                            )
                                        } else if resp.hovered() {
                                            Color32::from_rgba_unmultiplied(255, 255, 255, 16)
                                        } else {
                                            Color32::from_rgba_unmultiplied(255, 255, 255, 8)
                                        };
                                        ui.painter().rect(
                                            rect,
                                            CornerRadius::same(11),
                                            bg,
                                            if selected {
                                                Stroke::new(1.2, accent)
                                            } else {
                                                card_stroke
                                            },
                                            egui::StrokeKind::Inside,
                                        );

                                        let icon_rect = Rect::from_center_size(
                                            Pos2::new(rect.center().x, rect.min.y + 38.0),
                                            Vec2::new(46.0, 42.0),
                                        );
                                        draw_file_badge(ui.painter(), icon_rect, entry, accent);

                                        let name_trunc = truncate_name(&entry.name, 15);
                                        ui.painter().text(
                                            Pos2::new(rect.center().x, rect.max.y - 22.0),
                                            egui::Align2::CENTER_CENTER,
                                            &name_trunc,
                                            FontId::proportional(12.0),
                                            Color32::WHITE,
                                        );

                                        if resp.double_clicked() {
                                            to_open = Some(entry.clone());
                                        } else if resp.clicked() {
                                            to_select = Some(entry.path.clone());
                                        } else if resp.secondary_clicked() {
                                            to_select = Some(entry.path.clone());
                                            let p = ctx
                                                .input(|i| i.pointer.interact_pos())
                                                .unwrap_or(rect.center());
                                            to_context = Some((entry.clone(), p));
                                        }
                                    }
                                });
                                ui.add_space(10.0);
                            }
                        }
                        ViewMode::List => {
                            for entry in &visible {
                                let selected = self.selected_path.as_ref() == Some(&entry.path);
                                let (rect, resp) = ui.allocate_exact_size(
                                    Vec2::new(ui.available_width(), 34.0),
                                    Sense::click(),
                                );
                                let bg = if selected {
                                    Color32::from_rgba_unmultiplied(
                                        accent.r(),
                                        accent.g(),
                                        accent.b(),
                                        64,
                                    )
                                } else if resp.hovered() {
                                    Color32::from_rgba_unmultiplied(255, 255, 255, 14)
                                } else {
                                    Color32::TRANSPARENT
                                };
                                ui.painter().rect_filled(rect, CornerRadius::same(7), bg);

                                let badge_r = Rect::from_center_size(
                                    Pos2::new(rect.min.x + 20.0, rect.center().y),
                                    Vec2::new(26.0, 22.0),
                                );
                                draw_file_badge(ui.painter(), badge_r, entry, accent);

                                ui.painter().text(
                                    Pos2::new(rect.min.x + 42.0, rect.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    &entry.name,
                                    FontId::proportional(13.0),
                                    Color32::WHITE,
                                );

                                let size_txt = if entry.is_dir {
                                    "Pasta".to_string()
                                } else {
                                    format_bytes(entry.size_bytes)
                                };
                                ui.painter().text(
                                    Pos2::new(rect.max.x - 150.0, rect.center().y),
                                    egui::Align2::RIGHT_CENTER,
                                    size_txt,
                                    FontId::proportional(11.5),
                                    Color32::from_rgb(160, 170, 188),
                                );
                                ui.painter().text(
                                    Pos2::new(rect.max.x - 12.0, rect.center().y),
                                    egui::Align2::RIGHT_CENTER,
                                    &entry.modified_str,
                                    FontId::proportional(11.5),
                                    Color32::from_rgb(145, 155, 174),
                                );

                                if resp.double_clicked() {
                                    to_open = Some(entry.clone());
                                } else if resp.clicked() {
                                    to_select = Some(entry.path.clone());
                                } else if resp.secondary_clicked() {
                                    to_select = Some(entry.path.clone());
                                    let p = ctx
                                        .input(|i| i.pointer.interact_pos())
                                        .unwrap_or(rect.center());
                                    to_context = Some((entry.clone(), p));
                                }
                                ui.add_space(2.0);
                            }
                        }
                    });

                if let Some(path) = to_select {
                    self.selected_path = Some(path);
                }
                if let Some(entry) = to_open {
                    self.open_entry(&entry);
                }
                if let Some(cm) = to_context {
                    self.context_menu = Some(cm);
                }
            });

        // Right-click Context Menu
        if let Some((entry, raw_pos)) = self.context_menu.clone() {
            let menu_w = 200.0;
            let pos = Pos2::new(
                raw_pos.x.clamp(10.0, (self.width as f32 - menu_w - 10.0).max(10.0)),
                raw_pos.y.clamp(10.0, (self.height as f32 - 180.0).max(10.0)),
            );
            let approx_rect = Rect::from_min_size(pos, Vec2::new(menu_w, 165.0));
            if ctx.input(|i| i.pointer.any_pressed())
                && ctx
                    .input(|i| i.pointer.interact_pos())
                    .is_some_and(|p| !approx_rect.contains(p))
            {
                self.context_menu = None;
                return;
            }

            egui::Window::new("rocketde_files_ctx_menu")
                .order(egui::Order::Tooltip)
                .title_bar(false)
                .resizable(false)
                .fixed_pos(pos)
                .fixed_size(Vec2::new(menu_w, 0.0))
                .frame(
                    Frame::NONE
                        .fill(Color32::from_rgb(24, 27, 39))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(8))
                        .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 42))),
                )
                .show(&ctx, |ui| {
                    ui.set_min_width(menu_w - 16.0);
                    if ctx_row(ui, "Abrir", accent) {
                        self.open_entry(&entry);
                        self.context_menu = None;
                    }
                    if entry.is_dir && ctx_row(ui, "Abrir no Terminal", accent) {
                        self.open_in_terminal(&entry.path);
                        self.context_menu = None;
                    }
                    if ctx_row(ui, "Copiar Caminho", accent) {
                        let txt = entry.path.display().to_string();
                        let _ = Command::new("wl-copy")
                            .arg(&txt)
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .spawn();
                        self.set_toast("Caminho copiado".to_string());
                        self.context_menu = None;
                    }
                    ui.separator();
                    if ctx_row(ui, "Renomear…", accent) {
                        self.rename_target = Some(entry.path.clone());
                        self.rename_buf = entry.name.clone();
                        self.context_menu = None;
                    }
                    if ctx_row(ui, "Excluir…", Color32::from_rgb(215, 55, 50)) {
                        self.delete_confirm_target = Some(entry.path.clone());
                        self.context_menu = None;
                    }
                });
        }
    }
}

fn draw_file_badge(painter: &egui::Painter, rect: Rect, entry: &FileEntry, accent: Color32) {
    let col = entry.kind.badge_color(accent);
    if entry.is_dir {
        let tab = Rect::from_min_size(
            rect.min,
            Vec2::new(rect.width() * 0.44, rect.height() * 0.24),
        );
        painter.rect_filled(tab, CornerRadius::same(4), col);
        let body = Rect::from_min_max(
            Pos2::new(rect.min.x, rect.min.y + rect.height() * 0.14),
            rect.max,
        );
        painter.rect_filled(body, CornerRadius::same(6), col);
        painter.rect_filled(
            body.shrink2(Vec2::new(2.0, 2.0)),
            CornerRadius::same(5),
            Color32::from_rgba_unmultiplied(255, 255, 255, 24),
        );
    } else {
        painter.rect(
            rect,
            CornerRadius::same(7),
            Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 44),
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 145)),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            entry.kind.badge_label(),
            FontId::proportional((rect.height() * 0.30).clamp(9.0, 12.0)),
            Color32::WHITE,
        );
    }
}

fn inline_action_bar(ui: &mut egui::Ui, stroke: Stroke, content: impl FnOnce(&mut egui::Ui)) {
    Frame::NONE
        .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 14))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(10, 7))
        .stroke(stroke)
        .show(ui, |ui| {
            ui.horizontal(content);
        });
}

fn nav_icon_btn(ui: &mut egui::Ui, kind: IconKind, enabled: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(28.0, 26.0), Sense::click());
    let bg = if enabled && resp.hovered() {
        Color32::from_rgba_unmultiplied(255, 255, 255, 32)
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 14)
    };
    ui.painter().rect_filled(rect, CornerRadius::same(7), bg);
    let col = if enabled {
        Color32::WHITE
    } else {
        Color32::from_rgb(100, 108, 124)
    };
    draw_icon(ui.painter(), rect.center(), 11.5, kind, col);
    enabled && resp.clicked()
}

fn pill_btn(ui: &mut egui::Ui, label: &str, bg: Color32) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(11.5).strong().color(Color32::WHITE))
            .fill(bg)
            .corner_radius(CornerRadius::same(8))
            .min_size(Vec2::new(0.0, 26.0)),
    )
}

fn ctx_row(ui: &mut egui::Ui, label: &str, hover_col: Color32) -> bool {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(r, CornerRadius::same(6), hover_col);
    }
    ui.painter().text(
        Pos2::new(r.min.x + 10.0, r.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.5),
        Color32::WHITE,
    );
    resp.clicked()
}

fn truncate_name(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{}…", head.trim_end())
    } else {
        head
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

fn format_system_time(st: SystemTime) -> String {
    let dt: DateTime<Local> = st.into();
    dt.format("%d/%m/%Y %H:%M").to_string()
}

fn build_places() -> Vec<(&'static str, PathBuf, IconKind)> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let mut list = vec![("Início", home.clone(), IconKind::Rocket)];
    for (label, candidates, icon) in [
        ("Área de Trabalho", vec!["Desktop"], IconKind::Star),
        ("Documentos", vec!["Documents", "Documentos"], IconKind::OpenApp),
        ("Downloads", vec!["Downloads"], IconKind::Bolt),
        ("Imagens", vec!["Pictures", "Imagens"], IconKind::Camera),
        ("Vídeos", vec!["Videos", "Vídeos"], IconKind::VideoRecord),
        ("Músicas", vec!["Music", "Músicas"], IconKind::MusicNote),
    ] {
        for sub in candidates {
            let p = home.join(sub);
            if p.is_dir() {
                list.push((label, p, icon));
                break;
            }
        }
    }
    list.push(("Sistema (/)", PathBuf::from("/"), IconKind::Terminal));
    list
}

fn main() {
    let start_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/"));

    let conn = Connection::connect_to_env().expect("Falha ao conectar ao Wayland");
    let (globals, mut event_queue) =
        registry_queue_init(&conn).expect("Falha ao inicializar fila Wayland");
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor indisponível");
    let xdg_shell = XdgShell::bind(&globals, &qh).expect("xdg_wm_base indisponível");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm indisponível");

    let wl_surface = compositor.create_surface(&qh);
    let window = xdg_shell.create_window(wl_surface, WindowDecorations::RequestServer, &qh);
    window.set_title("Arquivos — RocketDE");
    window.set_app_id("rocketde-files");
    window.set_min_size(Some((800, 520)));
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
        label: Some("rocketde_files_device"),
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

    let init_w = 980u32;
    let init_h = 640u32;
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

    let mut app = FilesApp {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        _compositor: compositor,
        shm,
        window,
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
        cfg,
        current_dir: start_dir.clone(),
        entries: Vec::new(),
        history_back: Vec::new(),
        history_fwd: Vec::new(),
        selected_path: None,
        view_mode: ViewMode::Grid,
        show_hidden: false,
        search_query: String::new(),
        editing_path: false,
        path_input: start_dir.display().to_string(),
        new_folder_mode: false,
        new_folder_name: String::new(),
        rename_target: None,
        rename_buf: String::new(),
        delete_confirm_target: None,
        context_menu: None,
        toast: None,
        places: build_places(),
    };
    app.navigate_to(start_dir, false);

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
        if app.dirty || !app.pending_events.is_empty() {
            app.render_frame();
        }
    }
}

impl CompositorHandler for FilesApp {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.dirty = true;
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for FilesApp {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output_state }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for FilesApp {
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
        let w = configure.new_size.0.map(NonZeroU32::get).unwrap_or(980);
        let h = configure.new_size.1.map(NonZeroU32::get).unwrap_or(640);
        self.resize(w, h);
        self.render_frame();
    }
}

impl SeatHandler for FilesApp {
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

impl KeyboardHandler for FilesApp {
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

impl FilesApp {
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

impl PointerHandler for FilesApp {
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
        Keysym::F5 => Some(Key::F5),
        Keysym::h | Keysym::H => Some(Key::H),
        _ => None,
    }
}

impl ShmHandler for FilesApp {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}

impl ProvidesRegistryState for FilesApp {
    fn registry(&mut self) -> &mut RegistryState { &mut self.registry_state }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(FilesApp);
delegate_registry!(FilesApp);
