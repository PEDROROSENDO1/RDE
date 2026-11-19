use crate::config::{OverviewPreviewMode, RocketConfig};
use crate::ipc::{IpcCommand, spawn_ipc_server};
use crate::system::apps::AppIndexer;
use crate::system::audio_brightness::HardwareController;
use crate::system::media::MediaController;
use crate::system::recorder::{
    ActiveRecording, RecorderStartConfig, encode_rgba_to_png_bytes, save_screenshot_png,
    start_recording,
};
use crate::system::stats::SystemMonitor;
use crate::system::{WAKE_ALL, WAKE_DOCK, WAKE_HUD, WAKE_TOP_BAR, WakeSignal};
use crate::ui::capture_overlay::{render_capture_overlay, render_capture_toast};
use crate::ui::control_center::render_control_center;
use crate::ui::dock::render_dock;
use crate::ui::launcher::render_launcher;
use crate::ui::overview::render_overview;
use crate::ui::top_bar::{render_global_menu_popups, render_top_bar};
use crate::ui::{
    CaptureMode, CaptureTarget, OsdKind, OverviewThumb, PendingCaptureAction, ShellUiState,
    apply_macos_theme, render_osd,
};
use crate::wayland::capture::{
    TrackedToplevel, WindowCaptureManager, pick_preferred_shm_format,
};
use crate::wayland::clipboard::{
    WaylandClipboard, dispatch_data_control_device, dispatch_data_control_offer,
    dispatch_data_control_source,
};
use crate::wayland::screen_capture::{
    CaptureStreamTag, CapturedOutputFrame, ScreenCaptureManager,
};
use crate::wayland::virtual_kbd::VirtualKeyboardController;
use crate::wm::pila::PilaConnector;

use crossbeam_channel::Receiver;
use egui::{Context, Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, Vec2};
use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::{
    background_effect::{BackgroundEffectHandler, BackgroundEffectState},
    compositor::{CompositorHandler, CompositorState, Region},
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
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler},
};
use std::num::NonZeroU32;
use std::os::fd::AsFd;
use std::os::unix::io::AsRawFd;
use std::ptr::NonNull;
use std::time::{Duration, Instant};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
    backend::ObjectId,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface},
};
use wayland_protocols::ext::{
    background_effect::v1::client::ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1,
    foreign_toplevel_list::v1::client::{
        ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
        ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
    },
    image_capture_source::v1::client::{
        ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
        ext_image_capture_source_v1::ExtImageCaptureSourceV1,
        ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
    },
    image_copy_capture::v1::client::{
        ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
        ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1,
        ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
    },
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::{
    data_control::v1::client::{
        zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
        zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
        zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
        zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
    },
    screencopy::v1::client::{
        zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
        zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
    },
};

/// Lightweight opt-in tracing (`ROCKETDE_TRACE=1`) for surface/overlay state transitions.
fn trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("ROCKETDE_TRACE").is_some_and(|v| v != "0"))
}

macro_rules! rtrace {
    ($($arg:tt)*) => {
        if trace_enabled() {
            eprintln!("[rocketde:trace {}] {}", chrono::Local::now().format("%H:%M:%S%.3f"), format!($($arg)*));
        }
    };
}

struct GpuSurface {
    name: &'static str,
    layer: LayerSurface,
    wgpu_surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    egui_ctx: Context,
    renderer: Renderer,
    width: u32,
    height: u32,
    configured: bool,
    mapped: bool,
    dirty: bool,
    repaint_delay: Option<Duration>,
    pending_events: Vec<Event>,
    pointer_pos: Pos2,
    modifiers: Modifiers,
}

impl GpuSurface {
    fn create(
        conn: &Connection,
        compositor: &CompositorState,
        layer_shell: &LayerShell,
        qh: &QueueHandle<WaylandApp>,
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        layer_tier: Layer,
        namespace: &str,
        anchor: Anchor,
        width: u32,
        height: u32,
        exclusive_zone: i32,
        kb_mode: KeyboardInteractivity,
        cfg: &RocketConfig,
    ) -> Self {
        let wl_surface = compositor.create_surface(qh);
        let layer =
            layer_shell.create_layer_surface(qh, wl_surface, layer_tier, Some(namespace), None);
        layer.set_anchor(anchor);
        layer.set_size(width, height);
        layer.set_exclusive_zone(exclusive_zone);
        layer.set_keyboard_interactivity(kb_mode);
        layer.commit();

        let raw_display_ptr = NonNull::new(conn.backend().display_ptr() as *mut _).unwrap();
        let raw_surface_ptr = NonNull::new(layer.wl_surface().id().as_ptr() as *mut _).unwrap();

        let raw_display_handle =
            RawDisplayHandle::Wayland(WaylandDisplayHandle::new(raw_display_ptr));
        let raw_window_handle = RawWindowHandle::Wayland(WaylandWindowHandle::new(raw_surface_ptr));

        let wgpu_surface = unsafe {
            instance
                .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                    raw_display_handle: Some(raw_display_handle),
                    raw_window_handle,
                })
                .expect("Falha ao criar superfície wgpu Wayland")
        };

        let caps = wgpu_surface.get_capabilities(adapter);
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
            .unwrap_or_else(|| {
                caps.formats
                    .first()
                    .copied()
                    .unwrap_or(wgpu::TextureFormat::Bgra8Unorm)
            });

        let alpha_mode = if caps
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
        {
            wgpu::CompositeAlphaMode::PreMultiplied
        } else if caps
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PostMultiplied)
        {
            wgpu::CompositeAlphaMode::PostMultiplied
        } else {
            caps.alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Auto)
        };

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
        };

        let egui_ctx = Context::default();
        apply_macos_theme(&egui_ctx, cfg);

        let renderer = Renderer::new(device, format, RendererOptions::default());

        Self {
            name: match namespace {
                "rocketde-bar" => "bar",
                "rocketde-dock" => "dock",
                _ => "hud",
            },
            layer,
            wgpu_surface,
            surface_config,
            egui_ctx,
            renderer,
            width: width.max(1),
            height: height.max(1),
            configured: false,
            mapped: false,
            dirty: true,
            repaint_delay: None,
            pending_events: Vec::with_capacity(32),
            pointer_pos: Pos2::ZERO,
            modifiers: Modifiers::NONE,
        }
    }

    fn resize(&mut self, device: &wgpu::Device, new_w: u32, new_h: u32) {
        self.width = new_w.max(1);
        self.height = new_h.max(1);
        self.surface_config.width = self.width;
        self.surface_config.height = self.height;
        self.wgpu_surface.configure(device, &self.surface_config);
        self.configured = true;
        self.dirty = true;
    }

    fn render_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        time_sec: f64,
        mut run_ui: impl FnMut(&mut egui::Ui),
    ) -> bool {
        if !self.configured {
            return false;
        }

        let raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(self.width as f32, self.height as f32),
            )),
            time: Some(time_sec),
            events: std::mem::take(&mut self.pending_events),
            ..Default::default()
        };

        let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
            run_ui(ui);
        });

        let min_delay = full_output
            .viewport_output
            .values()
            .map(|v| v.repaint_delay)
            .min();
        let needs_repaint = min_delay.map(|d| d.is_zero()).unwrap_or(false);
        self.repaint_delay = min_delay.filter(|d| !d.is_zero() && *d < Duration::from_secs(60));

        let acquire_start = Instant::now();
        let acquired = self.wgpu_surface.get_current_texture();
        let acquire_ms = acquire_start.elapsed().as_secs_f32() * 1000.0;
        let frame_tex = match acquired {
            wgpu::CurrentSurfaceTexture::Success(tex) => tex,
            wgpu::CurrentSurfaceTexture::Suboptimal(tex) => {
                self.wgpu_surface.configure(device, &self.surface_config);
                tex
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                rtrace!("{}: acquire outdated/lost ({acquire_ms:.1}ms)", self.name);
                self.wgpu_surface.configure(device, &self.surface_config);
                return true;
            }
            other => {
                rtrace!(
                    "{}: acquire FAILED {:?} ({acquire_ms:.1}ms)",
                    self.name,
                    std::mem::discriminant(&other)
                );
                return needs_repaint;
            }
        };
        if acquire_ms > 8.0 {
            rtrace!("{}: slow acquire {acquire_ms:.1}ms", self.name);
        }

        let view = frame_tex
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        for (id, deltas) in &full_output.textures_delta.set {
            for image_delta in deltas {
                self.renderer
                    .update_texture(device, queue, *id, image_delta);
            }
        }

        let paint_jobs = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);

        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [self.width, self.height],
            pixels_per_point: full_output.pixels_per_point,
        };

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rocketde_egui_encoder"),
        });

        let user_cmd_bufs = self.renderer.update_buffers(
            device,
            queue,
            &mut encoder,
            &paint_jobs,
            &screen_descriptor,
        );

        {
            let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rocketde_render_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
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
        queue.submit(all_cmds);
        let present_start = Instant::now();
        queue.present(frame_tex);
        let present_ms = present_start.elapsed().as_secs_f32() * 1000.0;
        rtrace!(
            "{}: presented {}x{} (was_mapped={}, shapes={}, present {present_ms:.1}ms)",
            self.name,
            self.width,
            self.height,
            self.mapped,
            paint_jobs.len()
        );
        self.mapped = true;

        needs_repaint
    }
}

struct WaylandApp {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    background_effect_state: BackgroundEffectState,
    hud_blur_surface: Option<ExtBackgroundEffectSurfaceV1>,
    last_hud_blur_region: Option<(bool, u32, u32)>,
    capture_mgr: WindowCaptureManager,
    screen_capture: ScreenCaptureManager,
    clipboard: WaylandClipboard,
    active_recording: Option<ActiveRecording>,
    pending_overlay_snapshot_mode: Option<CaptureMode>,
    pending_capture_overlay_delay: Option<(CaptureMode, Instant)>,
    pending_screenshot_action: Option<PendingCaptureAction>,
    cached_bg_frame: Option<CapturedOutputFrame>,
    qh: QueueHandle<WaylandApp>,

    device: wgpu::Device,
    queue: wgpu::Queue,

    top_bar_surface: GpuSurface,
    dock_surface: GpuSurface,
    hud_surface: GpuSurface,
    bg_surface: GpuSurface,
    loaded_wallpaper_path: Option<String>,
    wallpaper_tex: Option<egui::TextureHandle>,
    wallpaper_tx: std::sync::mpsc::Sender<(Option<String>, Option<egui::ColorImage>)>,
    wallpaper_rx: std::sync::mpsc::Receiver<(Option<String>, Option<egui::ColorImage>)>,

