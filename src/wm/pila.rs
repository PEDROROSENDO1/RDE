use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: String,
    pub title: String,
    pub app_id: String,
    pub title_lower: String,
    pub app_id_lower: String,
    pub workspace: usize,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub all_workspaces: bool,
    pub focused: bool,
    pub maximized: bool,
    pub minimized: bool,
    pub fullscreen: bool,
    pub always_on_top: bool,
    pub snapped: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceInfo {
    pub index: usize,
    pub active: bool,
    pub occupied: bool,
    pub urgent: bool,
}

#[derive(Debug, Clone, Eq)]
pub struct WmState {
    pub workspaces: Vec<WorkspaceInfo>,
    pub windows: Vec<WindowInfo>,
    pub focused_window: Option<WindowInfo>,
    pub keyboard_layout: String,
    pub version: u64,
}

impl PartialEq for WmState {
    fn eq(&self, other: &Self) -> bool {
        self.workspaces == other.workspaces
            && self.windows == other.windows
            && self.focused_window == other.focused_window
            && self.keyboard_layout == other.keyboard_layout
    }
}

impl Default for WmState {
    fn default() -> Self {
        let workspaces = (1..=4)
            .map(|i| WorkspaceInfo {
                index: i,
                active: i == 1,
                occupied: i == 1,
                urgent: false,
            })
            .collect();

        Self {
            workspaces,
            windows: Vec::new(),
            focused_window: None,
            keyboard_layout: "US".to_string(),
            version: 0,
        }
    }
}

pub struct PilaConnector {
    pub state: Arc<Mutex<WmState>>,
}

impl PilaConnector {
    pub fn new(workspace_count: usize, wake_fn: impl Fn() + Send + Sync + 'static) -> Self {
        let mut initial = WmState::default();
        initial.workspaces = (1..=workspace_count.max(1))
            .map(|i| WorkspaceInfo {
                index: i,
                active: i == 1,
                occupied: i == 1,
                urgent: false,
            })
            .collect();

        // Initialize immediately in background so startup UI is never blocked by IPC
        let state = Arc::new(Mutex::new(initial));
        let wake_shared = Arc::new(wake_fn);
        let subscribe_connected = Arc::new(AtomicBool::new(false));
        let refresh_signal = Arc::new((Mutex::new(false), Condvar::new()));

        // Coalesced refresh worker: batches burst events from `pila msg --json subscribe`
        // (e.g. workspace + focus + title changing together) into a single refresh.
        let state_worker = state.clone();
        let wake_worker = wake_shared.clone();
        let signal_worker = refresh_signal.clone();
        thread::spawn(move || {
            if refresh_from_pila(&state_worker, workspace_count) {
                wake_worker();
            }
            let (lock, cvar) = &*signal_worker;
            loop {
                {
                    let mut pending = lock.lock().unwrap();
                    while !*pending {
                        pending = cvar.wait(pending).unwrap();
                    }
                    *pending = false;
                }
                // Short coalesce window to absorb multi-line event bursts
                thread::sleep(Duration::from_millis(18));
                if let Ok(mut pending) = lock.lock() {
                    *pending = false;
                }
                if refresh_from_pila(&state_worker, workspace_count) {
                    wake_worker();
                }
            }
        });

        // 1. Real-time zero-latency event stream via `pila msg --json subscribe`
        let sub_flag = subscribe_connected.clone();
        let signal_sub = refresh_signal.clone();
        thread::spawn(move || {
            loop {
                if let Ok(mut child) = Command::new("pila")
                    .args(["msg", "--json", "subscribe"])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    sub_flag.store(true, Ordering::Relaxed);
                    if let Some(stdout) = child.stdout.take() {
                        let reader = BufReader::new(stdout);
                        for line in reader.lines().flatten() {
                            if !line.trim().is_empty() {
                                let (lock, cvar) = &*signal_sub;
                                if let Ok(mut pending) = lock.lock() {
                                    *pending = true;
                                    cvar.notify_one();
                                }
                            }
                        }
                    }
                    sub_flag.store(false, Ordering::Relaxed);
                    let _ = child.wait();
                }
                thread::sleep(Duration::from_millis(1000));
            }
        });

