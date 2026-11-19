use crate::wayland::screen_capture::RawVideoFrame;
use crossbeam_channel::{Receiver, Sender};
use image::{ImageBuffer, Rgba};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use wayland_client::protocol::wl_shm;

pub fn xdg_screenshots_dir(override_dir: Option<&str>) -> PathBuf {
    let base = override_dir
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::picture_dir()
                .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join("Pictures"))
                .join("Screenshots")
        });
    let _ = fs::create_dir_all(&base);
    base
}

pub fn xdg_recordings_dir(override_dir: Option<&str>) -> PathBuf {
    let base = override_dir
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::video_dir()
                .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")).join("Videos"))
                .join("Screencasts")
        });
    let _ = fs::create_dir_all(&base);
    base
}

pub fn encode_rgba_to_png_bytes(
    rgba: &[u8],
    full_w: u32,
    full_h: u32,
    crop: Option<[u32; 4]>,
) -> Result<Vec<u8>, String> {
    let (cx, cy, cw, ch) = match crop {
        Some([x, y, w, h]) => {
            let x = x.min(full_w.saturating_sub(1));
            let y = y.min(full_h.saturating_sub(1));
            let w = w.min(full_w - x).max(1);
            let h = h.min(full_h - y).max(1);
            (x, y, w, h)
        }
        None => (0, 0, full_w.max(1), full_h.max(1)),
    };

    let cropped_rgba: Vec<u8> = if cx == 0 && cy == 0 && cw == full_w && ch == full_h {
        rgba.to_vec()
    } else {
        let mut out = Vec::with_capacity((cw * ch * 4) as usize);
        let full_stride = (full_w * 4) as usize;
        let row_bytes = (cw * 4) as usize;
        let start_col = (cx * 4) as usize;
        for row in cy..(cy + ch) {
            let off = (row as usize) * full_stride + start_col;
            if let Some(slice) = rgba.get(off..off + row_bytes) {
                out.extend_from_slice(slice);
            }
        }
        out
    };

    let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_raw(cw, ch, cropped_rgba).ok_or_else(|| "Buffer RGBA inválido".to_string())?;
    let mut png_bytes = Vec::with_capacity((cw * ch) as usize / 2);
    let mut cursor = std::io::Cursor::new(&mut png_bytes);
    img.write_to(&mut cursor, image::ImageFormat::Png)
        .map_err(|e| format!("Falha ao codificar PNG: {e}"))?;
    Ok(png_bytes)
}

pub fn save_screenshot_png(png_bytes: &[u8], override_dir: Option<&str>) -> Result<PathBuf, String> {
    let dir = xdg_screenshots_dir(override_dir);
    let ts = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
    let path = dir.join(format!("Screenshot_{ts}.png"));
    fs::write(&path, png_bytes).map_err(|e| format!("Falha ao salvar screenshot: {e}"))?;
    Ok(path)
}

#[derive(Clone, Debug)]
pub struct RecorderStartConfig {
    pub width: u32,
    pub height: u32,
    pub crop_rect: Option<[u32; 4]>,
    pub fps: u32,
    pub system_audio: bool,
    pub mic_audio: bool,
    pub prefer_vaapi: bool,
    pub override_dir: Option<String>,
}

pub struct ActiveRecording {
    pub started_at: Instant,
    pub output_path: PathBuf,
    pub frame_tx: Sender<RawVideoFrame>,
    stop_flag: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    pub last_error: Arc<Mutex<Option<String>>>,
}

impl ActiveRecording {
    pub fn elapsed_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    pub fn is_finished(&self) -> bool {
        self.worker.as_ref().is_some_and(|h| h.is_finished())
    }

    pub fn stop(mut self) -> Result<PathBuf, String> {
        self.stop_flag.store(true, Ordering::SeqCst);
        // Drop sender by replacing with a dummy? Actually we take worker after dropping frame_tx
        drop(self.frame_tx);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
        unsafe {
            libc::malloc_trim(0);
        }
        if let Ok(guard) = self.last_error.lock() {
            if let Some(ref err) = *guard {
                return Err(err.clone());
            }
        }
        Ok(self.output_path)
    }
}

