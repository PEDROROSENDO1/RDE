use egui::ColorImage;
use smithay_client_toolkit::shm::{
    Shm, ShmHandler,
    slot::{Buffer, SlotPool},
};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use wayland_client::{
    QueueHandle, WEnum,
    backend::ObjectId,
    globals::GlobalList,
    protocol::wl_shm,
};
use wayland_protocols::ext::{
    foreign_toplevel_list::v1::client::{
        ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
    },
    image_capture_source::v1::client::{
        ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
        ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    },
    image_copy_capture::v1::client::{
        ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1,
        ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
        ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1,
    },
};

const MAX_THUMB_W: u32 = 320;
const MAX_THUMB_H: u32 = 200;
const MIN_CAPTURE_INTERVAL: Duration = Duration::from_millis(1200);
const MAX_IN_FLIGHT_CAPTURES: usize = 1;

pub struct TrackedToplevel {
    pub handle: ExtForeignToplevelHandleV1,
    pub app_id: String,
    pub title: String,
    pub identifier: String,
    pub done: bool,
}

pub struct CaptureSessionState {
    pub source: ExtImageCaptureSourceV1,
    pub session: ExtImageCopyCaptureSessionV1,
    pub width: u32,
    pub height: u32,
    pub format: Option<wl_shm::Format>,
    pub pool: Option<SlotPool>,
    pub buffer: Option<Buffer>,
    pub buf_size: (u32, u32),
    pub pending_frame: Option<ExtImageCopyCaptureFrameV1>,
    pub capturing: bool,
    pub ready_for_capture: bool,
    pub captured_once: bool,
    pub stopped: bool,
    pub last_capture_at: Instant,
}

pub struct CapturedThumbnail {
    pub identifier: String,
    pub app_id: String,
    pub title: String,
    pub image: ColorImage,
}

pub struct WindowCaptureManager {
    pub shm: Option<Shm>,
    pub toplevel_list: Option<ExtForeignToplevelListV1>,
    pub source_manager: Option<ExtForeignToplevelImageCaptureSourceManagerV1>,
    pub copy_manager: Option<ExtImageCopyCaptureManagerV1>,
    pub toplevels: HashMap<ObjectId, TrackedToplevel>,
    pub sessions: HashMap<ObjectId, CaptureSessionState>,
    pub active: bool,
    pub ready_updates: VecDeque<(ObjectId, CapturedThumbnail)>,
    pub removed_ids: Vec<ObjectId>,
    scratch_rgba: Vec<u8>,
    scratch_lut: Vec<usize>,
}

impl WindowCaptureManager {
    pub fn bind<D>(globals: &GlobalList, qh: &QueueHandle<D>) -> Self
    where
        D: ShmHandler
            + wayland_client::Dispatch<wl_shm::WlShm, smithay_client_toolkit::globals::GlobalData>
            + wayland_client::Dispatch<ExtForeignToplevelListV1, ()>
            + wayland_client::Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()>
            + wayland_client::Dispatch<ExtImageCopyCaptureManagerV1, ()>
            + 'static,
    {
        let shm = Shm::bind(globals, qh).ok();
        let toplevel_list = globals.bind::<ExtForeignToplevelListV1, _, _>(qh, 1..=1, ()).ok();
        let source_manager = globals
            .bind::<ExtForeignToplevelImageCaptureSourceManagerV1, _, _>(qh, 1..=1, ())
            .ok();
        let copy_manager = globals
            .bind::<ExtImageCopyCaptureManagerV1, _, _>(qh, 1..=1, ())
            .ok();

        Self {
            shm,
            toplevel_list,
            source_manager,
            copy_manager,
            toplevels: HashMap::new(),
            sessions: HashMap::new(),
            active: false,
            ready_updates: VecDeque::new(),
            removed_ids: Vec::new(),
            scratch_rgba: Vec::new(),
            scratch_lut: Vec::new(),
        }
    }

    pub fn is_supported(&self) -> bool {
        self.shm.is_some()
            && self.toplevel_list.is_some()
            && self.source_manager.is_some()
            && self.copy_manager.is_some()
    }

    pub fn set_active<D>(&mut self, active: bool, paused: bool, live_mode: bool, qh: &QueueHandle<D>)
    where
        D: wayland_client::Dispatch<ExtImageCaptureSourceV1, ()>
            + wayland_client::Dispatch<ExtImageCopyCaptureSessionV1, ObjectId>
            + wayland_client::Dispatch<ExtImageCopyCaptureFrameV1, ObjectId>
            + 'static,
    {
        if self.active == active {
            if active {
                self.ensure_sessions(qh);
                self.pump_captures(qh, paused, live_mode);
            }
            return;
        }

        self.active = active;
        if !active {
            for (_, mut sess) in self.sessions.drain() {
                if let Some(frame) = sess.pending_frame.take() {
                    frame.destroy();
                }
                sess.session.destroy();
                sess.source.destroy();
            }
            self.ready_updates.clear();
            self.removed_ids.clear();
        } else {
            self.ensure_sessions(qh);
            self.pump_captures(qh, paused, live_mode);
        }
    }