    ui_state: ShellUiState,
    vkbd: VirtualKeyboardController,
    ipc_rx: Receiver<IpcCommand>,
    start_instant: Instant,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    active_pointer_surface: Option<wl_surface::WlSurface>,
    last_overlay_kb_exclusive: bool,
    last_kb_mode: u8,
    last_fullscreen_hidden: bool,
    last_hide_bar: bool,
    last_hide_dock: bool,
    last_hud_needed: bool,
    hot_corner_armed: bool,
    last_hot_corner_trigger: Instant,
    last_hud_input_region: Option<(bool, Option<[i32; 4]>, u32, u32)>,
    last_dock_input_region: Option<(u8, Option<[i32; 4]>, u32, u32)>,
    last_dock_surface_size: (u32, u32),
    last_dock_bounds: Option<Rect>,
    last_periodic_refresh: Instant,
    exit: bool,
}

impl WaylandApp {
    fn update_fullscreen_state(&mut self) {
        let is_fs = if self.ui_state.launcher_open || self.ui_state.overview_open {
            false
        } else if let Ok(guard) = self.ui_state.wm.state.lock() {
            let active_ws = guard
                .workspaces
                .iter()
                .find(|ws| ws.active)
                .map(|ws| ws.index);
            guard
                .focused_window
                .as_ref()
                .map(|w| w.fullscreen)
                .unwrap_or(false)
                || guard.windows.iter().any(|w| {
                    w.fullscreen && active_ws.map(|aws| w.workspace == aws).unwrap_or(true)
                })
        } else {
            false
        };

        self.ui_state.fullscreen_hidden = is_fs;
    }

    fn sync_dock_auto_hide(&mut self) {
        let (screen_w, screen_h) = self
            .output_state
            .outputs()
            .next()
            .and_then(|o| self.output_state.info(&o))
            .and_then(|info| info.logical_size)
            .unwrap_or((
                self.top_bar_surface.width.max(800) as i32,
                (self.top_bar_surface.height + self.hud_surface.height).max(600) as i32,
            ));

        let dock_h = (self.ui_state.config.dock.icon_size
            + 18.0
            + self.ui_state.config.dock.margin_bottom.max(0) as f32)
            .ceil() as i32;
        let dock_top_y = screen_h - dock_h;
        let dock_w = self
            .last_dock_bounds
            .map(|r| r.width().ceil() as i32)
            .unwrap_or(320);
        let dock_left_x = (screen_w - dock_w) / 2;
        let dock_right_x = dock_left_x + dock_w;

        let obstructed = if let Ok(guard) = self.ui_state.wm.state.lock() {
            let active_ws = guard
                .workspaces
                .iter()
                .find(|ws| ws.active)
                .map(|ws| ws.index);
            guard.windows.iter().any(|w| {
                if w.minimized {
                    return false;
                }
                let on_ws = w.all_workspaces
                    || active_ws.map(|aws| w.workspace == aws).unwrap_or(true);
                if !on_ws {
                    return false;
                }
                if w.maximized || w.fullscreen {
                    return true;
                }
                w.y + w.height > dock_top_y
                    && w.x < dock_right_x
                    && (w.x + w.width) > dock_left_x
            })
        } else {
            false
        };

        if obstructed != self.ui_state.dock_obstructed {
            rtrace!("dock_obstructed -> {obstructed}");
            self.ui_state.dock_obstructed = obstructed;
            self.dock_surface.dirty = true;
        }
    }

    fn sync_hud_interactivity(&mut self) {
        self.update_fullscreen_state();
        self.sync_dock_auto_hide();
        let battery_popup_open =
            self.ui_state.active_menu == crate::ui::ActiveGlobalMenu::BatteryPopup;
        let tray_popup_open = matches!(
            self.ui_state.active_menu,
            crate::ui::ActiveGlobalMenu::VolumePopup
                | crate::ui::ActiveGlobalMenu::BrightnessPopup
                | crate::ui::ActiveGlobalMenu::BatteryPopup
        );
        self.ui_state
            .sys
            .set_detailed_active(self.ui_state.control_center_open || battery_popup_open);
        self.ui_state.sys.set_cpu_ram_active(
            self.ui_state.config.top_bar.show_cpu_ram
                || self.ui_state.control_center_open
                || battery_popup_open,
        );
        self.ui_state.media.set_active(
            self.ui_state.config.top_bar.show_media || self.ui_state.control_center_open,
        );
        self.ui_state
            .hw
            .set_detailed_active(self.ui_state.control_center_open || tray_popup_open);

        let fullscreen_hidden = self.ui_state.fullscreen_hidden;
        if fullscreen_hidden != self.last_fullscreen_hidden {
            rtrace!("fullscreen_hidden -> {fullscreen_hidden}");
            self.last_fullscreen_hidden = fullscreen_hidden;
            self.top_bar_surface.dirty = true;
            self.dock_surface.dirty = true;
            self.hud_surface.dirty = true;

            if fullscreen_hidden {
                self.ui_state.close_overlays();
                self.top_bar_surface.layer.set_layer(Layer::Background);
                self.top_bar_surface.layer.set_exclusive_zone(0);
                if let Ok(empty_region) = Region::new(&self.compositor) {
                    self.top_bar_surface
                        .layer
                        .wl_surface()
                        .set_input_region(Some(empty_region.wl_region()));
                }
                self.top_bar_surface.layer.commit();

                self.hud_surface.layer.set_layer(Layer::Background);
                self.hud_surface.layer.commit();
            } else {
                let bar_h = self.ui_state.config.top_bar.height.max(24) as i32;
                self.top_bar_surface.layer.set_layer(Layer::Top);
                self.top_bar_surface.layer.set_exclusive_zone(bar_h);
                self.top_bar_surface.layer.wl_surface().set_input_region(None);
                self.top_bar_surface.layer.commit();

                self.hud_surface.layer.set_layer(Layer::Top);
                self.hud_surface.layer.commit();
            }
        }

        // Elevate to Overlay full-screen when Launchpad or Overview is open.
        // Launchpad / Wi-Fi password uses Exclusive; Overview uses OnDemand so Pila's
        // `exclusive_layer()` stays None and `pila msg focus <id>` + `move-to-workspace` works!
        let fullscreen_launcher = self.ui_state.launcher_open
            || self.ui_state.overview_open
            || self.ui_state.capture_overlay_open;
        let kb_mode: u8 = if self.ui_state.launcher_open
            || self.ui_state.capture_overlay_open
            || (self.ui_state.control_center_open && self.ui_state.cc_wifi_selected_ssid.is_some())
        {
            2
        } else if self.ui_state.overview_open
            || self.ui_state.control_center_open
            || self.ui_state.active_menu != crate::ui::ActiveGlobalMenu::None
        {
            1
        } else {
            0
        };

        let mut needs_commit = false;
        if fullscreen_launcher != self.last_overlay_kb_exclusive {
            rtrace!("fullscreen overlay -> {fullscreen_launcher}");
            self.last_overlay_kb_exclusive = fullscreen_launcher;
            self.top_bar_surface.dirty = true;
            self.hud_surface.dirty = true;
            self.hud_surface.layer.set_layer(if fullscreen_launcher {
                Layer::Overlay
            } else if fullscreen_hidden {
                Layer::Background
            } else {
                Layer::Top
            });
            self.hud_surface
                .layer
                .set_exclusive_zone(if fullscreen_launcher { -1 } else { 0 });
            needs_commit = true;

            if !fullscreen_hidden {
                if let Ok(empty_region) = Region::new(&self.compositor) {
                    self.top_bar_surface.layer.wl_surface().set_input_region(
                        if fullscreen_launcher {
                            Some(empty_region.wl_region())
                        } else {
                            None
                        },
                    );
                    self.top_bar_surface.layer.wl_surface().commit();
                }
            }
        }

        if kb_mode != self.last_kb_mode {
            rtrace!("hud kb_mode {} -> {kb_mode}", self.last_kb_mode);
            self.last_kb_mode = kb_mode;
            self.hud_surface.layer.set_keyboard_interactivity(match kb_mode {
                2 => KeyboardInteractivity::Exclusive,
                1 => KeyboardInteractivity::OnDemand,
                _ => KeyboardInteractivity::None,
            });
            needs_commit = true;
        }

        if needs_commit {
            self.hud_surface.layer.commit();
        }
    }

    fn update_hud_input_region(&mut self) {
        let overlay_open = self.ui_state.any_overlay_open();
        let state_key = (overlay_open, None, self.hud_surface.width, self.hud_surface.height);
        if self.last_hud_input_region == Some(state_key) {
            return;
        }
        self.last_hud_input_region = Some(state_key);

        let Ok(region) = Region::new(&self.compositor) else {
            return;
        };

        if overlay_open {
            region.add(
                0,
                0,
                self.hud_surface.width as i32,
                self.hud_surface.height as i32,
            );
        }

        self.hud_surface
            .layer
            .wl_surface()
            .set_input_region(Some(region.wl_region()));
        self.hud_surface.layer.wl_surface().commit();
    }

    fn update_dock_input_region(&mut self, dock_rect: Option<Rect>) {
        let hidden = self.ui_state.fullscreen_hidden
            || self.ui_state.any_overlay_open()
            || !self.ui_state.config.dock.enabled;
        let ctx_menu_open = !hidden && self.ui_state.dock_context_menu.is_some();
        let auto_hidden = !hidden
            && !ctx_menu_open
            && self.ui_state.dock_obstructed
            && !self.ui_state.dock_edge_hovered
            && self.ui_state.dock_reveal_t < 0.1;
        let effective_rect = if hidden { None } else { dock_rect };
        let rect_key = effective_rect.map(|r| {
            [
                r.min.x.floor() as i32,
                r.min.y.floor() as i32,
                r.width().ceil() as i32,
                r.height().ceil() as i32,
            ]
        });
        let mode: u8 = if ctx_menu_open {
            2
        } else if auto_hidden {
            1
        } else {
            0
        };
        let state_key = (
            mode,
            rect_key,
            self.dock_surface.width,
            self.dock_surface.height,
        );
        if self.last_dock_input_region == Some(state_key) {
            return;
        }
        self.last_dock_input_region = Some(state_key);

        let Ok(region) = Region::new(&self.compositor) else {
            return;
        };

        if ctx_menu_open {
            region.add(
                0,
                0,
                self.dock_surface.width as i32,
                self.dock_surface.height as i32,
            );
        } else if auto_hidden {
            let strip_h = 6;
            let strip_y = (self.dock_surface.height as i32 - strip_h).max(0);
            if let Some([x, _, w, _]) = rect_key {
                region.add(x, strip_y, w, strip_h);
            } else {
                region.add(0, strip_y, self.dock_surface.width as i32, strip_h);
            }
        } else if let Some([x, y, w, h]) = rect_key {
            region.add(x, y, w, h);
        }

        self.dock_surface
            .layer
            .wl_surface()
            .set_input_region(Some(region.wl_region()));
        self.dock_surface.layer.wl_surface().commit();
    }

