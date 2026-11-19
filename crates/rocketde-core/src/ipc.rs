use crossbeam_channel::Sender;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum IpcCommand {
    ToggleLauncher,
    OpenLauncher,
    CloseLauncher,
    ToggleOverview,
    ToggleControlCenter,
    ToggleDock,
    OpenScreenshot,
    OpenScreenRecord,
    StopRecording,
    VolumeDelta(i32),
    MuteToggle,
    BrightnessDelta(i32),
    ReloadConfig,
    Ping,
    Quit,
}

pub fn socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime).join("rocketde.sock")
}

/// Sends a command to a running `rocketde` daemon if one exists.
/// Returns `Ok(true)` if the daemon received and acknowledged the command.
pub fn send_command_to_daemon(cmd: &IpcCommand) -> Result<bool, String> {
    let path = socket_path();
    if !path.exists() {
        return Ok(false);
    }

    match UnixStream::connect(&path) {
        Ok(mut stream) => {
            let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let mut payload = serde_json::to_string(cmd).map_err(|e| e.to_string())?;
            payload.push('\n');
            stream.write_all(payload.as_bytes()).map_err(|e| e.to_string())?;
            stream.flush().map_err(|e| e.to_string())?;

            let mut reader = BufReader::new(stream);
            let mut resp = String::new();
            let _ = reader.read_line(&mut resp);
            Ok(resp.trim() == "OK")
        }
        Err(_) => {
            // Stale socket file
            let _ = fs::remove_file(&path);
            Ok(false)
        }
    }
}

/// Starts the background IPC listener thread and Global Hotkey listener (`evdev` fallback if readable)
pub fn spawn_ipc_server(tx: Sender<IpcCommand>, wake_fn: impl Fn() + Send + Sync + 'static) {
    let path = socket_path();
    let _ = fs::remove_file(&path);

    let wake = std::sync::Arc::new(wake_fn);
    let wake_ipc = wake.clone();
    let tx_ipc = tx.clone();

    thread::spawn(move || {
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[rocketde] Falha ao criar Unix Socket em {:?}: {}", path, e);
                return;
            }
        };

        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() {
                if let Ok(cmd) = serde_json::from_str::<IpcCommand>(line.trim()) {
                    let _ = tx_ipc.send(cmd);
                    wake_ipc();
                    if let Ok(mut writer) = stream.try_clone() {
                        let _ = writer.write_all(b"OK\n");
                        let _ = writer.flush();
                    }
                }
            }
        }
    });

    // Also spawn a lightweight Linux raw input hotkey listener (Super+Space / Alt+Space)
    // when the user is in the `input` group, so hotkeys work even without WM config changes!
    spawn_evdev_hotkey_listener(tx.clone(), wake.clone());

    // Watch ~/.config/rocketde/config.toml for live hot-reloading using `notify`
    spawn_config_watcher(tx, wake);
}

fn spawn_config_watcher(tx: Sender<IpcCommand>, wake: std::sync::Arc<dyn Fn() + Send + Sync>) {
    use notify::{RecursiveMode, Watcher};
    use std::sync::Mutex;
    use std::time::Instant;

    let cfg_path = crate::config::RocketConfig::config_path();
    let theme_path = crate::design::ThemeConfig::theme_path();
    let cfg_dir = cfg_path.parent().map(|p| p.to_path_buf());
    let theme_dir = theme_path.parent().map(|p| p.to_path_buf());
    if cfg_dir.is_none() && theme_dir.is_none() {
        return;
    }

    thread::spawn(move || {
        let last_emit = Mutex::new(Instant::now() - Duration::from_secs(10));
        let cfg_name = cfg_path.file_name().map(|s| s.to_os_string());
        let theme_name = theme_path.file_name().map(|s| s.to_os_string());

        let mut watcher = match notify::recommended_watcher(move |res: Result<notify::Event, _>| {
            let Ok(event) = res else { return };
            if !(event.kind.is_modify() || event.kind.is_create()) {
                return;
            }
            let relevant = event.paths.iter().any(|p| {
                let fname = p.file_name().map(|s| s.to_os_string());
                (cfg_name.is_some() && fname == cfg_name)
                    || (theme_name.is_some() && fname == theme_name)
            });
            if !relevant {
                return;
            }
            if let Ok(mut last) = last_emit.lock() {
                if last.elapsed() < Duration::from_millis(150) {
                    return;
                }
                *last = Instant::now();
            }
            let _ = tx.send(IpcCommand::ReloadConfig);
            wake();
        }) {
            Ok(w) => w,
            Err(_) => return,
        };

        if let Some(dir) = cfg_dir.as_ref() {
            let _ = watcher.watch(dir, RecursiveMode::NonRecursive);
        }
        if let Some(dir) = theme_dir.as_ref() {
            if cfg_dir.as_ref() != Some(dir) {
                let _ = watcher.watch(dir, RecursiveMode::NonRecursive);
            }
        }
        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    });
}

fn spawn_evdev_hotkey_listener(tx: Sender<IpcCommand>, wake: std::sync::Arc<dyn Fn() + Send + Sync>) {
    thread::spawn(move || {
        let Ok(entries) = fs::read_dir("/dev/input") else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !name.starts_with("event") {
                continue;
            }

            if let Ok(file) = fs::File::open(&path) {
                let tx_clone = tx.clone();
                let wake_clone = wake.clone();
                thread::spawn(move || {
                    read_evdev_keyboard(file, tx_clone, wake_clone);
                });
            }
        }
    });
}

#[repr(C)]
struct InputEvent {
    tv_sec: libc::time_t,
    tv_usec: libc::suseconds_t,
    type_: u16,
    code: u16,
    value: i32,
}

fn read_evdev_keyboard(
    mut file: fs::File,
    tx: Sender<IpcCommand>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
) {
    use std::io::Read;
    const EV_KEY: u16 = 0x01;
    const KEY_SPACE: u16 = 57;
    const KEY_LEFTALT: u16 = 56;
    const KEY_RIGHTALT: u16 = 100;
    const KEY_LEFTMETA: u16 = 125;
    const KEY_RIGHTMETA: u16 = 126;

    let mut super_down = false;
    let mut alt_down = false;
    let mut buf = [0u8; std::mem::size_of::<InputEvent>()];

    while file.read_exact(&mut buf).is_ok() {
        let ev: InputEvent = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const InputEvent) };
        if ev.type_ == EV_KEY {
            let pressed = ev.value == 1;
            let held = ev.value == 1 || ev.value == 2;
            match ev.code {
                KEY_LEFTMETA | KEY_RIGHTMETA => super_down = held,
                KEY_LEFTALT | KEY_RIGHTALT => alt_down = held,
                KEY_SPACE if pressed && (super_down || alt_down) => {
                    let _ = tx.send(IpcCommand::ToggleLauncher);
                    wake();
                }
                _ => {}
            }
        }
    }
}
