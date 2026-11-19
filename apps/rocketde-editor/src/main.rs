mod session;

use crate::session::{TabDoc, default_session_dir, load_session_from, save_session_to};
use egui::{
    Align, Align2, Color32, Context, CornerRadius, Event, FontId, Frame, Key, Layout, Margin,
    Modifiers, Pos2, RawInput, Rect, RichText, Sense, Stroke, Vec2,
};
use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use rocketde_core::config::RocketConfig;
use rocketde_ui_common::components::{
    accent_color, border_stroke, card_bg, danger_color, elevated_bg, panel_bg, success_color,
    text_color, text_muted_color, warning_color,
};
use rocketde_ui_common::clipboard::{handle_egui_clipboard_output, push_clipboard_events};
use rocketde_ui_common::header_bar::{
    ResizeDir, WindowAction, detect_window_resize, header_bar_drag_area, window_controls_right,
};
use rocketde_ui_common::icons::{IconKind, draw_icon};
use rocketde_ui_common::keysym::translate_keysym_raw;
use rocketde_ui_common::theme::apply_macos_theme;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::protocol::{
    wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface,
};
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle, globals::registry_queue_init};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::registry_handlers;
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers as WlModifiers, RawModifiers,
};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::xdg::window::{Window, WindowConfigure, WindowDecorations, WindowHandler};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::delegate_registry;
use std::fs;
use std::os::fd::AsFd;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileModalMode {
    Open,
    SaveAs,
}

#[derive(Debug, Clone)]
struct BrowserEntry {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

struct EditorApp {
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

    cfg: RocketConfig,
    session_dir: PathBuf,
    tabs: Vec<TabDoc>,
    active_tab: usize,
    next_untitled: usize,
    font_size: f32,
    word_wrap: bool,
    session_dirty: bool,
    last_edit_at: Option<Instant>,

    // Find & Replace
    find_open: bool,
    replace_open: bool,
    focus_find: bool,
    find_query: String,
    replace_query: String,
    focus_editor: bool,
    more_menu_open: bool,

    // Modals & notifications
    file_modal: Option<FileModalMode>,
    modal_dir: PathBuf,
    modal_filename: String,
    modal_show_hidden: bool,
    modal_entries: Vec<BrowserEntry>,
    close_confirm_idx: Option<usize>,
    toast: Option<(String, bool, Instant)>,
}

impl EditorApp {
    fn new_tab_id(&self) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("tab_{}_{}", nanos, self.next_untitled)
    }

    fn create_empty_tab(&mut self) {
        let id = self.new_tab_id();
        let title = format!("Sem título {}", self.next_untitled);
        self.next_untitled += 1;
        self.tabs.push(TabDoc {
            id,
            title,
            path: None,
            content: String::new(),
            dirty: false,
            cursor_line: 1,
            cursor_col: 1,
        });
        self.active_tab = self.tabs.len() - 1;
        self.focus_editor = true;
        self.mark_session_dirty();
    }

    fn open_path(&mut self, path: PathBuf) {
        let canonical = fs::canonicalize(&path).unwrap_or(path);
        if let Some(existing) = self
            .tabs
            .iter()
            .position(|t| t.path.as_ref() == Some(&canonical))
        {
            self.active_tab = existing;
            self.focus_editor = true;
            self.mark_session_dirty();
            return;
        }

        match fs::read_to_string(&canonical) {
            Ok(content) => {
                let title = canonical
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("arquivo")
                    .to_string();
                let id = self.new_tab_id();
                self.next_untitled += 1;
                // Replace initial untouched empty tab if it's the only one
                if self.tabs.len() == 1
                    && self.tabs[0].path.is_none()
                    && !self.tabs[0].dirty
                    && self.tabs[0].content.is_empty()
                {
                    self.tabs[0] = TabDoc {
                        id,
                        title,
                        path: Some(canonical.clone()),
                        content,
                        dirty: false,
                        cursor_line: 1,
                        cursor_col: 1,
                    };
                    self.active_tab = 0;
                } else {
                    self.tabs.push(TabDoc {
                        id,
                        title,
                        path: Some(canonical.clone()),
                        content,
                        dirty: false,
                        cursor_line: 1,
                        cursor_col: 1,
                    });
                    self.active_tab = self.tabs.len() - 1;
                }
                self.focus_editor = true;
                self.flush_session();
                self.set_toast(format!("Aberto: {}", canonical.display()), false);
            }
            Err(err) => {
                self.set_toast(format!("Erro ao abrir arquivo: {err}"), true);
            }
        }
    }

    fn save_active_tab(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        let idx = self.active_tab.min(self.tabs.len() - 1);
        if let Some(path) = self.tabs[idx].path.clone() {
            self.write_tab_to_path(idx, path);
        } else {
            self.open_file_modal(FileModalMode::SaveAs);
        }
    }

    fn write_tab_to_path(&mut self, idx: usize, path: PathBuf) {
        let Some(tab) = self.tabs.get_mut(idx) else {
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(err) = fs::create_dir_all(parent) {
                self.set_toast(format!("Erro ao criar pasta: {err}"), true);
                return;
            }
        }
        match fs::write(&path, &tab.content) {
            Ok(()) => {
                tab.path = Some(path.clone());
                tab.title = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("arquivo")
                    .to_string();
                tab.dirty = false;
                self.flush_session();
                self.set_toast(format!("Salvo em {}", path.display()), false);
            }
            Err(err) => {
                self.set_toast(format!("Erro ao salvar: {err}"), true);
            }
        }
    }