    fn update_hud_blur_region(&mut self) {
        // Keep compositor blur off for Launchpad so opening and interacting with it stays
        // at full framerate with zero delayed blur pop or iGPU stall.
        let overview_static = self.ui_state.overview_open
            && !self.ui_state.overview_closing
            && self.ui_state.config.overview.compositor_blur
            && self.ui_state.overview_anim_t >= 0.99;
        let want_blur = overview_static && !self.ui_state.fullscreen_hidden;
        let key = (want_blur, self.hud_surface.width, self.hud_surface.height);
        if self.last_hud_blur_region == Some(key) {
            return;
        }
        self.last_hud_blur_region = Some(key);

        if let Some(ref blur_surf) = self.hud_blur_surface {
            if want_blur {
                if let Ok(region) = Region::new(&self.compositor) {
                    region.add(
                        0,
                        0,
                        self.hud_surface.width as i32,
                        self.hud_surface.height as i32,
                    );
                    blur_surf.set_blur_region(Some(region.wl_region()));
                }
            } else {
                blur_surf.set_blur_region(None);
            }
            self.hud_surface.layer.wl_surface().commit();
        }
    }

    fn process_ipc_messages(&mut self) -> bool {
        let mut changed = false;
        while let Ok(cmd) = self.ipc_rx.try_recv() {
            changed = true;
            match cmd {
                IpcCommand::ToggleLauncher => self.ui_state.toggle_launcher(),
                IpcCommand::OpenLauncher => self.ui_state.open_launcher(),
                IpcCommand::CloseLauncher => {
                    if self.ui_state.overview_open {
                        self.ui_state.overview_closing = true;
                        self.ui_state.overview_drag_window_id = None;
                    } else {
                        self.ui_state.close_overlays();
                    }
                }
                IpcCommand::ToggleOverview => self.ui_state.toggle_overview(),
                IpcCommand::ToggleControlCenter => {
                    self.ui_state.control_center_open = !self.ui_state.control_center_open;
                    self.ui_state.launcher_open = false;
                }
                IpcCommand::ToggleDock => {
                    self.ui_state.config.dock.enabled = !self.ui_state.config.dock.enabled;
                }
                IpcCommand::OpenScreenshot => {
                    self.request_open_capture_overlay(CaptureMode::Screenshot);
                }
                IpcCommand::OpenScreenRecord => {
                    self.request_open_capture_overlay(CaptureMode::Record);
                }
                IpcCommand::StopRecording => {
                    self.ui_state.pending_capture_action =
                        Some(PendingCaptureAction::StopRecording);
                }
                IpcCommand::VolumeDelta(delta) => {
                    let (cur, muted) = self
                        .ui_state
                        .hw
                        .state
                        .lock()
                        .map(|g| (g.volume_percent, g.muted))
                        .unwrap_or((50.0, false));
                    let next = (cur + (delta as f32)).clamp(0.0, 150.0);
                    self.ui_state.hw.set_volume(next);
                    self.ui_state.osd = Some((
                        OsdKind::Volume,
                        next,
                        muted,
                        std::time::Instant::now(),
                    ));
                    self.hud_surface.dirty = true;
                }
                IpcCommand::MuteToggle => {
                    self.ui_state.hw.toggle_mute();
                    let (cur, was_muted) = self
                        .ui_state
                        .hw
                        .state
                        .lock()
                        .map(|g| (g.volume_percent, g.muted))
                        .unwrap_or((50.0, false));
                    self.ui_state.osd = Some((
                        OsdKind::Volume,
                        cur,
                        !was_muted,
                        std::time::Instant::now(),
                    ));
                    self.hud_surface.dirty = true;
                }
                IpcCommand::BrightnessDelta(delta) => {
                    let cur = self
                        .ui_state
                        .hw
                        .state
                        .lock()
                        .map(|g| g.brightness_percent)
                        .unwrap_or(80.0);
                    let next = (cur + (delta as f32)).clamp(1.0, 100.0);
                    self.ui_state.hw.set_brightness(next);
                    self.ui_state.osd = Some((
                        OsdKind::Brightness,
                        next,
                        false,
                        std::time::Instant::now(),
                    ));
                    self.hud_surface.dirty = true;
                }
                IpcCommand::ReloadConfig => {
                    self.ui_state.config = RocketConfig::load_or_create();
                    self.ui_state
                        .apps
                        .set_icon_pack(self.ui_state.config.desktop.icon_pack.as_deref());
                    let bar_h = self.ui_state.config.top_bar.height.max(24);
                    self.top_bar_surface.layer.set_size(0, bar_h);
                    self.top_bar_surface.layer.set_exclusive_zone(bar_h as i32);
                    self.top_bar_surface.layer.commit();
                    self.bg_surface.dirty = true;
                    self.dock_surface.dirty = true;
                    self.hud_surface.dirty = true;
                }
                IpcCommand::Ping => {}
                IpcCommand::Quit => self.exit = true,
            }
        }
        if changed {
            self.top_bar_surface.dirty = true;
            self.dock_surface.dirty = true;
            self.hud_surface.dirty = true;
        }
        changed
    }

    fn request_open_capture_overlay(&mut self, mode: CaptureMode) {
        self.ui_state.close_overlays();
        self.cached_bg_frame = None;
        self.pending_overlay_snapshot_mode = Some(mode);

        let primary_output = self.output_state.outputs().next();
        if let Some(output) = primary_output {
            if self
                .screen_capture
                .request_output_snapshot(&output, false, &self.qh)
            {
                return;
            }
        }

        // Fallback: open overlay without frozen background texture
        self.pending_overlay_snapshot_mode = None;
        self.ui_state.capture_mode = mode;
        self.ui_state.capture_overlay_open = true;
        self.hud_surface.dirty = true;
        self.top_bar_surface.dirty = true;
    }

    fn finish_screenshot_frame(
        &mut self,
        frame: CapturedOutputFrame,
        crop: Option<[u32; 4]>,
        copy_clipboard: bool,
        save_disk: bool,
    ) {
        let override_dir = self.ui_state.config.capture.screenshots_dir.clone();
        match encode_rgba_to_png_bytes(&frame.rgba, frame.width, frame.height, crop) {
            Ok(png_bytes) => {
                let mut copied = false;
                if copy_clipboard {
                    copied = self.clipboard.copy_png(png_bytes.clone(), &self.qh);
                }
                let save_res = if save_disk {
                    Some(save_screenshot_png(&png_bytes, override_dir.as_deref()))
                } else {
                    None
                };
                let toast = match (save_res, copied) {
                    (Some(Ok(_)), true) => "Screenshot salva e copiada para o clipboard!".to_string(),
                    (Some(Ok(_)), false) => "Screenshot salva em Screenshots!".to_string(),
                    (Some(Err(err)), true) => format!("Copiada para clipboard; erro ao salvar: {err}"),
                    (Some(Err(err)), false) => format!("Erro ao salvar screenshot: {err}"),
                    (None, true) => "Screenshot copiada para o clipboard!".to_string(),
                    (None, false) => "Captura concluída.".to_string(),
                };
                self.ui_state.capture_toast = Some((toast, Instant::now()));
            }
            Err(err) => {
                self.ui_state.capture_toast = Some((err, Instant::now()));
            }
        }
        self.hud_surface.dirty = true;
        unsafe {
            libc::malloc_trim(0);
        }
    }

