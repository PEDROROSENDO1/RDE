use std::collections::{HashMap, VecDeque};
use std::ffi::CString;
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    pub uid: u32,
    pub user: String,
    pub name: String,
    pub cmdline: String,
    pub state: char,
    pub cpu_pct: f32,
    pub rss_bytes: u64,
    pub io_read_bps: u64,
    pub io_write_bps: u64,
    pub threads: u32,
    pub nice: i32,
    pub is_kernel_thread: bool,
    pub app_group: String,
    pub cgroup_unit: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AppGroup {
    pub name: String,
    pub total_rss_bytes: u64,
    pub total_cpu_pct: f32,
    pub total_io_bps: u64,
    pub processes: Vec<ProcessInfo>,
}

#[derive(Debug, Clone, Default)]
pub struct MemoryStats {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub available_bytes: u64,
    pub real_used_bytes: u64,
    pub buffers_bytes: u64,
    pub cached_bytes: u64,
    pub sreclaimable_bytes: u64,
    pub shmem_bytes: u64,
    pub reclaimable_cache_bytes: u64,
    pub active_bytes: u64,
    pub inactive_bytes: u64,
    pub dirty_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_free_bytes: u64,
    pub swap_used_bytes: u64,
    pub swap_cached_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct SwapDevice {
    pub path: String,
    pub kind: String,
    pub size_bytes: u64,
    pub used_bytes: u64,
    pub priority: i32,
}

#[derive(Debug, Clone)]
pub struct ZramDevice {
    pub name: String,
    pub algorithm: String,
    pub disksize_bytes: u64,
    pub orig_data_bytes: u64,
    pub compr_data_bytes: u64,
    pub mem_used_bytes: u64,
    pub saved_bytes: u64,
    pub ratio: f32,
}

#[derive(Debug, Clone, Default)]
pub struct CpuStats {
    pub model: String,
    pub physical_cores: usize,
    pub logical_cores: usize,
    pub total_pct: f32,
    pub per_core_pct: Vec<f32>,
    pub avg_freq_mhz: f32,
    pub max_freq_mhz: f32,
    pub temp_c: Option<f32>,
    pub load_1: f32,
    pub load_5: f32,
    pub load_15: f32,
    pub uptime_secs: u64,
    pub total_threads: usize,
}

#[derive(Debug, Clone)]
pub struct GpuStats {
    pub name: String,
    pub driver: String,
    pub busy_pct: Option<f32>,
    pub vram_used_bytes: Option<u64>,
    pub vram_total_bytes: Option<u64>,
    pub gtt_used_bytes: Option<u64>,
    pub temp_c: Option<f32>,
    pub power_w: Option<f32>,
    pub freq_mhz: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct MountInfo {
    pub device: String,
    pub mount_point: String,
    pub fs_type: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub avail_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct DiskIoInfo {
    pub name: String,
    pub read_bps: u64,
    pub write_bps: u64,
    pub iops: u64,
}

#[derive(Debug, Clone)]
pub struct NetIfInfo {
    pub name: String,
    pub rx_bps: u64,
    pub tx_bps: u64,
    pub total_rx_bytes: u64,
    pub total_tx_bytes: u64,
}

#[derive(Debug, Clone, Default)]
pub struct SystemTelemetry {
    pub cpu: CpuStats,
    pub mem: MemoryStats,
    pub swap_devices: Vec<SwapDevice>,
    pub zram_devices: Vec<ZramDevice>,
    pub gpus: Vec<GpuStats>,
    pub mounts: Vec<MountInfo>,
    pub disks: Vec<DiskIoInfo>,
    pub networks: Vec<NetIfInfo>,
    pub processes: Vec<ProcessInfo>,
    pub app_groups: Vec<AppGroup>,
    pub cpu_history: VecDeque<f32>,
    pub ram_history: VecDeque<f32>,
    pub gpu_history: VecDeque<f32>,
    pub disk_read_history: VecDeque<f32>,
    pub disk_write_history: VecDeque<f32>,
    pub net_rx_history: VecDeque<f32>,
    pub net_tx_history: VecDeque<f32>,
}

pub struct TelemetryCollector {
    pub data: Arc<Mutex<SystemTelemetry>>,
    refresh_ms: Arc<AtomicU64>,
    history_cap: Arc<AtomicU64>,
    paused: Arc<AtomicBool>,
}

impl TelemetryCollector {
    pub fn start(
        refresh_ms: u64,
        history_cap: usize,
        wake_fn: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let data = Arc::new(Mutex::new(SystemTelemetry::default()));
        let refresh_atomic = Arc::new(AtomicU64::new(refresh_ms.max(250)));
        let hist_atomic = Arc::new(AtomicU64::new(history_cap.max(20) as u64));
        let paused = Arc::new(AtomicBool::new(false));

        let data_c = data.clone();
        let refresh_c = refresh_atomic.clone();
        let hist_c = hist_atomic.clone();
        let paused_c = paused.clone();
        let wake = Arc::new(wake_fn);

        thread::Builder::new()
            .name("rocketde-tasks-telemetry".to_string())
            .spawn(move || {
                let uid_map = load_passwd_users();
                let (cpu_model, phys_cores, logical_cores) = read_cpu_static_info();
                let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as u64;
                let ticks_per_sec = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;

                let mut prev_cpu_total: Option<(u64, u64)> = None;
                let mut prev_cpu_cores: Vec<(u64, u64)> = Vec::new();
                let mut prev_total_jiffies: u64 = 0;
                let mut prev_proc_jiffies: HashMap<u32, u64> = HashMap::new();
                let mut prev_proc_io: HashMap<u32, (u64, u64)> = HashMap::new();
                let mut prev_disks: HashMap<String, (u64, u64, u64)> = HashMap::new();
                let mut prev_nets: HashMap<String, (u64, u64)> = HashMap::new();
                let mut last_sample = Instant::now();
                let mut first_run = true;

                loop {
                    let is_paused = paused_c.load(Ordering::Relaxed);
                    if !is_paused || first_run {
                        first_run = false;
                        let now = Instant::now();
                        let dt = now.duration_since(last_sample).as_secs_f64().max(0.1);
                        last_sample = now;

                        let (cpu_pct, per_core, total_jiffies, total_jiffies_delta) =
                            read_proc_stat(&mut prev_cpu_total, &mut prev_cpu_cores, prev_total_jiffies);
                        prev_total_jiffies = total_jiffies;

                        let (avg_freq, max_freq) = read_cpu_freqs();
                        let (load_1, load_5, load_15) = read_loadavg();
                        let uptime_secs = read_uptime();
                        let temp_c = read_cpu_temp();
                        let mem = read_meminfo();
                        let swap_devices = read_swaps();
                        let zram_devices = read_zram();
                        let gpus = read_gpus();
                        let mounts = read_mounts();
                        let disks = read_diskstats(&mut prev_disks, dt);
                        let networks = read_net_dev(&mut prev_nets, dt);

                        let processes = read_processes(
                            &uid_map,
                            page_size,
                            ticks_per_sec,
                            total_jiffies_delta,
                            logical_cores.max(1),
                            dt,
                            &mut prev_proc_jiffies,
                            &mut prev_proc_io,
                        );
                        let total_threads = processes.iter().map(|p| p.threads as usize).sum();
                        let app_groups = build_app_groups(&processes);

                        let cap = hist_c.load(Ordering::Relaxed) as usize;
                        if let Ok(mut guard) = data_c.lock() {
                            guard.cpu = CpuStats {
                                model: cpu_model.clone(),
                                physical_cores: phys_cores,
                                logical_cores,
                                total_pct: cpu_pct,
                                per_core_pct: per_core,
                                avg_freq_mhz: avg_freq,
                                max_freq_mhz: max_freq,
                                temp_c,
                                load_1,
                                load_5,
                                load_15,
                                uptime_secs,
                                total_threads,
                            };
                            guard.mem = mem;
                            guard.swap_devices = swap_devices;
                            guard.zram_devices = zram_devices;
                            guard.gpus = gpus;
                            guard.mounts = mounts;

                            let total_read_bps: u64 = disks.iter().map(|d| d.read_bps).sum();
                            let total_write_bps: u64 = disks.iter().map(|d| d.write_bps).sum();
                            let total_rx_bps: u64 = networks.iter().map(|n| n.rx_bps).sum();
                            let total_tx_bps: u64 = networks.iter().map(|n| n.tx_bps).sum();

                            guard.disks = disks;
                            guard.networks = networks;
                            guard.processes = processes;
                            guard.app_groups = app_groups;

                            push_hist(&mut guard.cpu_history, cpu_pct, cap);
                            let ram_pct = if guard.mem.total_bytes > 0 {
                                (guard.mem.real_used_bytes as f64 / guard.mem.total_bytes as f64
                                    * 100.0) as f32
                            } else {
                                0.0
                            };
                            push_hist(&mut guard.ram_history, ram_pct, cap);

                            let gpu_pct = guard
                                .gpus
                                .first()
                                .and_then(|g| g.busy_pct)
                                .unwrap_or(0.0);
                            push_hist(&mut guard.gpu_history, gpu_pct, cap);
                            push_hist(
                                &mut guard.disk_read_history,
                                total_read_bps as f32 / (1024.0 * 1024.0),
                                cap,
                            );
                            push_hist(
                                &mut guard.disk_write_history,
                                total_write_bps as f32 / (1024.0 * 1024.0),
                                cap,
                            );
                            push_hist(
                                &mut guard.net_rx_history,
                                total_rx_bps as f32 / 1024.0,
                                cap,
                            );
                            push_hist(
                                &mut guard.net_tx_history,
                                total_tx_bps as f32 / 1024.0,
                                cap,
                            );
                        }
                        wake();
                    }

                    let sleep_ms = refresh_c.load(Ordering::Relaxed).clamp(250, 10_000);
                    thread::sleep(Duration::from_millis(sleep_ms));
                }
            })
            .ok();

        Self {
            data,
            refresh_ms: refresh_atomic,
            history_cap: hist_atomic,
            paused,
        }
    }

    pub fn set_refresh_ms(&self, ms: u64) {
        self.refresh_ms.store(ms.clamp(250, 10_000), Ordering::Relaxed);
    }

    pub fn set_history_cap(&self, cap: usize) {
        self.history_cap.store(cap.clamp(20, 300) as u64, Ordering::Relaxed);
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }
}

fn push_hist(q: &mut VecDeque<f32>, val: f32, cap: usize) {
    while q.len() >= cap {
        q.pop_front();
    }
    q.push_back(val);
}

fn load_passwd_users() -> HashMap<u32, String> {
    let mut map = HashMap::new();
    if let Ok(content) = fs::read_to_string("/etc/passwd") {
        for line in content.lines() {
            let mut parts = line.split(':');
            if let (Some(name), Some(_), Some(uid_s)) = (parts.next(), parts.next(), parts.next()) {
                if let Ok(uid) = uid_s.parse::<u32>() {
                    map.insert(uid, name.to_string());
                }
            }
        }
    }
    map
}

fn read_cpu_static_info() -> (String, usize, usize) {
    let mut model = String::from("CPU Linux");
    let mut logical = 0usize;
    let mut cores = 0usize;
    if let Ok(content) = fs::read_to_string("/proc/cpuinfo") {
        for line in content.lines() {
            if let Some((k, v)) = line.split_once(':') {
                let k = k.trim();
                let v = v.trim();
                if k == "model name" && model == "CPU Linux" {
                    model = v.to_string();
                } else if k == "processor" {
                    logical += 1;
                } else if k == "cpu cores" && cores == 0 {
                    cores = v.parse().unwrap_or(0);
                }
            }
        }
    }
    if logical == 0 {
        logical = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
    }
    if cores == 0 {
        cores = logical;
    }
    (model, cores, logical)
}

fn parse_cpu_line(line: &str) -> Option<(u64, u64)> {
    let mut parts = line.split_whitespace();
    let _label = parts.next()?;
    let nums: Vec<u64> = parts.filter_map(|s| s.parse::<u64>().ok()).collect();
    if nums.len() < 4 {
        return None;
    }
    let idle = nums[3] + nums.get(4).copied().unwrap_or(0);
    let total: u64 = nums.iter().sum();
    Some((total.saturating_sub(idle), total))
}

fn read_proc_stat(
    prev_total: &mut Option<(u64, u64)>,
    prev_cores: &mut Vec<(u64, u64)>,
    prev_total_jiffies: u64,
) -> (f32, Vec<f32>, u64, u64) {
    let Ok(content) = fs::read_to_string("/proc/stat") else {
        return (0.0, Vec::new(), prev_total_jiffies, 0);
    };
    let mut total_pct = 0.0f32;
    let mut cur_total_jiffies = prev_total_jiffies;
    let mut core_pcts = Vec::new();
    let mut core_idx = 0usize;

    for line in content.lines() {
        if line.starts_with("cpu ") {
            if let Some((active, total)) = parse_cpu_line(line) {
                cur_total_jiffies = total;
                if let Some((pa, pt)) = *prev_total {
                    let dt = total.saturating_sub(pt);
                    let da = active.saturating_sub(pa);
                    if dt > 0 {
                        total_pct = ((da as f64 / dt as f64) * 100.0).clamp(0.0, 100.0) as f32;
                    }
                }
                *prev_total = Some((active, total));
            }
        } else if line.starts_with("cpu") {
            if let Some((active, total)) = parse_cpu_line(line) {
                if core_idx >= prev_cores.len() {
                    prev_cores.push((active, total));
                    core_pcts.push(0.0);
                } else {
                    let (pa, pt) = prev_cores[core_idx];
                    let dt = total.saturating_sub(pt);
                    let da = active.saturating_sub(pa);
                    let pct = if dt > 0 {
                        ((da as f64 / dt as f64) * 100.0).clamp(0.0, 100.0) as f32
                    } else {
                        0.0
                    };
                    prev_cores[core_idx] = (active, total);
                    core_pcts.push(pct);
                }
                core_idx += 1;
            }
        }
    }
    let delta_jiffies = cur_total_jiffies.saturating_sub(prev_total_jiffies);
    (total_pct, core_pcts, cur_total_jiffies, delta_jiffies)
}

fn read_cpu_freqs() -> (f32, f32) {
    let mut sum = 0.0f64;
    let mut max_f = 0.0f32;
    let mut count = 0usize;
    if let Ok(entries) = fs::read_dir("/sys/devices/system/cpu") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s.starts_with("cpu") && s[3..].chars().all(|c| c.is_ascii_digit()) {
                let p = entry.path().join("cpufreq/scaling_cur_freq");
                if let Ok(raw) = fs::read_to_string(p) {
                    if let Ok(khz) = raw.trim().parse::<f32>() {
                        let mhz = khz / 1000.0;
                        sum += mhz as f64;
                        if mhz > max_f {
                            max_f = mhz;
                        }
                        count += 1;
                    }
                }
            }
        }
    }
    if count > 0 {
        ((sum / count as f64) as f32, max_f)
    } else {
        (0.0, 0.0)
    }
}

fn read_loadavg() -> (f32, f32, f32) {
    if let Ok(raw) = fs::read_to_string("/proc/loadavg") {
        let mut p = raw.split_whitespace();
        let l1 = p.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let l5 = p.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let l15 = p.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        return (l1, l5, l15);
    }
    (0.0, 0.0, 0.0)
}

fn read_uptime() -> u64 {
    if let Ok(raw) = fs::read_to_string("/proc/uptime") {
        if let Some(first) = raw.split_whitespace().next() {
            if let Ok(secs) = first.parse::<f64>() {
                return secs as u64;
            }
        }
    }
    0
}

fn read_cpu_temp() -> Option<f32> {
    let mut best: Option<f32> = None;
    if let Ok(entries) = fs::read_dir("/sys/class/hwmon") {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = fs::read_to_string(path.join("name"))
                .unwrap_or_default()
                .trim()
                .to_lowercase();
            let is_cpu = name.contains("k10temp")
                || name.contains("coretemp")
                || name.contains("zenpower")
                || name.contains("cpu")
                || name.contains("acpitz");
            if is_cpu {
                if let Ok(raw) = fs::read_to_string(path.join("temp1_input")) {
                    if let Ok(mc) = raw.trim().parse::<f32>() {
                        let c = mc / 1000.0;
                        if c > 0.0 && c < 130.0 {
                            best = Some(best.map_or(c, |b| b.max(c)));
                        }
                    }
                }
            }
        }
    }
    if best.is_none() {
        if let Ok(entries) = fs::read_dir("/sys/class/thermal") {
            for entry in entries.flatten() {
                let p = entry.path().join("temp");
                if let Ok(raw) = fs::read_to_string(p) {
                    if let Ok(mc) = raw.trim().parse::<f32>() {
                        let c = mc / 1000.0;
                        if c > 0.0 && c < 130.0 {
                            best = Some(best.map_or(c, |b| b.max(c)));
                        }
                    }
                }
            }
        }
    }
    best
}