    fn close_tab_confirmed(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            self.create_empty_tab();
        } else if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        self.flush_session();
    }

    fn request_close_tab(&mut self, idx: usize) {
        if let Some(tab) = self.tabs.get(idx) {
            if tab.dirty {
                self.close_confirm_idx = Some(idx);
            } else {
                self.close_tab_confirmed(idx);
            }
        }
    }

    fn open_file_modal(&mut self, mode: FileModalMode) {
        let start_dir = self
            .tabs
            .get(self.active_tab)
            .and_then(|t| t.path.as_ref())
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .filter(|p| p.is_dir())
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));

        let default_name = if mode == FileModalMode::SaveAs {
            self.tabs
                .get(self.active_tab)
                .map(|t| {
                    t.path
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .and_then(|s| s.to_str())
                        .unwrap_or("documento.txt")
                        .to_string()
                })
                .unwrap_or_else(|| "documento.txt".to_string())
        } else {
            String::new()
        };

        self.file_modal = Some(mode);
        self.modal_dir = start_dir;
        self.modal_filename = default_name;
        self.refresh_modal_entries();
    }

    fn refresh_modal_entries(&mut self) {
        let mut list = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.modal_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if !self.modal_show_hidden && name.starts_with('.') {
                    continue;
                }
                let is_dir = p.is_dir();
                list.push(BrowserEntry {
                    name,
                    path: p,
                    is_dir,
                });
            }
        }
        list.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.modal_entries = list;
    }

    fn mark_session_dirty(&mut self) {
        self.session_dirty = true;
        self.last_edit_at = Some(Instant::now());
    }

    fn jump_find_match(&mut self, ctx: &Context, forward: bool) {
        if self.find_query.is_empty() {
            return;
        }
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let buf_id = egui::Id::unique(("rocketde_editor_buf", &tab.id));
        let mut state = egui::TextEdit::load_state(ctx, buf_id).unwrap_or_default();
        let cur_char = state
            .cursor
            .char_range()
            .map(|r| usize::from(r.primary.index))
            .unwrap_or(0);
        let cur_byte = tab
            .content
            .char_indices()
            .nth(cur_char)
            .map(|(b, _)| b)
            .unwrap_or(tab.content.len());

        let matches: Vec<(usize, usize)> = tab
            .content
            .match_indices(&self.find_query)
            .map(|(b, s)| (b, b + s.len()))
            .collect();
        if matches.is_empty() {
            return;
        }

        let target = if forward {
            matches
                .iter()
                .find(|(s, _)| *s > cur_byte)
                .copied()
                .unwrap_or(matches[0])
        } else {
            matches
                .iter()
                .rev()
                .find(|(s, _)| *s < cur_byte)
                .copied()
                .unwrap_or(*matches.last().unwrap())
        };

        let start_c = tab.content[..target.0].chars().count();
        let end_c = tab.content[..target.1].chars().count();
        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(start_c),
            egui::text::CCursor::new(end_c),
        )));
        state.store(ctx, buf_id);
        self.focus_editor = true;
    }

    fn flush_session(&mut self) {
        save_session_to(
            &self.session_dir,
            &self.tabs,
            self.active_tab,
            self.font_size,
            self.next_untitled,
            self.word_wrap,
        );
        self.session_dirty = false;
        self.last_edit_at = None;
    }

    fn set_toast(&mut self, msg: String, is_err: bool) {
        self.toast = Some((msg, is_err, Instant::now()));
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

    fn handle_shortcuts(&mut self, ctx: &Context) {
        let (ctrl, shift, esc) = ctx.input(|i| {
            (
                i.modifiers.ctrl || i.modifiers.command,
                i.modifiers.shift,
                i.key_pressed(Key::Escape),
            )
        });

        if esc {
            if self.more_menu_open {
                self.more_menu_open = false;
                return;
            }
            if self.close_confirm_idx.is_some() {
                self.close_confirm_idx = None;
                return;
            }
            if self.file_modal.is_some() {
                self.file_modal = None;
                return;
            }
            if self.find_open {
                self.find_open = false;
                self.focus_editor = true;
                return;
            }
        }

        if self.file_modal.is_some() || self.close_confirm_idx.is_some() {
            return;
        }

        if ctrl && shift && ctx.input(|i| i.key_pressed(Key::S)) {
            self.open_file_modal(FileModalMode::SaveAs);
        } else if ctrl && ctx.input(|i| i.key_pressed(Key::S)) {
            self.save_active_tab();
        } else if ctrl && ctx.input(|i| i.key_pressed(Key::O)) {
            self.open_file_modal(FileModalMode::Open);
        } else if ctrl && ctx.input(|i| i.key_pressed(Key::N)) {
            self.create_empty_tab();
        } else if ctrl && ctx.input(|i| i.key_pressed(Key::W)) {
            self.request_close_tab(self.active_tab);
        } else if ctrl && ctx.input(|i| i.key_pressed(Key::F)) {
            self.find_open = !self.find_open;
            if self.find_open {
                self.focus_find = true;
            } else {
                self.focus_editor = true;
            }
        } else if ctrl && ctx.input(|i| i.key_pressed(Key::Q)) {
            self.flush_session();
            self.exit = true;
        } else if ctrl && ctx.input(|i| i.key_pressed(Key::Tab)) && self.tabs.len() > 1 {
            if shift {
                self.active_tab = if self.active_tab == 0 {
                    self.tabs.len() - 1
                } else {
                    self.active_tab - 1
                };
            } else {
                self.active_tab = (self.active_tab + 1) % self.tabs.len();
            }
            self.focus_editor = true;
            self.mark_session_dirty();
        } else if ctrl && ctx.input(|i| i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals)) {
            self.font_size = (self.font_size + 1.0).min(28.0);
            self.mark_session_dirty();
        } else if ctrl && ctx.input(|i| i.key_pressed(Key::Minus)) {
            self.font_size = (self.font_size - 1.0).max(10.0);
            self.mark_session_dirty();
        } else if ctrl && ctx.input(|i| i.key_pressed(Key::Num0)) {
            self.font_size = 14.0;
            self.mark_session_dirty();
        } else if ctrl {
            let num_keys = [
                Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5,
                Key::Num6, Key::Num7, Key::Num8, Key::Num9,
            ];
            for (idx, k) in num_keys.into_iter().enumerate() {
                if idx < self.tabs.len() && ctx.input(|i| i.key_pressed(k)) {
                    self.active_tab = idx;
                    self.focus_editor = true;
                    self.mark_session_dirty();
                    break;
                }
            }
        }
    }

    fn render_frame(&mut self) {
        if !self.configured {
            return;
        }
        if self.session_dirty
            && self
                .last_edit_at
                .is_some_and(|t| t.elapsed() >= Duration::from_millis(350))
        {
            self.flush_session();
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
            let ctx = ui.ctx().clone();
            self.handle_shortcuts(&ctx);
            self.draw_ui(ui);
        });
        handle_egui_clipboard_output(&full_output.platform_output.commands);

        let min_delay = full_output
            .viewport_output
            .values()
            .map(|v| v.repaint_delay)
            .min();
        let needs_repaint = min_delay.map(|d| d.is_zero()).unwrap_or(false);
        self.repaint_delay = min_delay.filter(|d| !d.is_zero() && *d < Duration::from_secs(60));

        let acquired = self.wgpu_surface.get_current_texture();
        let frame_tex = match acquired {
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
            _ => {
                self.dirty = true;
                return;
            }
        };

        let view = frame_tex
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let paint_jobs = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        let screen_desc = ScreenDescriptor {
            size_in_pixels: [self.surface_config.width, self.surface_config.height],
            pixels_per_point: self.scale_factor,
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rocketde_editor_enc"),
            });

        for (id, deltas) in &full_output.textures_delta.set {
            for image_delta in deltas {
                self.renderer
                    .update_texture(&self.device, &self.queue, *id, image_delta);
            }
        }
        self.renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &paint_jobs,
            &screen_desc,
        );

        let bg = panel_bg(&self.cfg.theme);
        {
            let rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rocketde_editor_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: bg.r() as f64 / 255.0,
                            g: bg.g() as f64 / 255.0,
                            b: bg.b() as f64 / 255.0,
                            a: 0.96,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });
            let mut rpass_static = rpass.forget_lifetime();
            self.renderer
                .render(&mut rpass_static, &paint_jobs, &screen_desc);
        }

        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame_tex);

        for id in &full_output.textures_delta.free {
            self.renderer.free_texture(id);
        }

        match self.pending_window_action {
            WindowAction::Drag => {
                if let Some(ref seat) = self.seat {
                    self.window.move_(seat, self.last_pointer_serial);
                }
                self.pending_window_action = WindowAction::None;
            }
            WindowAction::Resize(dir) => {
                if let Some(ref seat) = self.seat {
                    self.window
                        .resize(seat, self.last_pointer_serial, to_xdg_edge(dir));
                }
                self.pending_window_action = WindowAction::None;
            }
            WindowAction::Minimize => {
                self.window.set_minimized();
                self.pending_window_action = WindowAction::None;
            }
            WindowAction::ToggleMaximize => {
                if self.is_maximized {
                    self.window.unset_maximized();
                } else {
                    self.window.set_maximized();
                }
                self.pending_window_action = WindowAction::None;
            }
            WindowAction::Close => {
                self.flush_session();
                self.exit = true;
                self.pending_window_action = WindowAction::None;
            }
            WindowAction::None => {}
        }

        self.dirty = needs_repaint;
    }

    fn draw_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
        if let Some(dir) = detect_window_resize(ctx, self.is_maximized) {
            self.pending_window_action = WindowAction::Resize(dir);
        }
        let t = self.cfg.theme.clone();
        let accent = accent_color(&t);
        let text_col = text_color(&t);
        let muted_col = text_muted_color(&t);
        let header_bg = elevated_bg(&t);
        let editor_bg = panel_bg(&t);
        let card = card_bg(&t);
        let stroke = border_stroke(&t);
        let header_h = t.spacing.header_height.max(40.0);
        let btn_r = CornerRadius::same(t.radii.button.round().clamp(0.0, 255.0) as u8);

        // 1. Unified Title Bar with Tabs, 3-dot Menu, Window Controls, and Full-Bar Drag
        let mut menu_btn_rect = Rect::NOTHING;
        egui::Panel::top("rocketde_editor_header")
            .exact_size(header_h)
            .frame(
                Frame::NONE
                    .fill(header_bg)
                    .inner_margin(Margin::symmetric(10, 0))
                    .stroke(stroke),
            )
            .show(ui, |ui| {
                let bar_rect = ui.max_rect();
                let full_bar_rect = Rect::from_min_max(
                    Pos2::new(bar_rect.min.x - 10.0, bar_rect.min.y),
                    Pos2::new(bar_rect.max.x + 10.0, bar_rect.max.y),
                );
                let drag_act = header_bar_drag_area(ui, full_bar_rect);
                if drag_act != WindowAction::None {
                    self.pending_window_action = drag_act;
                }

                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let right_reserved = 114.0;
                    let tabs_max_w = (ui.available_width() - right_reserved).max(120.0);

                    ui.allocate_ui_with_layout(
                        Vec2::new(tabs_max_w, header_h - 6.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            egui::ScrollArea::horizontal()
                                .id_salt("editor_titlebar_tabs_scroll")
                                .scroll_source(egui::scroll_area::ScrollSource::MOUSE_WHEEL)
                                .auto_shrink([true, false])
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 5.0;
                                        let mut switch_to: Option<usize> = None;
                                        let mut close_idx: Option<usize> = None;

                                        for (idx, tab) in self.tabs.iter().enumerate() {
                                            let selected = idx == self.active_tab;
                                            let label_w = (tab.title.chars().count().min(22)
                                                as f32
                                                * 7.2
                                                + 44.0)
                                                .clamp(92.0, 200.0);
                                            let (tab_rect, tab_resp) = ui.allocate_exact_size(
                                                Vec2::new(label_w, 27.0),
                                                Sense::click(),
                                            );

                                            let fill = if selected {
                                                Color32::from_rgba_unmultiplied(
                                                    accent.r(),
                                                    accent.g(),
                                                    accent.b(),
                                                    42,
                                                )
                                            } else if tab_resp.hovered() {
                                                card
                                            } else {
                                                Color32::from_rgba_unmultiplied(255, 255, 255, 8)
                                            };
                                            ui.painter().rect_filled(tab_rect, btn_r, fill);
                                            if selected {
                                                ui.painter().rect_stroke(
                                                    tab_rect,
                                                    btn_r,
                                                    Stroke::new(
                                                        1.0,
                                                        Color32::from_rgba_unmultiplied(
                                                            accent.r(),
                                                            accent.g(),
                                                            accent.b(),
                                                            130,
                                                        ),
                                                    ),
                                                    egui::StrokeKind::Inside,
                                                );
                                            }

                                            let close_r = Rect::from_center_size(
                                                Pos2::new(
                                                    tab_rect.max.x - 13.0,
                                                    tab_rect.center().y,
                                                ),
                                                Vec2::splat(16.0),
                                            );
                                            let close_resp = ui.interact(
                                                close_r,
                                                egui::Id::unique(("tab_close", idx)),
                                                Sense::click(),
                                            );
                                            if close_resp.hovered() {
                                                ui.painter().circle_filled(
                                                    close_r.center(),
                                                    7.5,
                                                    Color32::from_rgba_unmultiplied(
                                                        255, 255, 255, 28,
                                                    ),
                                                );
                                            }
                                            draw_icon(
                                                ui.painter(),
                                                close_r.center(),
                                                8.0,
                                                IconKind::Close,
                                                if close_resp.hovered() {
                                                    text_col
                                                } else {
                                                    muted_col
                                                },
                                            );

                                            let mut text_left = tab_rect.min.x + 10.0;
                                            if tab.dirty {
                                                ui.painter().circle_filled(
                                                    Pos2::new(
                                                        tab_rect.min.x + 11.0,
                                                        tab_rect.center().y,
                                                    ),
                                                    3.5,
                                                    warning_color(&t),
                                                );
                                                text_left += 10.0;
                                            }

                                            let title_clip = Rect::from_min_max(
                                                Pos2::new(text_left, tab_rect.min.y),
                                                Pos2::new(close_r.min.x - 4.0, tab_rect.max.y),
                                            );
                                            ui.painter().with_clip_rect(title_clip).text(
                                                Pos2::new(text_left, tab_rect.center().y),
                                                Align2::LEFT_CENTER,
                                                &tab.title,
                                                FontId::proportional(t.typography.small.max(12.0)),
                                                if selected { text_col } else { muted_col },
                                            );

                                            if close_resp.clicked() || tab_resp.middle_clicked() {
                                                close_idx = Some(idx);
                                            } else if tab_resp.clicked() {
                                                switch_to = Some(idx);
                                            }
                                        }

                                        if let Some(i) = switch_to {
                                            self.active_tab = i;
                                            self.focus_editor = true;
                                            self.mark_session_dirty();
                                        }
                                        if let Some(i) = close_idx {
                                            self.request_close_tab(i);
                                        }

                                        let (add_r, add_resp) = ui.allocate_exact_size(
                                            Vec2::splat(26.0),
                                            Sense::click(),
                                        );
                                        if add_resp.hovered() {
                                            ui.painter().rect_filled(add_r, btn_r, card);
                                        }
                                        ui.painter().text(
                                            add_r.center(),
                                            Align2::CENTER_CENTER,
                                            "+",
                                            FontId::proportional(16.0),
                                            text_col,
                                        );
                                        if add_resp.on_hover_text("Nova aba (Ctrl+N)").clicked() {
                                            self.create_empty_tab();
                                        }
                                    });
                                });
                        },
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let ctrl_act = window_controls_right(ui, &t, self.is_maximized);
                        if ctrl_act != WindowAction::None {
                            self.pending_window_action = ctrl_act;
                        }

                        let (dots_r, dots_resp) =
                            ui.allocate_exact_size(Vec2::new(26.0, 24.0), Sense::click());
                        menu_btn_rect = dots_r;
                        let dots_bg = if self.more_menu_open {
                            Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 80)
                        } else if dots_resp.hovered() {
                            card
                        } else {
                            Color32::from_rgba_unmultiplied(255, 255, 255, 12)
                        };
                        ui.painter().rect_filled(dots_r, btn_r, dots_bg);
                        for dy in [-4.5f32, 0.0, 4.5] {
                            ui.painter().circle_filled(
                                Pos2::new(dots_r.center().x, dots_r.center().y + dy),
                                1.5,
                                text_col,
                            );
                        }
                        if dots_resp.on_hover_text("Menu e configurações").clicked() {
                            self.more_menu_open = !self.more_menu_open;
                        }
                    });
                });
            });

        // 3. Bottom Status Bar
        egui::Panel::bottom("rocketde_editor_status")
            .exact_size(26.0)
            .frame(
                Frame::NONE
                    .fill(header_bg)
                    .inner_margin(Margin::symmetric(12, 0))
                    .stroke(stroke),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let active = self.tabs.get(self.active_tab);
                    let path_desc = match active.and_then(|tab| tab.path.as_ref()) {
                        Some(p) => {
                            if active.is_some_and(|tab| tab.dirty) {
                                format!("{} (alterações em rascunho temporário)", p.display())
                            } else {
                                p.display().to_string()
                            }
                        }
                        None => "Rascunho temporário (restaurado automaticamente ao reabrir)"
                            .to_string(),
                    };
                    ui.label(
                        RichText::new(path_desc)
                            .size(t.typography.small)
                            .color(muted_col),
                    );

                    if let Some((ref msg, is_err, ts)) = self.toast {
                        if ts.elapsed() < Duration::from_secs(3) {
                            ui.separator();
                            ui.label(
                                RichText::new(msg)
                                    .size(t.typography.small)
                                    .color(if is_err {
                                        danger_color(&t)
                                    } else {
                                        success_color(&t)
                                    }),
                            );
                            ctx.request_repaint_after(Duration::from_millis(250));
                        } else {
                            self.toast = None;
                        }
                    }

                    let active_stats = active.map(|tab| {
                        (
                            tab.cursor_line,
                            tab.cursor_col,
                            tab.content.lines().count().max(1),
                            tab.content.chars().count(),
                        )
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if ui.small_button("+").clicked() {
                            self.font_size = (self.font_size + 1.0).min(28.0);
                            self.mark_session_dirty();
                        }
                        ui.label(
                            RichText::new(format!("{:.0}px", self.font_size))
                                .size(t.typography.small)
                                .color(muted_col),
                        );
                        if ui.small_button("−").clicked() {
                            self.font_size = (self.font_size - 1.0).max(10.0);
                            self.mark_session_dirty();
                        }
                        ui.separator();
                        let wrap_lbl = if self.word_wrap {
                            "Quebra: ON"
                        } else {
                            "Quebra: OFF"
                        };
                        if ui.small_button(wrap_lbl).clicked() {
                            self.word_wrap = !self.word_wrap;
                            self.mark_session_dirty();
                        }
                        ui.separator();
                        if let Some((ln, col, lines, chars)) = active_stats {
                            ui.label(
                                RichText::new(format!(
                                    "Ln {}, Col {}  •  {} linhas  •  {} chars  •  UTF-8",
                                    ln, col, lines, chars
                                ))
                                .size(t.typography.small)
                                .color(muted_col),
                            );
                        }
                    });
                });
            });

        // 4. Optional Compact Floating Find & Replace Card (top-right over editor)
        if self.find_open {
            let win_r = CornerRadius::same(t.radii.card.round().clamp(0.0, 255.0) as u8);
            egui::Area::new(egui::Id::unique("rocketde_editor_find_float"))
                .order(egui::Order::Foreground)
                .anchor(Align2::RIGHT_TOP, Vec2::new(-14.0, header_h + 10.0))
                .show(ctx, |ui| {
                    Frame::NONE
                        .fill(header_bg)
                        .corner_radius(win_r)
                        .inner_margin(Margin::symmetric(10, 8))
                        .stroke(stroke)
                        .show(ui, |ui| {
                            ui.set_max_width(430.0);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                let find_resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.find_query)
                                        .hint_text("Localizar...")
                                        .desired_width(170.0),
                                );
                                if self.focus_find {
                                    find_resp.request_focus();
                                    self.focus_find = false;
                                }
                                let matches = if self.find_query.is_empty() {
                                    0
                                } else {
                                    self.tabs
                                        .get(self.active_tab)
                                        .map(|tab| tab.content.matches(&self.find_query).count())
                                        .unwrap_or(0)
                                };
                                ui.label(
                                    RichText::new(format!("{matches} enc."))
                                        .size(t.typography.small)
                                        .color(muted_col),
                                );

                                let find_enter = find_resp.lost_focus()
                                    && ui.input(|i| i.key_pressed(Key::Enter));
                                if (toolbar_btn(ui, "↑", card, text_col, btn_r).clicked())
                                    && !self.find_query.is_empty()
                                {
                                    self.jump_find_match(ctx, false);
                                }
                                if (toolbar_btn(ui, "↓", card, text_col, btn_r).clicked()
                                    || find_enter)
                                    && !self.find_query.is_empty()
                                {
                                    self.jump_find_match(ctx, true);
                                }
                                if toolbar_btn(
                                    ui,
                                    "⇄",
                                    if self.replace_open { accent } else { card },
                                    if self.replace_open { Color32::WHITE } else { text_col },
                                    btn_r,
                                )
                                .on_hover_text("Alternar substituir")
                                .clicked()
                                {
                                    self.replace_open = !self.replace_open;
                                }
                                if toolbar_btn(ui, "×", card, muted_col, btn_r).clicked() {
                                    self.find_open = false;
                                    self.focus_editor = true;
                                }
                            });

                            if self.replace_open {
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.replace_query)
                                            .hint_text("Substituir por...")
                                            .desired_width(170.0),
                                    );
                                    if toolbar_btn(ui, "Subst.", card, text_col, btn_r).clicked()
                                        && !self.find_query.is_empty()
                                    {
                                        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                                            if let Some(pos) = tab.content.find(&self.find_query) {
                                                tab.content.replace_range(
                                                    pos..pos + self.find_query.len(),
                                                    &self.replace_query,
                                                );
                                                tab.dirty = true;
                                                self.mark_session_dirty();
                                            }
                                        }
                                    }
                                    if toolbar_btn(ui, "Tudo", accent, Color32::WHITE, btn_r)
                                        .clicked()
                                        && !self.find_query.is_empty()
                                    {
                                        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                                            if tab.content.contains(&self.find_query) {
                                                tab.content = tab
                                                    .content
                                                    .replace(&self.find_query, &self.replace_query);
                                                tab.dirty = true;
                                                self.mark_session_dirty();
                                            }
                                        }
                                    }
                                });
                            }
                        });
                });
        }

        // 4b. 3-Dots Dropdown Menu (File actions, Settings, Session info)
        if self.more_menu_open {
            let menu_w = 240.0;
            let pos = Pos2::new(
                (menu_btn_rect.max.x - menu_w).max(8.0),
                header_h + 4.0,
            );
            let menu_id = egui::Id::unique("rocketde_editor_more_menu");
            let area_resp = egui::Area::new(menu_id)
                .order(egui::Order::Tooltip)
                .fixed_pos(pos)
                .show(ctx, |ui| {
                    Frame::NONE
                        .fill(header_bg)
                        .corner_radius(CornerRadius::same(t.radii.card.round().clamp(0.0, 255.0) as u8))
                        .inner_margin(Margin::same(8))
                        .stroke(stroke)
                        .show(ui, |ui| {
                            ui.set_width(menu_w - 16.0);
                            ui.spacing_mut().item_spacing.y = 3.0;

                            if menu_action_row(ui, "Nova Aba", "Ctrl+N", accent, text_col, muted_col) {
                                self.more_menu_open = false;
                                self.create_empty_tab();
                            }
                            if menu_action_row(ui, "Abrir Arquivo...", "Ctrl+O", accent, text_col, muted_col) {
                                self.more_menu_open = false;
                                self.open_file_modal(FileModalMode::Open);
                            }
                            if menu_action_row(ui, "Salvar", "Ctrl+S", accent, text_col, muted_col) {
                                self.more_menu_open = false;
                                self.save_active_tab();
                            }
                            if menu_action_row(ui, "Salvar Como...", "Ctrl+Shift+S", accent, text_col, muted_col) {
                                self.more_menu_open = false;
                                self.open_file_modal(FileModalMode::SaveAs);
                            }
                            if menu_action_row(ui, "Localizar e Substituir", "Ctrl+F", accent, text_col, muted_col) {
                                self.more_menu_open = false;
                                self.find_open = !self.find_open;
                                if self.find_open {
                                    self.focus_find = true;
                                }
                            }
                            ui.separator();
                            let wrap_txt = if self.word_wrap {
                                "Quebra de Linha (ON)"
                            } else {
                                "Quebra de Linha (OFF)"
                            };
                            if menu_action_row(ui, wrap_txt, "", accent, text_col, muted_col) {
                                self.word_wrap = !self.word_wrap;
                                self.mark_session_dirty();
                            }
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new("  Tamanho da Fonte")
                                        .size(12.0)
                                        .color(text_col),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.small_button("+").clicked() {
                                        self.font_size = (self.font_size + 1.0).min(28.0);
                                        self.mark_session_dirty();
                                    }
                                    ui.label(
                                        RichText::new(format!("{:.0}px", self.font_size))
                                            .size(11.5)
                                            .color(muted_col),
                                    );
                                    if ui.small_button("−").clicked() {
                                        self.font_size = (self.font_size - 1.0).max(10.0);
                                        self.mark_session_dirty();
                                    }
                                });
                            });
                            ui.separator();
                            ui.label(
                                RichText::new("  Rascunhos salvos automaticamente\n  ao editar ou fechar a janela.")
                                    .size(11.0)
                                    .color(muted_col),
                            );
                        });
                });

            if ctx.input(|i| i.pointer.any_pressed()) {
                if let Some(p) = ctx.input(|i| i.pointer.interact_pos()) {
                    if !area_resp.response.rect.expand(4.0).contains(p) && !menu_btn_rect.contains(p) {
                        self.more_menu_open = false;
                    }
                }
            }
        }

        // 5. Main Code/Text Editor Area
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(editor_bg).inner_margin(Margin::same(0)))
            .show(ui, |ui| {
                let idx = self.active_tab.min(self.tabs.len().saturating_sub(1));
                let mut edited = false;
                if let Some(tab) = self.tabs.get_mut(idx) {
                    egui::ScrollArea::both()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                let line_count = tab.content.split('\n').count().max(1);
                                let gutter_digits = format!("{line_count}").len().max(3);
                                let gutter_w = (gutter_digits as f32 * self.font_size * 0.65) + 18.0;
                                let mut line_nums = String::with_capacity(line_count * 5);
                                for n in 1..=line_count {
                                    use std::fmt::Write;
                                    let _ = writeln!(line_nums, "{n:>gutter_digits$}");
                                }

                                Frame::NONE
                                    .fill(Color32::from_rgba_unmultiplied(0, 0, 0, 35))
                                    .inner_margin(Margin {
                                        left: 6,
                                        right: 8,
                                        top: 8,
                                        bottom: 8,
                                    })
                                    .show(ui, |ui| {
                                        ui.set_min_width(gutter_w);
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(line_nums)
                                                    .font(FontId::monospace(self.font_size))
                                                    .color(muted_col),
                                            )
                                            .selectable(false),
                                        );
                                    });

                                Frame::NONE
                                    .inner_margin(Margin::same(8))
                                    .show(ui, |ui| {
                                        let buf_id = egui::Id::unique(("rocketde_editor_buf", &tab.id));
                                        let desired_w = if self.word_wrap {
                                            ui.available_width()
                                        } else {
                                            f32::INFINITY
                                        };
                                        let mut edit_out = egui::TextEdit::multiline(&mut tab.content)
                                            .id(buf_id)
                                            .code_editor()
                                            .font(FontId::monospace(self.font_size))
                                            .text_color(text_col)
                                            .frame(Frame::NONE)
                                            .desired_width(desired_w)
                                            .desired_rows(30)
                                            .lock_focus(true)
                                            .show(ui);

                                        if edit_out.response.has_focus()
                                            && ui.input(|i| i.key_pressed(Key::Tab) && !i.modifiers.ctrl)
                                        {
                                            if let Some(cursor) = edit_out.cursor_range {
                                                let char_idx: usize = cursor.primary.index.into();
                                                let byte_idx = tab
                                                    .content
                                                    .char_indices()
                                                    .nth(char_idx)
                                                    .map(|(b, _)| b)
                                                    .unwrap_or(tab.content.len());
                                                tab.content.insert_str(byte_idx, "    ");
                                                let new_c = egui::text::CCursor::new(char_idx + 4);
                                                edit_out.state.cursor.set_char_range(Some(
                                                    egui::text::CCursorRange::one(new_c),
                                                ));
                                                edit_out.state.store(ctx, buf_id);
                                                tab.dirty = true;
                                                edited = true;
                                            }
                                        }

                                        if self.focus_editor {
                                            edit_out.response.request_focus();
                                            self.focus_editor = false;
                                        }
                                        if edit_out.response.changed() {
                                            tab.dirty = true;
                                            edited = true;
                                        }
                                        if let Some(cursor) = edit_out.cursor_range {
                                            let char_idx: usize = cursor.primary.index.into();
                                            let prefix: String =
                                                tab.content.chars().take(char_idx).collect();
                                            tab.cursor_line = prefix.split('\n').count().max(1);
                                            tab.cursor_col = prefix
                                                .rsplit('\n')
                                                .next()
                                                .map(|l| l.chars().count() + 1)
                                                .unwrap_or(1);
                                        }
                                    });
                            });
                        });
                }
                if edited {
                    self.mark_session_dirty();
                }
            });

        // 6. Modals (Open / SaveAs / Close dirty tab confirm)
        self.render_modals(ctx, &t);
    }

    fn render_modals(&mut self, ctx: &Context, t: &rocketde_core::design::ThemeConfig) {
        let accent = accent_color(t);
        let text_col = text_color(t);
        let muted_col = text_muted_color(t);
        let modal_bg = elevated_bg(t);
        let card = card_bg(t);
        let stroke = border_stroke(t);
        let btn_r = CornerRadius::same(t.radii.button.round().clamp(0.0, 255.0) as u8);
        let win_r = CornerRadius::same(t.radii.window.round().clamp(0.0, 255.0) as u8);

        if let Some(mode) = self.file_modal {
            let mut close = false;
            let mut open_target: Option<PathBuf> = None;
            let mut save_target: Option<PathBuf> = None;
            let mut nav_dir: Option<PathBuf> = None;

            let title = match mode {
                FileModalMode::Open => "Abrir Arquivo",
                FileModalMode::SaveAs => "Salvar Arquivo Como",
            };

            egui::Window::new(title)
                .order(egui::Order::Foreground)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .fixed_size(Vec2::new(500.0, 380.0))
                .frame(
                    Frame::NONE
                        .fill(modal_bg)
                        .corner_radius(win_r)
                        .inner_margin(Margin::same(14))
                        .stroke(stroke),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        if toolbar_btn(ui, "↑ Acima", card, text_col, btn_r).clicked() {
                            if let Some(p) = self.modal_dir.parent() {
                                nav_dir = Some(p.to_path_buf());
                            }
                        }
                        if toolbar_btn(ui, "Home", card, text_col, btn_r).clicked() {
                            if let Some(h) = dirs::home_dir() {
                                nav_dir = Some(h);
                            }
                        }
                        if toolbar_btn(
                            ui,
                            if self.modal_show_hidden {
                                "Ocultos: ON"
                            } else {
                                "Ocultos: OFF"
                            },
                            card,
                            text_col,
                            btn_r,
                        )
                        .clicked()
                        {
                            self.modal_show_hidden = !self.modal_show_hidden;
                            nav_dir = Some(self.modal_dir.clone());
                        }
                        ui.label(
                            RichText::new(self.modal_dir.display().to_string())
                                .size(t.typography.small)
                                .color(muted_col),
                        );
                    });
                    ui.add_space(6.0);

                    Frame::NONE
                        .fill(card)
                        .corner_radius(btn_r)
                        .inner_margin(Margin::same(6))
                        .stroke(stroke)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .max_height(220.0)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    for entry in &self.modal_entries {
                                        let label = if entry.is_dir {
                                            format!("📁  {}", entry.name)
                                        } else {
                                            format!("📄  {}", entry.name)
                                        };
                                        let resp = ui.selectable_label(false, label);
                                        if resp.clicked() {
                                            if entry.is_dir {
                                                nav_dir = Some(entry.path.clone());
                                            } else if mode == FileModalMode::Open {
                                                open_target = Some(entry.path.clone());
                                            } else {
                                                self.modal_filename = entry.name.clone();
                                            }
                                        }
                                    }
                                });
                        });

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Nome/Caminho:").color(muted_col));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.modal_filename)
                                .desired_width(ui.available_width()),
                        );
                    });

                    ui.add_space(10.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let primary_lbl = if mode == FileModalMode::Open {
                            "Abrir"
                        } else {
                            "Salvar"
                        };
                        let enter_confirm = ctx.input(|i| i.key_pressed(Key::Enter));
                        if toolbar_btn(ui, primary_lbl, accent, Color32::WHITE, btn_r).clicked()
                            || enter_confirm
                        {
                            let entered = PathBuf::from(self.modal_filename.trim());
                            let full = if entered.is_absolute() {
                                entered
                            } else {
                                self.modal_dir.join(entered)
                            };
                            if mode == FileModalMode::Open {
                                if full.is_dir() {
                                    nav_dir = Some(full);
                                } else {
                                    open_target = Some(full);
                                }
                            } else if !self.modal_filename.trim().is_empty() {
                                save_target = Some(full);
                            }
                        }
                        if toolbar_btn(ui, "Cancelar", card, text_col, btn_r).clicked() {
                            close = true;
                        }
                    });
                });

            if let Some(d) = nav_dir {
                self.modal_dir = d;
                self.refresh_modal_entries();
            } else if let Some(p) = open_target {
                self.file_modal = None;
                self.open_path(p);
            } else if let Some(p) = save_target {
                self.file_modal = None;
                let idx = self.active_tab.min(self.tabs.len().saturating_sub(1));
                self.write_tab_to_path(idx, p);
            } else if close {
                self.file_modal = None;
            }
        }

        if let Some(idx) = self.close_confirm_idx {
            let tab_title = self
                .tabs
                .get(idx)
                .map(|tab| tab.title.clone())
                .unwrap_or_default();
            let mut action = 0u8; // 1=save, 2=discard, 3=cancel

            egui::Window::new("Alterações não salvas")
                .order(egui::Order::Foreground)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .fixed_size(Vec2::new(380.0, 0.0))
                .frame(
                    Frame::NONE
                        .fill(modal_bg)
                        .corner_radius(win_r)
                        .inner_margin(Margin::same(16))
                        .stroke(stroke),
                )
                .show(ctx, |ui| {
                    ui.label(
                        RichText::new(format!(
                            "Deseja salvar as alterações de '{}' antes de fechar esta aba?",
                            tab_title
                        ))
                        .size(t.typography.body)
                        .color(text_col),
                    );
                    ui.add_space(14.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if toolbar_btn(ui, "Salvar", accent, Color32::WHITE, btn_r).clicked() {
                            action = 1;
                        }
                        if toolbar_btn(ui, "Descartar", danger_color(t), Color32::WHITE, btn_r)
                            .clicked()
                        {
                            action = 2;
                        }
                        if toolbar_btn(ui, "Cancelar", card, text_col, btn_r).clicked() {
                            action = 3;
                        }
                    });
                });

            match action {
                1 => {
                    self.close_confirm_idx = None;
                    self.active_tab = idx;
                    if let Some(p) = self.tabs.get(idx).and_then(|tab| tab.path.clone()) {
                        self.write_tab_to_path(idx, p);
                        self.close_tab_confirmed(idx);
                    } else {
                        self.open_file_modal(FileModalMode::SaveAs);
                    }
                }
                2 => {
                    self.close_confirm_idx = None;
                    self.close_tab_confirmed(idx);
                }
                3 => {
                    self.close_confirm_idx = None;
                }
                _ => {}
            }
        }
    }
}

