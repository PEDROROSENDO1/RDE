use chrono::{DateTime, Local};
use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::{
    Align, Align2, Color32, ColorImage, Context, CornerRadius, Event, FontId, Frame, Key, Layout,
    Margin, Modifiers, PointerButton, Pos2, RawInput, Rect, RichText, Sense, Shape, Stroke,
    TextureFilter, TextureHandle, TextureOptions, Vec2, pos2, vec2,
};
use egui_wgpu::{Renderer, RendererOptions, ScreenDescriptor};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use rocketde_core::config::RocketConfig;
use rocketde_ui_common::clipboard::{handle_egui_clipboard_output, push_clipboard_events};
use rocketde_ui_common::header_bar::{
    ResizeDir, WindowAction, detect_window_resize, header_bar_drag_area,
    header_bar_drag_from_response, window_controls_right,
};
use rocketde_ui_common::icons::{IconKind, draw_icon};
use rocketde_ui_common::theme::apply_macos_theme;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers as WlModifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        xdg::{
            XdgShell,
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{Shm, ShmHandler},
};
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::fs;
use std::num::NonZeroU32;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::ptr::NonNull;
use std::time::{Duration, Instant, SystemTime};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Grid,
    List,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortField {
    Name,
    Kind,
    Size,
    Modified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardMode {
    Copy,
    Cut,
}

#[derive(Debug, Clone)]
struct FileClipboard {
    mode: ClipboardMode,
    paths: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
enum UndoAction {
    Rename { from: PathBuf, to: PathBuf },
    CreatePath { path: PathBuf },
    CopyCreated { created: Vec<PathBuf> },
    Moved { pairs: Vec<(PathBuf, PathBuf)> },
    Trashed { trash_items: Vec<PathBuf> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarIcon {
    Home,
    Desktop,
    Documents,
    Downloads,
    Pictures,
    Videos,
    Music,
    Starred,
    Trash,
    Drive,
    Root,
}

#[derive(Debug, Clone)]
struct SidebarPlace {
    label: String,
    path: PathBuf,
    icon: SidebarIcon,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeviceEntry {
    label: String,
    dev_node: String,
    mount_point: Option<PathBuf>,
    size_str: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    Dir,
    Image,
    Video,
    Audio,
    Archive,
    Code,
    Document,
    Other,
}

impl FileKind {
    fn from_path(path: &Path, is_dir: bool) -> Self {
        if is_dir {
            return Self::Dir;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "ico" | "avif" => Self::Image,
            "mp4" | "mkv" | "webm" | "avi" | "mov" | "m4v" => Self::Video,
            "mp3" | "flac" | "ogg" | "wav" | "m4a" | "opus" | "aac" => Self::Audio,
            "zip" | "tar" | "gz" | "xz" | "zst" | "7z" | "rar" | "bz2" => Self::Archive,
            "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "cpp" | "h" | "hpp" | "go"
            | "java" | "toml" | "json" | "yaml" | "yml" | "sh" | "bash" | "zsh" | "html"
            | "css" | "sql" | "xml" | "nix" => Self::Code,
            "pdf" | "md" | "txt" | "doc" | "docx" | "odt" | "csv" | "rtf" | "epub" => {
                Self::Document
            }
            _ => Self::Other,
        }
    }

    fn type_label(self) -> &'static str {
        match self {
            Self::Dir => "Pasta",
            Self::Image => "Imagem",
            Self::Video => "Vídeo",
            Self::Audio => "Áudio",
            Self::Archive => "Arquivo",
            Self::Code => "Código",
            Self::Document => "Documento",
            Self::Other => "Arquivo",
        }
    }

    fn mime_guess(self, ext_upper: &str) -> String {
        match (self, ext_upper) {
            (Self::Dir, _) => "inode/directory".to_string(),
            (Self::Image, e) if !e.is_empty() => format!("image/{}", e.to_lowercase()),
            (Self::Video, e) if !e.is_empty() => format!("video/{}", e.to_lowercase()),
            (Self::Audio, e) if !e.is_empty() => format!("audio/{}", e.to_lowercase()),
            (Self::Code, "RS") => "text/rust".to_string(),
            (Self::Code, "PY") => "text/x-python".to_string(),
            (Self::Code, "JSON") => "application/json".to_string(),
            (Self::Document, "PDF") => "application/pdf".to_string(),
            (Self::Document, "MD") => "text/markdown".to_string(),
            (Self::Document, "TXT") => "text/plain".to_string(),
            (Self::Archive, e) if !e.is_empty() => format!("application/x-{}", e.to_lowercase()),
            _ => "application/octet-stream".to_string(),
        }
    }

    fn accent_color(self, accent: Color32) -> Color32 {
        match self {
            Self::Dir => Color32::from_rgb(86, 168, 245),
            Self::Image => Color32::from_rgb(191, 90, 242),
            Self::Video => Color32::from_rgb(255, 69, 58),
            Self::Audio => Color32::from_rgb(48, 209, 88),
            Self::Archive => Color32::from_rgb(255, 159, 10),
            Self::Code => Color32::from_rgb(56, 189, 248),
            Self::Document => accent,
            Self::Other => Color32::from_rgb(138, 148, 168),
        }
    }
}

#[derive(Debug, Clone)]
struct FileEntry {
    name: String,
    ext_upper: String,
    path: PathBuf,
    is_dir: bool,
    is_hidden: bool,
    is_symlink: bool,
    size_bytes: u64,
    modified_ts: u64,
    modified_str: String,
    kind: FileKind,
}

#[derive(Debug, Clone)]
enum ContextTarget {
    Entry(FileEntry),
    Background,
}

#[derive(Debug, Clone)]
struct PropertiesInfo {
    entry: FileEntry,
    parent_dir: String,
    mime_type: String,
    permissions_str: String,
    is_executable: bool,
    owner_group: String,
    accessed_str: String,
    created_str: String,
    modified_full_str: String,
    volume_str: String,
    symlink_target: Option<String>,
    dir_stats: Option<(usize, usize, u64)>, // (folders, files, total_bytes)
    dir_scanning: bool,
}

struct ThumbResult {
    path: PathBuf,
    size: [usize; 2],
    rgba: Vec<u8>,
}

enum BgEvent {
    FsChanged,
    OpResult {
        msg: String,
        is_err: bool,
        refresh: bool,
        undo: Option<UndoAction>,
    },
    MountReady {
        mount_point: PathBuf,
        label: String,
    },
    DevicesUpdated(Vec<DeviceEntry>),
    DirStatsReady {
        path: PathBuf,
        folders: usize,
        files: usize,
        bytes: u64,
    },
    ThumbReady(ThumbResult),
}

struct FilesApp {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    _compositor: CompositorState,
    shm: Shm,
    window: Window,
    wl_surface: wl_surface::WlSurface,

    device: wgpu::Device,
    queue: wgpu::Queue,
    wgpu_surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    egui_ctx: Context,
    renderer: Renderer,

    width: u32,
    height: u32,
    scale_factor: f32,
    configured: bool,
    dirty: bool,
    repaint_delay: Option<Duration>,
    pending_events: Vec<Event>,
    modifiers: Modifiers,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    seat: Option<wl_seat::WlSeat>,
    last_pointer_serial: u32,
    is_maximized: bool,
    pending_window_action: WindowAction,
    start_instant: Instant,
    exit: bool,

    // File manager state
    cfg: RocketConfig,
    current_dir: PathBuf,
    viewing_starred: bool,
    entries: Vec<FileEntry>,
    history_back: Vec<(PathBuf, bool)>,
    history_fwd: Vec<(PathBuf, bool)>,
    selected_paths: Vec<PathBuf>,
    selection_anchor: Option<PathBuf>,
    scroll_to_selection: bool,
    marquee_start: Option<Pos2>,
    marquee_active: bool,
    marquee_base: Vec<PathBuf>,
    free_space_str: String,

    view_mode: ViewMode,
    sort_field: SortField,
    sort_asc: bool,
    show_hidden: bool,
    search_query: String,
    focus_search: bool,

    editing_path: bool,
    focus_path: bool,
    path_input: String,

    new_folder_mode: bool,
    focus_new_folder: bool,
    new_folder_name: String,

    new_file_mode: bool,
    focus_new_file: bool,
    new_file_name: String,

    rename_target: Option<PathBuf>,
    focus_rename: bool,
    rename_buf: String,

    delete_confirm_targets: Option<Vec<PathBuf>>,
    empty_trash_confirm: bool,
    properties_modal: Option<PropertiesInfo>,
    context_menu: Option<(ContextTarget, Pos2)>,
    clipboard: Option<FileClipboard>,
    undo_stack: Vec<UndoAction>,
    toast: Option<(String, bool, Instant)>,

    places: Vec<SidebarPlace>,
    devices: Vec<DeviceEntry>,
    last_device_scan: Instant,
    starred: HashSet<PathBuf>,
    starred_file: PathBuf,
    trash_dir: PathBuf,
    trash_info_dir: PathBuf,

    // Watcher & async workers
    watcher: Option<RecommendedWatcher>,
    watched_dir: Option<PathBuf>,
    last_fs_refresh: Instant,
    bg_tx: Sender<BgEvent>,
    bg_rx: Receiver<BgEvent>,
    thumb_tx: Sender<PathBuf>,
    thumb_cache: HashMap<PathBuf, TextureHandle>,
    thumb_requested: HashSet<PathBuf>,
}

impl FilesApp {
    fn is_in_trash(&self) -> bool {
        !self.viewing_starred && self.current_dir == self.trash_dir
    }

    fn navigate_to(&mut self, dest: PathBuf, push_history: bool) {
        let dest = expand_tilde(&dest);
        if !dest.is_dir() {
            self.set_toast(format!("Pasta inacessível: {}", dest.display()), true);
            return;
        }
        if push_history && (self.viewing_starred || self.current_dir != dest) {
            self.history_back
                .push((self.current_dir.clone(), self.viewing_starred));
            self.history_fwd.clear();
        }
        self.viewing_starred = false;
        self.current_dir = dest;
        self.clear_transient_state();
        self.refresh_entries();
        self.update_watcher();
        self.refresh_devices_async();
        if self.thumb_cache.len() > 256 {
            self.thumb_cache.clear();
            self.thumb_requested.clear();
        }

        let title = if self.is_in_trash() {
            "Lixeira".to_string()
        } else {
            self.current_dir
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("/")
                .to_string()
        };
        self.window.set_title(&format!("Arquivos — {}", title));
        self.dirty = true;
    }

    fn navigate_to_starred(&mut self, push_history: bool) {
        if push_history && !self.viewing_starred {
            self.history_back
                .push((self.current_dir.clone(), self.viewing_starred));
            self.history_fwd.clear();
        }
        self.viewing_starred = true;
        self.clear_transient_state();
        self.refresh_entries();
        self.window.set_title("Arquivos — Com Estrela");
        self.free_space_str.clear();
        self.dirty = true;
    }

    fn clear_transient_state(&mut self) {
        self.selected_paths.clear();
        self.selection_anchor = None;
        self.marquee_start = None;
        self.marquee_active = false;
        self.marquee_base.clear();
        self.search_query.clear();
        self.editing_path = false;
        self.new_folder_mode = false;
        self.new_file_mode = false;
        self.rename_target = None;
        self.delete_confirm_targets = None;
        self.properties_modal = None;
        self.context_menu = None;
    }

    fn update_watcher(&mut self) {
        if self.viewing_starred || self.watched_dir.as_ref() == Some(&self.current_dir) {
            return;
        }
        if let Some(ref mut w) = self.watcher {
            if let Some(old) = self.watched_dir.take() {
                let _ = w.unwatch(&old);
            }
            if w.watch(&self.current_dir, RecursiveMode::NonRecursive).is_ok() {
                self.watched_dir = Some(self.current_dir.clone());
            }
        }
    }

    fn go_back(&mut self) {
        if let Some((prev, was_starred)) = self.history_back.pop() {
            self.history_fwd
                .push((self.current_dir.clone(), self.viewing_starred));
            if was_starred {
                self.navigate_to_starred(false);
            } else {
                self.navigate_to(prev, false);
            }
        }
    }

    fn go_forward(&mut self) {
        if let Some((next, was_starred)) = self.history_fwd.pop() {
            self.history_back
                .push((self.current_dir.clone(), self.viewing_starred));
            if was_starred {
                self.navigate_to_starred(false);
            } else {
                self.navigate_to(next, false);
            }
        }
    }

    fn go_up(&mut self) {
        if self.viewing_starred {
            self.navigate_to(self.current_dir.clone(), true);
        } else if let Some(parent) = self.current_dir.parent() {
            self.navigate_to(parent.to_path_buf(), true);
        }
    }

    fn refresh_entries(&mut self) {
        let mut list = Vec::new();
        if self.viewing_starred {
            let paths: Vec<PathBuf> = self.starred.iter().cloned().collect();
            for path in paths {
                if let Some(ent) = build_file_entry(&path) {
                    list.push(ent);
                }
            }
        } else {
            match fs::read_dir(&self.current_dir) {
                Ok(rd) => {
                    for item in rd.flatten() {
                        if let Some(ent) = build_file_entry(&item.path()) {
                            list.push(ent);
                        }
                    }
                }
                Err(e) => {
                    self.set_toast(format!("Erro ao ler pasta: {e}"), true);
                }
            }
            self.free_space_str = query_free_space(&self.current_dir);
        }
        self.entries = list;
        self.sort_entries();
        let existing: HashSet<PathBuf> = self.entries.iter().map(|e| e.path.clone()).collect();
        self.selected_paths.retain(|p| existing.contains(p));
        self.dirty = true;
    }

    fn sort_entries(&mut self) {
        let field = self.sort_field;
        let asc = self.sort_asc;
        self.entries.sort_by(|a, b| {
            let dir_ord = b.is_dir.cmp(&a.is_dir);
            if dir_ord != std::cmp::Ordering::Equal {
                return dir_ord;
            }
            let ord = match field {
                SortField::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortField::Kind => a
                    .kind
                    .type_label()
                    .cmp(b.kind.type_label())
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortField::Size => a
                    .size_bytes
                    .cmp(&b.size_bytes)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortField::Modified => a
                    .modified_ts
                    .cmp(&b.modified_ts)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
            };
            if asc { ord } else { ord.reverse() }
        });
    }

    fn toggle_sort(&mut self, field: SortField) {
        if self.sort_field == field {
            self.sort_asc = !self.sort_asc;
        } else {
            self.sort_field = field;
            self.sort_asc = !matches!(field, SortField::Modified | SortField::Size);
        }
        self.sort_entries();
        self.dirty = true;
    }

    fn set_toast(&mut self, msg: String, is_error: bool) {
        self.toast = Some((msg, is_error, Instant::now()));
        self.dirty = true;
    }

    fn toggle_star(&mut self, path: &Path) {
        if self.starred.contains(path) {
            self.starred.remove(path);
            self.set_toast("Removido de Com Estrela".to_string(), false);
        } else {
            self.starred.insert(path.to_path_buf());
            self.set_toast("Adicionado a Com Estrela".to_string(), false);
        }
        save_starred(&self.starred_file, &self.starred);
        if self.viewing_starred {
            self.refresh_entries();
        }
        self.dirty = true;
    }

    fn visible_entries(&self) -> Vec<FileEntry> {
        let q = self.search_query.trim().to_lowercase();
        self.entries
            .iter()
            .filter(|e| {
                (self.show_hidden || !e.is_hidden)
                    && (q.is_empty() || e.name.to_lowercase().contains(&q))
            })
            .cloned()
            .collect()
    }

    fn is_selected(&self, path: &Path) -> bool {
        self.selected_paths.iter().any(|p| p == path)
    }

    fn select_single(&mut self, path: PathBuf) {
        self.selected_paths.clear();
        self.selected_paths.push(path.clone());
        self.selection_anchor = Some(path);
        self.dirty = true;
    }

    fn handle_item_click(&mut self, path: PathBuf, visible: &[FileEntry], mods: Modifiers) {
        if mods.ctrl || mods.command {
            if let Some(pos) = self.selected_paths.iter().position(|p| p == &path) {
                self.selected_paths.remove(pos);
            } else {
                self.selected_paths.push(path.clone());
            }
            self.selection_anchor = Some(path);
        } else if mods.shift {
            if let Some(anchor) = self.selection_anchor.clone() {
                let a_idx = visible.iter().position(|e| e.path == anchor);
                let b_idx = visible.iter().position(|e| e.path == path);
                if let (Some(i0), Some(i1)) = (a_idx, b_idx) {
                    let (lo, hi) = if i0 <= i1 { (i0, i1) } else { (i1, i0) };
                    self.selected_paths = visible[lo..=hi].iter().map(|e| e.path.clone()).collect();
                    return;
                }
            }
            self.select_single(path);
        } else {
            self.select_single(path);
        }
    }

    fn open_entry(&mut self, entry: &FileEntry) {
        if entry.is_dir {
            self.navigate_to(entry.path.clone(), true);
        } else {
            let p = entry.path.clone();
            std::thread::spawn(move || {
                let _ = Command::new("xdg-open")
                    .arg(&p)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
            });
            self.set_toast(format!("Abrindo {}", entry.name), false);
        }
    }

    fn open_in_terminal(&mut self, dir: &Path) {
        let d = dir.to_path_buf();
        std::thread::spawn(move || {
            if let Ok(term) = std::env::var("TERMINAL") {
                if !term.trim().is_empty()
                    && Command::new(&term)
                        .current_dir(&d)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .is_ok()
                {
                    return;
                }
            }
            for (bin, flag) in [
                ("foot", Some("--working-directory")),
                ("alacritty", Some("--working-directory")),
                ("kitty", Some("--directory")),
                ("wezterm", Some("start")),
                ("gnome-terminal", Some("--working-directory")),
                ("konsole", Some("--workdir")),
            ] {
                let mut cmd = Command::new(bin);
                if bin == "wezterm" {
                    cmd.args(["start", "--cwd"]).arg(&d);
                } else if let Some(f) = flag {
                    cmd.arg(f).arg(&d);
                }
                if cmd
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .is_ok()
                {
                    return;
                }
            }
        });
        self.set_toast("Abrindo terminal na pasta".to_string(), false);
    }

    fn copy_text_to_clipboard(&mut self, ctx: &Context, text: String, toast_msg: &str) {
        ctx.copy_text(text.clone());
        std::thread::spawn(move || {
            for (bin, args) in [
                ("wl-copy", vec![]),
                ("xclip", vec!["-selection", "clipboard"]),
                ("xsel", vec!["--clipboard", "--input"]),
            ] {
                if let Ok(mut child) = Command::new(bin)
                    .args(&args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    use std::io::Write;
                    if let Some(mut stdin) = child.stdin.take() {
                        let _ = stdin.write_all(text.as_bytes());
                    }
                    let _ = child.wait();
                    return;
                }
            }
        });
        self.set_toast(toast_msg.to_string(), false);
    }

    fn start_copy_or_cut(&mut self, mode: ClipboardMode) {
        if self.selected_paths.is_empty() {
            return;
        }
        let count = self.selected_paths.len();
        self.clipboard = Some(FileClipboard {
            mode,
            paths: self.selected_paths.clone(),
        });
        let verb = match mode {
            ClipboardMode::Copy => "copiado(s)",
            ClipboardMode::Cut => "recortado(s)",
        };
        self.set_toast(format!("{count} item(ns) {verb}"), false);
    }

    fn paste_clipboard(&mut self, target_dir: PathBuf) {
        let Some(clip) = self.clipboard.clone() else {
            return;
        };
        if !target_dir.is_dir() {
            return;
        }
        let tx = self.bg_tx.clone();
        let is_cut = clip.mode == ClipboardMode::Cut;
        if is_cut {
            self.clipboard = None;
        }
        std::thread::spawn(move || {
            let mut ok_count = 0usize;
            let mut last_err: Option<String> = None;
            let mut created = Vec::new();
            let mut moved_pairs = Vec::new();

            for src in &clip.paths {
                let Some(fname) = src.file_name() else {
                    continue;
                };
                if src == &target_dir || target_dir.starts_with(src) {
                    last_err = Some("Não é possível colar uma pasta dentro dela mesma".to_string());
                    continue;
                }
                let mut dest = target_dir.join(fname);
                if dest.exists() {
                    if is_cut && dest == *src {
                        continue;
                    }
                    dest = unique_dest_path(&target_dir, fname);
                }
                let res = if is_cut {
                    move_path(src, &dest)
                } else {
                    copy_path_recursive(src, &dest)
                };
                match res {
                    Ok(_) => {
                        ok_count += 1;
                        if is_cut {
                            moved_pairs.push((src.clone(), dest));
                        } else {
                            created.push(dest);
                        }
                    }
                    Err(e) => last_err = Some(e.to_string()),
                }
            }
            let undo = if is_cut && !moved_pairs.is_empty() {
                Some(UndoAction::Moved { pairs: moved_pairs })
            } else if !is_cut && !created.is_empty() {
                Some(UndoAction::CopyCreated { created })
            } else {
                None
            };
            let has_err = last_err.is_some();
            let msg = if let Some(err) = last_err {
                format!("{ok_count} colado(s); erro: {err}")
            } else if is_cut {
                format!("{ok_count} item(ns) movido(s)")
            } else {
                format!("{ok_count} item(ns) colado(s)")
            };
            let _ = tx.send(BgEvent::OpResult {
                msg,
                is_err: has_err,
                refresh: true,
                undo,
            });
        });
    }

    fn duplicate_entries(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let tx = self.bg_tx.clone();
        std::thread::spawn(move || {
            let mut ok = 0usize;
            let mut created = Vec::new();
            let mut last_err = None;
            for src in paths {
                if let (Some(parent), Some(fname)) = (src.parent(), src.file_name()) {
                    let dest = unique_dest_path(parent, fname);
                    match copy_path_recursive(&src, &dest) {
                        Ok(_) => {
                            ok += 1;
                            created.push(dest);
                        }
                        Err(e) => last_err = Some(e.to_string()),
                    }
                }
            }
            let undo = if created.is_empty() {
                None
            } else {
                Some(UndoAction::CopyCreated { created })
            };
            let has_err = last_err.is_some();
            let msg = if let Some(e) = last_err {
                format!("Duplicados {ok}; erro: {e}")
            } else {
                format!("{ok} item(ns) duplicado(s)")
            };
            let _ = tx.send(BgEvent::OpResult {
                msg,
                is_err: has_err,
                refresh: true,
                undo,
            });
        });
    }

    fn move_to_trash(&mut self, targets: Vec<PathBuf>) {
        if targets.is_empty() {
            return;
        }
        if self.is_in_trash() {
            self.delete_confirm_targets = Some(targets);
            return;
        }
        let trash_files = self.trash_dir.clone();
        let trash_info = self.trash_info_dir.clone();
        let tx = self.bg_tx.clone();
        std::thread::spawn(move || {
            let _ = fs::create_dir_all(&trash_files);
            let _ = fs::create_dir_all(&trash_info);
            let mut moved = 0usize;
            let mut last_err = None;
            let mut trashed = Vec::new();
            let now_iso = Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();

            for src in &targets {
                let Some(fname) = src.file_name() else {
                    continue;
                };
                let dest = unique_dest_path(&trash_files, fname);
                let trash_name = dest
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("item")
                    .to_string();
                let info_path = trash_info.join(format!("{trash_name}.trashinfo"));
                let info_body = format!(
                    "[Trash Info]\nPath={}\nDeletionDate={}\n",
                    percent_encode_path(src),
                    now_iso
                );
                if let Err(e) = fs::write(&info_path, info_body) {
                    last_err = Some(e.to_string());
                    continue;
                }
                match move_path(src, &dest) {
                    Ok(_) => {
                        moved += 1;
                        trashed.push(dest);
                    }
                    Err(e) => {
                        let _ = fs::remove_file(&info_path);
                        last_err = Some(e.to_string());
                    }
                }
            }
            let undo = if trashed.is_empty() {
                None
            } else {
                Some(UndoAction::Trashed {
                    trash_items: trashed,
                })
            };
            let has_err = last_err.is_some();
            let msg = if let Some(err) = last_err {
                format!("Erro ao mover para Lixeira: {err}")
            } else {
                format!("{moved} item(ns) movido(s) para a Lixeira")
            };
            let _ = tx.send(BgEvent::OpResult {
                msg,
                is_err: has_err,
                refresh: true,
                undo,
            });
        });
    }

    fn restore_from_trash(&mut self, targets: Vec<PathBuf>) {
        let trash_info = self.trash_info_dir.clone();
        let tx = self.bg_tx.clone();
        std::thread::spawn(move || {
            let mut restored = 0usize;
            let mut last_err = None;
            for src in &targets {
                match restore_single_trash_item(src, &trash_info) {
                    Ok(_) => restored += 1,
                    Err(e) => last_err = Some(e),
                }
            }
            let has_err = last_err.is_some();
            let msg = if let Some(err) = last_err {
                format!("Erro ao restaurar: {err}")
            } else {
                format!("{restored} item(ns) restaurado(s)")
            };
            let _ = tx.send(BgEvent::OpResult {
                msg,
                is_err: has_err,
                refresh: true,
                undo: None,
            });
        });
    }

    fn undo_last(&mut self) {
        let Some(action) = self.undo_stack.pop() else {
            self.set_toast("Nada para desfazer".to_string(), false);
            return;
        };
        let tx = self.bg_tx.clone();
        let trash_info = self.trash_info_dir.clone();
        std::thread::spawn(move || {
            let res: Result<String, String> = match action {
                UndoAction::Rename { from, to } => fs::rename(&to, &from)
                    .map(|_| "Renomeação desfeita".to_string())
                    .map_err(|e| e.to_string()),
                UndoAction::CreatePath { path } => {
                    let r = if path.is_dir() {
                        fs::remove_dir(&path)
                    } else {
                        fs::remove_file(&path)
                    };
                    r.map(|_| "Criação desfeita".to_string())
                        .map_err(|e| e.to_string())
                }
                UndoAction::CopyCreated { created } => {
                    let mut cnt = 0usize;
                    for p in created {
                        let ok = if p.is_dir() && !p.is_symlink() {
                            fs::remove_dir_all(&p).is_ok()
                        } else {
                            fs::remove_file(&p).is_ok()
                        };
                        if ok {
                            cnt += 1;
                        }
                    }
                    Ok(format!("Cópia desfeita ({cnt} removido(s))"))
                }
                UndoAction::Moved { pairs } => {
                    let mut cnt = 0usize;
                    let mut err = None;
                    for (orig, curr) in pairs {
                        match move_path(&curr, &orig) {
                            Ok(_) => cnt += 1,
                            Err(e) => err = Some(e.to_string()),
                        }
                    }
                    if let Some(e) = err {
                        Err(e)
                    } else {
                        Ok(format!("Movimentação desfeita ({cnt} item(ns))"))
                    }
                }
                UndoAction::Trashed { trash_items } => {
                    let mut cnt = 0usize;
                    let mut err = None;
                    for p in trash_items {
                        match restore_single_trash_item(&p, &trash_info) {
                            Ok(_) => cnt += 1,
                            Err(e) => err = Some(e),
                        }
                    }
                    if let Some(e) = err {
                        Err(e)
                    } else {
                        Ok(format!("Restaurado(s) {cnt} item(ns) da Lixeira"))
                    }
                }
            };
            match res {
                Ok(msg) => {
                    let _ = tx.send(BgEvent::OpResult {
                        msg,
                        is_err: false,
                        refresh: true,
                        undo: None,
                    });
                }
                Err(e) => {
                    let _ = tx.send(BgEvent::OpResult {
                        msg: format!("Não foi possível desfazer: {e}"),
                        is_err: true,
                        refresh: true,
                        undo: None,
                    });
                }
            }
        });
    }

    fn empty_trash(&mut self) {
        let files_dir = self.trash_dir.clone();
        let info_dir = self.trash_info_dir.clone();
        let tx = self.bg_tx.clone();
        std::thread::spawn(move || {
            for dir in [&files_dir, &info_dir] {
                if let Ok(rd) = fs::read_dir(dir) {
                    for ent in rd.flatten() {
                        let p = ent.path();
                        if p.is_dir() && !p.is_symlink() {
                            let _ = fs::remove_dir_all(&p);
                        } else {
                            let _ = fs::remove_file(&p);
                        }
                    }
                }
            }
            let _ = tx.send(BgEvent::OpResult {
                msg: "Lixeira esvaziada".to_string(),
                is_err: false,
                refresh: true,
                undo: None,
            });
        });
    }

    fn mount_and_open_device(&mut self, dev: &DeviceEntry) {
        if let Some(ref mp) = dev.mount_point {
            self.navigate_to(mp.clone(), true);
            return;
        }
        let dev_node = dev.dev_node.clone();
        let label = dev.label.clone();
        let tx = self.bg_tx.clone();
        self.set_toast(format!("Montando {}…", label), false);
        std::thread::spawn(move || {
            let out = Command::new("udisksctl")
                .args(["mount", "--no-user-interaction", "-b", &dev_node])
                .output();
            match out {
                Ok(o) if o.status.success() => {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    let mp = parse_udisksctl_mountpoint(&stdout).or_else(|| {
                        scan_devices()
                            .into_iter()
                            .find(|d| d.dev_node == dev_node)
                            .and_then(|d| d.mount_point)
                    });
                    if let Some(mount_point) = mp {
                        let _ = tx.send(BgEvent::MountReady { mount_point, label });
                        let _ = tx.send(BgEvent::DevicesUpdated(scan_devices()));
                    } else {
                        let _ = tx.send(BgEvent::OpResult {
                            msg: format!("{} montado, mas ponto não encontrado", label),
                            is_err: true,
                            refresh: false,
                            undo: None,
                        });
                    }
                }
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    let _ = tx.send(BgEvent::OpResult {
                        msg: format!("Falha ao montar {}: {}", label, err),
                        is_err: true,
                        refresh: false,
                        undo: None,
                    });
                }
                Err(e) => {
                    let _ = tx.send(BgEvent::OpResult {
                        msg: format!("udisksctl indisponível: {e}"),
                        is_err: true,
                        refresh: false,
                        undo: None,
                    });
                }
            }
        });
    }

    fn unmount_device(&mut self, dev: &DeviceEntry) {
        let dev_node = dev.dev_node.clone();
        let label = dev.label.clone();
        let was_inside = dev
            .mount_point
            .as_ref()
            .is_some_and(|mp| self.current_dir.starts_with(mp));
        if was_inside {
            if let Some(home) = dirs::home_dir() {
                self.navigate_to(home, true);
            }
        }
        let tx = self.bg_tx.clone();
        std::thread::spawn(move || {
            let out = Command::new("udisksctl")
                .args(["unmount", "--no-user-interaction", "-b", &dev_node])
                .output();
            match out {
                Ok(o) if o.status.success() => {
                    let _ = tx.send(BgEvent::DevicesUpdated(scan_devices()));
                    let _ = tx.send(BgEvent::OpResult {
                        msg: format!("{} desmontado com segurança", label),
                        is_err: false,
                        refresh: false,
                        undo: None,
                    });
                }
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    let _ = tx.send(BgEvent::OpResult {
                        msg: format!("Erro ao desmontar {}: {}", label, err),
                        is_err: true,
                        refresh: false,
                        undo: None,
                    });
                }
                Err(e) => {
                    let _ = tx.send(BgEvent::OpResult {
                        msg: format!("Erro: {e}"),
                        is_err: true,
                        refresh: false,
                        undo: None,
                    });
                }
            }
        });
    }

    fn refresh_devices_async(&mut self) {
        self.last_device_scan = Instant::now();
        let tx = self.bg_tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(BgEvent::DevicesUpdated(scan_devices()));
        });
    }

    fn open_properties(&mut self, entry: FileEntry) {
        let sym_meta = fs::symlink_metadata(&entry.path).ok();
        let meta = fs::metadata(&entry.path).ok().or(sym_meta);
        let mode = meta.as_ref().map(|m| m.permissions().mode()).unwrap_or(0);
        let permissions_str = format_unix_mode(mode);
        let is_executable = (mode & 0o111) != 0;
        let owner_group = meta
            .as_ref()
            .map(|m| format!("{} : {}", lookup_user(m.uid()), lookup_group(m.gid())))
            .unwrap_or_else(|| "—".to_string());
        let accessed_str = meta
            .as_ref()
            .and_then(|m| m.accessed().ok())
            .map(format_system_time_full)
            .unwrap_or_else(|| "—".to_string());
        let created_str = meta
            .as_ref()
            .and_then(|m| m.created().ok())
            .map(format_system_time_full)
            .unwrap_or_else(|| "Não suportado".to_string());
        let modified_full_str = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .map(format_system_time_full)
            .unwrap_or_else(|| entry.modified_str.clone());

        let symlink_target = if entry.is_symlink {
            fs::read_link(&entry.path)
                .ok()
                .map(|p| p.display().to_string())
        } else {
            None
        };
        let parent_dir = entry
            .path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "/".to_string());
        let mime_type = entry.kind.mime_guess(&entry.ext_upper);
        let volume_str = query_volume_details(&entry.path);
        let is_dir = entry.is_dir;
        let scan_path = entry.path.clone();

        self.properties_modal = Some(PropertiesInfo {
            entry,
            parent_dir,
            mime_type,
            permissions_str,
            is_executable,
            owner_group,
            accessed_str,
            created_str,
            modified_full_str,
            volume_str,
            symlink_target,
            dir_stats: None,
            dir_scanning: is_dir,
        });

        if is_dir {
            let tx = self.bg_tx.clone();
            std::thread::spawn(move || {
                let (folders, files, bytes) = compute_dir_stats(&scan_path);
                let _ = tx.send(BgEvent::DirStatsReady {
                    path: scan_path,
                    folders,
                    files,
                    bytes,
                });
            });
        }
        self.dirty = true;
    }

    fn process_bg_events(&mut self) {
        let mut need_refresh = false;
        while let Ok(ev) = self.bg_rx.try_recv() {
            match ev {
                BgEvent::FsChanged => {
                    if self.last_fs_refresh.elapsed() >= Duration::from_millis(250) {
                        need_refresh = true;
                    }
                }
                BgEvent::OpResult {
                    msg,
                    is_err,
                    refresh,
                    undo,
                } => {
                    if let Some(u) = undo {
                        self.undo_stack.push(u);
                    }
                    self.set_toast(msg, is_err);
                    if refresh {
                        need_refresh = true;
                    }
                }
                BgEvent::MountReady { mount_point, label } => {
                    self.set_toast(format!("{} montado", label), false);
                    self.navigate_to(mount_point, true);
                }
                BgEvent::DevicesUpdated(devs) => {
                    if self.devices != devs {
                        self.devices = devs;
                        self.dirty = true;
                    }
                }
                BgEvent::DirStatsReady {
                    path,
                    folders,
                    files,
                    bytes,
                } => {
                    if let Some(ref mut prop) = self.properties_modal {
                        if prop.entry.path == path {
                            prop.dir_stats = Some((folders, files, bytes));
                            prop.dir_scanning = false;
                            self.dirty = true;
                        }
                    }
                }
                BgEvent::ThumbReady(res) => {
                    let img = ColorImage::from_rgba_unmultiplied(res.size, &res.rgba);
                    let opts = TextureOptions {
                        magnification: TextureFilter::Linear,
                        minification: TextureFilter::Linear,
                        ..Default::default()
                    };
                    let tex = self.egui_ctx.load_texture(
                        format!("thumb:{}", res.path.display()),
                        img,
                        opts,
                    );
                    self.thumb_cache.insert(res.path, tex);
                    self.dirty = true;
                }
            }
        }
        if need_refresh {
            self.last_fs_refresh = Instant::now();
            self.refresh_entries();
        }
        if self.last_device_scan.elapsed() >= Duration::from_secs(4) {
            self.refresh_devices_async();
        }
    }

    fn request_thumbnail(&mut self, entry: &FileEntry) {
        if entry.kind != FileKind::Image || entry.size_bytes > 25 * 1024 * 1024 {
            return;
        }
        if self.thumb_cache.contains_key(&entry.path)
            || self.thumb_requested.contains(&entry.path)
        {
            return;
        }
        self.thumb_requested.insert(entry.path.clone());
        let _ = self.thumb_tx.send(entry.path.clone());
    }

    fn resize(&mut self, w: u32, h: u32) {
        self.width = w.max(640);
        self.height = h.max(420);
        let phys_w = ((self.width as f32) * self.scale_factor).round().max(1.0) as u32;
        let phys_h = ((self.height as f32) * self.scale_factor).round().max(1.0) as u32;
        self.surface_config.width = phys_w;
        self.surface_config.height = phys_h;
        self.wgpu_surface.configure(&self.device, &self.surface_config);
        self.configured = true;
        self.dirty = true;
    }

    fn render_frame(&mut self) {
        if !self.configured {
            return;
        }
        self.process_bg_events();
        self.egui_ctx.set_pixels_per_point(self.scale_factor);

        let elapsed = self.start_instant.elapsed().as_secs_f64();
        let raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(self.width as f32, self.height as f32),
            )),
            time: Some(elapsed),
            events: std::mem::take(&mut self.pending_events),
            ..Default::default()
        };

        let egui_ctx = self.egui_ctx.clone();
        let full_output = egui_ctx.run_ui(raw_input, |ui| {
            self.draw_ui(ui);
        });
        handle_egui_clipboard_output(&full_output.platform_output.commands);

        match std::mem::replace(&mut self.pending_window_action, WindowAction::None) {
            WindowAction::Drag => {
                if let Some(seat) = &self.seat {
                    self.window.move_(seat, self.last_pointer_serial);
                }
            }
            WindowAction::Resize(dir) => {
                if let Some(seat) = &self.seat {
                    self.window.resize(seat, self.last_pointer_serial, to_xdg_edge(dir));
                }
            }
            WindowAction::Minimize => {
                self.window.set_minimized();
            }
            WindowAction::ToggleMaximize => {
                if self.is_maximized {
                    self.window.unset_maximized();
                } else {
                    self.window.set_maximized();
                }
            }
            WindowAction::Close => {
                self.exit = true;
            }
            WindowAction::None => {}
        }

        let min_delay = full_output
            .viewport_output
            .values()
            .map(|v| v.repaint_delay)
            .min();
        let needs_repaint = min_delay.map(|d| d.is_zero()).unwrap_or(false);
        self.repaint_delay = min_delay.filter(|d| !d.is_zero() && *d < Duration::from_secs(60));
        self.dirty = needs_repaint;

        let frame_tex = match self.wgpu_surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(tex) => tex,
            wgpu::CurrentSurfaceTexture::Suboptimal(tex) => {
                self.wgpu_surface.configure(&self.device, &self.surface_config);
                tex
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.wgpu_surface.configure(&self.device, &self.surface_config);
                self.dirty = true;
                return;
            }
            _ => return,
        };

        let view = frame_tex
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        for (id, deltas) in &full_output.textures_delta.set {
            for image_delta in deltas {
                self.renderer
                    .update_texture(&self.device, &self.queue, *id, image_delta);
            }
        }

        let paint_jobs = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [self.surface_config.width, self.surface_config.height],
            pixels_per_point: full_output.pixels_per_point,
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rocketde_files_encoder"),
            });
        let user_cmd_bufs = self.renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &paint_jobs,
            &screen_descriptor,
        );
        {
            let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rocketde_files_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.07,
                            g: 0.078,
                            b: 0.105,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let mut static_pass = render_pass.forget_lifetime();
            self.renderer
                .render(&mut static_pass, &paint_jobs, &screen_descriptor);
        }

        for id in &full_output.textures_delta.free {
            self.renderer.free_texture(id);
        }

        let mut all_cmds = user_cmd_bufs;
        all_cmds.push(encoder.finish());
        self.queue.submit(all_cmds);
        self.queue.present(frame_tex);
    }

    fn handle_shortcuts(&mut self, ctx: &Context, visible: &[FileEntry]) {
        let esc = ctx.input(|i| i.key_pressed(Key::Escape));
        if esc {
            if self.context_menu.is_some() {
                self.context_menu = None;
                return;
            }
            if self.properties_modal.is_some() {
                self.properties_modal = None;
                return;
            }
            if self.delete_confirm_targets.is_some() {
                self.delete_confirm_targets = None;
                return;
            }
            if self.empty_trash_confirm {
                self.empty_trash_confirm = false;
                return;
            }
            if self.rename_target.is_some() {
                self.rename_target = None;
                return;
            }
            if self.new_folder_mode {
                self.new_folder_mode = false;
                return;
            }
            if self.new_file_mode {
                self.new_file_mode = false;
                return;
            }
            if self.editing_path {
                self.editing_path = false;
                return;
            }
            if !self.search_query.is_empty() {
                self.search_query.clear();
                return;
            }
            self.selected_paths.clear();
            return;
        }

        let typing_in_input = ctx.egui_wants_keyboard_input()
            || self.editing_path
            || self.new_folder_mode
            || self.new_file_mode
            || self.rename_target.is_some();

        let (ctrl, shift, alt) =
            ctx.input(|i| (i.modifiers.ctrl || i.modifiers.command, i.modifiers.shift, i.modifiers.alt));

        if ctrl && ctx.input(|i| i.key_pressed(Key::L)) {
            self.path_input = self.current_dir.display().to_string();
            self.editing_path = true;
            self.focus_path = true;
            return;
        }
        if ctrl && ctx.input(|i| i.key_pressed(Key::F)) {
            self.focus_search = true;
            return;
        }
        if ctrl && ctx.input(|i| i.key_pressed(Key::H)) {
            self.show_hidden = !self.show_hidden;
            let msg = if self.show_hidden {
                "Mostrando arquivos ocultos"
            } else {
                "Arquivos ocultos escondidos"
            };
            self.set_toast(msg.to_string(), false);
            return;
        }
        if ctrl && shift && ctx.input(|i| i.key_pressed(Key::N)) && !self.is_in_trash() && !self.viewing_starred {
            self.new_folder_mode = true;
            self.focus_new_folder = true;
            self.new_folder_name.clear();
            return;
        }

        if typing_in_input || self.delete_confirm_targets.is_some() || self.properties_modal.is_some()
        {
            return;
        }

        if ctrl && ctx.input(|i| i.key_pressed(Key::A)) {
            self.selected_paths = visible.iter().map(|e| e.path.clone()).collect();
            self.dirty = true;
            return;
        }
        if ctrl && ctx.input(|i| i.key_pressed(Key::C)) {
            self.start_copy_or_cut(ClipboardMode::Copy);
            return;
        }
        if ctrl && ctx.input(|i| i.key_pressed(Key::X)) {
            self.start_copy_or_cut(ClipboardMode::Cut);
            return;
        }
        if ctrl && ctx.input(|i| i.key_pressed(Key::V)) && !self.is_in_trash() && !self.viewing_starred {
            let cur = self.current_dir.clone();
            self.paste_clipboard(cur);
            return;
        }
        if ctrl
            && ctx.input(|i| i.key_pressed(Key::D))
            && !self.selected_paths.is_empty()
            && !self.is_in_trash()
            && !self.viewing_starred
        {
            let paths = self.selected_paths.clone();
            self.duplicate_entries(paths);
            return;
        }
        if ctrl && ctx.input(|i| i.key_pressed(Key::Z)) {
            self.undo_last();
            return;
        }
        if alt && ctx.input(|i| i.key_pressed(Key::ArrowLeft)) {
            self.go_back();
            return;
        }
        if alt && ctx.input(|i| i.key_pressed(Key::ArrowRight)) {
            self.go_forward();
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::Backspace)) {
            self.go_up();
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::F5)) || (ctrl && ctx.input(|i| i.key_pressed(Key::R))) {
            self.refresh_entries();
            self.refresh_devices_async();
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::F2)) && !self.is_in_trash() && !self.viewing_starred {
            if let Some(first) = self.selected_paths.first().cloned() {
                if let Some(ent) = self.entries.iter().find(|e| e.path == first) {
                    self.rename_target = Some(ent.path.clone());
                    self.rename_buf = ent.name.clone();
                    self.focus_rename = true;
                }
            }
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::Delete)) && !self.selected_paths.is_empty() {
            let targets = self.selected_paths.clone();
            if shift || self.is_in_trash() {
                self.delete_confirm_targets = Some(targets);
            } else {
                self.move_to_trash(targets);
            }
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::Enter)) {
            if let Some(first) = self.selected_paths.first().cloned() {
                if let Some(ent) = visible.iter().find(|e| e.path == first).cloned() {
                    self.open_entry(&ent);
                }
            }
            return;
        }

        // Arrow & jump navigation across visible items
        if !visible.is_empty() {
            let jump_idx = if ctx.input(|i| i.key_pressed(Key::Home)) {
                Some(0usize)
            } else if ctx.input(|i| i.key_pressed(Key::End)) {
                Some(visible.len() - 1)
            } else {
                None
            };
            let cols = if self.view_mode == ViewMode::List {
                1isize
            } else {
                self.estimated_grid_cols() as isize
            };
            let step = if ctx.input(|i| i.key_pressed(Key::PageDown)) {
                Some(cols * 4)
            } else if ctx.input(|i| i.key_pressed(Key::PageUp)) {
                Some(-(cols * 4))
            } else if self.view_mode == ViewMode::List {
                if ctx.input(|i| i.key_pressed(Key::ArrowDown)) {
                    Some(1isize)
                } else if ctx.input(|i| i.key_pressed(Key::ArrowUp)) {
                    Some(-1isize)
                } else {
                    None
                }
            } else {
                if ctx.input(|i| i.key_pressed(Key::ArrowRight)) {
                    Some(1)
                } else if ctx.input(|i| i.key_pressed(Key::ArrowLeft)) {
                    Some(-1)
                } else if ctx.input(|i| i.key_pressed(Key::ArrowDown)) {
                    Some(cols)
                } else if ctx.input(|i| i.key_pressed(Key::ArrowUp)) {
                    Some(-cols)
                } else {
                    None
                }
            };

            let target_idx = if let Some(j) = jump_idx {
                Some(j)
            } else if let Some(delta) = step {
                let cur_idx = self
                    .selected_paths
                    .last()
                    .and_then(|p| visible.iter().position(|e| &e.path == p));
                Some(match cur_idx {
                    Some(i) => (i as isize + delta).clamp(0, visible.len() as isize - 1) as usize,
                    None => 0,
                })
            } else {
                None
            };

            if let Some(next_idx) = target_idx {
                if shift {
                    self.handle_item_click(visible[next_idx].path.clone(), visible, Modifiers::SHIFT);
                } else {
                    self.select_single(visible[next_idx].path.clone());
                }
                self.scroll_to_selection = true;
            }
        }
    }

    fn estimated_grid_cols(&self) -> usize {
        let avail_w = (self.width as f32 - 220.0 - 28.0).max(240.0);
        ((avail_w / 118.0).floor() as usize).max(2)
    }

    fn draw_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let accent = Color32::from_rgb(
            self.cfg.theme.accent_rgb[0],
            self.cfg.theme.accent_rgb[1],
            self.cfg.theme.accent_rgb[2],
        );
        let sidebar_bg = Color32::from_rgb(20, 22, 30);
        let toolbar_bg = Color32::from_rgb(24, 26, 36);
        let content_bg = Color32::from_rgb(18, 20, 27);
        let status_bg = Color32::from_rgb(22, 24, 33);
        let divider_col = Color32::from_rgba_unmultiplied(255, 255, 255, 14);
        let card_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 22));

        let visible = self.visible_entries();
        self.handle_shortcuts(&ctx, &visible);
        if let Some(dir) = detect_window_resize(&ctx, self.is_maximized) {
            self.pending_window_action = WindowAction::Resize(dir);
        }

        // 1. Left Sidebar (Places + Starred + Devices + Trash)
        egui::Panel::left("files_sidebar")
            .exact_size(220.0)
            .frame(
                Frame::NONE
                    .fill(sidebar_bg)
                    .inner_margin(Margin::symmetric(10, 12)),
            )
            .show(ui, |ui| {
                let panel_rect = ui.max_rect();
                ui.painter().line_segment(
                    [
                        pos2(panel_rect.max.x + 10.0, panel_rect.min.y - 12.0),
                        pos2(panel_rect.max.x + 10.0, panel_rect.max.y + 12.0),
                    ],
                    Stroke::new(1.0, divider_col),
                );

                ui.add_space(
                    (self.cfg.theme.spacing.header_height.clamp(32.0, 64.0) - 20.0).max(2.0),
                );
                sidebar_section_label(ui, "LOCAIS");
                if sidebar_row(
                    ui,
                    "Com Estrela",
                    SidebarIcon::Starred,
                    self.viewing_starred,
                    accent,
                ) {
                    self.navigate_to_starred(true);
                }

                let places = self.places.clone();
                for place in places {
                    let active = !self.viewing_starred && self.current_dir == place.path;
                    if sidebar_row(ui, &place.label, place.icon, active, accent) {
                        self.navigate_to(place.path, true);
                    }
                }

                if !self.devices.is_empty() {
                    ui.add_space(10.0);
                    sidebar_section_label(ui, "DISPOSITIVOS");
                    let devs = self.devices.clone();
                    for dev in devs {
                        let active = !self.viewing_starred
                            && dev
                                .mount_point
                                .as_ref()
                                .is_some_and(|mp| self.current_dir.starts_with(mp));
                        let (clicked, eject_clicked) =
                            sidebar_device_row(ui, &dev, active, accent);
                        if eject_clicked {
                            self.unmount_device(&dev);
                        } else if clicked {
                            self.mount_and_open_device(&dev);
                        }
                    }
                }

                ui.add_space(10.0);
                sidebar_section_label(ui, "SISTEMA");
                let trash_path = self.trash_dir.clone();
                if sidebar_row(ui, "Lixeira", SidebarIcon::Trash, self.is_in_trash(), accent) {
                    let _ = fs::create_dir_all(&trash_path);
                    self.navigate_to(trash_path, true);
                }
                if sidebar_row(
                    ui,
                    "Sistema (/)",
                    SidebarIcon::Root,
                    !self.viewing_starred && self.current_dir == Path::new("/"),
                    accent,
                ) {
                    self.navigate_to(PathBuf::from("/"), true);
                }

                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    let (r, resp) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 32.0),
                        Sense::click(),
                    );
                    let bg = if resp.hovered() {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 22)
                    } else {
                        Color32::from_rgba_unmultiplied(255, 255, 255, 11)
                    };
                    ui.painter().rect(
                        r,
                        CornerRadius::same(8),
                        bg,
                        Stroke::new(1.0, divider_col),
                        egui::StrokeKind::Inside,
                    );
                    draw_icon(
                        ui.painter(),
                        pos2(r.min.x + 16.0, r.center().y),
                        13.0,
                        IconKind::Terminal,
                        Color32::from_rgb(200, 210, 228),
                    );
                    ui.painter().text(
                        pos2(r.min.x + 32.0, r.center().y),
                        Align2::LEFT_CENTER,
                        "Abrir no Terminal",
                        FontId::proportional(12.5),
                        Color32::from_rgb(225, 232, 244),
                    );
                    if resp.clicked() {
                        let cur = self.current_dir.clone();
                        self.open_in_terminal(&cur);
                    }
                });
            });

        // 2. Top Toolbar
        let search_input_id = egui::Id::unique("files_search_input");
        let mut search_box_rect = Rect::NOTHING;

        let header_h = self.cfg.theme.spacing.header_height.clamp(32.0, 64.0);
        egui::Panel::top("files_toolbar")
            .exact_size(header_h)
            .frame(
                Frame::NONE
                    .fill(toolbar_bg)
                    .inner_margin(Margin::symmetric(12, 5)),
            )
            .show(ui, |ui| {
                ui.style_mut().interaction.selectable_labels = false;
                let pr = ui.max_rect();
                let drag_action = header_bar_drag_area(
                    ui,
                    Rect::from_min_max(
                        pos2(pr.min.x - 12.0, pr.min.y - 5.0),
                        pos2(pr.max.x + 12.0, pr.max.y + 5.0),
                    ),
                );
                if drag_action != WindowAction::None {
                    self.pending_window_action = drag_action;
                }
                ui.painter().line_segment(
                    [
                        pos2(pr.min.x - 12.0, pr.max.y + 5.0),
                        pos2(pr.max.x + 12.0, pr.max.y + 5.0),
                    ],
                    Stroke::new(1.0, divider_col),
                );

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;

                    if nav_icon_btn(ui, IconKind::ChevronLeft, !self.history_back.is_empty(), "Voltar (Alt+←)") {
                        self.go_back();
                    }
                    if nav_icon_btn(ui, IconKind::ChevronRight, !self.history_fwd.is_empty(), "Avançar (Alt+→)") {
                        self.go_forward();
                    }
                    if nav_vector_btn(ui, VectorBtn::ArrowUp, self.viewing_starred || self.current_dir.parent().is_some(), "Pasta acima (Backspace)") {
                        self.go_up();
                    }
                    if nav_vector_btn(ui, VectorBtn::Refresh, true, "Atualizar (F5)") {
                        self.refresh_entries();
                        self.refresh_devices_async();
                    }

                    ui.add_space(4.0);

                    let right_reserve = if self.is_in_trash() { 290.0 } else { 210.0 };
                    let addr_w = (ui.available_width() - right_reserve).max(160.0);

                    if self.editing_path {
                        Frame::NONE
                            .fill(Color32::from_rgb(14, 16, 22))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(Margin::symmetric(8, 4))
                            .stroke(Stroke::new(1.2, accent))
                            .show(ui, |ui| {
                                ui.set_width(addr_w - 16.0);
                                let resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.path_input)
                                        .frame(Frame::NONE)
                                        .desired_width(addr_w - 24.0),
                                );
                                if self.focus_path {
                                    resp.request_focus();
                                    self.focus_path = false;
                                }
                                if ctx.input(|i| i.key_pressed(Key::Enter)) {
                                    let target = PathBuf::from(self.path_input.trim());
                                    self.navigate_to(target, true);
                                }
                            });
                    } else {
                        let (addr_rect, addr_resp) = ui.allocate_exact_size(
                            Vec2::new(addr_w, 30.0),
                            Sense::click_and_drag(),
                        );
                        if header_bar_drag_from_response(&addr_resp, &ctx) == WindowAction::Drag {
                            self.pending_window_action = WindowAction::Drag;
                        }
                        ui.painter().rect(
                            addr_rect,
                            CornerRadius::same(8),
                            Color32::from_rgba_unmultiplied(255, 255, 255, 10),
                            card_stroke,
                            egui::StrokeKind::Inside,
                        );

                        let mut clicked_crumb: Option<PathBuf> = None;
                        let mut edit_clicked = false;
                        ui.scope_builder(
                            egui::UiBuilder::new().max_rect(addr_rect.shrink2(Vec2::new(6.0, 2.0))),
                            |ui| {
                                ui.horizontal_centered(|ui| {
                                    ui.spacing_mut().item_spacing.x = 2.0;
                                    if self.viewing_starred {
                                        ui.label(
                                            RichText::new("Com Estrela")
                                                .size(12.5)
                                                .strong()
                                                .color(Color32::WHITE),
                                        );
                                    } else if self.is_in_trash() {
                                        ui.label(
                                            RichText::new("Lixeira")
                                                .size(12.5)
                                                .strong()
                                                .color(Color32::WHITE),
                                        );
                                    } else {
                                        let ancestors: Vec<PathBuf> = self
                                            .current_dir
                                            .ancestors()
                                            .map(|p| p.to_path_buf())
                                            .collect::<Vec<_>>()
                                            .into_iter()
                                            .rev()
                                            .collect();
                                        let max_crumbs = if addr_w < 260.0 { 3 } else { 5 };
                                        let start_idx = ancestors.len().saturating_sub(max_crumbs);
                                        if start_idx > 0 {
                                            ui.label(
                                                RichText::new("…")
                                                    .size(12.0)
                                                    .color(Color32::from_rgb(130, 140, 160)),
                                            );
                                            ui.label(
                                                RichText::new("›")
                                                    .size(12.0)
                                                    .color(Color32::from_rgb(100, 110, 130)),
                                            );
                                        }
                                        for (i, anc) in ancestors[start_idx..].iter().enumerate() {
                                            let label = anc
                                                .file_name()
                                                .and_then(|s| s.to_str())
                                                .filter(|s| !s.is_empty())
                                                .unwrap_or("/");
                                            let is_last = start_idx + i + 1 == ancestors.len();
                                            let col = if is_last {
                                                Color32::WHITE
                                            } else {
                                                Color32::from_rgb(165, 175, 194)
                                            };
                                            let btn = ui.add(
                                                egui::Button::new(
                                                    RichText::new(label).size(12.0).color(col),
                                                )
                                                .fill(Color32::TRANSPARENT)
                                                .corner_radius(CornerRadius::same(5)),
                                            );
                                            if btn.clicked() {
                                                clicked_crumb = Some(anc.clone());
                                            }
                                            if !is_last {
                                                ui.label(
                                                    RichText::new("›")
                                                        .size(12.0)
                                                        .color(Color32::from_rgb(100, 110, 130)),
                                                );
                                            }
                                        }
                                    }
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if nav_vector_btn(ui, VectorBtn::Edit, true, "Editar caminho (Ctrl+L)") {
                                            edit_clicked = true;
                                        }
                                    });
                                });
                            },
                        );

                        if let Some(target) = clicked_crumb {
                            self.navigate_to(target, true);
                        } else if edit_clicked || addr_resp.double_clicked() {
                            self.path_input = self.current_dir.display().to_string();
                            self.editing_path = true;
                            self.focus_path = true;
                        }
                    }

                    // Right-side toolbar controls
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;

                        let win_act = window_controls_right(ui, &self.cfg.theme, self.is_maximized);
                        if win_act != WindowAction::None {
                            self.pending_window_action = win_act;
                        }

                        ui.add_space(4.0);

                        Frame::NONE
                            .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 10))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(Margin::same(2))
                            .stroke(card_stroke)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 2.0;
                                    if seg_icon_btn(
                                        ui,
                                        VectorBtn::Grid,
                                        self.view_mode == ViewMode::Grid,
                                        accent,
                                        "Visualização em Grade",
                                    ) {
                                        self.view_mode = ViewMode::Grid;
                                    }
                                    if seg_icon_btn(
                                        ui,
                                        VectorBtn::List,
                                        self.view_mode == ViewMode::List,
                                        accent,
                                        "Visualização em Lista",
                                    ) {
                                        self.view_mode = ViewMode::List;
                                    }
                                });
                            });

                        if self.is_in_trash()
                            && !self.entries.is_empty()
                            && pill_btn(ui, "Esvaziar", Color32::from_rgb(200, 58, 52)).clicked()
                        {
                            self.empty_trash_confirm = true;
                        }

                        let search_frame = Frame::NONE
                            .fill(Color32::from_rgba_unmultiplied(255, 255, 255, 10))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(Margin::symmetric(8, 4))
                            .stroke(card_stroke)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 5.0;
                                    let (sr, _) = ui.allocate_exact_size(Vec2::splat(13.0), Sense::hover());
                                    draw_icon(
                                        ui.painter(),
                                        sr.center(),
                                        12.0,
                                        IconKind::Search,
                                        Color32::from_rgb(145, 155, 175),
                                    );
                                    let resp = ui.add(
                                        egui::TextEdit::singleline(&mut self.search_query)
                                            .id(search_input_id)
                                            .frame(Frame::NONE)
                                            .hint_text("Filtrar...")
                                            .desired_width(110.0),
                                    );
                                    if self.focus_search {
                                        resp.request_focus();
                                        self.focus_search = false;
                                    }
                                    if !self.search_query.is_empty()
                                        && nav_vector_btn(ui, VectorBtn::Close, true, "Limpar filtro")
                                    {
                                        self.search_query.clear();
                                    }
                                });
                            });
                        search_box_rect = search_frame.response.rect;
                    });
                });
            });

        // 3. Bottom Status Bar
        egui::Panel::bottom("files_status")
            .exact_size(28.0)
            .frame(
                Frame::NONE
                    .fill(status_bg)
                    .inner_margin(Margin::symmetric(14, 5)),
            )
            .show(ui, |ui| {
                let sr = ui.max_rect();
                ui.painter().line_segment(
                    [
                        pos2(sr.min.x - 14.0, sr.min.y - 5.0),
                        pos2(sr.max.x + 14.0, sr.min.y - 5.0),
                    ],
                    Stroke::new(1.0, divider_col),
                );
                ui.horizontal(|ui| {
                    let vis_count = visible.len();
                    let hidden_cnt = self.entries.iter().filter(|e| e.is_hidden).count();
                    let status_txt = if self.selected_paths.len() > 1 {
                        let total_sz: u64 = self
                            .entries
                            .iter()
                            .filter(|e| !e.is_dir && self.selected_paths.contains(&e.path))
                            .map(|e| e.size_bytes)
                            .sum();
                        if total_sz > 0 {
                            format!(
                                "{} itens selecionados ({})",
                                self.selected_paths.len(),
                                format_bytes(total_sz)
                            )
                        } else {
                            format!("{} itens selecionados", self.selected_paths.len())
                        }
                    } else if let Some(sel) = self.selected_paths.first() {
                        if let Some(ent) = self.entries.iter().find(|e| &e.path == sel) {
                            if ent.is_dir {
                                format!("{} • Pasta • {}", ent.name, ent.modified_str)
                            } else {
                                format!(
                                    "{} • {} • {}",
                                    ent.name,
                                    format_bytes(ent.size_bytes),
                                    ent.modified_str
                                )
                            }
                        } else {
                            format!("{vis_count} itens")
                        }
                    } else if hidden_cnt > 0 && !self.show_hidden {
                        format!("{vis_count} itens ({hidden_cnt} ocultos)")
                    } else {
                        format!("{vis_count} itens")
                    };

                    ui.label(
                        RichText::new(status_txt)
                            .size(11.5)
                            .color(Color32::from_rgb(160, 170, 190)),
                    );

                    if !self.free_space_str.is_empty() {
                        ui.label(
                            RichText::new(format!("  •  Livre: {}", self.free_space_str))
                                .size(11.5)
                                .color(Color32::from_rgb(125, 135, 155)),
                        );
                    }

                    if let Some(ref clip) = self.clipboard {
                        ui.label(
                            RichText::new(format!(
                                "  •  {} {} na área de transferência",
                                clip.paths.len(),
                                if clip.mode == ClipboardMode::Cut {
                                    "recortado(s)"
                                } else {
                                    "copiado(s)"
                                }
                            ))
                            .size(11.5)
                            .color(accent),
                        );
                    }

                    if let Some((ref msg, is_err, created)) = self.toast {
                        if created.elapsed() < Duration::from_secs(3) {
                            let col = if is_err {
                                Color32::from_rgb(255, 130, 120)
                            } else {
                                Color32::from_rgb(115, 225, 150)
                            };
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(RichText::new(msg).size(11.5).color(col));
                            });
                            ctx.request_repaint_after(Duration::from_millis(250));
                        } else {
                            self.toast = None;
                        }
                    }
                });
            });

        // 4. Central Files Area
        egui::CentralPanel::default()
            .frame(
                Frame::NONE
                    .fill(content_bg)
                    .inner_margin(Margin::symmetric(14, 12)),
            )
            .show(ui, |ui| {
                let central_rect = ui.max_rect();
                let mut ui_bar_clicked = false;

                // List View Header
                if self.view_mode == ViewMode::List && !visible.is_empty() {
                    let hdr_rect = ui
                        .allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::hover())
                        .0;
                    ui.painter().line_segment(
                        [
                            pos2(hdr_rect.min.x, hdr_rect.max.y),
                            pos2(hdr_rect.max.x, hdr_rect.max.y),
                        ],
                        Stroke::new(1.0, divider_col),
                    );
                    let kind_x = hdr_rect.max.x - 270.0;
                    let size_x = hdr_rect.max.x - 165.0;
                    let mod_x = hdr_rect.max.x - 12.0;

                    if list_col_header(
                        ui,
                        Rect::from_min_max(
                            pos2(hdr_rect.min.x + 8.0, hdr_rect.min.y),
                            pos2(kind_x - 8.0, hdr_rect.max.y),
                        ),
                        "Nome",
                        SortField::Name,
                        self.sort_field,
                        self.sort_asc,
                        Align2::LEFT_CENTER,
                    ) {
                        self.toggle_sort(SortField::Name);
                        ui_bar_clicked = true;
                    }
                    if list_col_header(
                        ui,
                        Rect::from_min_max(
                            pos2(kind_x - 60.0, hdr_rect.min.y),
                            pos2(kind_x + 12.0, hdr_rect.max.y),
                        ),
                        "Tipo",
                        SortField::Kind,
                        self.sort_field,
                        self.sort_asc,
                        Align2::RIGHT_CENTER,
                    ) {
                        self.toggle_sort(SortField::Kind);
                        ui_bar_clicked = true;
                    }
                    if list_col_header(
                        ui,
                        Rect::from_min_max(
                            pos2(size_x - 70.0, hdr_rect.min.y),
                            pos2(size_x + 12.0, hdr_rect.max.y),
                        ),
                        "Tamanho",
                        SortField::Size,
                        self.sort_field,
                        self.sort_asc,
                        Align2::RIGHT_CENTER,
                    ) {
                        self.toggle_sort(SortField::Size);
                        ui_bar_clicked = true;
                    }
                    if list_col_header(
                        ui,
                        Rect::from_min_max(
                            pos2(size_x + 20.0, hdr_rect.min.y),
                            pos2(mod_x, hdr_rect.max.y),
                        ),
                        "Modificado",
                        SortField::Modified,
                        self.sort_field,
                        self.sort_asc,
                        Align2::RIGHT_CENTER,
                    ) {
                        self.toggle_sort(SortField::Modified);
                        ui_bar_clicked = true;
                    }
                    ui.add_space(4.0);
                }

                let mut to_open: Option<FileEntry> = None;
                let mut clicked_item: Option<PathBuf> = None;
                let mut star_toggle_path: Option<PathBuf> = None;
                let mut to_context: Option<(ContextTarget, Pos2)> = None;
                let mut item_rects: Vec<(PathBuf, Rect)> = Vec::with_capacity(visible.len());
                let mods = ctx.input(|i| i.modifiers);

                if visible.is_empty() {
                    let has_filter = !self.search_query.trim().is_empty();
                    let empty_msg = if has_filter {
                        "Nenhum item corresponde ao filtro"
                    } else if self.viewing_starred {
                        "Nenhum item com estrela ainda"
                    } else if self.is_in_trash() {
                        "A Lixeira está vazia"
                    } else {
                        "Pasta vazia"
                    };
                    let empty_resp = ui.allocate_response(ui.available_size(), Sense::click());
                    ui.painter().text(
                        empty_resp.rect.center() - Vec2::new(0.0, if has_filter { 14.0 } else { 0.0 }),
                        Align2::CENTER_CENTER,
                        empty_msg,
                        FontId::proportional(14.5),
                        Color32::from_rgb(125, 136, 158),
                    );
                    if has_filter {
                        let btn_rect = Rect::from_center_size(
                            empty_resp.rect.center() + Vec2::new(0.0, 16.0),
                            Vec2::new(112.0, 26.0),
                        );
                        let mut clear_clicked = false;
                        ui.scope_builder(egui::UiBuilder::new().max_rect(btn_rect), |ui| {
                            if pill_btn(ui, "Limpar filtro", accent).clicked() {
                                clear_clicked = true;
                            }
                        });
                        if clear_clicked {
                            self.search_query.clear();
                        }
                    }
                    if empty_resp.secondary_clicked() {
                        let p = ctx
                            .input(|i| i.pointer.interact_pos())
                            .unwrap_or(empty_resp.rect.center());
                        to_context = Some((ContextTarget::Background, p));
                    }
                } else {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| match self.view_mode {
                            ViewMode::Grid => {
                                let avail_w = ui.available_width().max(240.0);
                                let gap = 10.0;
                                let cols = ((avail_w / 118.0).floor() as usize).max(2);
                                let card_w = (avail_w - (cols as f32 - 1.0) * gap) / cols as f32;
                                let card_h = 114.0;

                                for chunk in visible.chunks(cols) {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = gap;
                                        for entry in chunk {
                                            self.request_thumbnail(entry);
                                            let selected = self.is_selected(&entry.path);
                                            let is_starred = self.starred.contains(&entry.path);
                                            let is_cut = self.clipboard.as_ref().is_some_and(|c| {
                                                c.mode == ClipboardMode::Cut
                                                    && c.paths.contains(&entry.path)
                                            });
                                            let (rect, resp) = ui.allocate_exact_size(
                                                Vec2::new(card_w, card_h),
                                                Sense::click(),
                                            );
                                            item_rects.push((entry.path.clone(), rect));
                                            if self.scroll_to_selection
                                                && self.selected_paths.last() == Some(&entry.path)
                                            {
                                                resp.scroll_to_me(Some(Align::Center));
                                                self.scroll_to_selection = false;
                                            }

                                            if selected {
                                                ui.painter().rect(
                                                    rect,
                                                    CornerRadius::same(11),
                                                    Color32::from_rgba_unmultiplied(
                                                        accent.r(),
                                                        accent.g(),
                                                        accent.b(),
                                                        52,
                                                    ),
                                                    Stroke::new(1.4, accent),
                                                    egui::StrokeKind::Inside,
                                                );
                                            } else if resp.hovered() {
                                                ui.painter().rect_filled(
                                                    rect,
                                                    CornerRadius::same(11),
                                                    Color32::from_rgba_unmultiplied(
                                                        255, 255, 255, 11,
                                                    ),
                                                );
                                            }

                                            let alpha =
                                                if is_cut || entry.is_hidden { 0.55 } else { 1.0 };
                                            let icon_rect = Rect::from_center_size(
                                                pos2(rect.center().x, rect.min.y + 38.0),
                                                Vec2::new(50.0, 48.0),
                                            );
                                            if let Some(tex) = self.thumb_cache.get(&entry.path) {
                                                draw_thumbnail_in_rect(
                                                    ui.painter(),
                                                    icon_rect,
                                                    tex,
                                                    alpha,
                                                );
                                            } else {
                                                draw_file_icon(
                                                    ui.painter(),
                                                    icon_rect,
                                                    entry,
                                                    accent,
                                                    alpha,
                                                );
                                            }

                                            // Star button in top-right of card
                                            let star_rect = Rect::from_min_size(
                                                pos2(rect.max.x - 22.0, rect.min.y + 5.0),
                                                Vec2::splat(16.0),
                                            );
                                            let ptr_pos = ctx.input(|i| i.pointer.hover_pos());
                                            let star_hovered =
                                                ptr_pos.is_some_and(|p| star_rect.contains(p));
                                            if is_starred || resp.hovered() || star_hovered {
                                                let s_col = if is_starred {
                                                    Color32::from_rgb(255, 204, 41)
                                                } else if star_hovered {
                                                    Color32::WHITE
                                                } else {
                                                    Color32::from_rgba_unmultiplied(
                                                        255, 255, 255, 110,
                                                    )
                                                };
                                                draw_icon(
                                                    ui.painter(),
                                                    star_rect.center(),
                                                    12.0,
                                                    if is_starred {
                                                        IconKind::Star
                                                    } else {
                                                        IconKind::StarOutline
                                                    },
                                                    s_col,
                                                );
                                            }

                                            let text_rect = Rect::from_min_max(
                                                pos2(rect.min.x + 6.0, rect.min.y + 68.0),
                                                pos2(rect.max.x - 6.0, rect.max.y - 4.0),
                                            );
                                            let was_truncated = draw_wrapped_label(
                                                ui.painter(),
                                                text_rect,
                                                &entry.name,
                                                alpha,
                                                selected,
                                            );
                                            if was_truncated {
                                                resp.clone().on_hover_text(&entry.name);
                                            }

                                            if resp.clicked() && star_hovered {
                                                star_toggle_path = Some(entry.path.clone());
                                            } else if resp.double_clicked() {
                                                to_open = Some(entry.clone());
                                            } else if resp.clicked() {
                                                clicked_item = Some(entry.path.clone());
                                            } else if resp.secondary_clicked() {
                                                if !selected {
                                                    clicked_item = Some(entry.path.clone());
                                                }
                                                let p = ctx
                                                    .input(|i| i.pointer.interact_pos())
                                                    .unwrap_or(rect.center());
                                                to_context =
                                                    Some((ContextTarget::Entry(entry.clone()), p));
                                            }
                                        }
                                    });
                                    ui.add_space(8.0);
                                }
                            }
                            ViewMode::List => {
                                for (idx, entry) in visible.iter().enumerate() {
                                    self.request_thumbnail(entry);
                                    let selected = self.is_selected(&entry.path);
                                    let is_starred = self.starred.contains(&entry.path);
                                    let is_cut = self.clipboard.as_ref().is_some_and(|c| {
                                        c.mode == ClipboardMode::Cut && c.paths.contains(&entry.path)
                                    });
                                    let (rect, resp) = ui.allocate_exact_size(
                                        Vec2::new(ui.available_width(), 32.0),
                                        Sense::click(),
                                    );
                                    item_rects.push((entry.path.clone(), rect));
                                    if self.scroll_to_selection
                                        && self.selected_paths.last() == Some(&entry.path)
                                    {
                                        resp.scroll_to_me(Some(Align::Center));
                                        self.scroll_to_selection = false;
                                    }

                                    let bg = if selected {
                                        Color32::from_rgba_unmultiplied(
                                            accent.r(),
                                            accent.g(),
                                            accent.b(),
                                            58,
                                        )
                                    } else if resp.hovered() {
                                        Color32::from_rgba_unmultiplied(255, 255, 255, 14)
                                    } else if idx % 2 == 1 {
                                        Color32::from_rgba_unmultiplied(255, 255, 255, 4)
                                    } else {
                                        Color32::TRANSPARENT
                                    };
                                    ui.painter().rect_filled(rect, CornerRadius::same(7), bg);
                                    if selected {
                                        ui.painter().rect_stroke(
                                            rect,
                                            CornerRadius::same(7),
                                            Stroke::new(1.0, accent),
                                            egui::StrokeKind::Inside,
                                        );
                                    }

                                    let alpha = if is_cut || entry.is_hidden { 0.55 } else { 1.0 };
                                    let icon_r = Rect::from_center_size(
                                        pos2(rect.min.x + 20.0, rect.center().y),
                                        Vec2::new(22.0, 22.0),
                                    );
                                    if let Some(tex) = self.thumb_cache.get(&entry.path) {
                                        draw_thumbnail_in_rect(ui.painter(), icon_r, tex, alpha);
                                    } else {
                                        draw_file_icon(ui.painter(), icon_r, entry, accent, alpha);
                                    }

                                    let kind_x = rect.max.x - 270.0;
                                    let size_x = rect.max.x - 165.0;
                                    let mod_x = rect.max.x - 12.0;
                                    let star_x = kind_x - 78.0;

                                    let name_clip = Rect::from_min_max(
                                        pos2(rect.min.x + 38.0, rect.min.y),
                                        pos2((star_x - 14.0).max(rect.min.x + 48.0), rect.max.y),
                                    );
                                    let name_painter = ui.painter().with_clip_rect(name_clip);
                                    name_painter.text(
                                        pos2(name_clip.min.x, rect.center().y),
                                        Align2::LEFT_CENTER,
                                        &entry.name,
                                        FontId::proportional(13.0),
                                        apply_alpha(Color32::WHITE, alpha),
                                    );

                                    let star_rect = Rect::from_center_size(
                                        pos2(star_x, rect.center().y),
                                        Vec2::splat(18.0),
                                    );
                                    let ptr_pos = ctx.input(|i| i.pointer.hover_pos());
                                    let star_hovered =
                                        ptr_pos.is_some_and(|p| star_rect.contains(p));
                                    if is_starred || resp.hovered() || star_hovered {
                                        let s_col = if is_starred {
                                            Color32::from_rgb(255, 204, 41)
                                        } else if star_hovered {
                                            Color32::WHITE
                                        } else {
                                            Color32::from_rgba_unmultiplied(255, 255, 255, 100)
                                        };
                                        draw_icon(
                                            ui.painter(),
                                            star_rect.center(),
                                            11.5,
                                            if is_starred {
                                                IconKind::Star
                                            } else {
                                                IconKind::StarOutline
                                            },
                                            s_col,
                                        );
                                    }

                                    let meta_col =
                                        apply_alpha(Color32::from_rgb(155, 165, 185), alpha);
                                    ui.painter().text(
                                        pos2(kind_x, rect.center().y),
                                        Align2::RIGHT_CENTER,
                                        entry.kind.type_label(),
                                        FontId::proportional(11.5),
                                        meta_col,
                                    );
                                    let size_txt = if entry.is_dir {
                                        "—".to_string()
                                    } else {
                                        format_bytes(entry.size_bytes)
                                    };
                                    ui.painter().text(
                                        pos2(size_x, rect.center().y),
                                        Align2::RIGHT_CENTER,
                                        size_txt,
                                        FontId::proportional(11.5),
                                        meta_col,
                                    );
                                    ui.painter().text(
                                        pos2(mod_x, rect.center().y),
                                        Align2::RIGHT_CENTER,
                                        &entry.modified_str,
                                        FontId::proportional(11.5),
                                        meta_col,
                                    );

                                    if resp.clicked() && star_hovered {
                                        star_toggle_path = Some(entry.path.clone());
                                    } else if resp.double_clicked() {
                                        to_open = Some(entry.clone());
                                    } else if resp.clicked() {
                                        clicked_item = Some(entry.path.clone());
                                    } else if resp.secondary_clicked() {
                                        if !selected {
                                            clicked_item = Some(entry.path.clone());
                                        }
                                        let p = ctx
                                            .input(|i| i.pointer.interact_pos())
                                            .unwrap_or(rect.center());
                                        to_context =
                                            Some((ContextTarget::Entry(entry.clone()), p));
                                    }
                                    ui.add_space(1.5);
                                }
                            }
                        });
                }

                // Global click-outside & marquee selection inside CentralPanel
                let ptr_pos = ctx.input(|i| i.pointer.interact_pos());
                let primary_pressed = ctx.input(|i| i.pointer.primary_pressed());
                let primary_down = ctx.input(|i| i.pointer.primary_down());
                let primary_clicked = ctx.input(|i| i.pointer.primary_clicked());
                let secondary_clicked = ctx.input(|i| i.pointer.secondary_clicked());

                if primary_clicked && ptr_pos.is_some_and(|p| !search_box_rect.contains(p)) {
                    ctx.memory_mut(|m| m.surrender_focus(search_input_id));
                }

                let in_central = ptr_pos.is_some_and(|p| central_rect.contains(p));
                let modal_open = self.properties_modal.is_some()
                    || self.context_menu.is_some()
                    || self.new_folder_mode
                    || self.new_file_mode
                    || self.rename_target.is_some()
                    || self.delete_confirm_targets.is_some();

                let ctrl_held = mods.ctrl || mods.command;

                // Start marquee / Ctrl+drag selection inside CentralPanel
                if primary_pressed
                    && in_central
                    && !modal_open
                    && !ui_bar_clicked
                    && let Some(p) = ptr_pos
                    && (ctrl_held || !item_rects.iter().any(|(_, r)| r.contains(p)))
                {
                    self.marquee_start = Some(p);
                    self.marquee_active = false;
                    self.marquee_base = if ctrl_held {
                        self.selected_paths.clone()
                    } else {
                        Vec::new()
                    };
                }

                let mut finished_marquee = false;
                if let Some(start) = self.marquee_start {
                    if primary_down && let Some(curr) = ptr_pos {
                        let sel_rect = Rect::from_two_pos(start, curr);
                        if self.marquee_active || start.distance(curr) > 5.0 {
                            self.marquee_active = true;
                            ui.painter().rect(
                                sel_rect,
                                CornerRadius::same(4),
                                Color32::from_rgba_unmultiplied(
                                    accent.r(),
                                    accent.g(),
                                    accent.b(),
                                    44,
                                ),
                                Stroke::new(1.2, accent),
                                egui::StrokeKind::Inside,
                            );
                            let hits: Vec<PathBuf> = item_rects
                                .iter()
                                .filter(|(_, r)| r.intersects(sel_rect))
                                .map(|(p, _)| p.clone())
                                .collect();
                            if ctrl_held {
                                let mut combined = self.marquee_base.clone();
                                for p in hits {
                                    if !combined.contains(&p) {
                                        combined.push(p);
                                    }
                                }
                                self.selected_paths = combined;
                            } else {
                                self.selected_paths = hits;
                            }
                            ctx.request_repaint();
                        }
                    } else {
                        finished_marquee = self.marquee_active;
                        self.marquee_start = None;
                        self.marquee_active = false;
                        self.marquee_base.clear();
                    }
                }

                if let Some(p) = star_toggle_path {
                    self.toggle_star(&p);
                } else if !finished_marquee {
                    if let Some(path) = clicked_item {
                        self.handle_item_click(path, &visible, mods);
                    } else if primary_clicked
                        && in_central
                        && !ui_bar_clicked
                        && !modal_open
                        && !ctrl_held
                        && !mods.shift
                    {
                        self.selected_paths.clear();
                    }
                }

                if secondary_clicked
                    && to_context.is_none()
                    && in_central
                    && !ui_bar_clicked
                    && !modal_open
                    && let Some(p) = ptr_pos
                {
                    to_context = Some((ContextTarget::Background, p));
                }

                if let Some(entry) = to_open {
                    self.open_entry(&entry);
                }
                if let Some(cm) = to_context {
                    self.context_menu = Some(cm);
                }
            });

        // 5. Right-click Context Menu
        if let Some((target, raw_pos)) = self.context_menu.clone() {
            let menu_w = 232.0;
            let pos = pos2(
                raw_pos.x.clamp(10.0, (self.width as f32 - menu_w - 10.0).max(10.0)),
                raw_pos.y.clamp(10.0, (self.height as f32 - 300.0).max(10.0)),
            );
            let menu_rect_id = egui::Id::unique("rocketde_files_ctx_rect");
            let hit_rect = ctx
                .data(|d| d.get_temp::<Rect>(menu_rect_id))
                .filter(|r| (r.min.x - pos.x).abs() < 4.0 && (r.min.y - pos.y).abs() < 4.0)
                .unwrap_or_else(|| Rect::from_min_size(pos, Vec2::new(menu_w, 250.0)));

            if ctx.input(|i| i.pointer.any_pressed())
                && ctx
                    .input(|i| i.pointer.interact_pos())
                    .is_some_and(|p| !hit_rect.expand(2.0).contains(p))
            {
                self.context_menu = None;
                return;
            }

            let area_resp = egui::Area::new(egui::Id::unique("rocketde_files_ctx_menu"))
                .order(egui::Order::Tooltip)
                .fixed_pos(pos)
                .show(&ctx, |ui| {
                    Frame::NONE
                        .fill(Color32::from_rgb(24, 27, 39))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(6))
                        .stroke(Stroke::new(
                            1.0,
                            Color32::from_rgba_unmultiplied(255, 255, 255, 38),
                        ))
                        .show(ui, |ui| {
                            ui.set_width(menu_w - 12.0);
                            ui.spacing_mut().item_spacing.y = 2.0;

                            match &target {
                                ContextTarget::Entry(entry) => {
                                    if self.is_in_trash() {
                                        if ctx_row(ui, "Restaurar", "", accent) {
                                            let targets = self.selected_or_single(&entry.path);
                                            self.restore_from_trash(targets);
                                            self.context_menu = None;
                                        }
                                        if ctx_row(
                                            ui,
                                            "Excluir Permanentemente…",
                                            "",
                                            Color32::from_rgb(215, 55, 50),
                                        ) {
                                            self.delete_confirm_targets =
                                                Some(self.selected_or_single(&entry.path));
                                            self.context_menu = None;
                                        }
                                    } else {
                                        if ctx_row(ui, "Abrir", "Enter", accent) {
                                            self.open_entry(entry);
                                            self.context_menu = None;
                                        }
                                        if entry.is_dir
                                            && ctx_row(ui, "Abrir no Terminal", "", accent)
                                        {
                                            self.open_in_terminal(&entry.path);
                                            self.context_menu = None;
                                        }
                                        let star_lbl = if self.starred.contains(&entry.path) {
                                            "Remover Estrela"
                                        } else {
                                            "Adicionar Estrela ★"
                                        };
                                        if ctx_row(ui, star_lbl, "", accent) {
                                            self.toggle_star(&entry.path);
                                            self.context_menu = None;
                                        }
                                        ui.separator();
                                        if ctx_row(ui, "Copiar", "Ctrl+C", accent) {
                                            if !self.is_selected(&entry.path) {
                                                self.select_single(entry.path.clone());
                                            }
                                            self.start_copy_or_cut(ClipboardMode::Copy);
                                            self.context_menu = None;
                                        }
                                        if ctx_row(ui, "Recortar", "Ctrl+X", accent) {
                                            if !self.is_selected(&entry.path) {
                                                self.select_single(entry.path.clone());
                                            }
                                            self.start_copy_or_cut(ClipboardMode::Cut);
                                            self.context_menu = None;
                                        }
                                        if ctx_row(ui, "Duplicar", "Ctrl+D", accent) {
                                            let targets = self.selected_or_single(&entry.path);
                                            self.duplicate_entries(targets);
                                            self.context_menu = None;
                                        }
                                        if entry.is_dir
                                            && self.clipboard.is_some()
                                            && ctx_row(ui, "Colar aqui", "", accent)
                                        {
                                            self.paste_clipboard(entry.path.clone());
                                            self.context_menu = None;
                                        }
                                        if ctx_row(ui, "Copiar Caminho", "", accent) {
                                            self.copy_text_to_clipboard(
                                                &ctx,
                                                entry.path.display().to_string(),
                                                "Caminho copiado",
                                            );
                                            self.context_menu = None;
                                        }
                                        ui.separator();
                                        if ctx_row(ui, "Renomear…", "F2", accent) {
                                            self.rename_target = Some(entry.path.clone());
                                            self.rename_buf = entry.name.clone();
                                            self.focus_rename = true;
                                            self.context_menu = None;
                                        }
                                        if ctx_row(ui, "Propriedades", "", accent) {
                                            self.open_properties(entry.clone());
                                            self.context_menu = None;
                                        }
                                        ui.separator();
                                        if ctx_row(
                                            ui,
                                            "Mover para Lixeira",
                                            "Del",
                                            Color32::from_rgb(215, 95, 50),
                                        ) {
                                            let targets = self.selected_or_single(&entry.path);
                                            self.move_to_trash(targets);
                                            self.context_menu = None;
                                        }
                                        if ctx_row(
                                            ui,
                                            "Excluir Permanentemente…",
                                            "Shift+Del",
                                            Color32::from_rgb(215, 55, 50),
                                        ) {
                                            self.delete_confirm_targets =
                                                Some(self.selected_or_single(&entry.path));
                                            self.context_menu = None;
                                        }
                                    }
                                }
                                ContextTarget::Background => {
                                    if !self.is_in_trash() && !self.viewing_starred {
                                        if ctx_row(ui, "Nova Pasta…", "Ctrl+Shift+N", accent) {
                                            self.new_folder_mode = true;
                                            self.new_file_mode = false;
                                            self.focus_new_folder = true;
                                            self.new_folder_name.clear();
                                            self.context_menu = None;
                                        }
                                        if ctx_row(ui, "Novo Arquivo…", "", accent) {
                                            self.new_file_mode = true;
                                            self.new_folder_mode = false;
                                            self.focus_new_file = true;
                                            self.new_file_name.clear();
                                            self.context_menu = None;
                                        }
                                        if self.clipboard.is_some()
                                            && ctx_row(ui, "Colar", "Ctrl+V", accent)
                                        {
                                            let cur = self.current_dir.clone();
                                            self.paste_clipboard(cur);
                                            self.context_menu = None;
                                        }
                                        ui.separator();
                                    }
                                    if !self.undo_stack.is_empty()
                                        && ctx_row(ui, "Desfazer", "Ctrl+Z", accent)
                                    {
                                        self.undo_last();
                                        self.context_menu = None;
                                    }
                                    if ctx_row(ui, "Selecionar Tudo", "Ctrl+A", accent) {
                                        self.selected_paths =
                                            visible.iter().map(|e| e.path.clone()).collect();
                                        self.context_menu = None;
                                    }
                                    if ctx_row(ui, "Abrir no Terminal", "", accent) {
                                        let cur = self.current_dir.clone();
                                        self.open_in_terminal(&cur);
                                        self.context_menu = None;
                                    }
                                    if ctx_row(ui, "Atualizar", "F5", accent) {
                                        self.refresh_entries();
                                        self.context_menu = None;
                                    }
                                    let hidden_lbl = if self.show_hidden {
                                        "Ocultar arquivos ocultos"
                                    } else {
                                        "Mostrar arquivos ocultos"
                                    };
                                    if ctx_row(ui, hidden_lbl, "Ctrl+H", accent) {
                                        self.show_hidden = !self.show_hidden;
                                        self.context_menu = None;
                                    }
                                }
                            }
                        });
                });
            ctx.data_mut(|d| d.insert_temp(menu_rect_id, area_resp.response.rect));
        }

        // 6. GNOME-style Properties Modal
        if let Some(mut info) = self.properties_modal.clone() {
            let mut close_props = false;
            let mut toggle_exec = false;
            let mut copy_path_req = false;
            egui::Window::new("Propriedades")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .fixed_size(Vec2::new(430.0, 0.0))
                .frame(
                    Frame::NONE
                        .fill(Color32::from_rgb(24, 27, 38))
                        .corner_radius(CornerRadius::same(12))
                        .inner_margin(Margin::same(18))
                        .stroke(Stroke::new(
                            1.0,
                            Color32::from_rgba_unmultiplied(255, 255, 255, 36),
                        )),
                )
                .show(&ctx, |ui| {
                    ui.horizontal(|ui| {
                        let (icon_r, _) =
                            ui.allocate_exact_size(Vec2::new(44.0, 44.0), Sense::hover());
                        if let Some(tex) = self.thumb_cache.get(&info.entry.path) {
                            draw_thumbnail_in_rect(ui.painter(), icon_r, tex, 1.0);
                        } else {
                            draw_file_icon(ui.painter(), icon_r, &info.entry, accent, 1.0);
                        }
                        ui.add_space(10.0);
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(&info.entry.name)
                                    .size(15.5)
                                    .strong()
                                    .color(Color32::WHITE),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "{} ({})",
                                    info.entry.kind.type_label(),
                                    info.mime_type
                                ))
                                .size(12.0)
                                .color(Color32::from_rgb(155, 165, 185)),
                            );
                        });
                    });
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(8.0);

                    if info.entry.is_dir {
                        let content_txt = match info.dir_stats {
                            Some((folders, files, bytes)) => format!(
                                "{files} arquivo(s), {folders} subpasta(s) • {}",
                                format_bytes(bytes)
                            ),
                            None if info.dir_scanning => "Calculando conteúdo…".to_string(),
                            None => "Inacessível".to_string(),
                        };
                        prop_row(ui, "Conteúdo", &content_txt);
                    } else {
                        prop_row(
                            ui,
                            "Tamanho",
                            &format!(
                                "{} ({} bytes)",
                                format_bytes(info.entry.size_bytes),
                                info.entry.size_bytes
                            ),
                        );
                    }
                    prop_row(ui, "Pasta pai", &info.parent_dir);
                    if let Some(ref target) = info.symlink_target {
                        prop_row(ui, "Link aponta para", target);
                    }
                    prop_row(ui, "Volume", &info.volume_str);

                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(6.0);

                    prop_row(ui, "Modificado", &info.modified_full_str);
                    prop_row(ui, "Acessado", &info.accessed_str);
                    prop_row(ui, "Criado", &info.created_str);

                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(6.0);

                    prop_row(ui, "Dono / Grupo", &info.owner_group);
                    prop_row(ui, "Permissões", &info.permissions_str);
                    if !info.entry.is_dir {
                        ui.horizontal(|ui| {
                            ui.add_space(120.0);
                            let mut exec = info.is_executable;
                            if ui
                                .checkbox(&mut exec, "Permitir execução como programa")
                                .changed()
                            {
                                info.is_executable = exec;
                                toggle_exec = true;
                            }
                        });
                    }

                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        if pill_btn(
                            ui,
                            "Copiar caminho",
                            Color32::from_rgba_unmultiplied(255, 255, 255, 18),
                        )
                        .clicked()
                        {
                            copy_path_req = true;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if pill_btn(ui, "Fechar", accent).clicked() {
                                close_props = true;
                            }
                        });
                    });
                });

            if copy_path_req {
                self.copy_text_to_clipboard(
                    &ctx,
                    info.entry.path.display().to_string(),
                    "Caminho copiado",
                );
            }
            if toggle_exec {
                if let Ok(meta) = fs::metadata(&info.entry.path) {
                    let mut perms = meta.permissions();
                    let cur = perms.mode();
                    let next = if info.is_executable {
                        cur | 0o111
                    } else {
                        cur & !0o111
                    };
                    perms.set_mode(next);
                    if fs::set_permissions(&info.entry.path, perms).is_ok() {
                        info.permissions_str = format_unix_mode(next);
                        self.set_toast("Permissões atualizadas".to_string(), false);
                    }
                }
            }
            self.properties_modal = if close_props { None } else { Some(info) };
        }

        // 7. Centered Action Modals (New Folder / New File / Rename / Delete Confirm)
        let any_action_modal = self.new_folder_mode
            || self.new_file_mode
            || self.rename_target.is_some()
            || self.delete_confirm_targets.is_some()
            || self.empty_trash_confirm
            || self.properties_modal.is_some();
        if any_action_modal {
            egui::Area::new(egui::Id::unique("rocketde_files_modal_dim"))
                .order(egui::Order::Middle)
                .fixed_pos(Pos2::ZERO)
                .show(&ctx, |ui| {
                    let screen = ctx.content_rect();
                    let (_, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                    ui.painter().rect_filled(
                        screen,
                        CornerRadius::ZERO,
                        Color32::from_rgba_unmultiplied(0, 0, 0, 120),
                    );
                    if resp.clicked() {
                        self.new_folder_mode = false;
                        self.new_file_mode = false;
                        self.rename_target = None;
                        self.delete_confirm_targets = None;
                        self.empty_trash_confirm = false;
                        self.properties_modal = None;
                    }
                });
        }

        if self.new_folder_mode {
            let mut close = false;
            let mut do_create = false;
            egui::Window::new("Nova Pasta")
                .order(egui::Order::Foreground)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .fixed_size(Vec2::new(360.0, 0.0))
                .frame(modal_frame())
                .show(&ctx, |ui| {
                    ui.label(
                        RichText::new("Digite o nome da nova pasta:")
                            .size(12.5)
                            .color(Color32::from_rgb(175, 185, 205)),
                    );
                    ui.add_space(8.0);
                    styled_modal_input(
                        ui,
                        &mut self.new_folder_name,
                        "Nome da pasta",
                        &mut self.focus_new_folder,
                        accent,
                    );
                    if ctx.input(|i| i.key_pressed(Key::Enter)) {
                        do_create = true;
                    }
                    ui.add_space(14.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if pill_btn(ui, "Criar", accent).clicked() {
                            do_create = true;
                        }
                        if pill_btn(ui, "Cancelar", Color32::from_rgba_unmultiplied(255, 255, 255, 20)).clicked() {
                            close = true;
                        }
                    });
                });
            if do_create {
                let name = self.new_folder_name.trim().to_string();
                if !name.is_empty() {
                    let target = self.current_dir.join(&name);
                    match fs::create_dir(&target) {
                        Ok(_) => {
                            self.new_folder_mode = false;
                            self.undo_stack.push(UndoAction::CreatePath {
                                path: target.clone(),
                            });
                            self.refresh_entries();
                            self.select_single(target);
                            self.set_toast(format!("Pasta '{}' criada", name), false);
                        }
                        Err(e) => self.set_toast(format!("Erro: {e}"), true),
                    }
                }
            } else if close {
                self.new_folder_mode = false;
            }
        }

        if self.new_file_mode {
            let mut close = false;
            let mut do_create = false;
            egui::Window::new("Novo Arquivo")
                .order(egui::Order::Foreground)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .fixed_size(Vec2::new(360.0, 0.0))
                .frame(modal_frame())
                .show(&ctx, |ui| {
                    ui.label(
                        RichText::new("Digite o nome do novo arquivo:")
                            .size(12.5)
                            .color(Color32::from_rgb(175, 185, 205)),
                    );
                    ui.add_space(8.0);
                    styled_modal_input(
                        ui,
                        &mut self.new_file_name,
                        "arquivo.txt",
                        &mut self.focus_new_file,
                        accent,
                    );
                    if ctx.input(|i| i.key_pressed(Key::Enter)) {
                        do_create = true;
                    }
                    ui.add_space(14.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if pill_btn(ui, "Criar", accent).clicked() {
                            do_create = true;
                        }
                        if pill_btn(ui, "Cancelar", Color32::from_rgba_unmultiplied(255, 255, 255, 20)).clicked() {
                            close = true;
                        }
                    });
                });
            if do_create {
                let name = self.new_file_name.trim().to_string();
                if !name.is_empty() {
                    let target = self.current_dir.join(&name);
                    match fs::OpenOptions::new().write(true).create_new(true).open(&target) {
                        Ok(_) => {
                            self.new_file_mode = false;
                            self.undo_stack.push(UndoAction::CreatePath {
                                path: target.clone(),
                            });
                            self.refresh_entries();
                            self.select_single(target);
                            self.set_toast(format!("Arquivo '{}' criado", name), false);
                        }
                        Err(e) => self.set_toast(format!("Erro: {e}"), true),
                    }
                }
            } else if close {
                self.new_file_mode = false;
            }
        }

        if let Some(target) = self.rename_target.clone() {
            let mut close = false;
            let mut do_rename = false;
            egui::Window::new("Renomear")
                .order(egui::Order::Foreground)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .fixed_size(Vec2::new(360.0, 0.0))
                .frame(modal_frame())
                .show(&ctx, |ui| {
                    ui.label(
                        RichText::new("Novo nome:")
                            .size(12.5)
                            .color(Color32::from_rgb(175, 185, 205)),
                    );
                    ui.add_space(8.0);
                    styled_modal_input(
                        ui,
                        &mut self.rename_buf,
                        "Nome",
                        &mut self.focus_rename,
                        accent,
                    );
                    if ctx.input(|i| i.key_pressed(Key::Enter)) {
                        do_rename = true;
                    }
                    ui.add_space(14.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if pill_btn(ui, "Renomear", accent).clicked() {
                            do_rename = true;
                        }
                        if pill_btn(ui, "Cancelar", Color32::from_rgba_unmultiplied(255, 255, 255, 20)).clicked() {
                            close = true;
                        }
                    });
                });
            if do_rename {
                let new_name = self.rename_buf.trim().to_string();
                if !new_name.is_empty()
                    && let Some(parent) = target.parent()
                {
                    let dest = parent.join(&new_name);
                    match fs::rename(&target, &dest) {
                        Ok(_) => {
                            self.rename_target = None;
                            self.undo_stack.push(UndoAction::Rename {
                                from: target.clone(),
                                to: dest.clone(),
                            });
                            self.refresh_entries();
                            self.select_single(dest);
                            self.set_toast(format!("Renomeado para '{}'", new_name), false);
                        }
                        Err(e) => self.set_toast(format!("Erro ao renomear: {e}"), true),
                    }
                }
            } else if close {
                self.rename_target = None;
            }
        }

        if let Some(targets) = self.delete_confirm_targets.clone() {
            let mut close = false;
            let mut do_delete = false;
            let prompt = if targets.len() == 1 {
                let name = targets[0]
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("item");
                format!("Tem certeza que deseja excluir '{}' permanentemente? Esta ação não pode ser desfeita.", name)
            } else {
                format!(
                    "Tem certeza que deseja excluir {} itens permanentemente? Esta ação não pode ser desfeita.",
                    targets.len()
                )
            };
            egui::Window::new("Excluir Permanentemente")
                .order(egui::Order::Foreground)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .fixed_size(Vec2::new(390.0, 0.0))
                .frame(modal_frame())
                .show(&ctx, |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(prompt)
                                .size(13.0)
                                .color(Color32::from_rgb(235, 215, 215)),
                        )
                        .wrap(),
                    );
                    if ctx.input(|i| i.key_pressed(Key::Enter)) {
                        do_delete = true;
                    }
                    ui.add_space(16.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if pill_btn(ui, "Excluir", Color32::from_rgb(215, 55, 50)).clicked() {
                            do_delete = true;
                        }
                        if pill_btn(ui, "Cancelar", Color32::from_rgba_unmultiplied(255, 255, 255, 20)).clicked() {
                            close = true;
                        }
                    });
                });
            if do_delete {
                self.delete_confirm_targets = None;
                let in_trash = self.is_in_trash();
                let trash_info = self.trash_info_dir.clone();
                let tx = self.bg_tx.clone();
                self.set_toast(format!("Excluindo {} item(ns)...", targets.len()), false);
                std::thread::spawn(move || {
                    let mut deleted = 0usize;
                    let mut err_msg = None;
                    for t in &targets {
                        let res = if t.is_dir() && !t.is_symlink() {
                            fs::remove_dir_all(t)
                        } else {
                            fs::remove_file(t)
                        };
                        match res {
                            Ok(_) => {
                                if in_trash {
                                    if let Some(n) = t.file_name().and_then(|s| s.to_str()) {
                                        let _ = fs::remove_file(
                                            trash_info.join(format!("{n}.trashinfo")),
                                        );
                                    }
                                }
                                deleted += 1;
                            }
                            Err(e) => err_msg = Some(e.to_string()),
                        }
                    }
                    let has_err = err_msg.is_some();
                    let msg = if let Some(e) = err_msg {
                        format!("Erro ao excluir: {e}")
                    } else {
                        format!("{deleted} item(ns) excluído(s)")
                    };
                    let _ = tx.send(BgEvent::OpResult {
                        msg,
                        is_err: has_err,
                        refresh: true,
                        undo: None,
                    });
                });
            } else if close {
                self.delete_confirm_targets = None;
            }
        }

        if self.empty_trash_confirm {
            let mut close = false;
            let mut do_empty = false;
            egui::Window::new("Esvaziar Lixeira")
                .order(egui::Order::Foreground)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .fixed_size(Vec2::new(390.0, 0.0))
                .frame(modal_frame())
                .show(&ctx, |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new("Tem certeza que deseja esvaziar toda a Lixeira permanentemente? Esta ação não pode ser desfeita.")
                                .size(13.0)
                                .color(Color32::from_rgb(235, 215, 215)),
                        )
                        .wrap(),
                    );
                    if ctx.input(|i| i.key_pressed(Key::Enter)) {
                        do_empty = true;
                    } else if ctx.input(|i| i.key_pressed(Key::Escape)) {
                        close = true;
                    }
                    ui.add_space(16.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if pill_btn(ui, "Esvaziar", Color32::from_rgb(215, 55, 50)).clicked() {
                            do_empty = true;
                        }
                        if pill_btn(ui, "Cancelar", Color32::from_rgba_unmultiplied(255, 255, 255, 20)).clicked() {
                            close = true;
                        }
                    });
                });
            if do_empty {
                self.empty_trash_confirm = false;
                self.empty_trash();
            } else if close {
                self.empty_trash_confirm = false;
            }
        }
    }

    fn selected_or_single(&self, path: &Path) -> Vec<PathBuf> {
        if self.is_selected(path) && !self.selected_paths.is_empty() {
            self.selected_paths.clone()
        } else {
            vec![path.to_path_buf()]
        }
    }
}

