use std::fs::{self, File};
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;
use sysinfo::{Components, Networks};

#[derive(Debug, Clone, Default)]
pub struct BatteryDetails {
    pub status: String,
    pub power_w: Option<f32>,
    pub voltage_v: Option<f32>,
    pub energy_now_wh: Option<f32>,
    pub energy_full_wh: Option<f32>,
    pub energy_design_wh: Option<f32>,
    pub health_percent: Option<f32>,
    pub cycle_count: Option<u32>,
    pub time_remaining_mins: Option<u32>,
    pub ac_online: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct SystemSnapshot {
    pub cpu_usage: f32,
    pub ram_used_gb: f32,
    pub ram_total_gb: f32,
    pub ram_percent: f32,
    pub cpu_temp_c: f32,
    pub battery_percent: Option<u8>,
    pub battery_charging: bool,
    pub battery_details: BatteryDetails,
    pub network_connected: bool,
    pub network_name: String,
    pub rx_kbps: f32,
    pub tx_kbps: f32,
}

impl Default for SystemSnapshot {
    fn default() -> Self {
        Self {
            cpu_usage: 0.0,
            ram_used_gb: 0.0,
            ram_total_gb: 16.0,
            ram_percent: 0.0,
            cpu_temp_c: 42.0,
            battery_percent: None,
            battery_charging: false,
            battery_details: BatteryDetails::default(),
            network_connected: true,
            network_name: "Wi-Fi".to_string(),
            rx_kbps: 0.0,
            tx_kbps: 0.0,
        }
    }
}

pub struct SystemMonitor {
    pub snapshot: Arc<Mutex<SystemSnapshot>>,
    detailed_active: Arc<AtomicBool>,
    cpu_ram_active: Arc<AtomicBool>,
}

impl SystemMonitor {
    pub fn new(wake_fn: impl Fn() + Send + Sync + 'static) -> Self {
        let snapshot = Arc::new(Mutex::new(SystemSnapshot::default()));
        let detailed_active = Arc::new(AtomicBool::new(false));
        let cpu_ram_active = Arc::new(AtomicBool::new(true));
        let snap_clone = snapshot.clone();
        let detailed_clone = detailed_active.clone();
        let cpu_ram_clone = cpu_ram_active.clone();
        let wake = Arc::new(wake_fn);

        thread::spawn(move || {
            let mut networks: Option<Networks> = None;
            let mut components: Option<Components> = None;
            let mut prev_cpu: Option<(u64, u64)> = None;
            let mut proc_buf = String::with_capacity(1024);

            loop {
                let detailed = detailed_clone.load(Ordering::Relaxed);
                let collect_cpu_ram = detailed || cpu_ram_clone.load(Ordering::Relaxed);
                let mut next = SystemSnapshot::default();
                if collect_cpu_ram {
                    if let Some(usage) = read_proc_cpu_usage(&mut prev_cpu, &mut proc_buf) {
                        next.cpu_usage = usage;
                    }
                    if let Some((used_gb, total_gb, pct)) = read_proc_meminfo(&mut proc_buf) {
                        next.ram_used_gb = used_gb;
                        next.ram_total_gb = total_gb;
                        next.ram_percent = pct;
                    }
                }
                if detailed {
                    if networks.is_none() {
                        networks = Networks::new_with_refreshed_list().ok();
                    } else if let Some(ref mut n) = networks {
                        let _ = n.refresh(true);
                    }
                    if components.is_none() {
                        components = Components::new_with_refreshed_list().ok();
                    } else if let Some(ref mut c) = components {
                        c.refresh(true);
                    }
                }

                if let Some(ref c) = components {
                    let mut max_temp = 0.0f32;
                    for comp in c {
                        if let Some(t) = comp.temperature() {
                            if t > max_temp && t < 130.0 {
                                max_temp = t;
                            }
                        }
                    }
                    if max_temp > 0.0 {
                        next.cpu_temp_c = max_temp;
                    }
                }

                if let Some(ref n) = networks {
                    let mut total_rx = 0u64;
                    let mut total_tx = 0u64;
                    let mut active_iface = String::new();
                    for (name, data) in n {
                        let name_str = name.to_string_lossy();
                        if name_str == "lo" {
                            continue;
                        }
                        total_rx += data.received();
                        total_tx += data.transmitted();
                        if active_iface.is_empty() {
                            active_iface = name_str.into_owned();
                        }
                    }
                    next.rx_kbps = (total_rx as f32) / 1024.0 / 2.0;
                    next.tx_kbps = (total_tx as f32) / 1024.0 / 2.0;
                    if !active_iface.is_empty() {
                        next.network_name = active_iface;
                        next.network_connected = true;
                    }
                }

                let (bat_pct, bat_chg, bat_det) = read_battery_sysfs();
                next.battery_percent = bat_pct;
                next.battery_charging = bat_chg;
                next.battery_details = bat_det;

                let mut changed = false;
                if let Ok(mut guard) = snap_clone.lock() {
                    changed = (guard.cpu_usage - next.cpu_usage).abs() >= 1.0
                        || (guard.ram_percent - next.ram_percent).abs() >= 0.5
                        || guard.battery_percent != next.battery_percent
                        || guard.battery_charging != next.battery_charging
                        || guard.battery_details.status != next.battery_details.status
                        || guard.network_connected != next.network_connected
                        || guard.network_name != next.network_name;
                    *guard = next;
                }
                if changed {
                    wake();
                }
                let sleep_ms = if collect_cpu_ram { 2000 } else { 10000 };
                thread::sleep(Duration::from_millis(sleep_ms));
            }
        });

        Self {
            snapshot,
            detailed_active,
            cpu_ram_active,
        }
    }

    pub fn set_detailed_active(&self, active: bool) {
        self.detailed_active.store(active, Ordering::Relaxed);
    }