    pub fn has_pending_work(&self, live_mode: bool) -> bool {
        if !self.active {
            return false;
        }
        if !self.ready_updates.is_empty() || !self.removed_ids.is_empty() {
            return true;
        }
        self.sessions.values().any(|sess| {
            !sess.stopped
                && (sess.capturing
                    || (sess.ready_for_capture
                        && sess.width > 0
                        && sess.height > 0
                        && sess.format.is_some()
                        && (live_mode || !sess.captured_once)))
        })
    }

    pub fn ensure_sessions<D>(&mut self, qh: &QueueHandle<D>)
    where
        D: wayland_client::Dispatch<ExtImageCaptureSourceV1, ()>
            + wayland_client::Dispatch<ExtImageCopyCaptureSessionV1, ObjectId>
            + 'static,
    {
        if !self.active || !self.is_supported() {
            return;
        }
        let (Some(src_mgr), Some(copy_mgr)) = (&self.source_manager, &self.copy_manager) else {
            return;
        };

        for (id, tl) in &self.toplevels {
            if !tl.done || self.sessions.contains_key(id) {
                continue;
            }
            if tl.app_id.to_lowercase().contains("rocketde") {
                continue;
            }
            let source = src_mgr.create_source(&tl.handle, qh, ());
            let session = copy_mgr.create_session(&source, Options::empty(), qh, id.clone());
            self.sessions.insert(
                id.clone(),
                CaptureSessionState {
                    source,
                    session,
                    width: 0,
                    height: 0,
                    format: None,
                    pool: None,
                    buffer: None,
                    buf_size: (0, 0),
                    pending_frame: None,
                    capturing: false,
                    ready_for_capture: false,
                    captured_once: false,
                    stopped: false,
                    last_capture_at: Instant::now() - MIN_CAPTURE_INTERVAL,
                },
            );
        }
    }

    pub fn pump_captures<D>(&mut self, qh: &QueueHandle<D>, paused: bool, live_mode: bool)
    where
        D: wayland_client::Dispatch<ExtImageCopyCaptureFrameV1, ObjectId> + 'static,
    {
        if !self.active || paused {
            return;
        }
        let Some(shm) = &self.shm else {
            return;
        };

        let in_flight = self.sessions.values().filter(|s| s.capturing).count();
        if in_flight >= MAX_IN_FLIGHT_CAPTURES {
            return;
        }

        let now = Instant::now();
        // Pick the stalest ready session (round-robin) so we never stall the compositor with a burst
        let best_id = self
            .sessions
            .iter()
            .filter(|(_, sess)| {
                !sess.stopped
                    && sess.ready_for_capture
                    && !sess.capturing
                    && (live_mode || !sess.captured_once)
                    && (!sess.captured_once
                        || now.duration_since(sess.last_capture_at) >= MIN_CAPTURE_INTERVAL)
                    && sess.width > 0
                    && sess.height > 0
                    && sess.format.is_some()
            })
            .min_by_key(|(_, sess)| (sess.captured_once, sess.last_capture_at))
            .map(|(id, _)| id.clone());

        let Some(id) = best_id else {
            return;
        };
        let Some(sess) = self.sessions.get_mut(&id) else {
            return;
        };
        let Some(fmt) = sess.format else {
            return;
        };

        let stride = sess.width * 4;
        let byte_len = (stride * sess.height) as usize;
        let need_realloc = sess.buffer.is_none() || sess.buf_size != (sess.width, sess.height);

        if need_realloc
            && let Ok(mut pool) = SlotPool::new(byte_len.max(4096), shm)
            && let Ok((buf, _)) = pool.create_buffer(
                sess.width as i32,
                sess.height as i32,
                stride as i32,
                fmt,
            )
        {
            sess.pool = Some(pool);
            sess.buffer = Some(buf);
            sess.buf_size = (sess.width, sess.height);
        }

        let Some(ref buf) = sess.buffer else {
            return;
        };

        let frame = sess.session.create_frame(qh, id);
        frame.attach_buffer(buf.wl_buffer());
        frame.damage_buffer(0, 0, sess.width as i32, sess.height as i32);
        frame.capture();
        sess.pending_frame = Some(frame);
        sess.capturing = true;
        sess.last_capture_at = now;
    }

    pub fn handle_session_done(&mut self, id: &ObjectId) {
        if let Some(sess) = self.sessions.get_mut(id) {
            sess.ready_for_capture = true;
            // Force buffer recreation if constraints changed
            if sess.buf_size != (sess.width, sess.height) {
                sess.buffer = None;
                sess.pool = None;
                sess.buf_size = (0, 0);
                sess.captured_once = false;
            }
        }
    }

    pub fn handle_session_stopped(&mut self, id: &ObjectId) {
        if let Some(sess) = self.sessions.get_mut(id) {
            sess.stopped = true;
            sess.capturing = false;
            if let Some(frame) = sess.pending_frame.take() {
                frame.destroy();
            }
        }
    }