fn build_file_entry(path: &Path) -> Option<FileEntry> {
    let name = path.file_name()?.to_string_lossy().to_string();
    let is_hidden = name.starts_with('.');
    let sym_meta = fs::symlink_metadata(path).ok();
    let is_symlink = sym_meta
        .as_ref()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    let meta = fs::metadata(path).ok().or(sym_meta)?;
    let is_dir = meta.is_dir();
    let size_bytes = meta.len();
    let mod_sys = meta.modified().ok();
    let modified_ts = mod_sys
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let modified_str = mod_sys
        .map(format_system_time)
        .unwrap_or_else(|| "—".to_string());
    let kind = FileKind::from_path(path, is_dir);
    let ext_upper = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.chars().take(4).collect::<String>().to_uppercase())
        .unwrap_or_default();

    Some(FileEntry {
        name,
        ext_upper,
        path: path.to_path_buf(),
        is_dir,
        is_hidden,
        is_symlink,
        size_bytes,
        modified_ts,
        modified_str,
        kind,
    })
}

fn prop_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.set_width(ui.available_width());
        ui.allocate_ui_with_layout(
            Vec2::new(115.0, 20.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.label(
                    RichText::new(label)
                        .size(12.0)
                        .color(Color32::from_rgb(145, 155, 175)),
                );
            },
        );
        ui.add(egui::Label::new(RichText::new(value).size(12.0).color(Color32::WHITE)).wrap());
    });
    ui.add_space(3.0);
}