fn read_meminfo() -> MemoryStats {
    let mut stats = MemoryStats::default();
    let Ok(content) = fs::read_to_string("/proc/meminfo") else {
        return stats;
    };
    for line in content.lines() {
        let Some((k, rest)) = line.split_once(':') else {
            continue;
        };
        let kb: u64 = rest
            .split_whitespace()
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let bytes = kb * 1024;
        match k {
            "MemTotal" => stats.total_bytes = bytes,
            "MemFree" => stats.free_bytes = bytes,
            "MemAvailable" => stats.available_bytes = bytes,
            "Buffers" => stats.buffers_bytes = bytes,
            "Cached" => stats.cached_bytes = bytes,
            "SReclaimable" => stats.sreclaimable_bytes = bytes,
            "Shmem" => stats.shmem_bytes = bytes,
            "Active" => stats.active_bytes = bytes,
            "Inactive" => stats.inactive_bytes = bytes,
            "Dirty" => stats.dirty_bytes = bytes,
            "SwapTotal" => stats.swap_total_bytes = bytes,
            "SwapFree" => stats.swap_free_bytes = bytes,
            "SwapCached" => stats.swap_cached_bytes = bytes,
            _ => {}
        }
    }
    // Exact partition of MemTotal (matches `free` / `htop` without double-counting cache):
    // 1. Reclaimable cache = (Cached - Shmem) + Buffers + SReclaimable
    // 2. Real used (Apps + Kernel + Shmem) = MemTotal - MemFree - reclaimable_cache_bytes
    // 3. MemFree = immediately free RAM
    // Guarantee: real_used_bytes + reclaimable_cache_bytes + free_bytes == total_bytes.
    let page_cache = stats.cached_bytes.saturating_sub(stats.shmem_bytes);
    stats.reclaimable_cache_bytes = (page_cache + stats.buffers_bytes + stats.sreclaimable_bytes)
        .min(stats.total_bytes.saturating_sub(stats.free_bytes));
    stats.real_used_bytes = stats
        .total_bytes
        .saturating_sub(stats.free_bytes)
        .saturating_sub(stats.reclaimable_cache_bytes);
    stats.swap_used_bytes = stats.swap_total_bytes.saturating_sub(stats.swap_free_bytes);
    stats
}

