use egui::{Event, Key, Modifiers, OutputCommand};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

fn fallback_clipboard() -> &'static Mutex<String> {
    static FALLBACK: OnceLock<Mutex<String>> = OnceLock::new();
    FALLBACK.get_or_init(|| Mutex::new(String::new()))
}

/// Copies `text` to the Wayland clipboard (`wl-copy`) with an in-process fallback.
pub fn copy_text(text: &str) {
    if let Ok(mut guard) = fallback_clipboard().lock() {
        *guard = text.to_string();
    }
    let owned = text.to_string();
    std::thread::spawn(move || {
        if let Ok(mut child) = Command::new("wl-copy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(owned.as_bytes());
            }
            let _ = child.wait();
            return;
        }
        if let Ok(mut child) = Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(owned.as_bytes());
            }
            let _ = child.wait();
        }
    });
}

/// Reads plain text from the Wayland clipboard (`wl-paste`) with an in-process fallback.
pub fn paste_text() -> Option<String> {
    if let Ok(out) = Command::new("wl-paste")
        .arg("--no-newline")
        .stderr(Stdio::null())
        .output()
    {
        if out.status.success() {
            return Some(String::from_utf8_lossy(&out.stdout).into_owned());
        }
    }
    if let Ok(out) = Command::new("wl-paste").stderr(Stdio::null()).output() {
        if out.status.success() {
            let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
            if s.ends_with('\n') {
                s.pop();
            }
            return Some(s);
        }
    }
    if let Ok(out) = Command::new("xclip")
        .args(["-o", "-selection", "clipboard"])
        .stderr(Stdio::null())
        .output()
    {
        if out.status.success() {
            return Some(String::from_utf8_lossy(&out.stdout).into_owned());
        }
    }
    fallback_clipboard()
        .lock()
        .ok()
        .map(|g| g.clone())
        .filter(|s| !s.is_empty())
}

/// Processes `egui::PlatformOutput::commands` for clipboard copy requests.
pub fn handle_egui_clipboard_output(commands: &[OutputCommand]) {
    for cmd in commands {
        if let OutputCommand::CopyText(text) = cmd {
            copy_text(text);
        }
    }
}

/// Translates `Ctrl+C`, `Ctrl+X`, and `Ctrl+V` into native `egui::Event::{Copy, Cut, Paste}`.
pub fn push_clipboard_events(
    key: Key,
    pressed: bool,
    modifiers: Modifiers,
    pending_events: &mut Vec<Event>,
) -> bool {
    if !pressed || !(modifiers.ctrl || modifiers.command) || modifiers.alt {
        return false;
    }
    match key {
        Key::C => {
            pending_events.push(Event::Copy);
            true
        }
        Key::X => {
            pending_events.push(Event::Cut);
            true
        }
        Key::V => {
            if let Some(text) = paste_text() {
                pending_events.push(Event::Paste(text));
            }
            true
        }
        _ => false,
    }
}