#[derive(Debug, Clone, Copy)]
enum VectorBtn {
    ArrowUp,
    Refresh,
    Edit,
    Grid,
    List,
    Close,
}

fn draw_vector_glyph(painter: &egui::Painter, c: Pos2, s: f32, kind: VectorBtn, col: Color32) {
    let stroke = Stroke::new(1.5, col);
    let r = s * 0.42;
    match kind {
        VectorBtn::ArrowUp => {
            painter.line_segment([pos2(c.x, c.y + r), pos2(c.x, c.y - r)], stroke);
            painter.line_segment([pos2(c.x - r * 0.7, c.y - r * 0.25), pos2(c.x, c.y - r)], stroke);
            painter.line_segment([pos2(c.x + r * 0.7, c.y - r * 0.25), pos2(c.x, c.y - r)], stroke);
        }
        VectorBtn::Refresh => {
            let segs = 14;
            let mut pts = Vec::with_capacity(segs + 1);
            for i in 0..=segs {
                let a = -0.35 + (i as f32 / segs as f32) * std::f32::consts::PI * 1.55;
                pts.push(pos2(c.x + a.cos() * r, c.y + a.sin() * r));
            }
            painter.add(Shape::line(pts, stroke));
            let tip = pos2(c.x + (-0.35f32).cos() * r, c.y + (-0.35f32).sin() * r);
            painter.line_segment([tip, pos2(tip.x - 4.0, tip.y - 1.5)], stroke);
            painter.line_segment([tip, pos2(tip.x + 1.0, tip.y - 4.2)], stroke);
        }
        VectorBtn::Edit => {
            painter.line_segment(
                [pos2(c.x - r * 0.8, c.y + r * 0.8), pos2(c.x + r * 0.75, c.y - r * 0.75)],
                Stroke::new(1.7, col),
            );
            painter.line_segment(
                [pos2(c.x - r * 0.85, c.y + r * 0.85), pos2(c.x - r * 0.35, c.y + r * 0.7)],
                Stroke::new(1.4, col),
            );
        }
        VectorBtn::Grid => {
            let gap = 1.5;
            let box_s = r - gap * 0.5;
            for dy in [-1.0, 1.0] {
                for dx in [-1.0, 1.0] {
                    let center = pos2(c.x + dx * (box_s * 0.5 + gap), c.y + dy * (box_s * 0.5 + gap));
                    painter.rect_filled(
                        Rect::from_center_size(center, Vec2::splat(box_s)),
                        CornerRadius::same(1),
                        col,
                    );
                }
            }
        }
        VectorBtn::List => {
            for dy in [-r * 0.65, 0.0, r * 0.65] {
                painter.line_segment(
                    [pos2(c.x - r * 0.9, c.y + dy), pos2(c.x + r * 0.9, c.y + dy)],
                    Stroke::new(1.6, col),
                );
            }
        }
        VectorBtn::Close => {
            let d = r * 0.72;
            painter.line_segment([pos2(c.x - d, c.y - d), pos2(c.x + d, c.y + d)], stroke);
            painter.line_segment([pos2(c.x + d, c.y - d), pos2(c.x - d, c.y + d)], stroke);
        }
    }
}