    fn process_capture_events(&mut self) {
        if self.active_recording.as_ref().is_some_and(|r| r.is_finished()) {
            self.screen_capture.stop_stream();
            if let Some(rec) = self.active_recording.take() {
                let msg = match rec.stop() {
                    Ok(path) => format!(
                        "Gravação encerrada: {}",
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("Screencast.mp4")
                    ),
                    Err(e) => format!("Erro na gravação: {e}"),
                };
                self.ui_state.capture_toast = Some((msg, Instant::now()));
            }
            self.ui_state.recording_active = false;
            self.ui_state.recording_started_at = None;
            self.top_bar_surface.dirty = true;
            self.hud_surface.dirty = true;
        }
        if let Some((mode, ready_at)) = self.pending_capture_overlay_delay {
            if Instant::now() >= ready_at {
                self.pending_capture_overlay_delay = None;
                self.request_open_capture_overlay(mode);
            }
        }
        if let Some(frame) = self.screen_capture.take_oneshot_result() {
            if let Some(mode) = self.pending_overlay_snapshot_mode.take() {
                let color_img = frame.to_color_image();
                let tex = self.hud_surface.egui_ctx.load_texture(
                    "rocketde_capture_bg",
                    color_img,
                    egui::TextureOptions::LINEAR,
                );
                self.ui_state.capture_bg_texture = Some(tex);
                self.cached_bg_frame = Some(frame);
                self.ui_state.capture_mode = mode;
                self.ui_state.capture_overlay_open = true;
                self.hud_surface.dirty = true;
                self.top_bar_surface.dirty = true;
            } else if let Some(PendingCaptureAction::ExecuteScreenshot {
                target,
                logical_rect,
                screen_size,
                ..
            }) = self.pending_screenshot_action.take()
            {
                let (copy_clipboard, save_disk) = (
                    self.ui_state.capture_copy_clipboard,
                    self.ui_state.capture_save_disk,
                );
                let crop = if target == CaptureTarget::Selection {
                    Some(logical_to_physical_crop(
                        logical_rect,
                        screen_size,
                        frame.width,
                        frame.height,
                    ))
                } else {
                    None
                };
                self.finish_screenshot_frame(frame, crop, copy_clipboard, save_disk);
            }
        } else if self.screen_capture.take_oneshot_failed() {
            if let Some(mode) = self.pending_overlay_snapshot_mode.take() {
                self.ui_state.capture_mode = mode;
                self.ui_state.capture_overlay_open = true;
                self.hud_surface.dirty = true;
                self.top_bar_surface.dirty = true;
            } else if self.pending_screenshot_action.take().is_some() {
                self.ui_state.capture_toast = Some((
                    "Falha ao capturar tela via Wayland.".to_string(),
                    Instant::now(),
                ));
                self.hud_surface.dirty = true;
            }
        }

        let Some(action) = self.ui_state.pending_capture_action.take() else {
            return;
        };
        match action {
            PendingCaptureAction::OpenOverlay(mode) => {
                let hud_was_visible = self.last_hud_needed;
                self.ui_state.close_overlays();
                self.top_bar_surface.dirty = true;
                self.dock_surface.dirty = true;
                self.hud_surface.dirty = true;
                if hud_was_visible {
                    // Let the HUD render and present a transparent frame so Control Center / popups
                    // disappear from the compositor output before taking the capture snapshot.
                    self.pending_capture_overlay_delay =
                        Some((mode, Instant::now() + Duration::from_millis(45)));
                } else {
                    self.request_open_capture_overlay(mode);
                }
            }
            PendingCaptureAction::ExecuteScreenshot {
                target,
                logical_rect,
                screen_size,
                include_cursor,
                copy_clipboard,
                save_disk,
                window_id,
            } => {
                self.ui_state.capture_copy_clipboard = copy_clipboard;
                self.ui_state.capture_save_disk = save_disk;

                if target == CaptureTarget::Window {
                    if let Some(ref wid) = window_id {
                        let handle_opt = self
                            .capture_mgr
                            .toplevels
                            .values()
                            .find(|tl| &tl.identifier == wid)
                            .or_else(|| {
                                self.capture_mgr
                                    .toplevels
                                    .iter()
                                    .find(|(oid, _)| format!("{:?}", oid).contains(wid))
                                    .map(|(_, tl)| tl)
                            })
                            .map(|tl| tl.handle.clone());
                        if let Some(handle) = handle_opt {
                            if self.screen_capture.request_toplevel_snapshot(
                                &handle,
                                include_cursor,
                                &self.qh,
                            ) {
                                self.cached_bg_frame = None;
                                self.pending_screenshot_action =
                                    Some(PendingCaptureAction::ExecuteScreenshot {
                                        target,
                                        logical_rect,
                                        screen_size,
                                        include_cursor,
                                        copy_clipboard,
                                        save_disk,
                                        window_id,
                                    });
                                return;
                            }
                        }
                    }
                }

                // Use cached freeze-frame immediately if cursor isn't needed and no timer delay
                if !include_cursor && self.ui_state.capture_timer_secs == 0 {
                    if let Some(frame) = self.cached_bg_frame.take() {
                        let crop = if target == CaptureTarget::Selection {
                            Some(logical_to_physical_crop(
                                logical_rect,
                                screen_size,
                                frame.width,
                                frame.height,
                            ))
                        } else {
                            None
                        };
                        self.finish_screenshot_frame(frame, crop, copy_clipboard, save_disk);
                        return;
                    }
                }

                self.cached_bg_frame = None;
                if let Some(output) = self.output_state.outputs().next() {
                    if self
                        .screen_capture
                        .request_output_snapshot(&output, include_cursor, &self.qh)
                    {
                        self.pending_screenshot_action =
                            Some(PendingCaptureAction::ExecuteScreenshot {
                                target,
                                logical_rect,
                                screen_size,
                                include_cursor,
                                copy_clipboard,
                                save_disk,
                                window_id,
                            });
                    }
                }
            }
            PendingCaptureAction::StartRecording {
                target,
                logical_rect,
                screen_size,
                include_cursor,
                fps,
                system_audio,
                mic_audio,
            } => {
                if self.active_recording.is_some() {
                    return;
                }
                let Some(output) = self.output_state.outputs().next() else {
                    self.ui_state.capture_toast = Some((
                        "Nenhum monitor encontrado para gravação.".to_string(),
                        Instant::now(),
                    ));
                    self.hud_surface.dirty = true;
                    return;
                };
                let (phys_w, phys_h) = if let Some(ref frame) = self.cached_bg_frame {
                    (frame.width, frame.height)
                } else if let Some(info) = self.output_state.info(&output) {
                    info.modes
                        .iter()
                        .find(|m| m.current)
                        .map(|m| (m.dimensions.0.max(2) as u32, m.dimensions.1.max(2) as u32))
                        .or_else(|| {
                            info.logical_size
                                .map(|(w, h)| (w.max(2) as u32, h.max(2) as u32))
                        })
                        .unwrap_or((
                            screen_size.x.round().max(2.0) as u32,
                            screen_size.y.round().max(2.0) as u32,
                        ))
                } else {
                    (
                        screen_size.x.round().max(2.0) as u32,
                        screen_size.y.round().max(2.0) as u32,
                    )
                };
                self.cached_bg_frame = None;

                let crop_rect = if target == CaptureTarget::Selection {
                    Some(logical_to_physical_crop(
                        logical_rect,
                        screen_size,
                        phys_w,
                        phys_h,
                    ))
                } else {
                    None
                };

                let (tx, rx) = crossbeam_channel::bounded(4);
                let rec_cfg = RecorderStartConfig {
                    width: phys_w,
                    height: phys_h,
                    crop_rect,
                    fps,
                    system_audio,
                    mic_audio,
                    prefer_vaapi: self.ui_state.config.capture.prefer_vaapi,
                    override_dir: self.ui_state.config.capture.recordings_dir.clone(),
                };
                match start_recording(rec_cfg, rx, tx.clone()) {
                    Ok(rec) => {
                        if self.screen_capture.start_stream(
                            output,
                            include_cursor,
                            fps,
                            tx,
                            &self.qh,
                        ) {
                            self.active_recording = Some(rec);
                            self.ui_state.recording_active = true;
                            self.ui_state.recording_started_at = Some(Instant::now());
                            self.top_bar_surface.dirty = true;
                            self.hud_surface.dirty = true;
                        } else {
                            let _ = rec.stop();
                            self.ui_state.capture_toast = Some((
                                "Compositor não suporta captura de vídeo contínua.".to_string(),
                                Instant::now(),
                            ));
                        }
                    }
                    Err(e) => {
                        self.ui_state.capture_toast = Some((e, Instant::now()));
                    }
                }
            }
            PendingCaptureAction::StopRecording => {
                self.screen_capture.stop_stream();
                if let Some(rec) = self.active_recording.take() {
                    let msg = match rec.stop() {
                        Ok(path) => format!(
                            "Gravação salva: {}",
                            path.file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("Screencast.mp4")
                        ),
                        Err(e) => format!("Erro ao finalizar gravação: {e}"),
                    };
                    self.ui_state.capture_toast = Some((msg, Instant::now()));
                }
                self.ui_state.recording_active = false;
                self.ui_state.recording_started_at = None;
                self.top_bar_surface.dirty = true;
                self.hud_surface.dirty = true;
            }
        }
    }

    fn overview_capture_paused(&self) -> bool {
        self.ui_state.overview_closing
            || (self.ui_state.config.overview.animations && self.ui_state.overview_anim_t < 0.99)
            || self.ui_state.overview_ws_slide.abs() > 0.01
            || self.ui_state.overview_drag_window_id.is_some()
    }

    fn sync_live_captures(&mut self) {
        let mode = self.ui_state.config.overview.preview_mode;
        let want_capture =
            self.ui_state.overview_open && mode != OverviewPreviewMode::Icons;
        let live_mode = mode == OverviewPreviewMode::Live;
        let paused = self.overview_capture_paused();
        let was_active = self.capture_mgr.active;
        self.capture_mgr
            .set_active(want_capture, paused, live_mode, &self.qh);

        if was_active && !want_capture {
            if !self.ui_state.overview_thumbnails.is_empty() {
                self.ui_state.overview_thumbnails.clear();
                unsafe {
                    libc::malloc_trim(0);
                }
            }
            return;
        }

        if !want_capture {
            return;
        }

        for removed in self.capture_mgr.removed_ids.drain(..) {
            let key = format!("{:?}", removed);
            self.ui_state
                .overview_thumbnails
                .retain(|t| t.object_key != key);
        }

        while let Some((obj_id, cap)) = self.capture_mgr.ready_updates.pop_front() {
            let key = format!("{:?}", obj_id);
            let ctx = &self.hud_surface.egui_ctx;
            let app_id_lower = cap.app_id.to_lowercase();
            let title_lower = cap.title.to_lowercase();
            if let Some(existing) = self
                .ui_state
                .overview_thumbnails
                .iter_mut()
                .find(|t| t.object_key == key)
            {
                existing.identifier = cap.identifier;
                existing.app_id = cap.app_id;
                existing.app_id_lower = app_id_lower;
                existing.title = cap.title;
                existing.title_lower = title_lower;
                existing
                    .texture
                    .set(cap.image, egui::TextureOptions::LINEAR);
            } else {
                let tex = ctx.load_texture(
                    format!("ov_thumb_{}", key),
                    cap.image,
                    egui::TextureOptions::LINEAR,
                );
                self.ui_state.overview_thumbnails.push(OverviewThumb {
                    object_key: key,
                    identifier: cap.identifier,
                    app_id: cap.app_id,
                    app_id_lower,
                    title: cap.title,
                    title_lower,
                    texture: tex,
                });
            }
            self.hud_surface.dirty = true;
        }
    }

    fn render_dock_surface(&mut self, elapsed: f64) {
        let mut dock_bounds = None;
        let needs_repaint = {
            let ui_state = &mut self.ui_state;
            self.dock_surface
                .render_frame(&self.device, &self.queue, elapsed, |ui| {
                    let ctx = ui.ctx().clone();
                    dock_bounds = render_dock(&ctx, ui_state);
                })
        };
        self.dock_surface.dirty = needs_repaint;
        if let Some(bounds) = dock_bounds {
            self.last_dock_bounds = Some(bounds);
            let needed_w = ((bounds.width() + 180.0).ceil() as u32)
                .next_multiple_of(128)
                .max(768);
            let needed_h = ((bounds.height() + self.ui_state.config.dock.margin_bottom.max(0) as f32 + 140.0).ceil() as u32)
                .max(280);
            if needed_w > self.dock_surface.width
                || (self.dock_surface.width > needed_w + 256 && self.dock_surface.width > 768)
                || needed_h > self.dock_surface.height
            {
                if self.last_dock_surface_size != (needed_w, needed_h) {
                    self.last_dock_surface_size = (needed_w, needed_h);
                    self.dock_surface.layer.set_size(needed_w, needed_h);
                    self.dock_surface.layer.commit();
                }
            }
        }
    }