fn read_swaps() -> Vec<SwapDevice> {
    let mut out = Vec::new();
    let Ok(content) = fs::read_to_string("/proc/swaps") else {
        return out;
    };
    for line in content.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 5 {
            out.push(SwapDevice {
                path: cols[0].to_string(),
                kind: cols[1].to_string(),
                size_bytes: cols[2].parse::<u64>().unwrap_or(0) * 1024,
                used_bytes: cols[3].parse::<u64>().unwrap_or(0) * 1024,
                priority: cols[4].parse::<i32>().unwrap_or(0),
            });
        }
    }
    out
}

fn read_zram() -> Vec<ZramDevice> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/block") else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("zram") {
            continue;
        }
        let path = entry.path();
        let disksize: u64 = fs::read_to_string(path.join("disksize"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        if disksize == 0 {
            continue;
        }
        let algo_raw = fs::read_to_string(path.join("comp_algorithm")).unwrap_or_default();
        let algorithm = algo_raw
            .split_whitespace()
            .find(|w| w.starts_with('[') && w.ends_with(']'))
            .map(|w| w.trim_matches(&['[', ']'][..]).to_string())
            .unwrap_or_else(|| algo_raw.trim().to_string());

        let mm_raw = fs::read_to_string(path.join("mm_stat")).unwrap_or_default();
        let nums: Vec<u64> = mm_raw
            .split_whitespace()
            .filter_map(|s| s.parse::<u64>().ok())
            .collect();
        let orig = nums.first().copied().unwrap_or(0);
        let compr = nums.get(1).copied().unwrap_or(0);
        let mem_used = nums.get(2).copied().unwrap_or(0);
        let saved = orig.saturating_sub(mem_used);
        let ratio = if mem_used > 0 {
            orig as f32 / mem_used as f32
        } else {
            1.0
        };

        out.push(ZramDevice {
            name,
            algorithm,
            disksize_bytes: disksize,
            orig_data_bytes: orig,
            compr_data_bytes: compr,
            mem_used_bytes: mem_used,
            saved_bytes: saved,
            ratio,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn read_gpus() -> Vec<GpuStats> {
    let mut gpus = Vec::new();
    if let Ok(entries) = fs::read_dir("/sys/class/drm") {
        let mut cards: Vec<_> = entries
            .flatten()
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.starts_with("card") && n[4..].chars().all(|c| c.is_ascii_digit())
            })
            .collect();
        cards.sort_by_key(|e| e.file_name());

        for card in cards {
            let dev = card.path().join("device");
            if !dev.exists() {
                continue;
            }
            let uevent = fs::read_to_string(dev.join("uevent")).unwrap_or_default();
            let mut driver = String::from("drm");
            let mut pci_id = String::new();
            for line in uevent.lines() {
                if let Some(v) = line.strip_prefix("DRIVER=") {
                    driver = v.to_string();
                } else if let Some(v) = line.strip_prefix("PCI_ID=") {
                    pci_id = v.to_string();
                }
            }

            let vendor_name = if driver.contains("amdgpu") || pci_id.starts_with("1002:") {
                "AMD Radeon"
            } else if driver.contains("i915") || driver.contains("xe") || pci_id.starts_with("8086:")
            {
                "Intel Graphics"
            } else if driver.contains("nvidia") || pci_id.starts_with("10DE:") {
                "NVIDIA GPU"
            } else {
                "GPU"
            };
            let name = if pci_id.is_empty() {
                format!("{vendor_name} ({driver})")
            } else {
                format!("{vendor_name} [{pci_id}]")
            };

            let mut busy_pct = fs::read_to_string(dev.join("gpu_busy_percent"))
                .ok()
                .and_then(|s| s.trim().parse::<f32>().ok());
            let mut vram_used_bytes = fs::read_to_string(dev.join("mem_info_vram_used"))
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok());
            let mut vram_total_bytes = fs::read_to_string(dev.join("mem_info_vram_total"))
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok());
            let gtt_used_bytes = fs::read_to_string(dev.join("mem_info_gtt_used"))
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok());

            let (mut temp_c, mut power_w, mut freq_mhz) = (None, None, None);
            if let Ok(hwmons) = fs::read_dir(dev.join("hwmon")) {
                for hw in hwmons.flatten() {
                    let hp = hw.path();
                    if temp_c.is_none() {
                        temp_c = fs::read_to_string(hp.join("temp1_input"))
                            .ok()
                            .and_then(|s| s.trim().parse::<f32>().ok())
                            .map(|v| v / 1000.0);
                    }
                    if power_w.is_none() {
                        power_w = fs::read_to_string(hp.join("power1_average"))
                            .ok()
                            .or_else(|| fs::read_to_string(hp.join("power1_input")).ok())
                            .and_then(|s| s.trim().parse::<f32>().ok())
                            .map(|uw| uw / 1_000_000.0);
                    }
                    if freq_mhz.is_none() {
                        freq_mhz = fs::read_to_string(hp.join("freq1_input"))
                            .ok()
                            .and_then(|s| s.trim().parse::<u64>().ok())
                            .map(|hz| (hz / 1_000_000) as u32);
                    }
                }
            }
            if freq_mhz.is_none() {
                freq_mhz = fs::read_to_string(card.path().join("gt_cur_freq_mhz"))
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok());
            }

            let mut final_name = name;
            if driver.contains("nvidia") && busy_pct.is_none() {
                if let Some(nv) = query_nvidia_smi() {
                    if !nv.0.is_empty() {
                        final_name = nv.0;
                    }
                    busy_pct = nv.1;
                    vram_used_bytes = nv.2;
                    vram_total_bytes = nv.3;
                    temp_c = temp_c.or(nv.4);
                    power_w = power_w.or(nv.5);
                    freq_mhz = freq_mhz.or(nv.6);
                }
            }

            gpus.push(GpuStats {
                name: final_name,
                driver,
                busy_pct,
                vram_used_bytes,
                vram_total_bytes,
                gtt_used_bytes,
                temp_c,
                power_w,
                freq_mhz,
            });
        }
    }
    gpus
}