fn draw_sidebar_icon(painter: &egui::Painter, c: Pos2, icon: SidebarIcon, col: Color32) {
    let stroke = Stroke::new(1.35, col);
    let r = 6.2;
    match icon {
        SidebarIcon::Home => {
            let roof = [
                pos2(c.x - r, c.y - 0.5),
                pos2(c.x, c.y - r),
                pos2(c.x + r, c.y - 0.5),
            ];
            painter.add(Shape::line(roof.to_vec(), stroke));
            painter.rect_stroke(
                Rect::from_min_max(pos2(c.x - r * 0.65, c.y - 0.5), pos2(c.x + r * 0.65, c.y + r * 0.85)),
                CornerRadius::same(1),
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        SidebarIcon::Desktop => {
            painter.rect_stroke(
                Rect::from_center_size(pos2(c.x, c.y - 1.0), Vec2::new(13.0, 9.0)),
                CornerRadius::same(2),
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.line_segment([pos2(c.x - 3.5, c.y + 5.5), pos2(c.x + 3.5, c.y + 5.5)], stroke);
        }
        SidebarIcon::Documents => {
            painter.rect_stroke(
                Rect::from_center_size(c, Vec2::new(10.0, 12.5)),
                CornerRadius::same(2),
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.line_segment([pos2(c.x - 2.5, c.y - 2.0), pos2(c.x + 2.5, c.y - 2.0)], stroke);
            painter.line_segment([pos2(c.x - 2.5, c.y + 1.5), pos2(c.x + 2.5, c.y + 1.5)], stroke);
        }
        SidebarIcon::Downloads => {
            painter.line_segment([pos2(c.x, c.y - 5.5), pos2(c.x, c.y + 2.5)], stroke);
            painter.line_segment([pos2(c.x - 3.5, c.y - 0.5), pos2(c.x, c.y + 2.8)], stroke);
            painter.line_segment([pos2(c.x + 3.5, c.y - 0.5), pos2(c.x, c.y + 2.8)], stroke);
            painter.line_segment([pos2(c.x - 5.0, c.y + 5.5), pos2(c.x + 5.0, c.y + 5.5)], stroke);
        }
        SidebarIcon::Pictures => draw_icon(painter, c, 13.0, IconKind::Camera, col),
        SidebarIcon::Videos => draw_icon(painter, c, 13.0, IconKind::VideoRecord, col),
        SidebarIcon::Music => draw_icon(painter, c, 13.0, IconKind::MusicNote, col),
        SidebarIcon::Starred => draw_icon(painter, c, 13.0, IconKind::Star, col),
        SidebarIcon::Trash => draw_icon(painter, c, 13.0, IconKind::Trash, col),
        SidebarIcon::Drive => {
            painter.rect_stroke(
                Rect::from_center_size(c, Vec2::new(13.0, 9.0)),
                CornerRadius::same(2),
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.circle_filled(pos2(c.x + 3.5, c.y + 1.5), 1.2, col);
        }
        SidebarIcon::Root => draw_icon(painter, c, 13.0, IconKind::Terminal, col),
    }
}

fn sidebar_section_label(ui: &mut egui::Ui, title: &str) {
    ui.add_space(2.0);
    ui.label(
        RichText::new(title)
            .size(10.5)
            .strong()
            .color(Color32::from_rgb(122, 132, 152)),
    );
    ui.add_space(4.0);
}

fn sidebar_row(
    ui: &mut egui::Ui,
    label: &str,
    icon: SidebarIcon,
    active: bool,
    accent: Color32,
) -> bool {
    let (row_r, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 29.0), Sense::click());
    let bg = if active {
        Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 56)
    } else if resp.hovered() {
        Color32::from_rgba_unmultiplied(255, 255, 255, 12)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(row_r, CornerRadius::same(7), bg);
    let icon_col = if active {
        accent
    } else if icon == SidebarIcon::Starred {
        Color32::from_rgb(255, 204, 41)
    } else {
        Color32::from_rgb(185, 195, 215)
    };
    draw_sidebar_icon(ui.painter(), pos2(row_r.min.x + 15.0, row_r.center().y), icon, icon_col);
    ui.painter().text(
        pos2(row_r.min.x + 32.0, row_r.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.0),
        if active {
            Color32::WHITE
        } else {
            Color32::from_rgb(215, 222, 236)
        },
    );
    ui.add_space(1.5);
    resp.clicked()
}

fn sidebar_device_row(
    ui: &mut egui::Ui,
    dev: &DeviceEntry,
    active: bool,
    accent: Color32,
) -> (bool, bool) {
    let (row_r, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), Sense::click());
    let bg = if active {
        Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 56)
    } else if resp.hovered() {
        Color32::from_rgba_unmultiplied(255, 255, 255, 12)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(row_r, CornerRadius::same(7), bg);
    let icon_col = if active {
        accent
    } else if dev.mount_point.is_some() {
        Color32::from_rgb(100, 210, 145)
    } else {
        Color32::from_rgb(165, 175, 195)
    };
    draw_sidebar_icon(
        ui.painter(),
        pos2(row_r.min.x + 15.0, row_r.center().y),
        SidebarIcon::Drive,
        icon_col,
    );

    let title = truncate_with_ellipsis(&dev.label, 14);
    ui.painter().text(
        pos2(row_r.min.x + 32.0, row_r.min.y + 10.0),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(12.5),
        Color32::WHITE,
    );
    let sub = if dev.mount_point.is_some() {
        dev.size_str.clone()
    } else {
        format!("{} • Clique p/ montar", dev.size_str)
    };
    ui.painter().text(
        pos2(row_r.min.x + 32.0, row_r.min.y + 23.0),
        Align2::LEFT_CENTER,
        sub,
        FontId::proportional(10.0),
        Color32::from_rgb(138, 148, 168),
    );

    let mut eject_clicked = false;
    if dev.mount_point.is_some() {
        let eject_r = Rect::from_center_size(pos2(row_r.max.x - 14.0, row_r.center().y), Vec2::splat(18.0));
        let ptr_pos = ui.ctx().input(|i| i.pointer.hover_pos());
        let eject_hov = ptr_pos.is_some_and(|p| eject_r.contains(p));
        if eject_hov {
            ui.painter().rect_filled(
                eject_r,
                CornerRadius::same(4),
                Color32::from_rgba_unmultiplied(255, 255, 255, 25),
            );
        }
        ui.painter().text(
            eject_r.center(),
            Align2::CENTER_CENTER,
            "⏏",
            FontId::proportional(11.0),
            if eject_hov { Color32::WHITE } else { Color32::from_rgb(160, 170, 190) },
        );
        if resp.clicked() && eject_hov {
            eject_clicked = true;
        }
    }
    ui.add_space(1.5);
    (resp.clicked() && !eject_clicked, eject_clicked)
}

fn draw_thumbnail_in_rect(painter: &egui::Painter, rect: Rect, tex: &TextureHandle, alpha: f32) {
    let sz = tex.size_vec2();
    let scale = (rect.width() / sz.x.max(1.0)).min(rect.height() / sz.y.max(1.0));
    let draw_sz = sz * scale;
    let img_rect = Rect::from_center_size(rect.center(), draw_sz);
    painter.rect_filled(
        img_rect.translate(vec2(0.0, 1.5)),
        CornerRadius::same(5),
        apply_alpha(Color32::from_rgba_unmultiplied(0, 0, 0, 75), alpha),
    );
    painter.image(
        tex.id(),
        img_rect,
        Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
        apply_alpha(Color32::WHITE, alpha),
    );
    painter.rect_stroke(
        img_rect,
        CornerRadius::same(4),
        Stroke::new(
            1.0,
            apply_alpha(Color32::from_rgba_unmultiplied(255, 255, 255, 45), alpha),
        ),
        egui::StrokeKind::Inside,
    );
}

fn draw_file_icon(
    painter: &egui::Painter,
    rect: Rect,
    entry: &FileEntry,
    accent: Color32,
    alpha: f32,
) {
    let col = entry.kind.accent_color(accent);
    if entry.is_dir {
        let r = rect.shrink2(vec2(rect.width() * 0.06, rect.height() * 0.10));
        let tab = Rect::from_min_size(r.min, Vec2::new(r.width() * 0.44, r.height() * 0.25));
        let back_col = apply_alpha(
            Color32::from_rgb(
                (col.r() as u16 * 75 / 100) as u8,
                (col.g() as u16 * 75 / 100) as u8,
                (col.b() as u16 * 75 / 100) as u8,
            ),
            alpha,
        );
        painter.rect_filled(tab, CornerRadius::same(4), back_col);
        let body = Rect::from_min_max(pos2(r.min.x, r.min.y + r.height() * 0.14), r.max);
        painter.rect_filled(
            body.translate(vec2(0.0, 2.0)),
            CornerRadius::same(6),
            apply_alpha(Color32::from_rgba_unmultiplied(0, 0, 0, 65), alpha),
        );
        painter.rect_filled(body, CornerRadius::same(6), apply_alpha(col, alpha));
        let inner_hi = Rect::from_min_size(
            pos2(body.min.x + 1.0, body.min.y + 1.0),
            Vec2::new((body.width() - 2.0).max(1.0), 3.0),
        );
        painter.rect_filled(
            inner_hi,
            CornerRadius::same(3),
            apply_alpha(Color32::from_rgba_unmultiplied(255, 255, 255, 55), alpha),
        );
    } else {
        let w = rect.width() * 0.72;
        let h = rect.height() * 0.88;
        let doc = Rect::from_center_size(rect.center(), Vec2::new(w, h));
        let fold = (w * 0.26).clamp(4.0, 11.0);

        painter.rect_filled(
            doc.translate(vec2(0.0, 1.5)),
            CornerRadius::same(5),
            apply_alpha(Color32::from_rgba_unmultiplied(0, 0, 0, 60), alpha),
        );
        let pts = vec![
            doc.min,
            pos2(doc.max.x - fold, doc.min.y),
            pos2(doc.max.x, doc.min.y + fold),
            doc.max,
            pos2(doc.min.x, doc.max.y),
        ];
        painter.add(Shape::convex_polygon(
            pts,
            apply_alpha(Color32::from_rgb(38, 43, 58), alpha),
            Stroke::new(
                1.0,
                apply_alpha(Color32::from_rgba_unmultiplied(255, 255, 255, 42), alpha),
            ),
        ));
        painter.add(Shape::convex_polygon(
            vec![
                pos2(doc.max.x - fold, doc.min.y),
                pos2(doc.max.x, doc.min.y + fold),
                pos2(doc.max.x - fold, doc.min.y + fold),
            ],
            apply_alpha(Color32::from_rgba_unmultiplied(255, 255, 255, 38), alpha),
            Stroke::NONE,
        ));

        let glyph_c = pos2(doc.center().x, doc.center().y + 1.0);
        let kind_col = apply_alpha(col, alpha);
        if doc.height() >= 28.0 {
            match entry.kind {
                FileKind::Code => {
                    painter.text(
                        glyph_c,
                        Align2::CENTER_CENTER,
                        "</>",
                        FontId::monospace(11.5),
                        kind_col,
                    );
                }
                FileKind::Audio => draw_icon(painter, glyph_c, 15.0, IconKind::MusicNote, kind_col),
                FileKind::Video => draw_icon(painter, glyph_c, 14.0, IconKind::Play, kind_col),
                FileKind::Image => draw_icon(painter, glyph_c, 15.0, IconKind::Camera, kind_col),
                _ => {
                    for dy in [-4.5, 0.0, 4.5] {
                        let w_line = if dy > 1.0 { w * 0.42 } else { w * 0.58 };
                        painter.line_segment(
                            [
                                pos2(doc.min.x + 6.0, glyph_c.y + dy),
                                pos2(doc.min.x + 6.0 + w_line, glyph_c.y + dy),
                            ],
                            Stroke::new(
                                1.6,
                                apply_alpha(Color32::from_rgba_unmultiplied(255, 255, 255, 65), alpha),
                            ),
                        );
                    }
                }
            }
            if !entry.ext_upper.is_empty() {
                let tag_rect = Rect::from_min_max(
                    pos2(doc.min.x + 3.0, doc.max.y - 12.0),
                    pos2(doc.max.x - 3.0, doc.max.y - 2.0),
                );
                painter.rect_filled(tag_rect, CornerRadius::same(3), kind_col);
                painter.text(
                    tag_rect.center(),
                    Align2::CENTER_CENTER,
                    &entry.ext_upper,
                    FontId::proportional(8.0),
                    Color32::WHITE,
                );
            }
        } else {
            painter.circle_filled(glyph_c, 3.2, kind_col);
        }
    }

    if entry.is_symlink {
        let badge = Rect::from_min_size(pos2(rect.min.x, rect.max.y - 11.0), Vec2::splat(11.0));
        painter.rect_filled(
            badge,
            CornerRadius::same(3),
            apply_alpha(Color32::from_rgb(20, 22, 30), alpha),
        );
        painter.text(
            badge.center(),
            Align2::CENTER_CENTER,
            "↗",
            FontId::proportional(9.5),
            apply_alpha(Color32::WHITE, alpha),
        );
    }
}

fn draw_wrapped_label(
    painter: &egui::Painter,
    rect: Rect,
    text: &str,
    alpha: f32,
    selected: bool,
) -> bool {
    let font = FontId::proportional(11.8);
    let col = apply_alpha(
        if selected {
            Color32::WHITE
        } else {
            Color32::from_rgb(232, 236, 245)
        },
        alpha,
    );
    let mut job = egui::text::LayoutJob::simple(text.to_string(), font.clone(), col, rect.width());
    job.halign = Align::Center;
    let galley = painter.layout_job(job);
    if galley.rows.len() <= 2 {
        painter.galley(pos2(rect.center().x, rect.min.y), galley, col);
        false
    } else {
        let line1 = galley.rows[0].text().to_string();
        let rest: String = galley.rows[1..].iter().map(|r| r.text()).collect();
        let line2 = truncate_with_ellipsis(&rest, 14);
        let two_line = format!("{}\n{}", line1.trim_end(), line2);
        let mut clamped = egui::text::LayoutJob::simple(two_line, font, col, rect.width());
        clamped.halign = Align::Center;
        let g2 = painter.layout_job(clamped);
        painter.galley(pos2(rect.center().x, rect.min.y), g2, col);
        true
    }
}

fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{}…", head.trim_end())
    } else {
        head
    }
}

fn apply_alpha(col: Color32, alpha: f32) -> Color32 {
    if alpha >= 0.99 {
        return col;
    }
    Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), (col.a() as f32 * alpha) as u8)
}