fn has_vaapi_encoder() -> bool {
    static VAAPI_OK: OnceLock<bool> = OnceLock::new();
    *VAAPI_OK.get_or_init(|| {
        if !std::path::Path::new("/dev/dri/renderD128").exists() {
            return false;
        }
        Command::new("ffmpeg")
            .args(["-hide_banner", "-encoders"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("h264_vaapi"))
            .unwrap_or(false)
    })
}

fn pactl_default_value(subcmd: &str) -> Option<String> {
    let out = Command::new("pactl").arg(subcmd).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn shm_format_to_ffmpeg_pix_fmt(fmt: wl_shm::Format) -> &'static str {
    match fmt {
        wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888 => "bgr0",
        wl_shm::Format::Abgr8888 | wl_shm::Format::Xbgr8888 => "rgb0",
        _ => "bgr0",
    }
}

pub fn start_recording(
    cfg: RecorderStartConfig,
    frame_rx: Receiver<RawVideoFrame>,
    frame_tx: Sender<RawVideoFrame>,
) -> Result<ActiveRecording, String> {
    let dir = xdg_recordings_dir(cfg.override_dir.as_deref());
    let ts = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
    let output_path = dir.join(format!("Screencast_{ts}.mp4"));

    // Compute even dimensions for H.264 compatibility
    let (crop_x, crop_y, out_w, out_h) = match cfg.crop_rect {
        Some([x, y, w, h]) => {
            let x = x.min(cfg.width.saturating_sub(2));
            let y = y.min(cfg.height.saturating_sub(2));
            let max_w = (cfg.width - x) & !1;
            let max_h = (cfg.height - y) & !1;
            let w = (w & !1).clamp(2, max_w.max(2));
            let h = (h & !1).clamp(2, max_h.max(2));
            (x, y, w, h)
        }
        None => (0, 0, (cfg.width & !1).max(2), (cfg.height & !1).max(2)),
    };

    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_worker = stop_flag.clone();
    let last_error = Arc::new(Mutex::new(None));
    let last_error_worker = last_error.clone();
    let out_path_clone = output_path.clone();

    let worker = thread::spawn(move || {
        // Wait for the first frame so we know the exact wl_shm pixel format
        let first_frame = loop {
            if stop_flag_worker.load(Ordering::SeqCst) {
                return;
            }
            match frame_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(f) => break f,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
            }
        };

        let pix_fmt = shm_format_to_ffmpeg_pix_fmt(first_frame.format);
        let use_vaapi = cfg.prefer_vaapi && has_vaapi_encoder();

        let mut cmd = Command::new("ffmpeg");
        cmd.arg("-y").arg("-loglevel").arg("error");

        if use_vaapi {
            cmd.args(["-vaapi_device", "/dev/dri/renderD128"]);
        }

        cmd.args([
            "-f",
            "rawvideo",
            "-pix_fmt",
            pix_fmt,
            "-s",
            &format!("{out_w}x{out_h}"),
            "-use_wallclock_as_timestamps",
            "1",
            "-i",
            "pipe:0",
        ]);

        let mut audio_inputs = 0u32;
        if cfg.system_audio {
            if let Some(sink) = pactl_default_value("get-default-sink") {
                cmd.args(["-f", "pulse", "-i", &format!("{sink}.monitor")]);
                audio_inputs += 1;
            }
        }
        if cfg.mic_audio {
            if let Some(source) = pactl_default_value("get-default-source") {
                cmd.args(["-f", "pulse", "-i", &source]);
                audio_inputs += 1;
            }
        }

        if use_vaapi {
            cmd.args(["-vf", "format=nv12,hwupload", "-c:v", "h264_vaapi", "-qp", "22"]);
        } else {
            cmd.args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-crf",
                "23",
                "-pix_fmt",
                "yuv420p",
            ]);
        }

        if audio_inputs == 2 {
            cmd.args(["-filter_complex", "[1:a][2:a]amix=inputs=2:duration=longest[aout]", "-map", "0:v", "-map", "[aout]", "-c:a", "aac", "-b:a", "128k"]);
        } else if audio_inputs == 1 {
            cmd.args(["-c:a", "aac", "-b:a", "128k"]);
        }

        cmd.args([
            "-r",
            &cfg.fps.to_string(),
            "-fps_mode",
            "cfr",
            "-movflags",
            "+faststart",
        ]);
        cmd.arg(&out_path_clone);

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child: Child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                if let Ok(mut g) = last_error_worker.lock() {
                    *g = Some(format!("Falha ao iniciar ffmpeg: {e}"));
                }
                return;
            }
        };

        let Some(stdin) = child.stdin.take() else {
            let _ = child.kill();
            return;
        };
        let mut writer = BufWriter::with_capacity((out_w * out_h * 4) as usize, stdin);

        let write_frame = |writer: &mut BufWriter<std::process::ChildStdin>, frame: &RawVideoFrame| -> bool {
            let src_stride = (frame.width * 4) as usize;
            let row_bytes = (out_w * 4) as usize;
            let start_col = (crop_x * 4) as usize;
            if crop_x == 0 && crop_y == 0 && out_w == frame.width && out_h == frame.height {
                writer.write_all(&frame.data).is_ok()
            } else {
                for row in crop_y..(crop_y + out_h).min(frame.height) {
                    let off = (row as usize) * src_stride + start_col;
                    if let Some(slice) = frame.data.get(off..off + row_bytes) {
                        if writer.write_all(slice).is_err() {
                            return false;
                        }
                    }
                }
                true
            }
        };

        if write_frame(&mut writer, &first_frame) {
            while !stop_flag_worker.load(Ordering::SeqCst) {
                match frame_rx.recv_timeout(Duration::from_millis(80)) {
                    Ok(frame) => {
                        if !write_frame(&mut writer, &frame) {
                            break;
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                }
            }
        }

        let _ = writer.flush();
        drop(writer);

        // Wait cleanly so ffmpeg writes the MP4 moov atom
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        if let Some(mut err_pipe) = child.stderr.take() {
                            let mut msg = String::new();
                            let _ = std::io::Read::read_to_string(&mut err_pipe, &mut msg);
                            if !msg.trim().is_empty() {
                                if let Ok(mut g) = last_error_worker.lock() {
                                    *g = Some(msg.trim().to_string());
                                }
                            }
                        }
                    }
                    break;
                }
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(40));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
    });

    Ok(ActiveRecording {
        started_at: Instant::now(),
        output_path,
        frame_tx,
        stop_flag,
        worker: Some(worker),
        last_error,
    })
}