fn read_mounts() -> Vec<MountInfo> {
    let mut out = Vec::new();
    let mut seen_devs = std::collections::HashSet::new();
    let Ok(content) = fs::read_to_string("/proc/mounts") else {
        return out;
    };
    for line in content.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 3 {
            continue;
        }
        let dev = cols[0];
        let mount = cols[1];
        let fstype = cols[2];
        if !dev.starts_with("/dev/") {
            continue;
        }
        if fstype == "squashfs" || dev.starts_with("/dev/loop") {
            continue;
        }
        if !seen_devs.insert(dev.to_string()) {
            continue;
        }
        if let Ok(c_path) = CString::new(mount) {
            let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
            let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut st) };
            if rc == 0 && st.f_blocks > 0 {
                let frsize = st.f_frsize as u64;
                let total = (st.f_blocks as u64).saturating_mul(frsize);
                let avail = (st.f_bavail as u64).saturating_mul(frsize);
                let free = (st.f_bfree as u64).saturating_mul(frsize);
                let used = total.saturating_sub(free);
                out.push(MountInfo {
                    device: dev.to_string(),
                    mount_point: mount.to_string(),
                    fs_type: fstype.to_string(),
                    total_bytes: total,
                    used_bytes: used,
                    avail_bytes: avail,
                });
            }
        }
    }
    out.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    out
}