        // 2. Fallback periodic sync every 5000ms ONLY when `subscribe` is disconnected
        let state_poll = state.clone();
        let wake_poll = wake_shared;
        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_millis(5000));
                if !subscribe_connected.load(Ordering::Relaxed)
                    && refresh_from_pila(&state_poll, workspace_count)
                {
                    wake_poll();
                }
            }
        });

        Self { state }
    }

    pub fn switch_workspace(index: usize) {
        thread::spawn(move || {
            let _ = Command::new("pila")
                .args(["msg", "action", "workspace", &index.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        });
    }

    pub fn focus_window(id: &str) {
        if id.is_empty() {
            return;
        }
        let id_owned = id.to_string();
        thread::spawn(move || {
            let _ = Command::new("pila")
                .args(["msg", "focus", &id_owned])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        });
    }

    pub fn close_window(id: &str) {
        if id.is_empty() {
            Self::run_action("close");
            return;
        }
        let id_owned = id.to_string();
        thread::spawn(move || {
            let _ = Command::new("pila")
                .args(["msg", "close", &id_owned])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        });
    }

    pub fn run_action(action: &str) {
        let action_owned = action.to_string();
        thread::spawn(move || {
            let mut cmd = Command::new("pila");
            cmd.arg("msg").arg("action");
            for part in action_owned.split_whitespace() {
                cmd.arg(part);
            }
            let _ = cmd.stdout(Stdio::null()).stderr(Stdio::null()).status();
        });
    }

    /// Focuses `id` and runs a Pila action (e.g. `move-to-workspace 2`, `snap-left`) sequentially
    /// on the same thread to avoid focus/action races.
    pub fn focus_and_run_action(id: &str, action: &str) {
        if id.is_empty() {
            Self::run_action(action);
            return;
        }
        let id_owned = id.to_string();
        let action_owned = action.to_string();
        thread::spawn(move || {
            let _ = Command::new("pila")
                .args(["msg", "focus", &id_owned])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            let mut cmd = Command::new("pila");
            cmd.arg("msg").arg("action");
            for part in action_owned.split_whitespace() {
                cmd.arg(part);
            }
            let _ = cmd.stdout(Stdio::null()).stderr(Stdio::null()).status();
        });
    }

    pub fn move_window_to_workspace(id: &str, workspace: usize) {
        Self::focus_and_run_action(id, &format!("move-to-workspace {}", workspace));
    }

    /// Synchronous version used after releasing layer-shell exclusive keyboard focus
    /// so Pila's `focused_window()` sees the target window before `move-to-workspace` runs.
    pub fn move_window_to_workspace_sync(id: &str, workspace: usize) {
        if !id.is_empty() {
            let _ = Command::new("pila")
                .args(["msg", "focus", id])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = Command::new("pila")
            .args(["msg", "action", "move-to-workspace", &workspace.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn query_pila_json(request: &str) -> Option<Value> {
    let output = Command::new("pila")
        .args(["msg", "--json", request])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

fn parse_window_value(item: &Value) -> Option<WindowInfo> {
    let id = match item.get("id") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    let title = item
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let app_id = item
        .get("app_id")
        .or_else(|| item.get("app-id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if app_id.is_empty() && title.is_empty() {
        return None;
    }
    if app_id.to_lowercase().contains("rocketde") {
        return None;
    }

    let workspace = item
        .get("workspace")
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as usize;
    let x = item.get("x").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let y = item.get("y").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let width = item.get("width").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let height = item.get("height").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let all_workspaces = item
        .get("all_workspaces")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let focused = item
        .get("focused")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let maximized = item
        .get("maximized")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let minimized = item
        .get("minimized")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let fullscreen = item
        .get("fullscreen")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let always_on_top = item
        .get("always_on_top")
        .or_else(|| item.get("on-top"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let snapped = item
        .get("snapped")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let title_lower = title.to_lowercase();
    let app_id_lower = app_id.to_lowercase();

    Some(WindowInfo {
        id,
        title,
        app_id,
        title_lower,
        app_id_lower,
        workspace,
        x,
        y,
        width,
        height,
        all_workspaces,
        focused,
        maximized,
        minimized,
        fullscreen,
        always_on_top,
        snapped,
    })
}

fn refresh_from_pila(state: &Arc<Mutex<WmState>>, default_ws_count: usize) -> bool {
    let (windows_json, workspaces_json, focused_json, keyboard_json) = thread::scope(|s| {
        let h_win = s.spawn(|| query_pila_json("windows"));
        let h_ws = s.spawn(|| query_pila_json("workspaces"));
        let h_foc = s.spawn(|| query_pila_json("focused"));
        let h_kb = s.spawn(|| query_pila_json("keyboard"));
        (
            h_win.join().ok().flatten(),
            h_ws.join().ok().flatten(),
            h_foc.join().ok().flatten(),
            h_kb.join().ok().flatten(),
        )
    });

    if windows_json.is_none() && workspaces_json.is_none() && focused_json.is_none() {
        return false;
    }

    let mut parsed_windows = Vec::new();
    if let Some(val) = windows_json {
        let arr_opt = val
            .as_array()
            .or_else(|| val.get("windows").and_then(|v| v.as_array()))
            .or_else(|| val.get("ok").and_then(|v| v.as_array()));

        if let Some(arr) = arr_opt {
            for item in arr {
                if let Some(win) = parse_window_value(item) {
                    parsed_windows.push(win);
                }
            }
        }
    }

    // 1. Check `pila msg --json focused` (which returns `{"ok":true,"protocol":1,"window":{...}}`)
    let mut focused_win = None;
    if let Some(fval) = focused_json {
        let win_obj = fval
            .get("window")
            .or_else(|| fval.get("ok"))
            .unwrap_or(&fval);
        if win_obj.is_object() {
            focused_win = parse_window_value(win_obj);
        }
    }

    // 2. Fallback to window marked `focused: true` inside `windows` array
    if focused_win.is_none() {
        focused_win = parsed_windows.iter().find(|w| w.focused).cloned();
    }

    let mut parsed_workspaces = Vec::new();
    if let Some(ws_val) = workspaces_json {
        let arr_opt = ws_val
            .as_array()
            .or_else(|| ws_val.get("workspaces").and_then(|v| v.as_array()))
            .or_else(|| ws_val.get("ok").and_then(|v| v.as_array()));

        if let Some(arr) = arr_opt {
            for (i, item) in arr.iter().enumerate() {
                let index = item
                    .get("index")
                    .or_else(|| item.get("id"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or((i + 1) as u64) as usize;
                let active = item
                    .get("active")
                    .or_else(|| item.get("focused"))
                    .or_else(|| item.get("current"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let occupied = item
                    .get("occupied")
                    .and_then(|v| v.as_bool())
                    .unwrap_or_else(|| {
                        item.get("windows")
                            .or_else(|| item.get("toplevels"))
                            .and_then(|v| v.as_u64())
                            .map(|c| c > 0)
                            .unwrap_or_else(|| parsed_windows.iter().any(|w| w.workspace == index))
                    });
                let urgent = item
                    .get("urgent")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                parsed_workspaces.push(WorkspaceInfo {
                    index,
                    active,
                    occupied,
                    urgent,
                });
            }
        }
    }

    if parsed_workspaces.is_empty() {
        let active_ws = focused_win.as_ref().map(|w| w.workspace).unwrap_or(1);
        for idx in 1..=default_ws_count.max(4) {
            parsed_workspaces.push(WorkspaceInfo {
                index: idx,
                active: idx == active_ws,
                occupied: parsed_windows.iter().any(|w| w.workspace == idx),
                urgent: false,
            });
        }
    }

    // Parse Pila's exact keyboard JSON: `{"layouts":[{"active":true,"index":1,"name":"English (US)"}],"ok":true,"protocol":1}`
    let mut kb_layout = "US".to_string();
    if let Some(kb_val) = keyboard_json {
        if let Some(layouts) = kb_val.get("layouts").and_then(|v| v.as_array()) {
            if let Some(active_layout) = layouts
                .iter()
                .find(|l| l.get("active").and_then(|v| v.as_bool()).unwrap_or(false))
                .or_else(|| layouts.first())
            {
                if let Some(name) = active_layout.get("name").and_then(|v| v.as_str()) {
                    kb_layout = shorten_keyboard_name(name);
                }
            }
        } else if let Some(layout) = kb_val
            .get("current")
            .or_else(|| kb_val.get("layout"))
            .and_then(|v| v.as_str())
        {
            kb_layout = shorten_keyboard_name(layout);
        }
    }

    let mut changed = false;
    if let Ok(mut guard) = state.lock() {
        // Preserve the last known focused window if focus temporarily left to a layer surface,
        // as long as that window still exists in `parsed_windows`!
        if focused_win.is_none() {
            if let Some(prev) = &guard.focused_window {
                if let Some(still_open) = parsed_windows.iter().find(|w| w.id == prev.id) {
                    focused_win = Some(still_open.clone());
                }
            }
        }
        if focused_win.is_none() {
            let active_ws = parsed_workspaces
                .iter()
                .find(|ws| ws.active)
                .map(|ws| ws.index)
                .unwrap_or(1);
            focused_win = parsed_windows
                .iter()
                .find(|w| w.workspace == active_ws)
                .or_else(|| parsed_windows.first())
                .cloned();
        }

        let mut next = WmState {
            workspaces: parsed_workspaces,
            windows: parsed_windows,
            focused_window: focused_win,
            keyboard_layout: kb_layout,
            version: guard.version,
        };
        if *guard != next {
            next.version = guard.version.wrapping_add(1);
            *guard = next;
            changed = true;
        }
    }

    changed
}

fn shorten_keyboard_name(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("brazil") || lower.contains("portugues") || lower == "br" || lower == "pt" {
        "BR".to_string()
    } else if lower.contains("english (us)") || lower == "us" {
        "US".to_string()
    } else if lower.contains("english (uk)") || lower == "gb" {
        "UK".to_string()
    } else if lower.contains("spanish") || lower == "es" {
        "ES".to_string()
    } else {
        raw.chars().take(3).collect::<String>().to_uppercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wm_state_eq_ignores_version() {
        let a = WmState::default();
        let mut b = WmState::default();
        b.version = 42;
        assert_eq!(a, b);
    }

    #[test]
    fn parse_window_skips_rocketde_and_handles_fields() {
        let own = serde_json::json!({"id": "1", "app_id": "rocketde", "title": "bar"});
        assert!(parse_window_value(&own).is_none());

        let app = serde_json::json!({
            "id": 99,
            "app_id": "foot",
            "title": "Terminal",
            "workspace": 2,
            "focused": true
        });
        let parsed = parse_window_value(&app).expect("valid window");
        assert_eq!(parsed.id, "99");
        assert_eq!(parsed.workspace, 2);
        assert!(parsed.focused);
    }
}