    pub fn set_cpu_ram_active(&self, active: bool) {
        self.cpu_ram_active.store(active, Ordering::Relaxed);
    }
}

fn read_proc_cpu_usage(prev: &mut Option<(u64, u64)>, buf: &mut String) -> Option<f32> {
    buf.clear();
    File::open("/proc/stat").ok()?.read_to_string(buf).ok()?;
    let line = buf.lines().next()?;
    let mut parts = line.split_whitespace();
    if parts.next()? != "cpu" {
        return None;
    }
    let mut vals = [0u64; 10];
    let mut count = 0;
    for p in parts.take(10) {
        vals[count] = p.parse::<u64>().ok()?;
        count += 1;
    }
    if count < 4 {
        return None;
    }
    let idle = vals[3].saturating_add(vals[4]);
    let total: u64 = vals[..count].iter().sum();
    let usage = if let Some((prev_idle, prev_total)) = *prev {
        let d_total = total.saturating_sub(prev_total);
        let d_idle = idle.saturating_sub(prev_idle);
        if d_total > 0 {
            ((d_total.saturating_sub(d_idle)) as f32 / d_total as f32) * 100.0
        } else {
            0.0
        }
    } else {
        0.0
    };
    *prev = Some((idle, total));
    Some(usage.clamp(0.0, 100.0))
}

fn read_proc_meminfo(buf: &mut String) -> Option<(f32, f32, f32)> {
    buf.clear();
    File::open("/proc/meminfo").ok()?.read_to_string(buf).ok()?;
    let mut total_kb: Option<u64> = None;
    let mut avail_kb: Option<u64> = None;
    for line in buf.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = rest.split_whitespace().next().and_then(|s| s.parse().ok());
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail_kb = rest.split_whitespace().next().and_then(|s| s.parse().ok());
        }
        if total_kb.is_some() && avail_kb.is_some() {
            break;
        }
    }
    let total = total_kb?.max(1) as f64;
    let avail = avail_kb.unwrap_or(0) as f64;
    let used = (total - avail).max(0.0);
    let total_gb = (total / 1_048_576.0) as f32;
    let used_gb = (used / 1_048_576.0) as f32;
    let pct = ((used / total) * 100.0) as f32;
    Some((used_gb, total_gb, pct))
}

fn find_battery_sysfs_dir() -> Option<PathBuf> {
    let entries = fs::read_dir("/sys/class/power_supply").ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name.starts_with("BAT") {
            return Some(path);
        }
    }
    None
}

fn read_sysfs_u64(path: &std::path::Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse::<u64>().ok()
}

fn find_ac_online() -> Option<bool> {
    let entries = fs::read_dir("/sys/class/power_supply").ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        let ty = fs::read_to_string(p.join("type")).unwrap_or_default();
        if ty.trim().eq_ignore_ascii_case("Mains") {
            if let Some(v) = read_sysfs_u64(&p.join("online")) {
                return Some(v == 1);
            }
        }
    }
    None
}

fn read_battery_sysfs() -> (Option<u8>, bool, BatteryDetails) {
    static BAT_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    let Some(path) = BAT_DIR.get_or_init(find_battery_sysfs_dir).as_ref() else {
        return (None, false, BatteryDetails::default());
    };

    let cap = read_sysfs_u64(&path.join("capacity")).map(|v| v.min(100) as u8);
    let raw_status = fs::read_to_string(path.join("status"))
        .unwrap_or_default()
        .trim()
        .to_string();
    let charging = raw_status.eq_ignore_ascii_case("Charging")
        || raw_status.eq_ignore_ascii_case("Full");

    let voltage_uv = read_sysfs_u64(&path.join("voltage_now"));
    let voltage_v = voltage_uv.map(|v| v as f32 / 1_000_000.0);

    let power_uw = read_sysfs_u64(&path.join("power_now")).or_else(|| {
        let curr_ua = read_sysfs_u64(&path.join("current_now"))?;
        let volt_uv = voltage_uv?;
        Some((curr_ua as u128 * volt_uv as u128 / 1_000_000) as u64)
    });
    let power_w = power_uw.map(|p| p as f32 / 1_000_000.0);

    let energy_now_wh = read_sysfs_u64(&path.join("energy_now")).map(|e| e as f32 / 1_000_000.0);
    let energy_full_wh = read_sysfs_u64(&path.join("energy_full")).map(|e| e as f32 / 1_000_000.0);
    let energy_design_wh =
        read_sysfs_u64(&path.join("energy_full_design")).map(|e| e as f32 / 1_000_000.0);

    let health_percent = match (energy_full_wh, energy_design_wh) {
        (Some(full), Some(design)) if design > 0.0 => {
            Some(((full / design) * 100.0).clamp(0.0, 100.0))
        }
        _ => None,
    };

    let cycle_count = read_sysfs_u64(&path.join("cycle_count")).map(|c| c as u32);

    let time_remaining_mins = match (power_w, energy_now_wh, energy_full_wh) {
        (Some(pw), Some(now_wh), Some(full_wh)) if pw > 0.2 => {
            if raw_status.eq_ignore_ascii_case("Discharging") {
                Some(((now_wh / pw) * 60.0).round() as u32)
            } else if raw_status.eq_ignore_ascii_case("Charging") && full_wh > now_wh {
                Some((((full_wh - now_wh) / pw) * 60.0).round() as u32)
            } else {
                None
            }
        }
        _ => None,
    };

    let details = BatteryDetails {
        status: raw_status,
        power_w,
        voltage_v,
        energy_now_wh,
        energy_full_wh,
        energy_design_wh,
        health_percent,
        cycle_count,
        time_remaining_mins,
        ac_online: find_ac_online(),
    };

    (cap, charging, details)
}