fn is_physical_disk(name: &str) -> bool {
    if name.starts_with("loop") || name.starts_with("ram") || name.starts_with("zram") || name.starts_with("dm-") {
        return false;
    }
    if name.starts_with("nvme") {
        // nvme0n1 is physical; nvme0n1p1 is partition
        return !name.contains('p') || !name.rsplit('p').next().is_some_and(|s| s.chars().all(|c| c.is_ascii_digit()));
    }
    if name.starts_with("mmcblk") {
        return !name.contains('p');
    }
    if name.starts_with("sd") || name.starts_with("vd") || name.starts_with("hd") {
        return !name.chars().last().is_some_and(|c| c.is_ascii_digit());
    }
    false
}

fn read_diskstats(prev: &mut HashMap<String, (u64, u64, u64)>, dt: f64) -> Vec<DiskIoInfo> {
    let mut out = Vec::new();
    let Ok(content) = fs::read_to_string("/proc/diskstats") else {
        return out;
    };
    for line in content.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 10 {
            continue;
        }
        let name = cols[2];
        if !is_physical_disk(name) {
            continue;
        }
        let reads_completed: u64 = cols[3].parse().unwrap_or(0);
        let sectors_read: u64 = cols[5].parse().unwrap_or(0);
        let writes_completed: u64 = cols[7].parse().unwrap_or(0);
        let sectors_written: u64 = cols[9].parse().unwrap_or(0);
        let io_ops = reads_completed + writes_completed;

        let (read_bps, write_bps, iops) = if let Some(&(pr, pw, pi)) = prev.get(name) {
            let dr = sectors_read.saturating_sub(pr) * 512;
            let dw = sectors_written.saturating_sub(pw) * 512;
            let di = io_ops.saturating_sub(pi);
            (
                (dr as f64 / dt) as u64,
                (dw as f64 / dt) as u64,
                (di as f64 / dt) as u64,
            )
        } else {
            (0, 0, 0)
        };
        prev.insert(name.to_string(), (sectors_read, sectors_written, io_ops));
        out.push(DiskIoInfo {
            name: name.to_string(),
            read_bps,
            write_bps,
            iops,
        });
    }
    out
}