fn list_col_header(
    ui: &mut egui::Ui,
    rect: Rect,
    title: &str,
    field: SortField,
    active_field: SortField,
    asc: bool,
    align: Align2,
) -> bool {
    let resp = ui.interact(rect, ui.make_persistent_id(("files_hdr", title)), Sense::click());
    let is_active = field == active_field;
    let label = if is_active {
        format!("{title} {}", if asc { "▲" } else { "▼" })
    } else {
        title.to_string()
    };
    let col = if is_active || resp.hovered() {
        Color32::WHITE
    } else {
        Color32::from_rgb(138, 148, 168)
    };
    let pos = match align {
        Align2::LEFT_CENTER => pos2(rect.min.x, rect.center().y),
        _ => pos2(rect.max.x, rect.center().y),
    };
    ui.painter().text(pos, align, label, FontId::proportional(11.5), col);
    resp.clicked()
}

fn modal_frame() -> Frame {
    Frame::NONE
        .fill(Color32::from_rgb(24, 27, 38))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(18))
        .stroke(Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, 36),
        ))
}

fn styled_modal_input(
    ui: &mut egui::Ui,
    buf: &mut String,
    hint: &str,
    focus_flag: &mut bool,
    accent: Color32,
) {
    Frame::NONE
        .fill(Color32::from_rgb(14, 16, 22))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 7))
        .stroke(Stroke::new(1.2, accent))
        .show(ui, |ui| {
            let w = ui.available_width();
            let resp = ui.add(
                egui::TextEdit::singleline(buf)
                    .frame(Frame::NONE)
                    .hint_text(hint)
                    .desired_width(w),
            );
            if *focus_flag {
                resp.request_focus();
                *focus_flag = false;
            }
        });
}

