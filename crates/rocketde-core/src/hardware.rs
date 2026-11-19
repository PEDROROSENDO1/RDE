use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    pub signal: u8,
    pub security: String,
    pub active: bool,
    pub saved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BluetoothDevice {
    pub mac: String,
    pub name: String,
    pub connected: bool,
    pub paired: bool,
    pub trusted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioBrightnessState {
    pub volume_percent: f32,
    pub muted: bool,
    pub mic_volume_percent: f32,
    pub mic_muted: bool,
    pub audio_sink_name: String,
    pub brightness_percent: f32,
    pub backlight_device: String,
    pub max_brightness: u32,
    pub wifi_enabled: bool,
    pub wifi_ssid: String,
    pub wifi_networks: Vec<WifiNetwork>,
    pub wifi_scanning: bool,
    pub wifi_connecting_ssid: Option<String>,
    pub wifi_status_msg: Option<String>,
    pub bluetooth_enabled: bool,
    pub bluetooth_connected_name: String,
    pub bluetooth_devices: Vec<BluetoothDevice>,
    pub bluetooth_scanning: bool,
    pub bluetooth_connecting_mac: Option<String>,
    pub bluetooth_status_msg: Option<String>,
    pub dnd_enabled: bool,
    pub power_saver: bool,
    pub power_profile_name: String,
}

impl Default for AudioBrightnessState {
    fn default() -> Self {
        Self {
            volume_percent: 50.0,
            muted: false,
            mic_volume_percent: 100.0,
            mic_muted: false,
            audio_sink_name: "Áudio Padrão".to_string(),
            brightness_percent: 100.0,
            backlight_device: String::new(),
            max_brightness: 255,
            wifi_enabled: true,
            wifi_ssid: String::new(),
            wifi_networks: Vec::new(),
            wifi_scanning: false,
            wifi_connecting_ssid: None,
            wifi_status_msg: None,
            bluetooth_enabled: false,
            bluetooth_connected_name: String::new(),
            bluetooth_devices: Vec::new(),
            bluetooth_scanning: false,
            bluetooth_connecting_mac: None,
            bluetooth_status_msg: None,
            dnd_enabled: false,
            power_saver: false,
            power_profile_name: "Balanced".to_string(),
        }
    }
}

const NO_PENDING_U32: u32 = u32::MAX;

pub struct HardwareController {
    pub state: Arc<Mutex<AudioBrightnessState>>,
    detailed_active: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
    pending_vol: Arc<AtomicU32>,
    vol_worker_running: Arc<AtomicBool>,
    pending_mic_vol: Arc<AtomicU32>,
    mic_worker_running: Arc<AtomicBool>,
    pending_brightness: Arc<AtomicU32>,
    brightness_worker_running: Arc<AtomicBool>,
}

impl HardwareController {
    pub fn new(wake_fn: impl Fn() + Send + Sync + 'static) -> Self {
        let initial = AudioBrightnessState::default();
        let state = Arc::new(Mutex::new(initial));
        let detailed_active = Arc::new(AtomicBool::new(false));
        let pactl_connected = Arc::new(AtomicBool::new(false));
        let state_clone = state.clone();
        let detailed_clone = detailed_active.clone();
        let pactl_flag_sub = pactl_connected.clone();
        let pactl_flag_poll = pactl_connected.clone();
        let wake = Arc::new(wake_fn);

        // Event-driven audio volume/mute updates via `pactl subscribe`
        let state_audio = state.clone();
        let wake_audio = wake.clone();
        thread::spawn(move || {
            loop {
                if let Ok(mut child) = Command::new("pactl")
                    .arg("subscribe")
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    pactl_flag_sub.store(true, Ordering::Relaxed);
                    if let Some(stdout) = child.stdout.take() {
                        let reader = BufReader::new(stdout);
                        for line in reader.lines().map_while(Result::ok) {
                            if line.contains("sink") {
                                if let Some((vol, muted)) = query_real_volume() {
                                    let mut changed = false;
                                    if let Ok(mut guard) = state_audio.lock() {
                                        if (guard.volume_percent - vol).abs() >= 0.5
                                            || guard.muted != muted
                                        {
                                            guard.volume_percent = vol;
                                            guard.muted = muted;
                                            changed = true;
                                        }
                                    }
                                    if changed {
                                        wake_audio();
                                    }
                                }
                            } else if line.contains("source") {
                                if let Some((mic_vol, mic_muted)) = query_real_mic_volume() {
                                    let mut changed = false;
                                    if let Ok(mut guard) = state_audio.lock() {
                                        if (guard.mic_volume_percent - mic_vol).abs() >= 0.5
                                            || guard.mic_muted != mic_muted
                                        {
                                            guard.mic_volume_percent = mic_vol;
                                            guard.mic_muted = mic_muted;
                                            changed = true;
                                        }
                                    }
                                    if changed {
                                        wake_audio();
                                    }
                                }
                            }
                        }
                    }
                    pactl_flag_sub.store(false, Ordering::Relaxed);
                    let _ = child.wait();
                }
                thread::sleep(Duration::from_secs(5));
            }
        });

        let wake_poll = wake.clone();
        thread::spawn(move || {
            let mut first_sync = true;
            loop {
                let query_audio = first_sync || !pactl_flag_poll.load(Ordering::Relaxed);
                first_sync = false;

                let mut sampled = AudioBrightnessState::default();
                refresh_hardware_state(&mut sampled, query_audio);

                let mut changed = false;
                if let Ok(mut guard) = state_clone.lock() {
                    if query_audio {
                        if (guard.volume_percent - sampled.volume_percent).abs() >= 0.5
                            || guard.muted != sampled.muted
                        {
                            guard.volume_percent = sampled.volume_percent;
                            guard.muted = sampled.muted;
                            changed = true;
                        }
                        if (guard.mic_volume_percent - sampled.mic_volume_percent).abs() >= 0.5
                            || guard.mic_muted != sampled.mic_muted
                        {
                            guard.mic_volume_percent = sampled.mic_volume_percent;
                            guard.mic_muted = sampled.mic_muted;
                            changed = true;
                        }
                    }
                    if (guard.brightness_percent - sampled.brightness_percent).abs() >= 0.5
                        || guard.backlight_device != sampled.backlight_device
                        || guard.max_brightness != sampled.max_brightness
                    {
                        guard.backlight_device = sampled.backlight_device;
                        guard.brightness_percent = sampled.brightness_percent;
                        guard.max_brightness = sampled.max_brightness;
                        changed = true;
                    }
                    if guard.wifi_enabled != sampled.wifi_enabled
                        || guard.bluetooth_enabled != sampled.bluetooth_enabled
                        || guard.wifi_ssid != sampled.wifi_ssid
                        || guard.power_saver != sampled.power_saver
                        || guard.power_profile_name != sampled.power_profile_name
                    {
                        guard.wifi_enabled = sampled.wifi_enabled;
                        guard.bluetooth_enabled = sampled.bluetooth_enabled;
                        guard.wifi_ssid = sampled.wifi_ssid;
                        guard.power_saver = sampled.power_saver;
                        guard.power_profile_name = sampled.power_profile_name;
                        changed = true;
                    }
                }
                if changed {
                    wake_poll();
                }
                let sleep_ms = if detailed_clone.load(Ordering::Relaxed) {
                    6000
                } else {
                    20000
                };
                thread::sleep(Duration::from_millis(sleep_ms));
            }
        });

        Self {
            state,
            detailed_active,
            wake,
            pending_vol: Arc::new(AtomicU32::new(NO_PENDING_U32)),
            vol_worker_running: Arc::new(AtomicBool::new(false)),
            pending_mic_vol: Arc::new(AtomicU32::new(NO_PENDING_U32)),
            mic_worker_running: Arc::new(AtomicBool::new(false)),
            pending_brightness: Arc::new(AtomicU32::new(NO_PENDING_U32)),
            brightness_worker_running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn set_detailed_active(&self, active: bool) {
        self.detailed_active.store(active, Ordering::Relaxed);
    }

    /// Sets the real system volume (0% .. 150%) using `pactl` (or `wpctl` if installed), coalescing rapid slider updates.
    pub fn set_volume(&self, new_vol: f32) {
        let clamped = new_vol.clamp(0.0, 150.0);
        if let Ok(mut guard) = self.state.lock() {
            guard.volume_percent = clamped;
            if clamped > 0.0 && guard.muted {
                guard.muted = false;
            }
        }
        self.pending_vol
            .store(clamped.round() as u32, Ordering::SeqCst);
        spawn_pulse_volume_coalescer(
            false,
            self.pending_vol.clone(),
            self.vol_worker_running.clone(),
        );
    }

    /// Toggles real audio mute state
    pub fn toggle_mute(&self) {
        if let Ok(mut guard) = self.state.lock() {
            guard.muted = !guard.muted;
        }
        apply_pulse_mute_toggle(false);
    }

    /// Sets the real microphone input volume (0% .. 100%) using `pactl` (or `wpctl` if installed), coalescing rapid slider updates.
    pub fn set_mic_volume(&self, new_vol: f32) {
        let clamped = new_vol.clamp(0.0, 100.0);
        if let Ok(mut guard) = self.state.lock() {
            guard.mic_volume_percent = clamped;
            if clamped > 0.0 && guard.mic_muted {
                guard.mic_muted = false;
            }
        }
        self.pending_mic_vol
            .store(clamped.round() as u32, Ordering::SeqCst);
        spawn_pulse_volume_coalescer(
            true,
            self.pending_mic_vol.clone(),
            self.mic_worker_running.clone(),
        );
    }

    /// Toggles real microphone mute state
    pub fn toggle_mic_mute(&self) {
        if let Ok(mut guard) = self.state.lock() {
            guard.mic_muted = !guard.mic_muted;
        }
        apply_pulse_mute_toggle(true);
    }

    /// Sets the real screen backlight brightness using systemd-logind D-Bus (`SetBrightness`),
    /// direct `/sys/class/backlight/<dev>/brightness` write, or `brightnessctl`
    pub fn set_brightness(&self, new_br: f32) {
        let clamped = new_br.clamp(2.0, 100.0);
        let (dev_name, max_val) = if let Ok(mut guard) = self.state.lock() {
            guard.brightness_percent = clamped;
            (guard.backlight_device.clone(), guard.max_brightness.max(1))
        } else {
            return;
        };

        let pct_int = clamped.round() as u32;
        self.pending_brightness.store(pct_int, Ordering::SeqCst);
        if self
            .brightness_worker_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
        {
            let pending = self.pending_brightness.clone();
            let running = self.brightness_worker_running.clone();
            thread::spawn(move || {
                loop {
                    thread::sleep(Duration::from_millis(35));
                    let pct = pending.swap(NO_PENDING_U32, Ordering::SeqCst);
                    if pct == NO_PENDING_U32 {
                        running.store(false, Ordering::SeqCst);
                        if pending.load(Ordering::SeqCst) == NO_PENDING_U32 {
                            break;
                        }
                        if running
                            .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    let raw = ((pct as f32 / 100.0) * (max_val as f32))
                        .round()
                        .clamp(1.0, max_val as f32) as u32;
                    apply_brightness_sync(&dev_name, raw, pct);
                }
            });
        }
    }

    /// Toggles real Wi-Fi radio state using `nmcli` and `rfkill`
    pub fn toggle_wifi(&self) {
        let target_enable = if let Ok(mut guard) = self.state.lock() {
            guard.wifi_enabled = !guard.wifi_enabled;
            if !guard.wifi_enabled {
                guard.wifi_ssid.clear();
                guard.wifi_networks.clear();
            }
            guard.wifi_enabled
        } else {
            return;
        };

        let state_arc = self.state.clone();
        let wake = self.wake.clone();
        thread::spawn(move || {
            let arg = if target_enable { "on" } else { "off" };
            let status = Command::new("nmcli")
                .args(["radio", "wifi", arg])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            if !status.map(|s| s.success()).unwrap_or(false) {
                let rf_cmd = if target_enable { "unblock" } else { "block" };
                let _ = Command::new("rfkill")
                    .args([rf_cmd, "wifi"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }

            if target_enable {
                thread::sleep(Duration::from_millis(600));
                let nets = query_wifi_networks(false);
                if let Ok(mut guard) = state_arc.lock() {
                    guard.wifi_networks = nets;
                    if let Some(ssid) = query_wifi_ssid() {
                        guard.wifi_ssid = ssid;
                    }
                }
            }
            wake();
        });
    }

    /// Scans available Wi-Fi networks asynchronously via `nmcli`
    pub fn scan_wifi_networks(&self, rescan: bool) {
        if let Ok(mut guard) = self.state.lock() {
            if guard.wifi_scanning {
                return;
            }
            guard.wifi_scanning = true;
            guard.wifi_status_msg = None;
        } else {
            return;
        }

        let state_arc = self.state.clone();
        let wake = self.wake.clone();
        thread::spawn(move || {
            let nets = query_wifi_networks(rescan);
            let active_ssid = query_wifi_ssid().unwrap_or_default();
            if let Ok(mut guard) = state_arc.lock() {
                guard.wifi_networks = nets;
                guard.wifi_ssid = active_ssid;
                guard.wifi_scanning = false;
            }
            wake();
        });
    }

    /// Connects to a Wi-Fi network (with optional password) via `nmcli`
    pub fn connect_wifi(&self, ssid: String, password: Option<String>) {
        if ssid.trim().is_empty() {
            return;
        }
        if let Ok(mut guard) = self.state.lock() {
            guard.wifi_connecting_ssid = Some(ssid.clone());
            guard.wifi_status_msg = Some(format!("Conectando a {}...", ssid));
        } else {
            return;
        }

        let state_arc = self.state.clone();
        let wake = self.wake.clone();
        thread::spawn(move || {
            let out = if let Some(ref pass) = password {
                if !pass.is_empty() {
                    Command::new("nmcli")
                        .args(["dev", "wifi", "connect", &ssid, "password", pass])
                        .output()
                } else {
                    Command::new("nmcli")
                        .args(["dev", "wifi", "connect", &ssid])
                        .output()
                }
            } else {
                // Try bringing up saved connection first, fallback to dev wifi connect
                let up_out = Command::new("nmcli")
                    .args(["connection", "up", &ssid])
                    .output();
                if up_out.as_ref().map(|o| o.status.success()).unwrap_or(false) {
                    up_out
                } else {
                    Command::new("nmcli")
                        .args(["dev", "wifi", "connect", &ssid])
                        .output()
                }
            };

            let (ok, err_msg) = match out {
                Ok(o) if o.status.success() => (true, None),
                Ok(o) => {
                    let msg = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    let clean = if msg.is_empty() {
                        "Falha ao conectar (verifique a senha).".to_string()
                    } else {
                        msg
                    };
                    (false, Some(clean))
                }
                Err(e) => (false, Some(format!("Erro ao executar nmcli: {e}"))),
            };

            let nets = query_wifi_networks(false);
            let active_ssid = query_wifi_ssid().unwrap_or_default();
            if let Ok(mut guard) = state_arc.lock() {
                guard.wifi_connecting_ssid = None;
                guard.wifi_networks = nets;
                guard.wifi_ssid = active_ssid;
                guard.wifi_status_msg = if ok {
                    Some(format!("Conectado a {}", ssid))
                } else {
                    err_msg
                };
            }
            wake();
        });
    }

    /// Disconnects from the given saved Wi-Fi connection via `nmcli`
    pub fn disconnect_wifi(&self, ssid: String) {
        let state_arc = self.state.clone();
        let wake = self.wake.clone();
        thread::spawn(move || {
            let _ = Command::new("nmcli")
                .args(["connection", "down", &ssid])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            let nets = query_wifi_networks(false);
            let active_ssid = query_wifi_ssid().unwrap_or_default();
            if let Ok(mut guard) = state_arc.lock() {
                guard.wifi_networks = nets;
                guard.wifi_ssid = active_ssid;
                guard.wifi_status_msg = Some(format!("Desconectado de {}", ssid));
            }
            wake();
        });
    }

    /// Forgets (deletes) a saved Wi-Fi profile so the user can re-enter credentials
    pub fn forget_wifi(&self, ssid: String) {
        let state_arc = self.state.clone();
        let wake = self.wake.clone();
        thread::spawn(move || {
            let _ = Command::new("nmcli")
                .args(["connection", "delete", &ssid])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            let nets = query_wifi_networks(false);
            let active_ssid = query_wifi_ssid().unwrap_or_default();
            if let Ok(mut guard) = state_arc.lock() {
                guard.wifi_networks = nets;
                guard.wifi_ssid = active_ssid;
                guard.wifi_status_msg = Some(format!("Rede {} esquecida", ssid));
            }
            wake();
        });
    }

    /// Toggles real Bluetooth radio state using `rfkill`, `bluetoothctl`, and BlueZ D-Bus
    pub fn toggle_bluetooth(&self) {
        let target_enable = if let Ok(mut guard) = self.state.lock() {
            guard.bluetooth_enabled = !guard.bluetooth_enabled;
            if !guard.bluetooth_enabled {
                guard.bluetooth_connected_name.clear();
                guard.bluetooth_devices.clear();
            }
            guard.bluetooth_status_msg = None;
            guard.bluetooth_enabled
        } else {
            return;
        };

        let state_arc = self.state.clone();
        let wake = self.wake.clone();
        thread::spawn(move || {
            let rf_cmd = if target_enable { "unblock" } else { "block" };
            let _ = Command::new("rfkill")
                .args([rf_cmd, "bluetooth"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            let power_arg = if target_enable { "on" } else { "off" };
            let _ = Command::new("bluetoothctl")
                .args(["power", power_arg])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            let bool_str = if target_enable { "true" } else { "false" };
            let _ = Command::new("busctl")
                .args([
                    "set-property",
                    "org.bluez",
                    "/org/bluez/hci0",
                    "org.bluez.Adapter1",
                    "Powered",
                    "b",
                    bool_str,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            if target_enable {
                let (devs, err_msg) = query_bluetooth_devices(false);
                let connected_name = devs
                    .iter()
                    .find(|d| d.connected)
                    .map(|d| d.name.clone())
                    .unwrap_or_default();
                if let Ok(mut guard) = state_arc.lock() {
                    guard.bluetooth_devices = devs;
                    guard.bluetooth_connected_name = connected_name;
                    if err_msg.is_some() {
                        guard.bluetooth_status_msg = err_msg;
                    }
                }
            }
            wake();
        });
    }

    /// Scans / lists Bluetooth devices via `bluetoothctl` (or BlueZ D-Bus fallback)
    pub fn scan_bluetooth_devices(&self, rescan: bool) {
        if let Ok(mut guard) = self.state.lock() {
            if guard.bluetooth_scanning {
                return;
            }
            guard.bluetooth_scanning = true;
            guard.bluetooth_status_msg = None;
        }

        let state_arc = self.state.clone();
        let wake = self.wake.clone();
        thread::spawn(move || {
            let (devs, err_msg) = query_bluetooth_devices(rescan);
            let connected_name = devs
                .iter()
                .find(|d| d.connected)
                .map(|d| d.name.clone())
                .unwrap_or_default();
            if let Ok(mut guard) = state_arc.lock() {
                guard.bluetooth_devices = devs;
                guard.bluetooth_connected_name = connected_name;
                guard.bluetooth_scanning = false;
                if err_msg.is_some() {
                    guard.bluetooth_status_msg = err_msg;
                }
            }
            wake();
        });
    }

    /// Pairs (if needed), trusts, and connects to a Bluetooth device by MAC address
    pub fn connect_bluetooth(&self, mac: String) {
        if let Ok(mut guard) = self.state.lock() {
            guard.bluetooth_connecting_mac = Some(mac.clone());
            guard.bluetooth_status_msg = Some(format!("Conectando a {}...", mac));
        }

        let state_arc = self.state.clone();
        let wake = self.wake.clone();
        thread::spawn(move || {
            let _ = Command::new("bluetoothctl")
                .args(["trust", &mac])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = Command::new("bluetoothctl")
                .args(["pair", &mac])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let out = Command::new("bluetoothctl")
                .args(["connect", &mac])
                .output();

            let (ok, err_detail) = match out {
                Ok(o) => {
                    let combined = format!(
                        "{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    );
                    (o.status.success(), combined.trim().to_string())
                }
                Err(_) => (
                    false,
                    "bluetoothctl não encontrado (instale bluez-utils e ative bluetooth.service)."
                        .to_string(),
                ),
            };

            let (devs, _) = query_bluetooth_devices(false);
            let connected_name = devs
                .iter()
                .find(|d| d.connected)
                .map(|d| d.name.clone())
                .unwrap_or_default();
            if let Ok(mut guard) = state_arc.lock() {
                guard.bluetooth_connecting_mac = None;
                guard.bluetooth_devices = devs;
                guard.bluetooth_connected_name = connected_name;
                guard.bluetooth_status_msg = Some(if ok {
                    format!("Conectado a {}", mac)
                } else if err_detail.is_empty() {
                    format!("Falha ao conectar a {}", mac)
                } else {
                    err_detail
                });
            }
            wake();
        });
    }

    /// Disconnects a Bluetooth device by MAC address
    pub fn disconnect_bluetooth(&self, mac: String) {
        let state_arc = self.state.clone();
        let wake = self.wake.clone();
        thread::spawn(move || {
            let _ = Command::new("bluetoothctl")
                .args(["disconnect", &mac])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            let (devs, _) = query_bluetooth_devices(false);
            let connected_name = devs
                .iter()
                .find(|d| d.connected)
                .map(|d| d.name.clone())
                .unwrap_or_default();
            if let Ok(mut guard) = state_arc.lock() {
                guard.bluetooth_devices = devs;
                guard.bluetooth_connected_name = connected_name;
                guard.bluetooth_status_msg = Some(format!("Desconectado de {}", mac));
            }
            wake();
        });
    }

    /// Removes/forgets a paired Bluetooth device by MAC address
    pub fn forget_bluetooth(&self, mac: String) {
        let state_arc = self.state.clone();
        let wake = self.wake.clone();
        thread::spawn(move || {
            let _ = Command::new("bluetoothctl")
                .args(["remove", &mac])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            let (devs, _) = query_bluetooth_devices(false);
            let connected_name = devs
                .iter()
                .find(|d| d.connected)
                .map(|d| d.name.clone())
                .unwrap_or_default();
            if let Ok(mut guard) = state_arc.lock() {
                guard.bluetooth_devices = devs;
                guard.bluetooth_connected_name = connected_name;
                guard.bluetooth_status_msg = Some(format!("Dispositivo {} removido", mac));
            }
            wake();
        });
    }

    /// Toggles real Power Profile via `net.hadess.PowerProfiles` D-Bus or `powerprofilesctl`
    pub fn toggle_power_saver(&self) {
        let target_saver = if let Ok(mut guard) = self.state.lock() {
            guard.power_saver = !guard.power_saver;
            guard.power_profile_name = if guard.power_saver {
                "power-saver".to_string()
            } else {
                "balanced".to_string()
            };
            guard.power_saver
        } else {
            return;
        };

        thread::spawn(move || {
            let profile = if target_saver {
                "power-saver"
            } else {
                "balanced"
            };
            let _ = Command::new("busctl")
                .args([
                    "set-property",
                    "net.hadess.PowerProfiles",
                    "/net/hadess/PowerProfiles",
                    "net.hadess.PowerProfiles",
                    "ActiveProfile",
                    "s",
                    profile,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        });
    }

    /// Toggles Do Not Disturb (and signals `makoctl` / `dunstctl` if running)
    pub fn toggle_dnd(&self) {
        let target_dnd = if let Ok(mut guard) = self.state.lock() {
            guard.dnd_enabled = !guard.dnd_enabled;
            guard.dnd_enabled
        } else {
            return;
        };

        thread::spawn(move || {
            let mako_arg = if target_dnd {
                "do-not-disturb"
            } else {
                "default"
            };
            let _ = Command::new("makoctl")
                .args(["mode", "-s", mako_arg])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            let dunst_arg = if target_dnd { "true" } else { "false" };
            let _ = Command::new("dunstctl")
                .args(["set-paused", dunst_arg])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        });
    }
}

fn spawn_pulse_volume_coalescer(
    is_source: bool,
    pending: Arc<AtomicU32>,
    running: Arc<AtomicBool>,
) {
    if running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_millis(35));
            let pct_int = pending.swap(NO_PENDING_U32, Ordering::SeqCst);
            if pct_int == NO_PENDING_U32 {
                running.store(false, Ordering::SeqCst);
                if pending.load(Ordering::SeqCst) == NO_PENDING_U32 {
                    break;
                }
                if running
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
                    .is_err()
                {
                    break;
                }
                continue;
            }
            apply_pulse_volume_sync(is_source, pct_int);
        }
    });
}

fn apply_pulse_volume_sync(is_source: bool, pct_int: u32) {
    let pct_str = format!("{}%", pct_int);
    let (pactl_mute_cmd, pactl_vol_cmd, pactl_target, wpctl_target) = if is_source {
        ("set-source-mute", "set-source-volume", "@DEFAULT_SOURCE@", "@DEFAULT_AUDIO_SOURCE@")
    } else {
        ("set-sink-mute", "set-sink-volume", "@DEFAULT_SINK@", "@DEFAULT_AUDIO_SINK@")
    };

    if pct_int > 0 {
        let _ = Command::new("pactl")
            .args([pactl_mute_cmd, pactl_target, "0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let status = Command::new("pactl")
        .args([pactl_vol_cmd, pactl_target, &pct_str])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    if !status.map(|s| s.success()).unwrap_or(false) {
        let _ = Command::new("wpctl")
            .args(["set-volume", "-l", "1.5", wpctl_target, &pct_str])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn apply_brightness_sync(dev_name: &str, raw_target: u32, pct_int: u32) {
    if !dev_name.is_empty() {
        let sysfs_path = PathBuf::from("/sys/class/backlight")
            .join(dev_name)
            .join("brightness");
        if fs::write(&sysfs_path, format!("{}\n", raw_target)).is_ok() {
            return;
        }

        let busctl_status = Command::new("busctl")
            .args([
                "call",
                "org.freedesktop.login1",
                "/org/freedesktop/login1/session/auto",
                "org.freedesktop.login1.Session",
                "SetBrightness",
                "ssu",
                "backlight",
                dev_name,
                &raw_target.to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        if busctl_status.map(|s| s.success()).unwrap_or(false) {
            return;
        }

        let _ = Command::new("gdbus")
            .args([
                "call",
                "--system",
                "--dest",
                "org.freedesktop.login1",
                "--object-path",
                "/org/freedesktop/login1/session/auto",
                "--method",
                "org.freedesktop.login1.Session.SetBrightness",
                "backlight",
                dev_name,
                &raw_target.to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    let pct_str = format!("{}%", pct_int);
    let _ = Command::new("brightnessctl")
        .args(["set", &pct_str])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn apply_pulse_mute_toggle(is_source: bool) {
    let (pactl_mute_cmd, pactl_target, wpctl_target) = if is_source {
        ("set-source-mute", "@DEFAULT_SOURCE@", "@DEFAULT_AUDIO_SOURCE@")
    } else {
        ("set-sink-mute", "@DEFAULT_SINK@", "@DEFAULT_AUDIO_SINK@")
    };

    thread::spawn(move || {
        let status = Command::new("pactl")
            .args([pactl_mute_cmd, pactl_target, "toggle"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        if !status.map(|s| s.success()).unwrap_or(false) {
            let _ = Command::new("wpctl")
                .args(["set-mute", wpctl_target, "toggle"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    });
}

fn refresh_hardware_state(state: &mut AudioBrightnessState, query_audio: bool) {
    if query_audio {
        if let Some((vol, muted)) = query_real_volume() {
            state.volume_percent = vol;
            state.muted = muted;
        }
        if let Some((mic_vol, mic_muted)) = query_real_mic_volume() {
            state.mic_volume_percent = mic_vol;
            state.mic_muted = mic_muted;
        }
    }
    if let Some((dev, br_pct, max_br)) = query_real_brightness() {
        state.backlight_device = dev;
        state.brightness_percent = br_pct;
        state.max_brightness = max_br;
    }
    let (wifi_on, bt_on) = query_real_rfkill();
    if let Some(w) = wifi_on {
        state.wifi_enabled = w;
    }
    if let Some(b) = bt_on {
        state.bluetooth_enabled = b;
    }
    if state.wifi_enabled {
        if let Some(ssid) = query_wifi_ssid() {
            state.wifi_ssid = ssid;
        }
    } else {
        state.wifi_ssid.clear();
    }
    if let Some((saver, profile)) = query_power_profile() {
        state.power_saver = saver;
        state.power_profile_name = profile;
    }
}

fn query_real_volume() -> Option<(f32, bool)> {
    query_pulse_volume(false)
}

fn query_real_mic_volume() -> Option<(f32, bool)> {
    query_pulse_volume(true)
}

fn query_pulse_volume(is_source: bool) -> Option<(f32, bool)> {
    let (pactl_vol_cmd, pactl_mute_cmd, pactl_target, wpctl_target) = if is_source {
        ("get-source-volume", "get-source-mute", "@DEFAULT_SOURCE@", "@DEFAULT_AUDIO_SOURCE@")
    } else {
        ("get-sink-volume", "get-sink-mute", "@DEFAULT_SINK@", "@DEFAULT_AUDIO_SINK@")
    };

    // 1. Try `pactl` (installed on this system at /usr/bin/pactl)
    if let Ok(vol_out) = Command::new("pactl")
        .env("LC_ALL", "C")
        .args([pactl_vol_cmd, pactl_target])
        .stderr(Stdio::null())
        .output()
    {
        if vol_out.status.success() {
            let text = String::from_utf8_lossy(&vol_out.stdout);
            // Format: "Volume: front-left: 42598 /  65% / -11.22 dB,   front-right: 42598 /  65% / -11.22 dB"
            let mut parsed_pct = None;
            for token in text.split_whitespace() {
                if let Some(num_str) = token.strip_suffix('%') {
                    if let Ok(pct) = num_str.trim().parse::<f32>() {
                        parsed_pct = Some(pct.clamp(0.0, 150.0));
                        break;
                    }
                }
            }

            let mut muted = false;
            if let Ok(mute_out) = Command::new("pactl")
                .env("LC_ALL", "C")
                .args([pactl_mute_cmd, pactl_target])
                .stderr(Stdio::null())
                .output()
            {
                let mtext = String::from_utf8_lossy(&mute_out.stdout).to_lowercase();
                muted = mtext.contains("yes") || mtext.contains("sim");
            }

            if let Some(pct) = parsed_pct {
                return Some((pct, muted));
            }
        }
    }

    // 2. Fallback to `wpctl` if available
    if let Ok(out) = Command::new("wpctl")
        .args(["get-volume", wpctl_target])
        .stderr(Stdio::null())
        .output()
    {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            let muted = text.contains("MUTED");
            for part in text.split_whitespace() {
                if let Ok(v) = part.parse::<f32>() {
                    return Some(((v * 100.0).clamp(0.0, 150.0), muted));
                }
            }
        }
    }

    None
}

fn query_real_brightness() -> Option<(String, f32, u32)> {
    let entries = fs::read_dir("/sys/class/backlight").ok()?;
    let mut best: Option<(u8, String, f32, u32)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let dev_name = entry.file_name().to_string_lossy().into_owned();
        let Ok(cur_str) = fs::read_to_string(path.join("actual_brightness"))
            .or_else(|_| fs::read_to_string(path.join("brightness")))
        else {
            continue;
        };
        let Ok(max_str) = fs::read_to_string(path.join("max_brightness")) else {
            continue;
        };
        let (Ok(cur), Ok(max)) = (cur_str.trim().parse::<f32>(), max_str.trim().parse::<u32>()) else {
            continue;
        };
        if max == 0 {
            continue;
        }
        let btype = fs::read_to_string(path.join("type")).unwrap_or_default();
        let priority = match btype.trim() {
            "raw" => 3,
            "platform" => 2,
            "firmware" => 1,
            _ => 0,
        };
        let pct = ((cur / (max as f32)) * 100.0).clamp(1.0, 100.0);
        if best.as_ref().is_none_or(|(p, _, _, _)| priority > *p) {
            best = Some((priority, dev_name, pct, max));
        }
    }
    best.map(|(_, dev, pct, max)| (dev, pct, max))
}

fn query_real_rfkill() -> (Option<bool>, Option<bool>) {
    let Ok(entries) = fs::read_dir("/sys/class/rfkill") else {
        return (None, None);
    };

    let mut wifi_on = None;
    let mut bt_on = None;

    for entry in entries.flatten() {
        let path = entry.path();
        let rtype = fs::read_to_string(path.join("type")).unwrap_or_default();
        let rtype = rtype.trim();
        let soft = fs::read_to_string(path.join("soft"))
            .ok()
            .and_then(|s| s.trim().parse::<u8>().ok())
            .unwrap_or(0);
        let hard = fs::read_to_string(path.join("hard"))
            .ok()
            .and_then(|s| s.trim().parse::<u8>().ok())
            .unwrap_or(0);
        let enabled = soft == 0 && hard == 0;

        match rtype {
            "wlan" => {
                // If any wlan rfkill device is unblocked, mark Wi-Fi enabled
                wifi_on = Some(wifi_on.unwrap_or(false) || enabled);
            }
            "bluetooth" => {
                bt_on = Some(bt_on.unwrap_or(false) || enabled);
            }
            _ => {}
        }
    }

    (wifi_on, bt_on)
}

fn query_wifi_ssid() -> Option<String> {
    let out = Command::new("nmcli")
        .env("LC_ALL", "C")
        .args(["-t", "-f", "ACTIVE,SSID", "dev", "wifi"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(ssid) = line.strip_prefix("yes:").or_else(|| line.strip_prefix("sim:")) {
            let trimmed = ssid.replace("\\:", ":").trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

fn split_nmcli_terse(line: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                cur.push(next);
                chars.next();
            }
        } else if c == ':' {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    parts.push(cur);
    parts
}

fn query_saved_wifi_connections() -> HashSet<String> {
    let mut saved = HashSet::new();
    let Ok(out) = Command::new("nmcli")
        .args(["-t", "-f", "NAME,TYPE", "connection", "show"])
        .stderr(Stdio::null())
        .output()
    else {
        return saved;
    };
    if !out.status.success() {
        return saved;
    }
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let cols = split_nmcli_terse(line);
        if cols.len() >= 2 && cols[1].contains("wireless") {
            let name = cols[0].trim();
            if !name.is_empty() {
                saved.insert(name.to_string());
            }
        }
    }
    saved
}

fn query_wifi_networks(rescan: bool) -> Vec<WifiNetwork> {
    let saved_set = query_saved_wifi_connections();
    let rescan_arg = if rescan { "yes" } else { "no" };
    let Ok(out) = Command::new("nmcli")
        .env("LC_ALL", "C")
        .args([
            "-t",
            "-f",
            "ACTIVE,SIGNAL,SECURITY,SSID",
            "dev",
            "wifi",
            "list",
            "--rescan",
            rescan_arg,
        ])
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }

    let mut by_ssid: HashMap<String, WifiNetwork> = HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let cols = split_nmcli_terse(line);
        if cols.len() < 4 {
            continue;
        }
        let active = cols[0].eq_ignore_ascii_case("yes") || cols[0].eq_ignore_ascii_case("sim");
        let signal = cols[1].trim().parse::<u8>().unwrap_or(0);
        let security = cols[2].trim().to_string();
        let ssid = cols[3..].join(":").trim().to_string();
        if ssid.is_empty() {
            continue;
        }
        let saved = active || saved_set.contains(&ssid);
        let entry = WifiNetwork {
            ssid: ssid.clone(),
            signal,
            security,
            active,
            saved,
        };
        match by_ssid.get_mut(&ssid) {
            Some(existing) => {
                if entry.active && !existing.active {
                    *existing = entry;
                } else if entry.active == existing.active && entry.signal > existing.signal {
                    *existing = entry;
                }
            }
            None => {
                by_ssid.insert(ssid, entry);
            }
        }
    }

    let mut list: Vec<WifiNetwork> = by_ssid.into_values().collect();
    list.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then_with(|| b.saved.cmp(&a.saved))
            .then_with(|| b.signal.cmp(&a.signal))
            .then_with(|| a.ssid.to_lowercase().cmp(&b.ssid.to_lowercase()))
    });
    list
}

fn query_bluetooth_devices(rescan: bool) -> (Vec<BluetoothDevice>, Option<String>) {
    if rescan {
        let _ = Command::new("bluetoothctl")
            .args(["--timeout", "5", "scan", "on"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    let out = match Command::new("bluetoothctl")
        .args(["devices"])
        .stderr(Stdio::null())
        .output()
    {
        Ok(o) => o,
        Err(_) => {
            return (
                Vec::new(),
                Some(
                    "bluetoothctl não instalado (instale bluez-utils e ative bluetooth.service)."
                        .to_string(),
                ),
            );
        }
    };

    if !out.status.success() {
        return (
            Vec::new(),
            Some("Serviço Bluetooth (bluetoothd) inativo.".to_string()),
        );
    }

    let mut list = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        // Format: "Device AA:BB:CC:DD:EE:FF Device Name"
        let rest = match line.trim().strip_prefix("Device ") {
            Some(r) => r,
            None => continue,
        };
        let mut parts = rest.splitn(2, ' ');
        let mac = parts.next().unwrap_or("").trim().to_string();
        if mac.is_empty() {
            continue;
        }
        let fallback_name = parts.next().unwrap_or(&mac).trim().to_string();
        let (name, connected, paired, trusted) = query_bluetooth_device_info(&mac, fallback_name);
        list.push(BluetoothDevice {
            mac,
            name,
            connected,
            paired,
            trusted,
        });
    }

    list.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| b.paired.cmp(&a.paired))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    (list, None)
}

fn query_bluetooth_device_info(mac: &str, fallback_name: String) -> (String, bool, bool, bool) {
    let Ok(out) = Command::new("bluetoothctl")
        .args(["info", mac])
        .stderr(Stdio::null())
        .output()
    else {
        return (fallback_name, false, false, false);
    };
    if !out.status.success() {
        return (fallback_name, false, false, false);
    }

    let mut name = fallback_name;
    let mut connected = false;
    let mut paired = false;
    let mut trusted = false;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("Alias:") {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                name = trimmed.to_string();
            }
        } else if let Some(v) = t.strip_prefix("Connected:") {
            connected = v.trim().eq_ignore_ascii_case("yes");
        } else if let Some(v) = t.strip_prefix("Paired:") {
            paired = v.trim().eq_ignore_ascii_case("yes");
        } else if let Some(v) = t.strip_prefix("Trusted:") {
            trusted = v.trim().eq_ignore_ascii_case("yes");
        }
    }
    (name, connected, paired, trusted)
}

fn query_power_profile() -> Option<(bool, String)> {
    // Check CPU scaling governor or energy_performance_preference from sysfs first (fast & zero-overhead)
    if let Ok(epp) = fs::read_to_string(
        "/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference",
    ) {
        let profile = epp.trim().to_string();
        let saver = profile.contains("power");
        return Some((saver, profile));
    }
    if let Ok(gov) = fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor") {
        let profile = gov.trim().to_string();
        let saver = profile.contains("powersave");
        return Some((saver, profile));
    }
    None
}
