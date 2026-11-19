use crate::wayland::capture::pick_preferred_shm_format;
use crossbeam_channel::Sender;
use egui::ColorImage;
use smithay_client_toolkit::shm::{
    Shm, ShmHandler,
    slot::{Buffer, SlotPool},
};
use std::time::{Duration, Instant};
use wayland_client::{
    QueueHandle, WEnum,
    globals::GlobalList,
    protocol::{wl_output::WlOutput, wl_shm},
};
use wayland_protocols::ext::{
    foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    image_capture_source::v1::client::{
        ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
        ext_image_capture_source_v1::ExtImageCaptureSourceV1,
        ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
    },
    image_copy_capture::v1::client::{
        ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1,
        ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
        ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1,
    },
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1,
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureStreamTag {
    OneShot,
    RecordStream,
}

pub struct CapturedOutputFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl CapturedOutputFrame {
    pub fn to_color_image(&self) -> ColorImage {
        ColorImage::from_rgba_unmultiplied(
            [self.width as usize, self.height as usize],
            &self.rgba,
        )
    }
}

pub struct RawVideoFrame {
    pub width: u32,
    pub height: u32,
    pub format: wl_shm::Format,
    pub data: Vec<u8>,
}

struct ExtSession {
    source: ExtImageCaptureSourceV1,
    session: ExtImageCopyCaptureSessionV1,
    width: u32,
    height: u32,
    format: Option<wl_shm::Format>,
    pool: Option<SlotPool>,
    buffer: Option<Buffer>,
    buf_size: (u32, u32),
    pending_frame: Option<ExtImageCopyCaptureFrameV1>,
    ready: bool,
    capturing: bool,
    stopped: bool,
}

struct WlrPendingFrame {
    frame: ZwlrScreencopyFrameV1,
    tag: CaptureStreamTag,
    width: u32,
    height: u32,
    stride: u32,
    format: Option<wl_shm::Format>,
    y_invert: bool,
    pool: Option<SlotPool>,
    buffer: Option<Buffer>,
}

pub struct ScreenCaptureManager {
    pub shm: Option<Shm>,
    pub output_source_mgr: Option<ExtOutputImageCaptureSourceManagerV1>,
    pub toplevel_source_mgr: Option<ExtForeignToplevelImageCaptureSourceManagerV1>,
    pub copy_mgr: Option<ExtImageCopyCaptureManagerV1>,
    pub wlr_screencopy: Option<ZwlrScreencopyManagerV1>,

    oneshot_ext: Option<ExtSession>,
    oneshot_result: Option<CapturedOutputFrame>,
    oneshot_failed: bool,

    stream_ext: Option<ExtSession>,
    stream_tx: Option<Sender<RawVideoFrame>>,
    stream_interval: Duration,
    stream_last_frame: Instant,
    stream_output: Option<WlOutput>,
    stream_with_cursor: bool,

    wlr_pending: Option<WlrPendingFrame>,
    wlr_stream_pool: Option<SlotPool>,
    wlr_stream_buffer: Option<Buffer>,
    wlr_stream_dims: (u32, u32, u32),
}

impl ScreenCaptureManager {
    pub fn bind<D>(globals: &GlobalList, qh: &QueueHandle<D>) -> Self
    where
        D: ShmHandler
            + wayland_client::Dispatch<wl_shm::WlShm, smithay_client_toolkit::globals::GlobalData>
            + wayland_client::Dispatch<ExtOutputImageCaptureSourceManagerV1, ()>
            + wayland_client::Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()>
            + wayland_client::Dispatch<ExtImageCopyCaptureManagerV1, ()>
            + wayland_client::Dispatch<ZwlrScreencopyManagerV1, ()>
            + 'static,
    {
        Self {
            shm: Shm::bind(globals, qh).ok(),
            output_source_mgr: globals
                .bind::<ExtOutputImageCaptureSourceManagerV1, _, _>(qh, 1..=1, ())
                .ok(),
            toplevel_source_mgr: globals
                .bind::<ExtForeignToplevelImageCaptureSourceManagerV1, _, _>(qh, 1..=1, ())
                .ok(),
            copy_mgr: globals
                .bind::<ExtImageCopyCaptureManagerV1, _, _>(qh, 1..=1, ())
                .ok(),
            wlr_screencopy: globals
                .bind::<ZwlrScreencopyManagerV1, _, _>(qh, 1..=3, ())
                .ok(),
            oneshot_ext: None,
            oneshot_result: None,
            oneshot_failed: false,
            stream_ext: None,
            stream_tx: None,
            stream_interval: Duration::from_millis(33),
            stream_last_frame: Instant::now(),
            stream_output: None,
            stream_with_cursor: true,
            wlr_pending: None,
            wlr_stream_pool: None,
            wlr_stream_buffer: None,
            wlr_stream_dims: (0, 0, 0),
        }
    }

    pub fn is_supported(&self) -> bool {
        self.shm.is_some()
            && ((self.output_source_mgr.is_some() && self.copy_mgr.is_some())
                || self.wlr_screencopy.is_some())
    }

    pub fn take_oneshot_result(&mut self) -> Option<CapturedOutputFrame> {
        self.oneshot_result.take()
    }

    pub fn take_oneshot_failed(&mut self) -> bool {
        let f = self.oneshot_failed;
        self.oneshot_failed = false;
        f
    }

    pub fn is_oneshot_pending(&self) -> bool {
        self.oneshot_ext.is_some()
            || self
                .wlr_pending
                .as_ref()
                .is_some_and(|w| w.tag == CaptureStreamTag::OneShot)
    }

    pub fn request_output_snapshot<D>(
        &mut self,
        output: &WlOutput,
        with_cursor: bool,
        qh: &QueueHandle<D>,
    ) -> bool
    where
        D: wayland_client::Dispatch<ExtImageCaptureSourceV1, ()>
            + wayland_client::Dispatch<ExtImageCopyCaptureSessionV1, CaptureStreamTag>
            + wayland_client::Dispatch<ZwlrScreencopyFrameV1, CaptureStreamTag>
            + 'static,
    {
        self.cancel_oneshot();
        self.oneshot_result = None;
        self.oneshot_failed = false;

        if let (Some(src_mgr), Some(copy_mgr)) = (&self.output_source_mgr, &self.copy_mgr) {
            let source = src_mgr.create_source(output, qh, ());
            let opts = if with_cursor {
                Options::PaintCursors
            } else {
                Options::empty()
            };
            let session = copy_mgr.create_session(&source, opts, qh, CaptureStreamTag::OneShot);
            self.oneshot_ext = Some(ExtSession {
                source,
                session,
                width: 0,
                height: 0,
                format: None,
                pool: None,
                buffer: None,
                buf_size: (0, 0),
                pending_frame: None,
                ready: false,
                capturing: false,
                stopped: false,
            });
            return true;
        }

        if let Some(ref wlr) = self.wlr_screencopy {
            let frame = wlr.capture_output(
                if with_cursor { 1 } else { 0 },
                output,
                qh,
                CaptureStreamTag::OneShot,
            );
            self.wlr_pending = Some(WlrPendingFrame {
                frame,
                tag: CaptureStreamTag::OneShot,
                width: 0,
                height: 0,
                stride: 0,
                format: None,
                y_invert: false,
                pool: None,
                buffer: None,
            });
            return true;
        }

        self.oneshot_failed = true;
        false
    }

    pub fn request_toplevel_snapshot<D>(
        &mut self,
        handle: &ExtForeignToplevelHandleV1,
        with_cursor: bool,
        qh: &QueueHandle<D>,
    ) -> bool
    where
        D: wayland_client::Dispatch<ExtImageCaptureSourceV1, ()>
            + wayland_client::Dispatch<ExtImageCopyCaptureSessionV1, CaptureStreamTag>
            + 'static,
    {
        self.cancel_oneshot();
        self.oneshot_result = None;
        self.oneshot_failed = false;

        let (Some(src_mgr), Some(copy_mgr)) = (&self.toplevel_source_mgr, &self.copy_mgr) else {
            self.oneshot_failed = true;
            return false;
        };

        let source = src_mgr.create_source(handle, qh, ());
        let opts = if with_cursor {
            Options::PaintCursors
        } else {
            Options::empty()
        };
        let session = copy_mgr.create_session(&source, opts, qh, CaptureStreamTag::OneShot);
        self.oneshot_ext = Some(ExtSession {
            source,
            session,
            width: 0,
            height: 0,
            format: None,
            pool: None,
            buffer: None,
            buf_size: (0, 0),
            pending_frame: None,
            ready: false,
            capturing: false,
            stopped: false,
        });
        true
    }

    pub fn cancel_oneshot(&mut self) {
        if let Some(mut sess) = self.oneshot_ext.take() {
            if let Some(frame) = sess.pending_frame.take() {
                frame.destroy();
            }
            sess.session.destroy();
            sess.source.destroy();
        }
        if self
            .wlr_pending
            .as_ref()
            .is_some_and(|w| w.tag == CaptureStreamTag::OneShot)
        {
            if let Some(w) = self.wlr_pending.take() {
                w.frame.destroy();
            }
        }
    }

    pub fn start_stream<D>(
        &mut self,
        output: WlOutput,
        with_cursor: bool,
        fps: u32,
        tx: Sender<RawVideoFrame>,
        qh: &QueueHandle<D>,
    ) -> bool
    where
        D: wayland_client::Dispatch<ExtImageCaptureSourceV1, ()>
            + wayland_client::Dispatch<ExtImageCopyCaptureSessionV1, CaptureStreamTag>
            + wayland_client::Dispatch<ZwlrScreencopyFrameV1, CaptureStreamTag>
            + 'static,
    {
        self.stop_stream();
        self.stream_tx = Some(tx);
        self.stream_interval = Duration::from_secs_f64(1.0 / (fps.max(1) as f64));
        self.stream_last_frame = Instant::now() - self.stream_interval;
        self.stream_with_cursor = with_cursor;
        self.stream_output = Some(output.clone());

        if let (Some(src_mgr), Some(copy_mgr)) = (&self.output_source_mgr, &self.copy_mgr) {
            let source = src_mgr.create_source(&output, qh, ());
            let opts = if with_cursor {
                Options::PaintCursors
            } else {
                Options::empty()
            };
            let session =
                copy_mgr.create_session(&source, opts, qh, CaptureStreamTag::RecordStream);
            self.stream_ext = Some(ExtSession {
                source,
                session,
                width: 0,
                height: 0,
                format: None,
                pool: None,
                buffer: None,
                buf_size: (0, 0),
                pending_frame: None,
                ready: false,
                capturing: false,
                stopped: false,
            });
            return true;
        }

        if self.wlr_screencopy.is_some() {
            return true;
        }

        self.stop_stream();
        false
    }

    pub fn stop_stream(&mut self) {
        if let Some(mut sess) = self.stream_ext.take() {
            if let Some(frame) = sess.pending_frame.take() {
                frame.destroy();
            }
            sess.session.destroy();
            sess.source.destroy();
        }
        if self
            .wlr_pending
            .as_ref()
            .is_some_and(|w| w.tag == CaptureStreamTag::RecordStream)
        {
            if let Some(w) = self.wlr_pending.take() {
                w.frame.destroy();
            }
        }
        self.stream_tx = None;
        self.stream_output = None;
        self.wlr_stream_buffer = None;
        self.wlr_stream_pool = None;
        self.wlr_stream_dims = (0, 0, 0);
    }

    pub fn pump_stream<D>(&mut self, qh: &QueueHandle<D>)
    where
        D: wayland_client::Dispatch<ExtImageCopyCaptureFrameV1, CaptureStreamTag>
            + wayland_client::Dispatch<ZwlrScreencopyFrameV1, CaptureStreamTag>
            + 'static,
    {
        if self.stream_tx.is_none() {
            return;
        }
        let now = Instant::now();
        if now.duration_since(self.stream_last_frame) < self.stream_interval {
            return;
        }

        if let Some(ref mut sess) = self.stream_ext {
            if let Some(ref shm) = self.shm {
                if Self::trigger_ext_capture(sess, shm, CaptureStreamTag::RecordStream, qh) {
                    self.stream_last_frame = now;
                }
            }
            return;
        }

        if self.wlr_pending.is_none() {
            if let (Some(wlr), Some(out)) = (&self.wlr_screencopy, &self.stream_output) {
                let frame = wlr.capture_output(
                    if self.stream_with_cursor { 1 } else { 0 },
                    out,
                    qh,
                    CaptureStreamTag::RecordStream,
                );
                self.wlr_pending = Some(WlrPendingFrame {
                    frame,
                    tag: CaptureStreamTag::RecordStream,
                    width: 0,
                    height: 0,
                    stride: 0,
                    format: None,
                    y_invert: false,
                    pool: None,
                    buffer: None,
                });
                self.stream_last_frame = now;
            }
        }
    }

    fn trigger_ext_capture<D>(
        sess: &mut ExtSession,
        shm: &Shm,
        tag: CaptureStreamTag,
        qh: &QueueHandle<D>,
    ) -> bool
    where
        D: wayland_client::Dispatch<ExtImageCopyCaptureFrameV1, CaptureStreamTag> + 'static,
    {
        if sess.stopped || !sess.ready || sess.capturing || sess.width == 0 || sess.height == 0 {
            return false;
        }
        let Some(fmt) = sess.format else {
            return false;
        };

        let stride = sess.width * 4;
        let byte_len = (stride * sess.height) as usize;
        let need_realloc = sess.buffer.is_none() || sess.buf_size != (sess.width, sess.height);

        if need_realloc {
            if let Ok(mut pool) = SlotPool::new(byte_len.max(4096), shm) {
                if let Ok((buf, _)) = pool.create_buffer(
                    sess.width as i32,
                    sess.height as i32,
                    stride as i32,
                    fmt,
                ) {
                    sess.pool = Some(pool);
                    sess.buffer = Some(buf);
                    sess.buf_size = (sess.width, sess.height);
                }
            }
        }

        let Some(ref buf) = sess.buffer else {
            return false;
        };

        let frame = sess.session.create_frame(qh, tag);
        frame.attach_buffer(buf.wl_buffer());
        frame.damage_buffer(0, 0, sess.width as i32, sess.height as i32);
        frame.capture();
        sess.pending_frame = Some(frame);
        sess.capturing = true;
        true
    }

    // --- ExtImageCopyCaptureSessionV1 callbacks ---

    pub fn handle_ext_buffer_size(&mut self, tag: CaptureStreamTag, width: u32, height: u32) {
        let sess = match tag {
            CaptureStreamTag::OneShot => self.oneshot_ext.as_mut(),
            CaptureStreamTag::RecordStream => self.stream_ext.as_mut(),
        };
        if let Some(s) = sess {
            s.width = width;
            s.height = height;
        }
    }

    pub fn handle_ext_shm_format(&mut self, tag: CaptureStreamTag, fmt: WEnum<wl_shm::Format>) {
        let sess = match tag {
            CaptureStreamTag::OneShot => self.oneshot_ext.as_mut(),
            CaptureStreamTag::RecordStream => self.stream_ext.as_mut(),
        };
        if let Some(s) = sess {
            s.format = pick_preferred_shm_format(s.format, fmt);
        }
    }

    pub fn handle_ext_session_done<D>(&mut self, tag: CaptureStreamTag, qh: &QueueHandle<D>)
    where
        D: wayland_client::Dispatch<ExtImageCopyCaptureFrameV1, CaptureStreamTag> + 'static,
    {
        let shm = self.shm.as_ref();
        let sess = match tag {
            CaptureStreamTag::OneShot => self.oneshot_ext.as_mut(),
            CaptureStreamTag::RecordStream => self.stream_ext.as_mut(),
        };
        if let (Some(s), Some(shm)) = (sess, shm) {
            s.ready = true;
            if s.buf_size != (s.width, s.height) {
                s.buffer = None;
                s.pool = None;
                s.buf_size = (0, 0);
            }
            if tag == CaptureStreamTag::OneShot && !s.capturing {
                Self::trigger_ext_capture(s, shm, tag, qh);
            }
        }
    }

    pub fn handle_ext_session_stopped(&mut self, tag: CaptureStreamTag) {
        match tag {
            CaptureStreamTag::OneShot => {
                self.cancel_oneshot();
                self.oneshot_failed = true;
            }
            CaptureStreamTag::RecordStream => {
                if let Some(ref mut s) = self.stream_ext {
                    s.stopped = true;
                    s.capturing = false;
                }
            }
        }
    }

    pub fn handle_ext_frame_ready(&mut self, tag: CaptureStreamTag) {
        match tag {
            CaptureStreamTag::OneShot => {
                let Some(mut sess) = self.oneshot_ext.take() else {
                    return;
                };
                if let Some(frame) = sess.pending_frame.take() {
                    frame.destroy();
                }
                if let (Some(pool), Some(buf), Some(fmt)) =
                    (&mut sess.pool, &sess.buffer, sess.format)
                {
                    if let Some(raw) = buf.canvas(pool) {
                        let rgba = shm_to_rgba(raw, sess.width, sess.height, fmt, false);
                        self.oneshot_result = Some(CapturedOutputFrame {
                            width: sess.width,
                            height: sess.height,
                            rgba,
                        });
                    } else {
                        self.oneshot_failed = true;
                    }
                } else {
                    self.oneshot_failed = true;
                }
                sess.session.destroy();
                sess.source.destroy();
            }
            CaptureStreamTag::RecordStream => {
                let Some(ref mut sess) = self.stream_ext else {
                    return;
                };
                if let Some(frame) = sess.pending_frame.take() {
                    frame.destroy();
                }
                sess.capturing = false;
                let (Some(pool), Some(buf), Some(fmt)) =
                    (&mut sess.pool, &sess.buffer, sess.format)
                else {
                    return;
                };
                let Some(raw) = buf.canvas(pool) else {
                    return;
                };
                let expected = (sess.width * sess.height * 4) as usize;
                if raw.len() >= expected {
                    if let Some(ref tx) = self.stream_tx {
                        let _ = tx.try_send(RawVideoFrame {
                            width: sess.width,
                            height: sess.height,
                            format: fmt,
                            data: raw[..expected].to_vec(),
                        });
                    }
                }
            }
        }
    }

    pub fn handle_ext_frame_failed(&mut self, tag: CaptureStreamTag, stopped: bool) {
        match tag {
            CaptureStreamTag::OneShot => {
                self.cancel_oneshot();
                self.oneshot_failed = true;
            }
            CaptureStreamTag::RecordStream => {
                if let Some(ref mut sess) = self.stream_ext {
                    if let Some(frame) = sess.pending_frame.take() {
                        frame.destroy();
                    }
                    sess.capturing = false;
                    if stopped {
                        sess.stopped = true;
                    }
                }
            }
        }
    }

    // --- ZwlrScreencopyFrameV1 callbacks (fallback) ---

    pub fn handle_wlr_buffer(
        &mut self,
        format: WEnum<wl_shm::Format>,
        width: u32,
        height: u32,
        stride: u32,
    ) {
        if let Some(ref mut pending) = self.wlr_pending {
            if let WEnum::Value(f) = format {
                pending.format = Some(f);
            }
            pending.width = width;
            pending.height = height;
            pending.stride = stride;
        }
    }

    pub fn handle_wlr_flags(&mut self, y_invert: bool) {
        if let Some(ref mut pending) = self.wlr_pending {
            pending.y_invert = y_invert;
        }
    }

    pub fn handle_wlr_buffer_done(&mut self) {
        let (Some(pending), Some(shm)) = (self.wlr_pending.as_mut(), self.shm.as_ref()) else {
            return;
        };
        let Some(fmt) = pending.format else {
            return;
        };
        let byte_len = (pending.stride * pending.height) as usize;

        if pending.tag == CaptureStreamTag::RecordStream {
            let dims = (pending.width, pending.height, pending.stride);
            if self.wlr_stream_dims != dims || self.wlr_stream_buffer.is_none() {
                if let Ok(mut pool) = SlotPool::new(byte_len.max(4096), shm) {
                    if let Ok((buf, _)) = pool.create_buffer(
                        pending.width as i32,
                        pending.height as i32,
                        pending.stride as i32,
                        fmt,
                    ) {
                        self.wlr_stream_pool = Some(pool);
                        self.wlr_stream_buffer = Some(buf);
                        self.wlr_stream_dims = dims;
                    }
                }
            }
            if let Some(ref buf) = self.wlr_stream_buffer {
                pending.frame.copy(buf.wl_buffer());
            }
        } else if let Ok(mut pool) = SlotPool::new(byte_len.max(4096), shm) {
            if let Ok((buf, _)) = pool.create_buffer(
                pending.width as i32,
                pending.height as i32,
                pending.stride as i32,
                fmt,
            ) {
                pending.frame.copy(buf.wl_buffer());
                pending.pool = Some(pool);
                pending.buffer = Some(buf);
            }
        }
    }

    pub fn handle_wlr_ready(&mut self) {
        let Some(mut pending) = self.wlr_pending.take() else {
            return;
        };
        pending.frame.destroy();

        let Some(fmt) = pending.format else {
            if pending.tag == CaptureStreamTag::OneShot {
                self.oneshot_failed = true;
            }
            return;
        };

        match pending.tag {
            CaptureStreamTag::OneShot => {
                if let (Some(pool), Some(buf)) = (&mut pending.pool, &pending.buffer) {
                    if let Some(raw) = buf.canvas(pool) {
                        let rgba = shm_to_rgba(
                            raw,
                            pending.width,
                            pending.height,
                            fmt,
                            pending.y_invert,
                        );
                        self.oneshot_result = Some(CapturedOutputFrame {
                            width: pending.width,
                            height: pending.height,
                            rgba,
                        });
                        return;
                    }
                }
                self.oneshot_failed = true;
            }
            CaptureStreamTag::RecordStream => {
                if let (Some(pool), Some(buf), Some(tx)) = (
                    &mut self.wlr_stream_pool,
                    &self.wlr_stream_buffer,
                    &self.stream_tx,
                ) {
                    if let Some(raw) = buf.canvas(pool) {
                        let expected = (pending.width * pending.height * 4) as usize;
                        if raw.len() >= expected {
                            let _ = tx.try_send(RawVideoFrame {
                                width: pending.width,
                                height: pending.height,
                                format: fmt,
                                data: raw[..expected].to_vec(),
                            });
                        }
                    }
                }
            }
        }
    }

    pub fn handle_wlr_failed(&mut self) {
        if let Some(pending) = self.wlr_pending.take() {
            pending.frame.destroy();
            if pending.tag == CaptureStreamTag::OneShot {
                self.oneshot_failed = true;
            }
        }
    }
}

pub fn shm_to_rgba(
    src: &[u8],
    width: u32,
    height: u32,
    fmt: wl_shm::Format,
    y_invert: bool,
) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let row_bytes = w * 4;
    let mut rgba = vec![255u8; w * h * 4];
    let swap_rb = matches!(fmt, wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888);
    let opaque = matches!(fmt, wl_shm::Format::Xrgb8888 | wl_shm::Format::Xbgr8888);

    for dy in 0..h {
        let sy = if y_invert { h - 1 - dy } else { dy };
        let Some(src_row) = src.get(sy * row_bytes..(sy + 1) * row_bytes) else {
            continue;
        };
        let dst_row = &mut rgba[dy * row_bytes..(dy + 1) * row_bytes];
        if !swap_rb && !opaque {
            dst_row.copy_from_slice(src_row);
            continue;
        }
        for (s_px, d_px) in src_row.chunks_exact(4).zip(dst_row.chunks_exact_mut(4)) {
            if swap_rb {
                d_px[0] = s_px[2];
                d_px[1] = s_px[1];
                d_px[2] = s_px[0];
            } else {
                d_px[0] = s_px[0];
                d_px[1] = s_px[1];
                d_px[2] = s_px[2];
            }
            d_px[3] = if opaque { 255 } else { s_px[3] };
        }
    }
    rgba
}