fn nav_icon_btn(ui: &mut egui::Ui, kind: IconKind, enabled: bool, tip: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(28.0, 28.0), Sense::click());
    let bg = if enabled && resp.hovered() {
        Color32::from_rgba_unmultiplied(255, 255, 255, 28)
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 12)
    };
    ui.painter().rect_filled(rect, CornerRadius::same(7), bg);
    let col = if enabled {
        Color32::WHITE
    } else {
        Color32::from_rgb(95, 104, 120)
    };
    draw_icon(ui.painter(), rect.center(), 11.5, kind, col);
    resp.on_hover_text(tip).clicked() && enabled
}

fn nav_vector_btn(ui: &mut egui::Ui, kind: VectorBtn, enabled: bool, tip: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(28.0, 28.0), Sense::click());
    let bg = if enabled && resp.hovered() {
        Color32::from_rgba_unmultiplied(255, 255, 255, 28)
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 12)
    };
    ui.painter().rect_filled(rect, CornerRadius::same(7), bg);
    let col = if enabled {
        Color32::WHITE
    } else {
        Color32::from_rgb(95, 104, 120)
    };
    draw_vector_glyph(ui.painter(), rect.center(), 14.0, kind, col);
    resp.on_hover_text(tip).clicked() && enabled
}

fn seg_icon_btn(
    ui: &mut egui::Ui,
    kind: VectorBtn,
    active: bool,
    accent: Color32,
    tip: &str,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(26.0, 24.0), Sense::click());
    let bg = if active {
        accent
    } else if resp.hovered() {
        Color32::from_rgba_unmultiplied(255, 255, 255, 18)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    let col = if active {
        Color32::WHITE
    } else {
        Color32::from_rgb(175, 185, 205)
    };
    draw_vector_glyph(ui.painter(), rect.center(), 13.0, kind, col);
    resp.on_hover_text(tip).clicked()
}