fn read_net_dev(prev: &mut HashMap<String, (u64, u64)>, dt: f64) -> Vec<NetIfInfo> {
    let mut out = Vec::new();
    let Ok(content) = fs::read_to_string("/proc/net/dev") else {
        return out;
    };
    for line in content.lines().skip(2) {
        let Some((iface, rest)) = line.split_once(':') else {
            continue;
        };
        let name = iface.trim();
        if name == "lo" {
            continue;
        }
        let cols: Vec<u64> = rest
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        if cols.len() < 9 {
            continue;
        }
        let rx = cols[0];
        let tx = cols[8];
        let (rx_bps, tx_bps) = if let Some(&(prx, ptx)) = prev.get(name) {
            (
                (rx.saturating_sub(prx) as f64 / dt) as u64,
                (tx.saturating_sub(ptx) as f64 / dt) as u64,
            )
        } else {
            (0, 0)
        };
        prev.insert(name.to_string(), (rx, tx));
        out.push(NetIfInfo {
            name: name.to_string(),
            rx_bps,
            tx_bps,
            total_rx_bytes: rx,
            total_tx_bytes: tx,
        });
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn read_processes(
    uid_map: &HashMap<u32, String>,
    page_size: u64,
    _ticks_per_sec: f64,
    total_jiffies_delta: u64,
    logical_cores: usize,
    dt: f64,
    prev_jiffies: &mut HashMap<u32, u64>,
    prev_io: &mut HashMap<u32, (u64, u64)>,
) -> Vec<ProcessInfo> {
    let mut list = Vec::new();
    let mut seen_pids = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return list;
    };

    for entry in entries.flatten() {
        let fname = entry.file_name();
        let s = fname.to_string_lossy();
        let Ok(pid) = s.parse::<u32>() else {
            continue;
        };
        seen_pids.push(pid);

        let pdir = entry.path();
        let Ok(stat_raw) = fs::read_to_string(pdir.join("stat")) else {
            continue;
        };
        let Some(lparen) = stat_raw.find('(') else {
            continue;
        };
        let Some(rparen) = stat_raw.rfind(')') else {
            continue;
        };
        if rparen <= lparen {
            continue;
        }
        let comm = stat_raw[lparen + 1..rparen].to_string();
        let after = &stat_raw[rparen + 2..];
        let fields: Vec<&str> = after.split_whitespace().collect();
        if fields.len() < 22 {
            continue;
        }
        let state = fields[0].chars().next().unwrap_or('?');
        let ppid: u32 = fields[1].parse().unwrap_or(0);
        let utime: u64 = fields[11].parse().unwrap_or(0);
        let stime: u64 = fields[12].parse().unwrap_or(0);
        let nice: i32 = fields[16].parse().unwrap_or(0);
        let threads: u32 = fields[17].parse().unwrap_or(1);
        let proc_jiffies = utime + stime;

        let cpu_pct = if total_jiffies_delta > 0 {
            if let Some(&prev_j) = prev_jiffies.get(&pid) {
                let dj = proc_jiffies.saturating_sub(prev_j);
                ((dj as f64 / total_jiffies_delta as f64) * 100.0 * logical_cores as f64)
                    .clamp(0.0, (logical_cores as f64) * 100.0) as f32
            } else {
                0.0
            }
        } else {
            0.0
        };
        prev_jiffies.insert(pid, proc_jiffies);

        let rss_bytes = read_pss_bytes(&pdir).unwrap_or_else(|| {
            fs::read_to_string(pdir.join("statm"))
                .ok()
                .and_then(|m| m.split_whitespace().nth(1)?.parse::<u64>().ok())
                .map(|pages| pages * page_size)
                .unwrap_or(0)
        });

        let uid = fs::read_to_string(pdir.join("status"))
            .ok()
            .and_then(|st| {
                st.lines()
                    .find(|l| l.starts_with("Uid:"))
                    .and_then(|l| l.split_whitespace().nth(1)?.parse::<u32>().ok())
            })
            .unwrap_or(0);
        let user = uid_map
            .get(&uid)
            .cloned()
            .unwrap_or_else(|| uid.to_string());

        let cmdline_raw = fs::read(pdir.join("cmdline")).unwrap_or_default();
        let is_kernel_thread = cmdline_raw.is_empty() && (ppid == 2 || pid == 2);
        let cmdline = if cmdline_raw.is_empty() {
            format!("[{comm}]")
        } else {
            cmdline_raw
                .split(|&b| b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s))
                .collect::<Vec<_>>()
                .join(" ")
        };

        let (io_read_bps, io_write_bps) = if let Ok(io_raw) = fs::read_to_string(pdir.join("io")) {
            let mut rb = 0u64;
            let mut wb = 0u64;
            for l in io_raw.lines() {
                if let Some(v) = l.strip_prefix("read_bytes: ") {
                    rb = v.trim().parse().unwrap_or(0);
                } else if let Some(v) = l.strip_prefix("write_bytes: ") {
                    wb = v.trim().parse().unwrap_or(0);
                }
            }
            let res = if let Some(&(pr, pw)) = prev_io.get(&pid) {
                (
                    (rb.saturating_sub(pr) as f64 / dt) as u64,
                    (wb.saturating_sub(pw) as f64 / dt) as u64,
                )
            } else {
                (0, 0)
            };
            prev_io.insert(pid, (rb, wb));
            res
        } else {
            (0, 0)
        };

        let cgroup_unit = fs::read_to_string(pdir.join("cgroup"))
            .ok()
            .and_then(|cg| extract_cgroup_unit(&cg));

        let app_group = infer_app_group(&comm, &cmdline, cgroup_unit.as_deref(), is_kernel_thread);

        list.push(ProcessInfo {
            pid,
            ppid,
            uid,
            user,
            name: comm,
            cmdline,
            state,
            cpu_pct,
            rss_bytes,
            io_read_bps,
            io_write_bps,
            threads,
            nice,
            is_kernel_thread,
            app_group,
            cgroup_unit,
        });
    }

    prev_jiffies.retain(|pid, _| seen_pids.contains(pid));
    prev_io.retain(|pid, _| seen_pids.contains(pid));
    list
}