    fn redraw_all(&mut self) {
        self.sync_hud_interactivity();
        self.sync_live_captures();
        let elapsed = self.start_instant.elapsed().as_secs_f64();
        let mut any_rendered = false;

        // 0. Sync & render Desktop Wallpaper surface (Layer::Background) only when dirty or changed.
        while let Ok((path_opt, img_opt)) = self.wallpaper_rx.try_recv() {
            if path_opt == self.loaded_wallpaper_path {
                self.wallpaper_tex = img_opt.map(|color_img| {
                    self.bg_surface.egui_ctx.load_texture(
                        "rocketde_wallpaper",
                        color_img,
                        egui::TextureOptions::LINEAR,
                    )
                });
                self.bg_surface.dirty = true;
            }
        }
        if self.ui_state.config.desktop.wallpaper_path != self.loaded_wallpaper_path {
            self.loaded_wallpaper_path = self.ui_state.config.desktop.wallpaper_path.clone();
            if let Some(path) = self.loaded_wallpaper_path.clone() {
                let tx = self.wallpaper_tx.clone();
                std::thread::spawn(move || {
                    let color_img = image::open(&path).ok().map(|img| {
                        let rgba = img.to_rgba8();
                        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
                        egui::ColorImage::from_rgba_unmultiplied([w, h], rgba.as_raw())
                    });
                    if color_img.is_none() {
                        eprintln!("[rocketde] Failed to load wallpaper: {path}");
                    }
                    let _ = tx.send((Some(path), color_img));
                });
            } else {
                self.wallpaper_tex = None;
                self.bg_surface.dirty = true;
            }
        }
        if self.bg_surface.dirty {
            any_rendered = true;
            let tex = self.wallpaper_tex.as_ref();
            let bg_w = self.bg_surface.width as f32;
            let bg_h = self.bg_surface.height as f32;
            let needs_repaint = self.bg_surface.render_frame(&self.device, &self.queue, elapsed, |ui| {
                if let Some(tex) = tex {
                    let screen_rect =
                        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(bg_w, bg_h));
                    ui.painter().image(
                        tex.id(),
                        screen_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }
            });
            self.bg_surface.dirty = needs_repaint;
        }

        // 1. Render Top Bar only when dirty (skipped when Launchpad/Overview is open or fullscreen app hides it)
        let hide_bar = self.ui_state.launcher_open
            || self.ui_state.overview_open
            || self.ui_state.capture_overlay_open
            || self.ui_state.fullscreen_hidden;
        if hide_bar != self.last_hide_bar {
            rtrace!("hide_bar -> {hide_bar}");
            self.last_hide_bar = hide_bar;
            self.top_bar_surface.dirty = true;
        }
        if self.top_bar_surface.dirty || !self.top_bar_surface.pending_events.is_empty() {
            any_rendered = true;
            let prev_overlay_key = (
                self.ui_state.active_menu,
                self.ui_state.control_center_open,
                self.ui_state.launcher_open,
                self.ui_state.overview_open,
            );
            let ui_state = &mut self.ui_state;
            let needs_repaint =
                self.top_bar_surface
                    .render_frame(&self.device, &self.queue, elapsed, |ui| {
                        if !hide_bar {
                            render_top_bar(ui, ui_state);
                        }
                    });
            self.top_bar_surface.dirty = needs_repaint && !hide_bar;
            let next_overlay_key = (
                self.ui_state.active_menu,
                self.ui_state.control_center_open,
                self.ui_state.launcher_open,
                self.ui_state.overview_open,
            );
            if next_overlay_key != prev_overlay_key {
                self.hud_surface.dirty = true;
                self.dock_surface.dirty = true;
            }
            self.sync_hud_interactivity();
        }

        // 2. Render dedicated small Dock Surface (avoids fullscreen GPU clear & blending on hover/bounce).
        // When hidden, render a single transparent frame (with empty input region) instead of
        // detaching the wl_surface buffer via unmap(), so wlr-layer-shell / wgpu remaps reliably.
        let hide_dock = self.ui_state.capture_overlay_open
            || self.ui_state.launcher_open
            || self.ui_state.overview_open
            || self.ui_state.fullscreen_hidden
            || !self.ui_state.config.dock.enabled;
        if hide_dock != self.last_hide_dock {
            rtrace!("hide_dock -> {hide_dock}");
            self.last_hide_dock = hide_dock;
            self.dock_surface.dirty = true;
        }
        if hide_dock {
            self.dock_surface.pending_events.clear();
            if self.dock_surface.dirty {
                any_rendered = true;
                let needs_repaint = self
                    .dock_surface
                    .render_frame(&self.device, &self.queue, elapsed, |_ui| {});
                self.dock_surface.dirty = needs_repaint;
            }
        } else if self.dock_surface.dirty || !self.dock_surface.pending_events.is_empty() {
            any_rendered = true;
            self.render_dock_surface(elapsed);
        }

        // 3. Render fullscreen HUD Surface ONLY when an overlay/popup/OSD/toast is actually active.
        // When not needed, render a single transparent frame (with empty input region) instead of
        // detaching the wl_surface buffer via attach(None), so wlr-layer-shell / wgpu remaps reliably.
        let hud_needed = self.ui_state.capture_overlay_open
            || self.ui_state.launcher_open
            || self.ui_state.overview_open
            || (!self.ui_state.fullscreen_hidden
                && (self.ui_state.active_menu != crate::ui::ActiveGlobalMenu::None
                    || self.ui_state.control_center_open))
            || self.ui_state.capture_toast.is_some()
            || self.ui_state.osd.is_some();

        if hud_needed != self.last_hud_needed {
            rtrace!("hud_needed -> {hud_needed}");
            self.last_hud_needed = hud_needed;
            self.hud_surface.dirty = true;
        }

        if !hud_needed {
            self.hud_surface.pending_events.clear();
            if self.hud_surface.dirty {
                any_rendered = true;
                let needs_repaint = self
                    .hud_surface
                    .render_frame(&self.device, &self.queue, elapsed, |_ui| {});
                self.hud_surface.dirty = needs_repaint;
            }
        } else if self.hud_surface.dirty || !self.hud_surface.pending_events.is_empty() {
            any_rendered = true;
            let was_launcher_open = self.ui_state.launcher_open;
            let was_overview_open = self.ui_state.overview_open;
            let prev_bar_key = (
                self.ui_state.active_menu,
                self.ui_state.control_center_open,
            );
            let needs_repaint = {
                let ui_state = &mut self.ui_state;
                self.hud_surface
                    .render_frame(&self.device, &self.queue, elapsed, |ui| {
                        let ctx = ui.ctx().clone();
                        if ui_state.capture_overlay_open {
                            render_capture_overlay(&ctx, ui_state);
                        } else if ui_state.launcher_open {
                            render_launcher(&ctx, ui_state);
                        } else if ui_state.overview_open {
                            render_overview(&ctx, ui_state);
                        } else if !ui_state.fullscreen_hidden {
                            render_global_menu_popups(&ctx, ui_state);
                            render_control_center(&ctx, ui_state);
                        }
                        render_capture_toast(&ctx, ui_state);
                        render_osd(&ctx, ui_state);
                    })
            };
            self.hud_surface.dirty = needs_repaint;
            let next_bar_key = (
                self.ui_state.active_menu,
                self.ui_state.control_center_open,
            );
            if next_bar_key != prev_bar_key {
                self.top_bar_surface.dirty = true;
            }

            // If Launchpad or Overview was just closed, immediately restore Top Bar & Dock
            if (was_launcher_open && !self.ui_state.launcher_open)
                || (was_overview_open && !self.ui_state.overview_open)
            {
                if was_launcher_open && !self.ui_state.launcher_open {
                    let mut dock_keys: Vec<String> = self
                        .ui_state
                        .config
                        .dock
                        .pinned_apps
                        .iter()
                        .flat_map(|p| [p.app_id.clone(), p.name.clone()])
                        .collect();
                    if let Ok(guard) = self.ui_state.wm.state.lock() {
                        for w in &guard.windows {
                            if !w.app_id.is_empty() {
                                dock_keys.push(w.app_id.clone());
                            }
                            if !w.title.is_empty() {
                                dock_keys.push(w.title.clone());
                            }
                        }
                    }
                    self.ui_state.apps.retain_dock_icons(&dock_keys);
                    unsafe {
                        libc::malloc_trim(0);
                    }
                }

                self.sync_hud_interactivity();
                let hide_bar_now = self.ui_state.launcher_open
                    || self.ui_state.overview_open
                    || self.ui_state.capture_overlay_open
                    || self.ui_state.fullscreen_hidden;
                self.last_hide_bar = hide_bar_now;
                if !hide_bar_now {
                    let ui_state = &mut self.ui_state;
                    let tb_repaint = self.top_bar_surface.render_frame(
                        &self.device,
                        &self.queue,
                        elapsed,
                        |ui| {
                            render_top_bar(ui, ui_state);
                        },
                    );
                    self.top_bar_surface.dirty = tb_repaint;
                } else {
                    self.top_bar_surface.dirty = false;
                }

                let hide_dock_now = self.ui_state.capture_overlay_open
                    || self.ui_state.launcher_open
                    || self.ui_state.overview_open
                    || self.ui_state.fullscreen_hidden
                    || !self.ui_state.config.dock.enabled;
                self.last_hide_dock = hide_dock_now;
                if !hide_dock_now {
                    self.render_dock_surface(elapsed);
                } else {
                    self.dock_surface.dirty = false;
                }
                self.hud_surface.dirty = true;
            }
        }

        if any_rendered {
            self.sync_hud_interactivity();
            let bounds = self.last_dock_bounds;
            self.update_dock_input_region(bounds);
            self.update_hud_input_region();
            self.update_hud_blur_region();
        }
    }

    fn flush_pending_shortcuts(&mut self, conn: &Connection) {
        if self.ui_state.pending_shortcuts.is_empty() {
            return;
        }
        let chords: Vec<_> = self.ui_state.pending_shortcuts.drain(..).collect();
        for chord in chords {
            self.vkbd.send_chord(conn, chord);
        }
    }

