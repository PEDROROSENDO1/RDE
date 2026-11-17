use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaInfo {
    pub playing: bool,
    pub available: bool,
    pub title: String,
    pub artist: String,
    pub player_bus: String,
}

impl Default for MediaInfo {
    fn default() -> Self {
        Self {
            playing: false,
            available: false,
            title: String::new(),
            artist: String::new(),
            player_bus: String::new(),
        }
    }
}

pub struct MediaController {
    pub info: Arc<Mutex<MediaInfo>>,
    active: Arc<AtomicBool>,
}

impl MediaController {
    pub fn new(wake_fn: impl Fn() + Send + Sync + 'static) -> Self {
        let info = Arc::new(Mutex::new(MediaInfo::default()));
        let active = Arc::new(AtomicBool::new(true));
        let info_clone = info.clone();
        let active_clone = active.clone();
        let wake = Arc::new(wake_fn);

        thread::spawn(move || {
            let mut cached_bus = String::new();
            loop {
                if !active_clone.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(2000));
                    continue;
                }

                let m = query_mpris_dbus(&mut cached_bus);
                if let Some(m) = m {
                    let mut changed = false;
                    if let Ok(mut guard) = info_clone.lock() {
                        if *guard != m {
                            *guard = m;
                            changed = true;
                        }
                    }
                    if changed {
                        wake();
                    }
                }
                let sleep_ms = if cached_bus.is_empty() { 5000 } else { 3000 };
                thread::sleep(Duration::from_millis(sleep_ms));
            }
        });

        Self { info, active }
    }

    pub fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Relaxed);
    }

    pub fn play_pause(&self) {
        let bus = self
            .info
            .lock()
            .ok()
            .map(|g| g.player_bus.clone())
            .unwrap_or_default();
        call_mpris_method(&bus, "PlayPause");
    }

    pub fn next(&self) {
        let bus = self
            .info
            .lock()
            .ok()
            .map(|g| g.player_bus.clone())
            .unwrap_or_default();
        call_mpris_method(&bus, "Next");
    }

    pub fn previous(&self) {
        let bus = self
            .info
            .lock()
            .ok()
            .map(|g| g.player_bus.clone())
            .unwrap_or_default();
        call_mpris_method(&bus, "Previous");
    }
}

fn call_mpris_method(player_bus: &str, method: &str) {
    let bus = if player_bus.is_empty() {
        find_first_mpris_bus().unwrap_or_default()
    } else {
        player_bus.to_string()
    };
    if bus.is_empty() {
        return;
    }

    let method_owned = method.to_string();
    thread::spawn(move || {
        let _ = Command::new("busctl")
            .args([
                "--user",
                "call",
                &bus,
                "/org/mpris/MediaPlayer2",
                "org.mpris.MediaPlayer2.Player",
                &method_owned,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    });
}

fn find_first_mpris_bus() -> Option<String> {
    let out = Command::new("busctl")
        .args(["--user", "--list"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let bus_name = line.split_whitespace().next().unwrap_or("");
        if bus_name.starts_with("org.mpris.MediaPlayer2.") {
            return Some(bus_name.to_string());
        }
    }
    None
}

fn query_mpris_dbus(cached_bus: &mut String) -> Option<MediaInfo> {
    if !cached_bus.is_empty() {
        if let Some(info) = query_mpris_for_bus(cached_bus) {
            return Some(info);
        }
        cached_bus.clear();
    }

    let bus = match find_first_mpris_bus() {
        Some(b) => b,
        None => return Some(MediaInfo::default()),
    };
    *cached_bus = bus.clone();
    query_mpris_for_bus(&bus).or_else(|| Some(MediaInfo::default()))
}

fn query_mpris_for_bus(bus: &str) -> Option<MediaInfo> {
    let status_out = Command::new("busctl")
        .args([
            "--user",
            "get-property",
            bus,
            "/org/mpris/MediaPlayer2",
            "org.mpris.MediaPlayer2.Player",
            "PlaybackStatus",
        ])
        .stderr(Stdio::null())
        .output()
        .ok()?;

    if !status_out.status.success() {
        return None;
    }
    let status_txt = String::from_utf8_lossy(&status_out.stdout);
    let playing = status_txt.contains("Playing");
    let available = playing || status_txt.contains("Paused");
    if !available {
        return Some(MediaInfo::default());
    }

    let meta_out = Command::new("busctl")
        .args([
            "--user",
            "get-property",
            bus,
            "/org/mpris/MediaPlayer2",
            "org.mpris.MediaPlayer2.Player",
            "Metadata",
        ])
        .stderr(Stdio::null())
        .output()
        .ok()?;

    let meta_txt = String::from_utf8_lossy(&meta_out.stdout);
    let title = extract_mpris_field(&meta_txt, "xesam:title").unwrap_or_else(|| {
        bus.trim_start_matches("org.mpris.MediaPlayer2.")
            .to_string()
    });
    let artist = extract_mpris_field(&meta_txt, "xesam:artist").unwrap_or_default();

    Some(MediaInfo {
        playing,
        available,
        title,
        artist,
        player_bus: bus.to_string(),
    })
}

fn extract_mpris_field(raw: &str, key: &str) -> Option<String> {
    let idx = raw.find(key)?;
    let rest = &raw[idx + key.len()..];
    // Look for the next quoted value after the key
    let mut quotes = rest.split('"');
    let _before_first = quotes.next()?;
    let val = quotes.next()?;
    if val.trim().is_empty() {
        None
    } else {
        Some(val.trim().to_string())
    }
}