fn pill_btn(ui: &mut egui::Ui, label: &str, bg: Color32) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(11.5).strong().color(Color32::WHITE))
            .fill(bg)
            .corner_radius(CornerRadius::same(8))
            .min_size(Vec2::new(0.0, 28.0)),
    )
}

fn ctx_row(ui: &mut egui::Ui, label: &str, shortcut: &str, hover_col: Color32) -> bool {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, CornerRadius::same(6), hover_col);
    }
    ui.painter().text(
        pos2(r.min.x + 10.0, r.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.5),
        Color32::WHITE,
    );
    if !shortcut.is_empty() {
        ui.painter().text(
            pos2(r.max.x - 10.0, r.center().y),
            Align2::RIGHT_CENTER,
            shortcut,
            FontId::proportional(11.0),
            if resp.hovered() {
                Color32::from_rgba_unmultiplied(255, 255, 255, 210)
            } else {
                Color32::from_rgb(130, 140, 160)
            },
        );
    }
    resp.clicked()
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}

fn format_system_time(st: SystemTime) -> String {
    let dt: DateTime<Local> = st.into();
    dt.format("%d/%m/%Y %H:%M").to_string()
}

fn format_system_time_full(st: SystemTime) -> String {
    let dt: DateTime<Local> = st.into();
    dt.format("%d/%m/%Y %H:%M:%S").to_string()
}

fn format_unix_mode(mode: u32) -> String {
    let mut s = String::with_capacity(9);
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    format!("{s} ({:o})", mode & 0o777)
}

fn lookup_user(uid: u32) -> String {
    unsafe {
        let pw = libc::getpwuid(uid);
        if !pw.is_null() && !(*pw).pw_name.is_null() {
            return CStr::from_ptr((*pw).pw_name).to_string_lossy().to_string();
        }
    }
    uid.to_string()
}

fn lookup_group(gid: u32) -> String {
    unsafe {
        let gr = libc::getgrgid(gid);
        if !gr.is_null() && !(*gr).gr_name.is_null() {
            return CStr::from_ptr((*gr).gr_name).to_string_lossy().to_string();
        }
    }
    gid.to_string()
}

fn query_statvfs(path: &Path) -> Option<(u64, u64)> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut vfs: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut vfs) } == 0 {
        let frsize = vfs.f_frsize as u64;
        let total = (vfs.f_blocks as u64).saturating_mul(frsize);
        let avail = (vfs.f_bavail as u64).saturating_mul(frsize);
        Some((avail, total))
    } else {
        None
    }
}

fn query_free_space(path: &Path) -> String {
    query_statvfs(path)
        .map(|(avail, _)| format_bytes(avail))
        .unwrap_or_default()
}

fn query_volume_details(path: &Path) -> String {
    query_statvfs(path)
        .map(|(avail, total)| {
            format!("{} livres de {}", format_bytes(avail), format_bytes(total))
        })
        .unwrap_or_else(|| "—".to_string())
}

fn compute_dir_stats(dir: &Path) -> (usize, usize, u64) {
    let mut folders = 0usize;
    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        if let Ok(rd) = fs::read_dir(&cur) {
            for ent in rd.flatten() {
                let p = ent.path();
                if let Ok(meta) = fs::symlink_metadata(&p) {
                    if meta.file_type().is_symlink() {
                        files += 1;
                    } else if meta.is_dir() {
                        folders += 1;
                        stack.push(p);
                    } else {
                        files += 1;
                        bytes = bytes.saturating_add(meta.len());
                    }
                }
            }
        }
    }
    (folders, files, bytes)
}

fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if s == "~" {
        return dirs::home_dir().unwrap_or_else(|| path.to_path_buf());
    }
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    path.to_path_buf()
}

fn unique_dest_path(dir: &Path, orig_name: &std::ffi::OsStr) -> PathBuf {
    let base = Path::new(orig_name);
    let stem = base.file_stem().and_then(|s| s.to_str()).unwrap_or("arquivo");
    let ext = base.extension().and_then(|s| s.to_str());
    for idx in 1..10_000 {
        let candidate_name = match (idx, ext) {
            (1, Some(e)) => format!("{stem} (cópia).{e}"),
            (1, None) => format!("{stem} (cópia)"),
            (n, Some(e)) => format!("{stem} (cópia {n}).{e}"),
            (n, None) => format!("{stem} (cópia {n})"),
        };
        let p = dir.join(candidate_name);
        if !p.exists() {
            return p;
        }
    }
    dir.join(orig_name)
}