    fn flush_pending_window_moves(&mut self, _conn: &Connection) {
        let Some((win_id, target_ws)) = self.ui_state.pending_move_window.take() else {
            return;
        };
        std::thread::spawn(move || {
            PilaConnector::move_window_to_workspace_sync(&win_id, target_ws);
        });
        self.hud_surface.dirty = true;
    }
}

pub fn run_wayland_daemon(initial_launcher_open: bool) -> Result<(), String> {
    let conn = Connection::connect_to_env()
        .map_err(|e| format!("Não foi possível conectar ao compositor Wayland: {e}"))?;

    let (globals, mut event_queue) =
        registry_queue_init(&conn).map_err(|e| format!("Erro ao inicializar fila Wayland: {e}"))?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|e| format!("wl_compositor indisponível: {e}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .map_err(|e| format!("zwlr_layer_shell_v1 indisponível neste compositor: {e}"))?;

    let mut instance_desc = wgpu::InstanceDescriptor::new_without_display_handle();
    instance_desc.backends = wgpu::Backends::VULKAN;
    let mut instance = wgpu::Instance::new(instance_desc);

    let cfg = RocketConfig::load_or_create();

    // Create a temporary surface to select a compatible GPU adapter
    let dummy_wl = compositor.create_surface(&qh);
    let raw_display_ptr = NonNull::new(conn.backend().display_ptr() as *mut _).unwrap();
    let raw_surface_ptr = NonNull::new(dummy_wl.id().as_ptr() as *mut _).unwrap();
    let create_dummy_surface = |inst: &wgpu::Instance| -> Result<wgpu::Surface<'static>, String> {
        unsafe {
            inst.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                    raw_display_ptr,
                ))),
                raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(
                    raw_surface_ptr,
                )),
            })
            .map_err(|e| format!("Erro wgpu surface: {e}"))
        }
    };

    let mut dummy_wgpu_surface = create_dummy_surface(&instance)?;
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        force_fallback_adapter: false,
        compatible_surface: Some(&dummy_wgpu_surface),
        apply_limit_buckets: false,
    })) {
        Ok(a) => a,
        Err(_) => {
            drop(dummy_wgpu_surface);
            let mut gl_desc = wgpu::InstanceDescriptor::new_without_display_handle();
            gl_desc.backends = wgpu::Backends::GL;
            instance = wgpu::Instance::new(gl_desc);
            dummy_wgpu_surface = create_dummy_surface(&instance)?;
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: false,
                compatible_surface: Some(&dummy_wgpu_surface),
                apply_limit_buckets: false,
            }))
            .map_err(|e| format!("Nenhum adaptador gráfico (Vulkan/GLES) encontrado: {e}"))?
        }
    };

    drop(dummy_wgpu_surface);
    dummy_wl.destroy();

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("rocketde_gpu_device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
    }))
    .map_err(|e| format!("Erro ao requisitar dispositivo wgpu: {e}"))?;

    // Surface 0: Desktop Wallpaper surface (Layer::Background, stretches image to fill screen)
    let bg_surface = GpuSurface::create(
        &conn,
        &compositor,
        &layer_shell,
        &qh,
        &instance,
        &adapter,
        &device,
        Layer::Background,
        "rocketde-wallpaper",
        Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
        0,
        0,
        -1,
        KeyboardInteractivity::None,
        &cfg,
    );
    if let Ok(empty_region) = Region::new(&compositor) {
        bg_surface
            .layer
            .wl_surface()
            .set_input_region(Some(empty_region.wl_region()));
        bg_surface.layer.wl_surface().commit();
    }

    // Surface 1: macOS Top Bar (Anchored Top + Left + Right, reserves exclusive zone)
    let bar_h = cfg.top_bar.height;
        let top_bar_surface = GpuSurface::create(
        &conn,
        &compositor,
        &layer_shell,
        &qh,
        &instance,
        &adapter,
        &device,
        Layer::Top,
        "rocketde-bar",
        Anchor::TOP | Anchor::LEFT | Anchor::RIGHT,
        0,
        bar_h,
        bar_h as i32,
        KeyboardInteractivity::None,
        &cfg,
    );

    // Surface 2: Dedicated compact Bottom Dock surface (avoids fullscreen GPU clear & blending)
    let dock_surface = GpuSurface::create(
        &conn,
        &compositor,
        &layer_shell,
        &qh,
        &instance,
        &adapter,
        &device,
        Layer::Top,
        "rocketde-dock",
        Anchor::BOTTOM,
        768,
        260,
        0,
        KeyboardInteractivity::None,
        &cfg,
    );

    // Surface 3: Fullscreen HUD Overlay (Launchpad, Overview, Capture, Popups, OSD, Toast)
    let hud_surface = GpuSurface::create(
        &conn,
        &compositor,
        &layer_shell,
        &qh,
        &instance,
        &adapter,
        &device,
        Layer::Top,
        "rocketde-hud",
        Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
        0,
        0,
        0,
        KeyboardInteractivity::None,
        &cfg,
    );

    let wake_fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
    let wake_signal = WakeSignal::new(wake_fd);
    let make_wake = |bits: u8| {
        let sig = wake_signal.clone();
        move || sig.notify(bits)
    };

    let (ipc_tx, ipc_rx) = crossbeam_channel::unbounded();
    spawn_ipc_server(ipc_tx, make_wake(WAKE_ALL));

    let wm = PilaConnector::new(cfg.top_bar.workspace_count, make_wake(WAKE_TOP_BAR | WAKE_DOCK));
    let sys = SystemMonitor::new(make_wake(WAKE_TOP_BAR));
    let hw = HardwareController::new(make_wake(WAKE_TOP_BAR | WAKE_HUD));
    let media = MediaController::new(make_wake(WAKE_TOP_BAR));
    let apps = AppIndexer::new_with_wake_and_pack(
        make_wake(WAKE_DOCK | WAKE_HUD),
        cfg.desktop.icon_pack.clone(),
    );

    let mut ui_state = ShellUiState::new(cfg, wm, sys, hw, media, apps);
    if initial_launcher_open {
        ui_state.open_launcher();
    }

    let vkbd = VirtualKeyboardController::bind(&globals, &qh);
    let background_effect_state = BackgroundEffectState::new(&globals, &qh);
    let hud_blur_surface = background_effect_state
        .get_background_effect(hud_surface.layer.wl_surface(), &qh)
        .ok();
    let capture_mgr = WindowCaptureManager::bind(&globals, &qh);
    let screen_capture = ScreenCaptureManager::bind(&globals, &qh);
    let clipboard = WaylandClipboard::bind(&globals, &qh);

    let (wallpaper_tx, wallpaper_rx) = std::sync::mpsc::channel();
    let mut app = WaylandApp {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        background_effect_state,
        hud_blur_surface,
        last_hud_blur_region: None,
        capture_mgr,
        screen_capture,
        clipboard,
        active_recording: None,
        pending_overlay_snapshot_mode: None,
        pending_capture_overlay_delay: None,
        pending_screenshot_action: None,
        cached_bg_frame: None,
        qh: qh.clone(),
        device,
        queue,
        top_bar_surface,
        dock_surface,
        hud_surface,
        bg_surface,
        loaded_wallpaper_path: None,
        wallpaper_tex: None,
        wallpaper_tx,
        wallpaper_rx,
        ui_state,
        vkbd,
        ipc_rx,
        start_instant: Instant::now(),
        keyboard: None,
        pointer: None,
        active_pointer_surface: None,
        last_overlay_kb_exclusive: false,
        last_kb_mode: 0,
        last_fullscreen_hidden: false,
        last_hide_bar: false,
        last_hide_dock: false,
        last_hud_needed: false,
        hot_corner_armed: true,
        last_hot_corner_trigger: Instant::now(),
        last_hud_input_region: None,
        last_dock_input_region: None,
        last_dock_surface_size: (768, 260),
        last_dock_bounds: None,
        last_periodic_refresh: Instant::now(),
        exit: false,
    };
    app.sync_hud_interactivity();
    unsafe {
        libc::malloc_trim(0);
    }

    let wayland_fd = conn.as_fd();
    let mut last_clock_bucket: i64 = -1;
    loop {
        let _ = conn.flush();

        if let Some(guard) = conn.prepare_read() {
            let mut pollfds = [
                libc::pollfd {
                    fd: wayland_fd.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: wake_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let nfds = if wake_fd >= 0 { 2 } else { 1 };
            let needs_seconds = app.ui_state.config.top_bar.clock_format.contains("%S")
                || app.ui_state.active_menu == crate::ui::ActiveGlobalMenu::CalendarPopup
                || app.active_recording.is_some();
            let live_mode =
                app.ui_state.config.overview.preview_mode == OverviewPreviewMode::Live;
            let min_egui_delay = [
                app.top_bar_surface.repaint_delay,
                app.dock_surface.repaint_delay,
                app.hud_surface.repaint_delay,
            ]
            .into_iter()
            .flatten()
            .min();
            let timeout_ms = if app.top_bar_surface.dirty
                || app.dock_surface.dirty
                || app.hud_surface.dirty
                || app.screen_capture.is_oneshot_pending()
                || app.pending_capture_overlay_delay.is_some()
            {
                16
            } else if app.active_recording.is_some() {
                (1000 / app.ui_state.capture_fps.max(15)) as i32
            } else if let Some(delay) = min_egui_delay {
                delay.as_millis().clamp(16, 1000) as i32
            } else if app.ui_state.overview_open && app.capture_mgr.has_pending_work(live_mode) {
                200
            } else if needs_seconds {
                1000
            } else {
                let sec_in_min = (chrono::Local::now().timestamp().rem_euclid(60)) as i32;
                ((60 - sec_in_min) * 1000).clamp(1000, 60000)
            };
            let ret = unsafe { libc::poll(pollfds.as_mut_ptr(), nfds, timeout_ms) };
            if ret > 0 && (pollfds[0].revents & libc::POLLIN) != 0 {
                let _ = guard.read();
            } else {
                drop(guard);
            }
            if wake_fd >= 0 && ret > 0 && (pollfds[1].revents & libc::POLLIN) != 0 {
                let mut buf = 0u64;
                unsafe {
                    libc::read(
                        wake_fd,
                        (&mut buf as *mut u64).cast::<libc::c_void>(),
                        std::mem::size_of::<u64>(),
                    );
                }
                let mask = wake_signal.take_mask();
                if (mask & WAKE_TOP_BAR) != 0 {
                    app.top_bar_surface.dirty = true;
                }
                if (mask & WAKE_DOCK) != 0 {
                    app.dock_surface.dirty = true;
                }
                if (mask & WAKE_HUD) != 0
                    || app.ui_state.overview_open
                    || app.ui_state.control_center_open
                    || app.ui_state.active_menu != crate::ui::ActiveGlobalMenu::None
                {
                    app.hud_surface.dirty = true;
                }
            }
            // If an egui repaint timer expired (e.g. OSD fade-out or toast expiry), mark the active surface dirty
            if app.ui_state.osd.is_some() || app.ui_state.capture_toast.is_some() {
                let expired = ret == 0
                    || app
                        .hud_surface
                        .repaint_delay
                        .is_some_and(|d| d.is_zero());
                if expired {
                    app.hud_surface.dirty = true;
                }
            }
        }

        let _ = event_queue.dispatch_pending(&mut app);
        app.process_ipc_messages();
        app.process_capture_events();
        if app.active_recording.is_some() {
            app.screen_capture.pump_stream(&app.qh);
        }

        let needs_seconds = app.ui_state.config.top_bar.clock_format.contains("%S")
            || app.ui_state.active_menu == crate::ui::ActiveGlobalMenu::CalendarPopup
            || app.active_recording.is_some();
        let now_ts = chrono::Local::now().timestamp();
        let clock_bucket = if needs_seconds { now_ts } else { now_ts / 60 };
        if !app.ui_state.launcher_open
            && !app.ui_state.overview_open
            && clock_bucket != last_clock_bucket
        {
            last_clock_bucket = clock_bucket;
            app.top_bar_surface.dirty = true;
            if app.ui_state.active_menu == crate::ui::ActiveGlobalMenu::CalendarPopup {
                app.hud_surface.dirty = true;
            }
            app.last_periodic_refresh = Instant::now();
        }

        if app.ui_state.overview_open
            && app.ui_state.config.overview.preview_mode != OverviewPreviewMode::Icons
        {
            let capture_paused = app.overview_capture_paused();
            let live_mode =
                app.ui_state.config.overview.preview_mode == OverviewPreviewMode::Live;
            app.capture_mgr
                .pump_captures(&app.qh, capture_paused, live_mode);
        }

        app.redraw_all();
        app.flush_pending_window_moves(&conn);
        app.flush_pending_shortcuts(&conn);

        if app.exit {
            break;
        }
    }

    if wake_fd >= 0 {
        unsafe {
            libc::close(wake_fd);
        }
    }

    Ok(())
}

impl CompositorHandler for WaylandApp {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        if self.top_bar_surface.dirty || self.hud_surface.dirty {
            self.redraw_all();
        }
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for WaylandApp {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for WaylandApp {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let w = NonZeroU32::new(configure.new_size.0).map_or(1920, NonZeroU32::get);
        let h = NonZeroU32::new(configure.new_size.1).map_or(1080, NonZeroU32::get);
        rtrace!(
            "configure {:?} -> {}x{} (raw {:?})",
            if layer == &self.top_bar_surface.layer {
                "bar"
            } else if layer == &self.dock_surface.layer {
                "dock"
            } else if layer == &self.bg_surface.layer {
                "bg"
            } else {
                "hud"
            },
            w,
            h,
            configure.new_size
        );

        if layer == &self.top_bar_surface.layer {
            let bar_h = self.ui_state.config.top_bar.height.max(24);
            self.top_bar_surface.resize(&self.device, w, bar_h);
        } else if layer == &self.dock_surface.layer {
            self.dock_surface.resize(&self.device, w, h);
        } else if layer == &self.bg_surface.layer {
            self.bg_surface.resize(&self.device, w, h);
        } else if layer == &self.hud_surface.layer {
            self.hud_surface.resize(&self.device, w, h);
        }

        self.redraw_all();
    }
}

impl SeatHandler for WaylandApp {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        self.vkbd.ensure_device(&seat, qh);
        self.clipboard.ensure_device(&seat, qh);
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            if let Ok(kb) = self.seat_state.get_keyboard(qh, &seat, None) {
                self.keyboard = Some(kb);
            }
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            if let Ok(ptr) = self.seat_state.get_pointer(qh, &seat) {
                self.pointer = Some(ptr);
            }
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            if let Some(kb) = self.keyboard.take() {
                kb.release();
            }
        }
        if capability == Capability::Pointer {
            if let Some(ptr) = self.pointer.take() {
                ptr.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for WaylandApp {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _keysyms: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        self.handle_key_event(event, true);
    }

    fn repeat_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        self.handle_key_event(event, true);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.handle_key_event(event, false);
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _serial: u32,
        modifiers: WlModifiers,
        _raw_modifiers: RawModifiers,
        _layout: u32,
    ) {
        let egui_mods = Modifiers {
            alt: modifiers.alt,
            ctrl: modifiers.ctrl,
            shift: modifiers.shift,
            mac_cmd: modifiers.logo,
            command: modifiers.ctrl || modifiers.logo,
        };
        self.top_bar_surface.modifiers = egui_mods;
        self.dock_surface.modifiers = egui_mods;
        self.hud_surface.modifiers = egui_mods;
    }
}

impl WaylandApp {
    fn handle_key_event(&mut self, event: KeyEvent, pressed: bool) {
        let mods = self.hud_surface.modifiers;

        // Global shortcut inside focused layer: Super+Space or Alt+Space toggles Spotlight Launcher
        if pressed && event.keysym == Keysym::space && (mods.mac_cmd || mods.alt) {
            self.ui_state.toggle_launcher();
            self.top_bar_surface.dirty = true;
            self.hud_surface.dirty = true;
            return;
        }

        if let Some(key) = translate_keysym(event.keysym) {
            self.hud_surface.pending_events.push(Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: mods,
            });
        }

        if pressed && !mods.ctrl && !mods.alt && !mods.mac_cmd {
            if let Some(txt) = &event.utf8 {
                let filtered: String = txt.chars().filter(|c| !c.is_control()).collect();
                if !filtered.is_empty() {
                    self.hud_surface
                        .pending_events
                        .push(Event::Text(filtered));
                }
            }
        }
    }
}

impl PointerHandler for WaylandApp {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let is_top_bar = &event.surface == self.top_bar_surface.layer.wl_surface();
            let is_dock = &event.surface == self.dock_surface.layer.wl_surface();
            let top_bar_w = self.top_bar_surface.width as f32;

            let target = if is_top_bar {
                &mut self.top_bar_surface
            } else if is_dock {
                &mut self.dock_surface
            } else if &event.surface == self.hud_surface.layer.wl_surface() {
                &mut self.hud_surface
            } else {
                continue;
            };

            let pos = Pos2::new(event.position.0 as f32, event.position.1 as f32);
            target.pointer_pos = pos;

            match event.kind {
                PointerEventKind::Enter { .. } => {
                    self.active_pointer_surface = Some(event.surface.clone());
                    target.pending_events.push(Event::PointerMoved(pos));
                    target.dirty = true;
                    if is_dock && !self.ui_state.dock_edge_hovered {
                        self.ui_state.dock_edge_hovered = true;
                        self.dock_surface.dirty = true;
                    }

                    if pos.x <= 6.0 && pos.y <= 6.0 {
                        if (is_top_bar || self.ui_state.overview_open)
                            && !self.ui_state.fullscreen_hidden
                            && self.hot_corner_armed
                            && self.last_hot_corner_trigger.elapsed().as_millis() >= 600
                        {
                            self.hot_corner_armed = false;
                            self.last_hot_corner_trigger = Instant::now();
                            self.ui_state.toggle_overview();
                            self.top_bar_surface.dirty = true;
                            self.hud_surface.dirty = true;
                        }
                    } else if pos.x > 24.0 || pos.y > 24.0 {
                        self.hot_corner_armed = true;
                    }
                }
                PointerEventKind::Leave { .. } => {
                    target.pending_events.push(Event::PointerGone);
                    self.hot_corner_armed = true;
                    if is_dock && self.ui_state.dock_edge_hovered {
                        self.ui_state.dock_edge_hovered = false;
                        self.dock_surface.dirty = true;
                    }
                }
                PointerEventKind::Motion { .. } => {
                    target.pending_events.push(Event::PointerMoved(pos));
                    target.dirty = true;
                    if is_dock && !self.ui_state.dock_edge_hovered {
                        self.ui_state.dock_edge_hovered = true;
                        self.dock_surface.dirty = true;
                    }

                    if pos.x <= 6.0 && pos.y <= 6.0 {
                        if (is_top_bar || self.ui_state.overview_open)
                            && !self.ui_state.fullscreen_hidden
                            && self.hot_corner_armed
                            && self.last_hot_corner_trigger.elapsed().as_millis() >= 600
                        {
                            self.hot_corner_armed = false;
                            self.last_hot_corner_trigger = Instant::now();
                            self.ui_state.toggle_overview();
                            self.top_bar_surface.dirty = true;
                            self.hud_surface.dirty = true;
                        }
                    } else if pos.x > 24.0 || pos.y > 24.0 {
                        self.hot_corner_armed = true;
                    }
                }
                PointerEventKind::Press { button, .. } => {
                    if button == 0x110
                        && pos.x <= 8.0
                        && pos.y <= 8.0
                        && (is_top_bar || self.ui_state.overview_open)
                        && !self.ui_state.fullscreen_hidden
                        && self.last_hot_corner_trigger.elapsed().as_millis() >= 600
                    {
                        self.hot_corner_armed = false;
                        self.last_hot_corner_trigger = Instant::now();
                        self.ui_state.toggle_overview();
                        self.top_bar_surface.dirty = true;
                        self.hud_surface.dirty = true;
                        continue;
                    }
                    if is_top_bar {
                        if self.ui_state.control_center_open && pos.x < (top_bar_w - 80.0) {
                            self.ui_state.control_center_open = false;
                        }
                        if matches!(
                            self.ui_state.active_menu,
                            crate::ui::ActiveGlobalMenu::AppleMenu
                                | crate::ui::ActiveGlobalMenu::FileMenu
                                | crate::ui::ActiveGlobalMenu::EditMenu
                                | crate::ui::ActiveGlobalMenu::ViewMenu
                                | crate::ui::ActiveGlobalMenu::WindowMenu
                                | crate::ui::ActiveGlobalMenu::HelpMenu
                        ) && pos.x > 420.0
                            && (pos.x - (top_bar_w * 0.5)).abs() > 140.0
                        {
                            self.ui_state.active_menu = crate::ui::ActiveGlobalMenu::None;
                        }
                    }

                    if let Some(btn) = translate_mouse_button(button) {
                        target.pending_events.push(Event::PointerButton {
                            pos,
                            button: btn,
                            pressed: true,
                            modifiers: target.modifiers,
                        });
                    }
                }
                PointerEventKind::Release { button, .. } => {
                    if let Some(btn) = translate_mouse_button(button) {
                        target.pending_events.push(Event::PointerButton {
                            pos,
                            button: btn,
                            pressed: false,
                            modifiers: target.modifiers,
                        });
                    }
                }
                PointerEventKind::Axis {
                    horizontal,
                    vertical,
                    ..
                } => {
                    let dx = -horizontal.absolute as f32;
                    let dy = -vertical.absolute as f32;
                    target.pending_events.push(Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: Vec2::new(dx, dy),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Unknown,
                        modifiers: target.modifiers,
                    });
                }
            }
        }
    }
}