fn extract_cgroup_unit(cg: &str) -> Option<String> {
    for line in cg.lines() {
        if let Some((_, path)) = line.rsplit_once(':') {
            for seg in path.split('/').rev() {
                if seg.ends_with(".scope") || seg.ends_with(".service") {
                    return Some(seg.to_string());
                }
            }
        }
    }
    None
}

fn infer_app_group(
    comm: &str,
    cmdline: &str,
    cgroup: Option<&str>,
    is_kernel: bool,
) -> String {
    if is_kernel {
        return "Kernel Linux".to_string();
    }
    if let Some(unit) = cgroup {
        if let Some(app_name) = parse_systemd_app_scope(unit) {
            return app_name;
        }
    }
    let lower = comm.to_lowercase();
    let cmd_lower = cmdline.to_lowercase();
    if lower.contains("chrome") || cmd_lower.contains("google-chrome") {
        return "Google Chrome".to_string();
    }
    if lower.contains("chromium") || cmd_lower.contains("chromium") {
        return "Chromium".to_string();
    }
    if lower.contains("firefox") || lower.contains("web content") || cmd_lower.contains("/firefox") {
        return "Firefox".to_string();
    }
    if lower.contains("zen") && cmd_lower.contains("zen") {
        return "Zen Browser".to_string();
    }
    if lower.contains("brave") || cmd_lower.contains("brave-browser") {
        return "Brave Browser".to_string();
    }
    if lower == "code" || cmd_lower.contains("/usr/share/code") || cmd_lower.contains("vscode") {
        return "VS Code".to_string();
    }
    if lower.contains("discord") || cmd_lower.contains("discord") {
        return "Discord".to_string();
    }
    if lower.contains("spotify") {
        return "Spotify".to_string();
    }
    if lower.contains("steam") || cmd_lower.contains("steam") {
        return "Steam".to_string();
    }
    if lower.starts_with("rocketde") {
        return comm.to_string();
    }
    if lower == "cargo" || lower == "rustc" || lower == "rust-analyzer" {
        return "Rust Toolchain".to_string();
    }
    if let Some(unit) = cgroup {
        if unit.ends_with(".service") {
            return unit.trim_end_matches(".service").to_string();
        }
    }
    comm.to_string()
}