fn menu_action_row(
    ui: &mut egui::Ui,
    label: &str,
    shortcut: &str,
    accent: Color32,
    text_col: Color32,
    muted_col: Color32,
) -> bool {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(
            r,
            CornerRadius::same(6),
            Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 60),
        );
    }
    ui.painter().text(
        Pos2::new(r.min.x + 8.0, r.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.5),
        text_col,
    );
    if !shortcut.is_empty() {
        ui.painter().text(
            Pos2::new(r.max.x - 8.0, r.center().y),
            Align2::RIGHT_CENTER,
            shortcut,
            FontId::proportional(11.0),
            muted_col,
        );
    }
    resp.clicked()
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

fn toolbar_btn(
    ui: &mut egui::Ui,
    label: &str,
    bg: Color32,
    fg: Color32,
    radius: CornerRadius,
) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(11.5).strong().color(fg))
            .fill(bg)
            .corner_radius(radius)
            .min_size(Vec2::new(0.0, 26.0)),
    )
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
    window.set_title("Editor de Texto — RocketDE");
    window.set_app_id("rocketde-editor");
    window.set_min_size(Some((680, 460)));
    window.commit();

    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::VULKAN | wgpu::Backends::GL;
    let instance = wgpu::Instance::new(desc);
    let raw_display_ptr = NonNull::new(conn.backend().display_ptr() as *mut _).unwrap();
    let raw_surface_ptr = NonNull::new(window.wl_surface().id().as_ptr() as *mut _).unwrap();
    let wgpu_surface = unsafe {
        instance
            .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                    raw_display_ptr,
                ))),
                raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(
                    raw_surface_ptr,
                )),
            })
            .expect("Falha ao criar superfície wgpu")
    };
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: Some(&wgpu_surface),
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("Nenhum adaptador wgpu compatível");
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("rocketde_editor_dev"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
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
        .find(|f| matches!(f, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm))
        .unwrap_or(wgpu::TextureFormat::Bgra8Unorm);
    let alpha_mode = caps
        .alpha_modes
        .first()
        .copied()
        .unwrap_or(wgpu::CompositeAlphaMode::Auto);

    let init_w = 960u32;
    let init_h = 660u32;
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

    let session_dir = default_session_dir();
    let (tabs, active_tab, font_size, next_untitled, word_wrap) =
        load_session_from(&session_dir);

    let mut app = EditorApp {
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
        cfg,
        session_dir,
        tabs,
        active_tab,
        next_untitled,
        font_size,
        word_wrap,
        session_dirty: false,
        last_edit_at: None,
        find_open: false,
        replace_open: false,
        focus_find: false,
        find_query: String::new(),
        replace_query: String::new(),
        focus_editor: true,
        more_menu_open: false,
        file_modal: None,
        modal_dir: dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
        modal_filename: String::new(),
        modal_show_hidden: false,
        modal_entries: Vec::new(),
        close_confirm_idx: None,
        toast: None,
    };

    if app.tabs.is_empty() {
        app.create_empty_tab();
    }
    for arg in std::env::args().skip(1) {
        let p = PathBuf::from(arg);
        if p.exists() {
            app.open_path(p);
        }
    }

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
            } else if app.session_dirty {
                120
            } else if let Some(d) = app.repaint_delay {
                d.as_millis().clamp(16, 1000) as i32
            } else {
                -1
            };
            let ret = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
            if ret > 0 && (pfd.revents & libc::POLLIN) != 0 {
                let _ = guard.read();
            } else {
                drop(guard);
                if ret == 0 && (app.repaint_delay.is_some() || app.session_dirty) {
                    app.dirty = true;
                }
            }
        }
        let _ = event_queue.dispatch_pending(&mut app);
        if app.exit {
            app.flush_session();
            break;
        }
        if app.dirty || !app.pending_events.is_empty() || app.session_dirty {
            app.render_frame();
        }
    }
}