fn translate_mouse_button(button: u32) -> Option<PointerButton> {
    match button {
        0x110 => Some(PointerButton::Primary),
        0x111 => Some(PointerButton::Secondary),
        0x112 => Some(PointerButton::Middle),
        _ => None,
    }
}

fn translate_keysym(sym: Keysym) -> Option<Key> {
    rocketde_ui_common::keysym::translate_keysym_raw(sym.raw())
}

delegate_registry!(WaylandApp);

impl ProvidesRegistryState for WaylandApp {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

impl BackgroundEffectHandler for WaylandApp {
    fn background_effect_state(&mut self) -> &mut BackgroundEffectState {
        &mut self.background_effect_state
    }

    fn update_capabilities(&mut self) {}
}

impl ShmHandler for WaylandApp {
    fn shm_state(&mut self) -> &mut Shm {
        self.capture_mgr
            .shm
            .as_mut()
            .expect("wl_shm não inicializado")
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for WaylandApp {
    fn event(
        state: &mut Self,
        _proxy: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            let id = toplevel.id();
            state.capture_mgr.toplevels.insert(
                id,
                TrackedToplevel {
                    handle: toplevel,
                    app_id: String::new(),
                    title: String::new(),
                    identifier: String::new(),
                    done: false,
                },
            );
        }
    }

    wayland_client::event_created_child!(WaylandApp, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for WaylandApp {
    fn event(
        state: &mut Self,
        proxy: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let id = proxy.id();
        match event {
            ext_foreign_toplevel_handle_v1::Event::Title { title } => {
                if let Some(tl) = state.capture_mgr.toplevels.get_mut(&id) {
                    tl.title = title;
                }
            }
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                if let Some(tl) = state.capture_mgr.toplevels.get_mut(&id) {
                    tl.app_id = app_id;
                }
            }
            ext_foreign_toplevel_handle_v1::Event::Identifier { identifier } => {
                if let Some(tl) = state.capture_mgr.toplevels.get_mut(&id) {
                    tl.identifier = identifier;
                }
            }
            ext_foreign_toplevel_handle_v1::Event::Done => {
                if let Some(tl) = state.capture_mgr.toplevels.get_mut(&id) {
                    tl.done = true;
                }
                state.capture_mgr.ensure_sessions(qh);
            }
            ext_foreign_toplevel_handle_v1::Event::Closed => {
                state.capture_mgr.remove_toplevel(&id);
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()> for WaylandApp {
    fn event(
        _state: &mut Self,
        _proxy: &ExtForeignToplevelImageCaptureSourceManagerV1,
        _event: <ExtForeignToplevelImageCaptureSourceManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtImageCaptureSourceV1, ()> for WaylandApp {
    fn event(
        _state: &mut Self,
        _proxy: &ExtImageCaptureSourceV1,
        _event: <ExtImageCaptureSourceV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtImageCopyCaptureManagerV1, ()> for WaylandApp {
    fn event(
        _state: &mut Self,
        _proxy: &ExtImageCopyCaptureManagerV1,
        _event: <ExtImageCopyCaptureManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ObjectId> for WaylandApp {
    fn event(
        state: &mut Self,
        _proxy: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        id: &ObjectId,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                if let Some(sess) = state.capture_mgr.sessions.get_mut(id) {
                    sess.width = width;
                    sess.height = height;
                }
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat { format } => {
                if let Some(sess) = state.capture_mgr.sessions.get_mut(id) {
                    sess.format = pick_preferred_shm_format(sess.format, format);
                }
            }
            ext_image_copy_capture_session_v1::Event::Done => {
                state.capture_mgr.handle_session_done(id);
                if state.ui_state.config.overview.preview_mode != OverviewPreviewMode::Icons {
                    let paused = state.overview_capture_paused();
                    let live_mode =
                        state.ui_state.config.overview.preview_mode == OverviewPreviewMode::Live;
                    state.capture_mgr.pump_captures(qh, paused, live_mode);
                }
            }
            ext_image_copy_capture_session_v1::Event::Stopped => {
                state.capture_mgr.handle_session_stopped(id);
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ObjectId> for WaylandApp {
    fn event(
        state: &mut Self,
        _proxy: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        id: &ObjectId,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => {
                state.capture_mgr.handle_frame_ready(id);
            }
            ext_image_copy_capture_frame_v1::Event::Failed { reason } => {
                let stopped = matches!(
                    reason,
                    WEnum::Value(ext_image_copy_capture_frame_v1::FailureReason::Stopped)
                );
                state.capture_mgr.handle_frame_failed(id, stopped);
            }
            _ => {}
        }
    }
}

smithay_client_toolkit::delegate_dispatch2!(WaylandApp);

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for WaylandApp {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpVirtualKeyboardManagerV1,
        _event: <ZwpVirtualKeyboardManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for WaylandApp {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpVirtualKeyboardV1,
        _event: <ZwpVirtualKeyboardV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtOutputImageCaptureSourceManagerV1, ()> for WaylandApp {
    fn event(
        _state: &mut Self,
        _proxy: &ExtOutputImageCaptureSourceManagerV1,
        _event: <ExtOutputImageCaptureSourceManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, CaptureStreamTag> for WaylandApp {
    fn event(
        state: &mut Self,
        _proxy: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        tag: &CaptureStreamTag,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                state.screen_capture.handle_ext_buffer_size(*tag, width, height);
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat { format } => {
                state.screen_capture.handle_ext_shm_format(*tag, format);
            }
            ext_image_copy_capture_session_v1::Event::Done => {
                state.screen_capture.handle_ext_session_done(*tag, qh);
            }
            ext_image_copy_capture_session_v1::Event::Stopped => {
                state.screen_capture.handle_ext_session_stopped(*tag);
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, CaptureStreamTag> for WaylandApp {
    fn event(
        state: &mut Self,
        _proxy: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        tag: &CaptureStreamTag,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => {
                state.screen_capture.handle_ext_frame_ready(*tag);
                if *tag == CaptureStreamTag::OneShot {
                    state.process_capture_events();
                }
            }
            ext_image_copy_capture_frame_v1::Event::Failed { reason } => {
                let stopped = matches!(
                    reason,
                    WEnum::Value(ext_image_copy_capture_frame_v1::FailureReason::Stopped)
                );
                state.screen_capture.handle_ext_frame_failed(*tag, stopped);
                if *tag == CaptureStreamTag::OneShot {
                    state.process_capture_events();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for WaylandApp {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrScreencopyManagerV1,
        _event: <ZwlrScreencopyManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, CaptureStreamTag> for WaylandApp {
    fn event(
        state: &mut Self,
        _proxy: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        tag: &CaptureStreamTag,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                state
                    .screen_capture
                    .handle_wlr_buffer(format, width, height, stride);
            }
            zwlr_screencopy_frame_v1::Event::Flags { flags } => {
                let y_invert = matches!(
                    flags,
                    WEnum::Value(f) if f.contains(zwlr_screencopy_frame_v1::Flags::YInvert)
                );
                state.screen_capture.handle_wlr_flags(y_invert);
            }
            zwlr_screencopy_frame_v1::Event::BufferDone => {
                state.screen_capture.handle_wlr_buffer_done();
            }
            zwlr_screencopy_frame_v1::Event::Ready { .. } => {
                state.screen_capture.handle_wlr_ready();
                if *tag == CaptureStreamTag::OneShot {
                    state.process_capture_events();
                }
            }
            zwlr_screencopy_frame_v1::Event::Failed => {
                state.screen_capture.handle_wlr_failed();
                if *tag == CaptureStreamTag::OneShot {
                    state.process_capture_events();
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrDataControlManagerV1, ()> for WaylandApp {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrDataControlManagerV1,
        _event: <ZwlrDataControlManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrDataControlDeviceV1, ()> for WaylandApp {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrDataControlDeviceV1,
        event: zwlr_data_control_device_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        dispatch_data_control_device(event);
    }
}

impl Dispatch<ZwlrDataControlOfferV1, ()> for WaylandApp {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrDataControlOfferV1,
        event: <ZwlrDataControlOfferV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        dispatch_data_control_offer(event);
    }
}

impl Dispatch<ZwlrDataControlSourceV1, ()> for WaylandApp {
    fn event(
        state: &mut Self,
        proxy: &ZwlrDataControlSourceV1,
        event: zwlr_data_control_source_v1::Event,
        _data: &(),
        conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        dispatch_data_control_source(&mut state.clipboard, proxy, event, conn);
    }
}

fn logical_to_physical_crop(
    logical_rect: Rect,
    screen_size: Vec2,
    phys_w: u32,
    phys_h: u32,
) -> [u32; 4] {
    let sx = (phys_w as f32) / screen_size.x.max(1.0);
    let sy = (phys_h as f32) / screen_size.y.max(1.0);
    let x = (logical_rect.min.x.max(0.0) * sx).round() as u32;
    let y = (logical_rect.min.y.max(0.0) * sy).round() as u32;
    let w = (logical_rect.width().max(1.0) * sx).round() as u32;
    let h = (logical_rect.height().max(1.0) * sy).round() as u32;
    let x = x.min(phys_w.saturating_sub(1));
    let y = y.min(phys_h.saturating_sub(1));
    let w = w.min(phys_w - x).max(1);
    let h = h.min(phys_h - y).max(1);
    [x, y, w, h]
}