    pub fn handle_frame_ready(&mut self, id: &ObjectId) {
        let Some(sess) = self.sessions.get_mut(id) else {
            return;
        };
        if let Some(frame) = sess.pending_frame.take() {
            frame.destroy();
        }
        sess.capturing = false;
        sess.captured_once = true;

        let (Some(pool), Some(buf), Some(fmt)) = (&mut sess.pool, &sess.buffer, sess.format) else {
            return;
        };
        let src_w = sess.width;
        let src_h = sess.height;
        if src_w == 0 || src_h == 0 {
            return;
        }
        let Some(raw) = buf.canvas(pool) else {
            return;
        };

        let img = downscale_shm_to_color_image(
            raw,
            src_w,
            src_h,
            fmt,
            &mut self.scratch_rgba,
            &mut self.scratch_lut,
        );
        if let Some(tl) = self.toplevels.get(id) {
            self.ready_updates.push_back((
                id.clone(),
                CapturedThumbnail {
                    identifier: tl.identifier.clone(),
                    app_id: tl.app_id.clone(),
                    title: tl.title.clone(),
                    image: img,
                },
            ));
        }
    }

    pub fn handle_frame_failed(&mut self, id: &ObjectId, stopped: bool) {
        if let Some(sess) = self.sessions.get_mut(id) {
            if let Some(frame) = sess.pending_frame.take() {
                frame.destroy();
            }
            sess.capturing = false;
            if stopped {
                sess.stopped = true;
            }
        }
    }

    pub fn remove_toplevel(&mut self, id: &ObjectId) {
        if let Some(tl) = self.toplevels.remove(id) {
            tl.handle.destroy();
        }
        if let Some(mut sess) = self.sessions.remove(id) {
            if let Some(frame) = sess.pending_frame.take() {
                frame.destroy();
            }
            sess.session.destroy();
            sess.source.destroy();
        }
        self.removed_ids.push(id.clone());
    }
}

pub fn pick_preferred_shm_format(cur: Option<wl_shm::Format>, new_fmt: WEnum<wl_shm::Format>) -> Option<wl_shm::Format> {
    let WEnum::Value(f) = new_fmt else {
        return cur;
    };
    match f {
        wl_shm::Format::Argb8888 => Some(f),
        wl_shm::Format::Xrgb8888 => match cur {
            Some(wl_shm::Format::Argb8888) => cur,
            _ => Some(f),
        },
        wl_shm::Format::Abgr8888 | wl_shm::Format::Xbgr8888 => {
            if cur.is_none() {
                Some(f)
            } else {
                cur
            }
        }
        _ => cur,
    }
}

fn downscale_shm_to_color_image(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    fmt: wl_shm::Format,
    scratch_rgba: &mut Vec<u8>,
    scratch_lut: &mut Vec<usize>,
) -> ColorImage {
    let scale = (MAX_THUMB_W as f32 / src_w as f32)
        .min(MAX_THUMB_H as f32 / src_h as f32)
        .min(1.0);
    let dst_w = ((src_w as f32 * scale).round() as usize).max(1);
    let dst_h = ((src_h as f32 * scale).round() as usize).max(1);
    let src_w_usize = src_w as usize;
    let src_h_usize = src_h as usize;
    let row_bytes = src_w_usize * 4;

    let needed_bytes = dst_w * dst_h * 4;
    scratch_rgba.resize(needed_bytes, 255u8);
    scratch_lut.clear();
    scratch_lut.extend((0..dst_w).map(|dx| ((dx * src_w_usize) / dst_w) * 4));

    let swap_rb = matches!(fmt, wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888);
    let opaque = matches!(fmt, wl_shm::Format::Xrgb8888 | wl_shm::Format::Xbgr8888);

    for dy in 0..dst_h {
        let sy = (dy * src_h_usize) / dst_h;
        let row_start = sy * row_bytes;
        let Some(src_row) = src.get(row_start..row_start + row_bytes) else {
            continue;
        };
        let dst_row = &mut scratch_rgba[dy * dst_w * 4..(dy + 1) * dst_w * 4];
        for (dx, &sx_off) in scratch_lut.iter().enumerate() {
            let d = dx * 4;
            let b0 = src_row[sx_off];
            let b1 = src_row[sx_off + 1];
            let b2 = src_row[sx_off + 2];
            let b3 = src_row[sx_off + 3];
            if swap_rb {
                dst_row[d] = b2;
                dst_row[d + 1] = b1;
                dst_row[d + 2] = b0;
                dst_row[d + 3] = if opaque { 255 } else { b3 };
            } else {
                dst_row[d] = b0;
                dst_row[d + 1] = b1;
                dst_row[d + 2] = b2;
                dst_row[d + 3] = if opaque { 255 } else { b3 };
            }
        }
    }

    ColorImage::from_rgba_unmultiplied([dst_w, dst_h], &scratch_rgba[..needed_bytes])
}