impl CompositorHandler for EditorApp {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, new_factor: i32) {
        self.scale_factor = (new_factor.max(1)) as f32;
        self.resize(self.width, self.height);
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for EditorApp {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output_state }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for EditorApp {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.flush_session();
        self.exit = true;
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window, configure: WindowConfigure, _: u32) {
        self.is_maximized = configure.is_maximized();
        let (w, h) = configure.new_size;
        let nw = w.map(|v| v.get()).unwrap_or(self.width);
        let nh = h.map(|v| v.get()).unwrap_or(self.height);
        self.resize(nw, nh);
        self.render_frame();
    }
}

impl SeatHandler for EditorApp {
    fn seat_state(&mut self) -> &mut SeatState { &mut self.seat_state }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if self.seat.is_none() {
            self.seat = Some(seat);
        }
    }
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seat_state.get_keyboard(qh, &seat, None).ok();
        }
        if cap == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard {
            if let Some(kb) = self.keyboard.take() { kb.release(); }
        }
        if cap == Capability::Pointer {
            if let Some(p) = self.pointer.take() { p.release(); }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for EditorApp {
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
            mac_cmd: false,
            command: mods.ctrl,
        };
    }
}

impl EditorApp {
    fn push_key(&mut self, event: KeyEvent, pressed: bool) {
        if let Some(key) = translate_keysym_raw(event.keysym.raw()) {
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
            if let Some(txt) = event.utf8 {
                if !txt.is_empty() && !txt.chars().all(|c| c.is_control()) {
                    self.pending_events.push(Event::Text(txt));
                }
            }
        }
        self.dirty = true;
    }
}

impl PointerHandler for EditorApp {
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

fn translate_btn(b: u32) -> Option<egui::PointerButton> {
    match b {
        0x110 => Some(egui::PointerButton::Primary),
        0x111 => Some(egui::PointerButton::Secondary),
        0x112 => Some(egui::PointerButton::Middle),
        _ => None,
    }
}

impl ShmHandler for EditorApp {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}

impl ProvidesRegistryState for EditorApp {
    fn registry(&mut self) -> &mut RegistryState { &mut self.registry_state }
    registry_handlers![OutputState, SeatState];
}
smithay_client_toolkit::delegate_dispatch2!(EditorApp);
delegate_registry!(EditorApp);