fn parse_systemd_app_scope(unit: &str) -> Option<String> {
    // e.g. app-gnome-firefox-12345.scope or app-flatpak-com.discordapp.Discord-123.scope
    if !unit.starts_with("app-") || !unit.ends_with(".scope") {
        return None;
    }
    let inner = unit.trim_start_matches("app-").trim_end_matches(".scope");
    let parts: Vec<&str> = inner.split('-').collect();
    if parts.len() >= 2 {
        let raw = if parts.len() >= 3 && parts.last().is_some_and(|p| p.chars().all(|c| c.is_ascii_digit())) {
            parts[parts.len() - 2]
        } else {
            parts[parts.len() - 1]
        };
        let clean = raw
            .rsplit('.')
            .next()
            .unwrap_or(raw)
            .replace('_', " ");
        if !clean.is_empty() {
            let mut chars = clean.chars();
            if let Some(first) = chars.next() {
                return Some(first.to_uppercase().collect::<String>() + chars.as_str());
            }
        }
    }
    None
}

fn build_app_groups(processes: &[ProcessInfo]) -> Vec<AppGroup> {
    let mut map: HashMap<String, Vec<ProcessInfo>> = HashMap::new();
    for p in processes {
        if p.is_kernel_thread {
            continue;
        }
        map.entry(p.app_group.clone()).or_default().push(p.clone());
    }
    let mut groups: Vec<AppGroup> = map
        .into_iter()
        .map(|(name, mut procs)| {
            procs.sort_by(|a, b| b.rss_bytes.cmp(&a.rss_bytes));
            let total_rss_bytes = procs.iter().map(|p| p.rss_bytes).sum();
            let total_cpu_pct = procs.iter().map(|p| p.cpu_pct).sum();
            let total_io_bps = procs.iter().map(|p| p.io_read_bps + p.io_write_bps).sum();
            AppGroup {
                name,
                total_rss_bytes,
                total_cpu_pct,
                total_io_bps,
                processes: procs,
            }
        })
        .collect();
    groups.sort_by(|a, b| b.total_rss_bytes.cmp(&a.total_rss_bytes));
    groups
}

fn read_pss_bytes(pdir: &std::path::Path) -> Option<u64> {
    let raw = fs::read_to_string(pdir.join("smaps_rollup")).ok()?;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("Pss:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

type NvidiaSmiTuple = (
    String,
    Option<f32>,
    Option<u64>,
    Option<u64>,
    Option<f32>,
    Option<f32>,
    Option<u32>,
);

fn query_nvidia_smi() -> Option<NvidiaSmiTuple> {
    let out = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw,clocks.current.graphics",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout).lines().next()?.to_string();
    let cols: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if cols.len() < 7 {
        return None;
    }
    Some((
        cols[0].to_string(),
        cols[1].parse::<f32>().ok(),
        cols[2].parse::<u64>().ok().map(|mib| mib * 1024 * 1024),
        cols[3].parse::<u64>().ok().map(|mib| mib * 1024 * 1024),
        cols[4].parse::<f32>().ok(),
        cols[5].parse::<f32>().ok(),
        cols[6].parse::<u32>().ok(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_systemd_app_scope_extracts_clean_name() {
        assert_eq!(
            parse_systemd_app_scope("app-gnome-firefox-12345.scope"),
            Some("Firefox".to_string())
        );
        assert_eq!(
            parse_systemd_app_scope("app-flatpak-com.discordapp.Discord-987.scope"),
            Some("Discord".to_string())
        );
        assert_eq!(parse_systemd_app_scope("user@1000.service"), None);
    }

    #[test]
    fn physical_disk_filter_skips_partitions_and_loops() {
        assert!(is_physical_disk("nvme0n1"));
        assert!(!is_physical_disk("nvme0n1p1"));
        assert!(is_physical_disk("sda"));
        assert!(!is_physical_disk("sda2"));
        assert!(!is_physical_disk("loop0"));
        assert!(!is_physical_disk("zram0"));
    }

    #[test]
    fn meminfo_partitions_total_without_double_counting_cache() {
        let m = read_meminfo();
        if m.total_bytes > 0 {
            assert_eq!(
                m.real_used_bytes + m.reclaimable_cache_bytes + m.free_bytes,
                m.total_bytes
            );
        }
    }
}