fn copy_path_recursive(src: &Path, dest: &Path) -> std::io::Result<()> {
    let sym_meta = fs::symlink_metadata(src)?;
    if sym_meta.file_type().is_symlink() {
        let target = fs::read_link(src)?;
        return std::os::unix::fs::symlink(target, dest);
    }
    if sym_meta.is_dir() {
        fs::create_dir_all(dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_path_recursive(&entry.path(), &dest.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(src, dest)?;
        Ok(())
    }
}

fn move_path(src: &Path, dest: &Path) -> std::io::Result<()> {
    match fs::rename(src, dest) {
        Ok(()) => Ok(()),
        Err(_) => {
            copy_path_recursive(src, dest)?;
            if src.is_dir() && !src.is_symlink() {
                fs::remove_dir_all(src)
            } else {
                fs::remove_file(src)
            }
        }
    }
}

fn restore_single_trash_item(src: &Path, trash_info: &Path) -> Result<(), String> {
    let fname = src
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "Nome inválido".to_string())?;
    let info_path = trash_info.join(format!("{fname}.trashinfo"));
    let orig = fs::read_to_string(&info_path)
        .ok()
        .and_then(|txt| {
            txt.lines()
                .find_map(|l| l.strip_prefix("Path="))
                .map(percent_decode_path)
        })
        .ok_or_else(|| format!("Metadados de restauração ausentes para {fname}"))?;
    if let Some(parent) = orig.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let final_dest = if orig.exists() {
        let parent = orig.parent().unwrap_or_else(|| Path::new("/"));
        let name_os = orig.file_name().unwrap_or_else(|| src.file_name().unwrap());
        unique_dest_path(parent, name_os)
    } else {
        orig
    };
    move_path(src, &final_dest).map_err(|e| e.to_string())?;
    let _ = fs::remove_file(&info_path);
    Ok(())
}

fn percent_encode_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let mut out = String::with_capacity(raw.len());
    for b in raw.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn percent_decode_path(s: &str) -> PathBuf {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(val) = u8::from_str_radix(hex, 16) {
                    out.push(val);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(String::from_utf8_lossy(&out).to_string())
}

fn load_starred(path: &Path) -> HashSet<PathBuf> {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<PathBuf>>(&s).ok())
        .map(|v| v.into_iter().collect())
        .unwrap_or_default()
}

fn save_starred(path: &Path, set: &HashSet<PathBuf>) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let list: Vec<&PathBuf> = set.iter().collect();
    if let Ok(json) = serde_json::to_string_pretty(&list) {
        let _ = fs::write(path, json);
    }
}

fn build_places() -> Vec<SidebarPlace> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let mut list = vec![SidebarPlace {
        label: "Início".to_string(),
        path: home.clone(),
        icon: SidebarIcon::Home,
    }];

    let xdg_dirs = [
        ("Área de Trabalho", dirs::desktop_dir(), vec!["Desktop"], SidebarIcon::Desktop),
        ("Documentos", dirs::document_dir(), vec!["Documents", "Documentos"], SidebarIcon::Documents),
        ("Downloads", dirs::download_dir(), vec!["Downloads"], SidebarIcon::Downloads),
        ("Imagens", dirs::picture_dir(), vec!["Pictures", "Imagens"], SidebarIcon::Pictures),
        ("Vídeos", dirs::video_dir(), vec!["Videos", "Vídeos"], SidebarIcon::Videos),
        ("Músicas", dirs::audio_dir(), vec!["Music", "Músicas"], SidebarIcon::Music),
    ];

    for (label, xdg_opt, fallbacks, icon) in xdg_dirs {
        let resolved = xdg_opt.filter(|p| p.is_dir() && p != &home).or_else(|| {
            fallbacks
                .into_iter()
                .map(|sub| home.join(sub))
                .find(|p| p.is_dir())
        });
        if let Some(p) = resolved {
            list.push(SidebarPlace {
                label: label.to_string(),
                path: p,
                icon,
            });
        }
    }
    list
}

fn parse_udisksctl_mountpoint(stdout: &str) -> Option<PathBuf> {
    // e.g. "Mounted /dev/sdb1 at /run/media/pedro/USB."
    let idx = stdout.find(" at ")?;
    let rest = stdout[idx + 4..].trim().trim_end_matches('.');
    let p = PathBuf::from(rest);
    if p.is_dir() { Some(p) } else { None }
}

fn scan_devices() -> Vec<DeviceEntry> {
    let mut devs = Vec::new();
    let mut seen_mounts = HashSet::new();

    if let Ok(out) = Command::new("lsblk")
        .args([
            "-J",
            "-b",
            "-o",
            "NAME,LABEL,SIZE,MOUNTPOINT,TRAN,RM,TYPE,FSTYPE",
        ])
        .output()
    {
        if out.status.success() {
            if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                if let Some(arr) = val.get("blockdevices").and_then(|v| v.as_array()) {
                    for dev in arr {
                        collect_lsblk_devices(dev, false, &mut devs, &mut seen_mounts);
                    }
                }
            }
        }
    }

    // Fallback scan for any manual mounts in /run/media, /media, /mnt
    let user = std::env::var("USER").unwrap_or_default();
    for root in [
        PathBuf::from(format!("/run/media/{user}")),
        PathBuf::from(format!("/media/{user}")),
        PathBuf::from("/mnt"),
    ] {
        if let Ok(rd) = fs::read_dir(&root) {
            for ent in rd.flatten() {
                let p = ent.path();
                if p.is_dir() && !seen_mounts.contains(&p) {
                    let label = ent.file_name().to_string_lossy().to_string();
                    seen_mounts.insert(p.clone());
                    devs.push(DeviceEntry {
                        label,
                        dev_node: String::new(),
                        mount_point: Some(p),
                        size_str: "Montado".to_string(),
                    });
                }
            }
        }
    }
    devs
}

fn collect_lsblk_devices(
    node: &serde_json::Value,
    parent_removable: bool,
    out: &mut Vec<DeviceEntry>,
    seen_mounts: &mut HashSet<PathBuf>,
) {
    let tran = node.get("tran").and_then(|v| v.as_str()).unwrap_or("");
    let rm = node.get("rm").and_then(|v| v.as_bool()).unwrap_or(false);
    let removable = parent_removable || rm || tran == "usb" || tran == "mmc";

    let kind = node.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let fstype = node.get("fstype").and_then(|v| v.as_str()).unwrap_or("");
    let mp_str = node
        .get("mountpoint")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let is_external_mount = mp_str.is_some_and(|m| {
        m.starts_with("/run/media/") || m.starts_with("/media/") || m.starts_with("/mnt/")
    });

    if (kind == "part" || (kind == "disk" && !fstype.is_empty()))
        && (removable || is_external_mount)
        && !fstype.is_empty()
        && fstype != "swap"
    {
        let name = node.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let label = node
            .get("label")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("Unidade ({name})"));
        let size_bytes = node.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
        let mount_point = mp_str.map(PathBuf::from).filter(|p| p.is_dir());
        if let Some(ref mp) = mount_point {
            seen_mounts.insert(mp.clone());
        }
        out.push(DeviceEntry {
            label,
            dev_node: format!("/dev/{name}"),
            mount_point,
            size_str: format_bytes(size_bytes),
        });
    }

    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        for ch in children {
            collect_lsblk_devices(ch, removable, out, seen_mounts);
        }
    }
}

fn spawn_thumbnail_worker(tx: Sender<BgEvent>) -> Sender<PathBuf> {
    let (req_tx, req_rx) = unbounded::<PathBuf>();
    std::thread::spawn(move || {
        while let Ok(path) = req_rx.recv() {
            if let Ok(img) = image::open(&path) {
                let thumb = img.thumbnail(96, 96).to_rgba8();
                let (w, h) = thumb.dimensions();
                if w > 0 && h > 0 {
                    let _ = tx.send(BgEvent::ThumbReady(ThumbResult {
                        path,
                        size: [w as usize, h as usize],
                        rgba: thumb.into_raw(),
                    }));
                }
            }
        }
    });
    req_tx
}

fn main() {
    let start_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/"));

    let conn = Connection::connect_to_env().expect("Falha ao conectar ao Wayland");
    let (globals, mut event_queue) =
        registry_queue_init(&conn).expect("Falha ao inicializar fila Wayland");
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor indisponível");
    let xdg_shell = XdgShell::bind(&globals, &qh).expect("xdg_wm_base indisponível");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm indisponível");

    let wl_surface = compositor.create_surface(&qh);
    let wl_surface_clone = wl_surface.clone();
    let window = xdg_shell.create_window(wl_surface, WindowDecorations::RequestClient, &qh);
    window.set_title("Arquivos — RocketDE");
    window.set_app_id("rocketde-files");
    window.set_min_size(Some((760, 480)));
    window.commit();

    let mut instance_desc = wgpu::InstanceDescriptor::new_without_display_handle();
    instance_desc.backends = wgpu::Backends::VULKAN;
    let mut instance = wgpu::Instance::new(instance_desc);

    let raw_display_ptr = NonNull::new(conn.backend().display_ptr() as *mut _).unwrap();
    let raw_surface_ptr = NonNull::new(window.wl_surface().id().as_ptr() as *mut _).unwrap();
    let create_surface = |inst: &wgpu::Instance| unsafe {
        inst.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                raw_display_ptr,
            ))),
            raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(
                raw_surface_ptr,
            )),
        })
        .expect("Falha ao criar superfície wgpu")
    };

    let mut wgpu_surface = create_surface(&instance);
    let adapter = match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        force_fallback_adapter: false,
        compatible_surface: Some(&wgpu_surface),
        apply_limit_buckets: false,
    })) {
        Ok(a) => a,
        Err(_) => {
            drop(wgpu_surface);
            let mut gl_desc = wgpu::InstanceDescriptor::new_without_display_handle();
            gl_desc.backends = wgpu::Backends::GL;
            instance = wgpu::Instance::new(gl_desc);
            wgpu_surface = create_surface(&instance);
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: false,
                compatible_surface: Some(&wgpu_surface),
                apply_limit_buckets: false,
            }))
            .expect("Nenhum adaptador gráfico encontrado")
        }
    };

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("rocketde_files_device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
    }))
    .expect("Erro ao requisitar device wgpu");

    let caps = wgpu_surface.get_capabilities(&adapter);
    let format = caps
        .formats
        .iter()
        .copied()
        .find(|f| {
            matches!(
                f,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
            )
        })
        .unwrap_or(wgpu::TextureFormat::Bgra8Unorm);
    let alpha_mode = caps
        .alpha_modes
        .first()
        .copied()
        .unwrap_or(wgpu::CompositeAlphaMode::Auto);

    let init_w = 1060u32;
    let init_h = 680u32;
    let surface_config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space: wgpu::SurfaceColorSpace::Auto,
        width: init_w,
        height: init_h,
        present_mode: wgpu::PresentMode::Fifo,
        alpha_mode,
        view_formats: vec![],
        desired_maximum_frame_latency: 1,
    };

    let cfg = RocketConfig::load_or_create();
    let egui_ctx = Context::default();
    apply_macos_theme(&egui_ctx, &cfg);
    let renderer = Renderer::new(&device, format, RendererOptions::default());

    let (bg_tx, bg_rx) = unbounded::<BgEvent>();
    let thumb_tx = spawn_thumbnail_worker(bg_tx.clone());
    let fs_tx = bg_tx.clone();
    let watcher = notify::recommended_watcher(move |res: Result<notify::Event, _>| {
        if res.is_ok() {
            let _ = fs_tx.send(BgEvent::FsChanged);
        }
    })
    .ok();

    let data_home = dirs::data_local_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")).join(".local/share"));
    let trash_base = data_home.join("Trash");
    let starred_file = data_home.join("rocketde/starred.json");
    let starred = load_starred(&starred_file);

    let mut app = FilesApp {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        _compositor: compositor,
        shm,
        window,
        wl_surface: wl_surface_clone,
        device,
        queue,
        wgpu_surface,
        surface_config,
        egui_ctx,
        renderer,
        width: init_w,
        height: init_h,
        scale_factor: 1.0,
        configured: false,
        dirty: true,
        seat: None,
        last_pointer_serial: 0,
        is_maximized: false,
        pending_window_action: WindowAction::None,
        repaint_delay: None,
        pending_events: Vec::with_capacity(32),
        modifiers: Modifiers::NONE,
        keyboard: None,
        pointer: None,
        start_instant: Instant::now(),
        exit: false,
        cfg,
        current_dir: start_dir.clone(),
        viewing_starred: false,
        entries: Vec::new(),
        history_back: Vec::new(),
        history_fwd: Vec::new(),
        selected_paths: Vec::new(),
        selection_anchor: None,
        scroll_to_selection: false,
        marquee_start: None,
        marquee_active: false,
        marquee_base: Vec::new(),
        free_space_str: String::new(),
        view_mode: ViewMode::Grid,
        sort_field: SortField::Name,
        sort_asc: true,
        show_hidden: false,
        search_query: String::new(),
        focus_search: false,
        editing_path: false,
        focus_path: false,
        path_input: start_dir.display().to_string(),
        new_folder_mode: false,
        focus_new_folder: false,
        new_folder_name: String::new(),
        new_file_mode: false,
        focus_new_file: false,
        new_file_name: String::new(),
        rename_target: None,
        focus_rename: false,
        rename_buf: String::new(),
        delete_confirm_targets: None,
        empty_trash_confirm: false,
        properties_modal: None,
        context_menu: None,
        clipboard: None,
        undo_stack: Vec::new(),
        toast: None,
        places: build_places(),
        devices: scan_devices(),
        last_device_scan: Instant::now(),
        starred,
        starred_file,
        trash_dir: trash_base.join("files"),
        trash_info_dir: trash_base.join("info"),
        watcher,
        watched_dir: None,
        last_fs_refresh: Instant::now(),
        bg_tx,
        bg_rx,
        thumb_tx,
        thumb_cache: HashMap::new(),
        thumb_requested: HashSet::new(),
    };
    app.navigate_to(start_dir, false);

    let wayland_fd = conn.as_fd();
    loop {
        let _ = conn.flush();
        if let Some(guard) = conn.prepare_read() {
            let mut pfd = libc::pollfd {
                fd: wayland_fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let timeout_ms = if app.dirty || !app.pending_events.is_empty() || !app.bg_rx.is_empty() {
                16
            } else if let Some(d) = app.repaint_delay {
                d.as_millis().clamp(16, 250) as i32
            } else {
                200
            };
            let ret = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
            if ret > 0 && (pfd.revents & libc::POLLIN) != 0 {
                let _ = guard.read();
            } else {
                drop(guard);
            }
        }
        let _ = event_queue.dispatch_pending(&mut app);
        if app.exit {
            break;
        }
        if !app.bg_rx.is_empty() {
            app.dirty = true;
        }
        if app.dirty || !app.pending_events.is_empty() {
            app.render_frame();
        }
    }
}

impl CompositorHandler for FilesApp {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        let factor = new_factor.max(1);
        if (self.scale_factor - factor as f32).abs() > f32::EPSILON {
            self.scale_factor = factor as f32;
            surface.set_buffer_scale(factor);
            let _ = &self.wl_surface;
            if self.configured {
                self.resize(self.width, self.height);
            }
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.dirty = true;
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for FilesApp {
    fn output_state(&mut self) -> &mut OutputState { &mut self.output_state }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for FilesApp {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &Window,
        configure: WindowConfigure,
        _: u32,
    ) {
        let w = configure.new_size.0.map(NonZeroU32::get).unwrap_or(1060);
        let h = configure.new_size.1.map(NonZeroU32::get).unwrap_or(680);
        self.is_maximized = configure.is_maximized();
        self.resize(w, h);
        self.render_frame();
    }
}

impl SeatHandler for FilesApp {
    fn seat_state(&mut self) -> &mut SeatState { &mut self.seat_state }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        self.seat = Some(seat.clone());
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            if let Ok(kb) = self.seat_state.get_keyboard(qh, &seat, None) {
                self.keyboard = Some(kb);
            }
        }
        if cap == Capability::Pointer && self.pointer.is_none() {
            if let Ok(ptr) = self.seat_state.get_pointer(qh, &seat) {
                self.pointer = Some(ptr);
            }
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard {
            if let Some(kb) = self.keyboard.take() { kb.release(); }
        }
        if cap == Capability::Pointer {
            if let Some(ptr) = self.pointer.take() { ptr.release(); }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for FilesApp {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {
        self.modifiers = Modifiers::NONE;
    }
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.push_key(event, true);
    }
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.push_key(event, true);
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.push_key(event, false);
    }
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, mods: WlModifiers, _: RawModifiers, _: u32) {
        self.modifiers = Modifiers {
            alt: mods.alt,
            ctrl: mods.ctrl,
            shift: mods.shift,
            mac_cmd: mods.logo,
            command: mods.ctrl || mods.logo,
        };
    }
}

impl FilesApp {
    fn push_key(&mut self, event: KeyEvent, pressed: bool) {
        if let Some(key) = translate_keysym(event.keysym) {
            self.pending_events.push(Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: self.modifiers,
            });
            push_clipboard_events(key, pressed, self.modifiers, &mut self.pending_events);
        }
        if pressed && !self.modifiers.ctrl && !self.modifiers.alt && !self.modifiers.mac_cmd {
            if let Some(txt) = &event.utf8 {
                let filtered: String = txt.chars().filter(|c| !c.is_control()).collect();
                if !filtered.is_empty() {
                    self.pending_events.push(Event::Text(filtered));
                }
            }
        }
        self.dirty = true;
    }
}

impl PointerHandler for FilesApp {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for ev in events {
            let pos = pos2(ev.position.0 as f32, ev.position.1 as f32);
            match ev.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    self.pending_events.push(Event::PointerMoved(pos));
                }
                PointerEventKind::Leave { .. } => {
                    self.pending_events.push(Event::PointerGone);
                }
                PointerEventKind::Press { button, serial, .. } => {
                    self.last_pointer_serial = serial;
                    if let Some(btn) = translate_btn(button) {
                        self.pending_events.push(Event::PointerButton {
                            pos,
                            button: btn,
                            pressed: true,
                            modifiers: self.modifiers,
                        });
                    }
                }
                PointerEventKind::Release { button, .. } => {
                    if let Some(btn) = translate_btn(button) {
                        self.pending_events.push(Event::PointerButton {
                            pos,
                            button: btn,
                            pressed: false,
                            modifiers: self.modifiers,
                        });
                    }
                }
                PointerEventKind::Axis { horizontal, vertical, .. } => {
                    self.pending_events.push(Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: Vec2::new(-horizontal.absolute as f32, -vertical.absolute as f32),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Unknown,
                        modifiers: self.modifiers,
                    });
                }
            }
            self.dirty = true;
        }
    }
}

fn to_xdg_edge(
    dir: ResizeDir,
) -> smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge {
    use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge;
    match dir {
        ResizeDir::Top => ResizeEdge::Top,
        ResizeDir::Bottom => ResizeEdge::Bottom,
        ResizeDir::Left => ResizeEdge::Left,
        ResizeDir::Right => ResizeEdge::Right,
        ResizeDir::TopLeft => ResizeEdge::TopLeft,
        ResizeDir::TopRight => ResizeEdge::TopRight,
        ResizeDir::BottomLeft => ResizeEdge::BottomLeft,
        ResizeDir::BottomRight => ResizeEdge::BottomRight,
    }
}

fn translate_btn(b: u32) -> Option<PointerButton> {
    match b {
        0x110 => Some(PointerButton::Primary),
        0x111 => Some(PointerButton::Secondary),
        0x112 => Some(PointerButton::Middle),
        _ => None,
    }
}

fn translate_keysym(sym: Keysym) -> Option<Key> {
    rocketde_ui_common::keysym::translate_keysym_raw(sym.raw())
}

impl ShmHandler for FilesApp {
    fn shm_state(&mut self) -> &mut Shm { &mut self.shm }
}

impl ProvidesRegistryState for FilesApp {
    fn registry(&mut self) -> &mut RegistryState { &mut self.registry_state }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(FilesApp);
delegate_registry!(FilesApp);
