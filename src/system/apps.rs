use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::thread;

#[derive(Debug, Clone)]
pub struct DesktopApp {
    pub name: String,
    pub description: String,
    pub exec: String,
    pub app_id: String,
    pub icon_name: String,
    pub icon_path: Option<PathBuf>,
    pub categories: String,
    pub glyph: String,
}

pub struct AppIndexer {
    pub apps: Vec<DesktopApp>,
    pub texture_gen: u64,
    matcher: SkimMatcherV2,
    decoded_images: HashMap<String, Option<Arc<ColorImage>>>,
    pending_icons: HashSet<String>,
    icon_req_tx: Sender<(String, PathBuf)>,
    icon_res_rx: Receiver<(String, Option<ColorImage>)>,
    apps_scan_rx: Option<Receiver<Vec<DesktopApp>>>,
    cached_query: Option<String>,
    cached_indices: Vec<usize>,
}

impl AppIndexer {
    pub fn new() -> Self {
        Self::new_with_wake(|| {})
    }

    pub fn new_with_wake(wake_fn: impl Fn() + Send + 'static) -> Self {
        let (apps_tx, apps_rx) = crossbeam_channel::bounded::<Vec<DesktopApp>>(1);
        thread::Builder::new()
            .name("rocketde-app-scanner".to_string())
            .spawn(move || {
                let mut apps = scan_desktop_entries();
                if apps.is_empty() {
                    apps = fallback_apps();
                }
                apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                let _ = apps_tx.send(apps);
                wake_fn();
            })
            .ok();

        let (icon_req_tx, icon_req_rx) = unbounded::<(String, PathBuf)>();
        let (icon_res_tx, icon_res_rx) = unbounded::<(String, Option<ColorImage>)>();

        thread::Builder::new()
            .name("rocketde-icon-loader".to_string())
            .spawn(move || {
                while let Ok((key, path)) = icon_req_rx.recv() {
                    let img = load_icon_file_to_color_image(&path, 96);
                    if icon_res_tx.send((key, img)).is_err() {
                        break;
                    }
                }
            })
            .ok();

        let pending_icons = HashSet::new();

        Self {
            apps: Vec::new(),
            texture_gen: 0,
            matcher: SkimMatcherV2::default(),
            decoded_images: HashMap::new(),
            pending_icons,
            icon_req_tx,
            icon_res_rx,
            apps_scan_rx: Some(apps_rx),
            cached_query: None,
            cached_indices: Vec::new(),
        }
    }

    /// Releases Launchpad-only icon images when the launcher closes, keeping Dock icons cached.
    pub fn retain_dock_icons(&mut self, keep_keys: &[String]) {
        self.decoded_images
            .retain(|k, _| keep_keys.iter().any(|keep| keep == k));
        self.pending_icons.clear();
        self.cached_query = None;
        self.cached_indices.clear();
        self.texture_gen = self.texture_gen.wrapping_add(1);
    }

    /// Collects any background-decoded icon images and returns true if new images arrived.
    pub fn poll_decoded_icons(&mut self, ctx: &Context) -> bool {
        let mut updated = false;
        if let Some(ref rx) = self.apps_scan_rx {
            if let Ok(scanned) = rx.try_recv() {
                self.apps = scanned;
                self.apps_scan_rx = None;
                // Drop negative cache entries so icons that needed `.desktop` metadata can now resolve
                self.decoded_images.retain(|_, v| v.is_some());
                self.cached_query = None;
                self.cached_indices.clear();
                updated = true;
            }
        }
        while let Ok((key, maybe_img)) = self.icon_res_rx.try_recv() {
            self.pending_icons.remove(&key);
            self.decoded_images.insert(key, maybe_img.map(Arc::new));
            updated = true;
        }
        if updated {
            self.texture_gen = self.texture_gen.wrapping_add(1);
            ctx.request_repaint();
        }
        updated
    }

    /// Loads or retrieves a per-Context GPU texture for the given icon name or app_id!
    /// Each Wayland surface has its own egui::Context & Renderer, so TextureHandles are stored
    /// in `ctx.data` to avoid cross-renderer TextureId mismatches.
    pub fn get_icon_texture(
        &mut self,
        ctx: &Context,
        key: &str,
        explicit_path: Option<&Path>,
    ) -> Option<TextureHandle> {
        if key.is_empty() {
            return None;
        }

        let tex_cache_id = egui::Id::unique(("rocketde_app_icon_tex", key));
        if let Some(handle) = ctx.data(|d| d.get_temp::<TextureHandle>(tex_cache_id)) {
            return Some(handle);
        }

        if let Some(maybe_img) = self.decoded_images.get(key) {
            let color_img = maybe_img.as_ref()?;
            let handle = ctx.load_texture(
                format!("app_icon_{}", key),
                (**color_img).clone(),
                TextureOptions::LINEAR,
            );
            ctx.data_mut(|d| d.insert_temp(tex_cache_id, handle.clone()));
            return Some(handle);
        }

        if self.pending_icons.contains(key) {
            return None;
        }

        let resolved_path = explicit_path
            .map(|p| p.to_path_buf())
            .or_else(|| {
                let key_lower = key.to_lowercase();
                self.apps
                    .iter()
                    .find(|a| {
                        a.app_id.eq_ignore_ascii_case(key)
                            || a.icon_name.eq_ignore_ascii_case(key)
                            || a.name.eq_ignore_ascii_case(key)
                            || a.exec.to_lowercase().contains(&key_lower)
                    })
                    .and_then(|a| a.icon_path.clone())
            })
            .or_else(|| resolve_icon_path(key));

        if let Some(path) = resolved_path {
            self.pending_icons.insert(key.to_string());
            let _ = self.icon_req_tx.send((key.to_string(), path));
        } else {
            self.decoded_images.insert(key.to_string(), None);
        }
        None
    }

    /// Returns cached indices into `self.apps` matching `query`, recomputing only when `query` changes.
    pub fn search_indices(&mut self, query: &str) -> &[usize] {
        let q = query.trim();
        if self.cached_query.as_deref() == Some(q) {
            return &self.cached_indices;
        }

        self.cached_indices.clear();
        if q.is_empty() {
            self.cached_indices.extend(0..self.apps.len());
        } else {
            let mut scored: Vec<(usize, i64)> = self
                .apps
                .iter()
                .enumerate()
                .filter_map(|(idx, app)| {
                    let s_name = self.matcher.fuzzy_match(&app.name, q).unwrap_or(0) * 3;
                    let s_desc = self.matcher.fuzzy_match(&app.description, q).unwrap_or(0);
                    let s_exec = self.matcher.fuzzy_match(&app.exec, q).unwrap_or(0) * 2;
                    let best = s_name.max(s_desc).max(s_exec);
                    if best > 0 {
                        Some((idx, best))
                    } else {
                        None
                    }
                })
                .collect();
            scored.sort_by(|a, b| b.1.cmp(&a.1));
            self.cached_indices.extend(scored.into_iter().map(|(i, _)| i));
        }

        self.cached_query = Some(q.to_string());
        &self.cached_indices
    }

    pub fn search(&self, query: &str, max_results: usize) -> Vec<(DesktopApp, i64)> {
        let q = query.trim();
        if q.is_empty() {
            return self
                .apps
                .iter()
                .take(max_results)
                .map(|a| (a.clone(), 100))
                .collect();
        }

        let mut scored: Vec<(DesktopApp, i64)> = self
            .apps
            .iter()
            .filter_map(|app| {
                let s_name = self.matcher.fuzzy_match(&app.name, q).unwrap_or(0) * 3;
                let s_desc = self.matcher.fuzzy_match(&app.description, q).unwrap_or(0);
                let s_exec = self.matcher.fuzzy_match(&app.exec, q).unwrap_or(0) * 2;
                let best = s_name.max(s_desc).max(s_exec);
                if best > 0 {
                    Some((app.clone(), best))
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by(|a, b| b.1.cmp(&a.1));
        scored.truncate(max_results);
        scored
    }

    pub fn launch(exec_cmd: &str) {
        let cleaned = clean_exec_command(exec_cmd);
        if cleaned.is_empty() {
            return;
        }
        std::thread::spawn(move || {
            let _ = Command::new("/bin/sh")
                .arg("-c")
                .arg(&cleaned)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        });
    }
}

fn icon_index() -> &'static HashMap<String, PathBuf> {
    static INDEX: OnceLock<HashMap<String, PathBuf>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let search_dirs = [
            "/usr/share/icons/hicolor/128x128/apps",
            "/usr/share/icons/hicolor/64x64/apps",
            "/usr/share/icons/hicolor/48x48/apps",
            "/usr/share/icons/hicolor/256x256/apps",
            "/usr/share/icons/hicolor/scalable/apps",
            "/usr/share/pixmaps",
            "/usr/share/icons/Adwaita/scalable/apps",
            "/usr/share/icons/Adwaita/64x64/apps",
            "/var/lib/flatpak/exports/share/icons/hicolor/128x128/apps",
            "/var/lib/flatpak/exports/share/icons/hicolor/scalable/apps",
        ];

        let mut map = HashMap::with_capacity(512);
        for dir in &search_dirs {
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let ext = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                if !ext.eq_ignore_ascii_case("png") && !ext.eq_ignore_ascii_case("svg") {
                    continue;
                }
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    map.entry(stem.to_lowercase()).or_insert(path);
                }
            }
        }
        map
    })
}

/// Resolves an XDG icon name (e.g. `firefox`, `foot`, `Alacritty`, `signal-desktop`) or path to an actual `.png`/`.svg` file on disk
pub fn resolve_icon_path(icon_name: &str) -> Option<PathBuf> {
    let trimmed = icon_name.trim();
    if trimmed.is_empty() {
        return None;
    }

    let as_path = Path::new(trimmed);
    if as_path.is_absolute() && as_path.exists() {
        return Some(as_path.to_path_buf());
    }

    let idx = icon_index();
    let lower = trimmed.to_lowercase();
    if let Some(p) = idx.get(&lower) {
        return Some(p.clone());
    }
    if let Some(stem) = lower.strip_suffix(".desktop") {
        if let Some(p) = idx.get(stem) {
            return Some(p.clone());
        }
    }

    None
}

fn load_icon_file_to_color_image(path: &Path, target_px: u32) -> Option<ColorImage> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    if ext == "svg" || ext == "svgz" {
        let data = fs::read(path).ok()?;
        let opt = resvg::usvg::Options::default();
        let tree = resvg::usvg::Tree::from_data(&data, &opt).ok()?;
        let size = tree.size();
        if size.width() <= 0.0 || size.height() <= 0.0 {
            return None;
        }
        let mut pixmap = resvg::tiny_skia::Pixmap::new(target_px, target_px)?;
        let sx = (target_px as f32) / size.width();
        let sy = (target_px as f32) / size.height();
        let scale = sx.min(sy);
        let tx = ((target_px as f32) - size.width() * scale) * 0.5;
        let ty = ((target_px as f32) - size.height() * scale) * 0.5;
        let transform =
            resvg::tiny_skia::Transform::from_scale(scale, scale).post_translate(tx, ty);
        resvg::render(&tree, transform, &mut pixmap.as_mut());

        // tiny-skia stores premultiplied RGBA; convert to unmultiplied for ColorImage::from_rgba_unmultiplied
        let mut rgba = Vec::with_capacity((target_px * target_px * 4) as usize);
        for px in pixmap.pixels() {
            let c = px.demultiply();
            rgba.push(c.red());
            rgba.push(c.green());
            rgba.push(c.blue());
            rgba.push(c.alpha());
        }
        Some(ColorImage::from_rgba_unmultiplied(
            [target_px as usize, target_px as usize],
            &rgba,
        ))
    } else {
        let img = image::open(path).ok()?;
        let resized = img.resize_exact(
            target_px,
            target_px,
            image::imageops::FilterType::Triangle,
        );
        let rgba = resized.to_rgba8();
        let dims = [rgba.width() as usize, rgba.height() as usize];
        Some(ColorImage::from_rgba_unmultiplied(dims, rgba.as_raw()))
    }
}

pub fn clean_exec_command(raw: &str) -> String {
    raw.split_whitespace()
        .filter(|tok| !tok.starts_with('%'))
        .collect::<Vec<_>>()
        .join(" ")
}

fn scan_desktop_entries() -> Vec<DesktopApp> {
    let mut dirs_to_scan = vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
    ];
    if let Some(data_dir) = dirs::data_dir() {
        dirs_to_scan.push(data_dir.join("applications"));
    }

    let mut seen_names = HashSet::new();
    let mut result = Vec::new();

    for dir in dirs_to_scan {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("desktop") {
                continue;
            }
            if let Ok(content) = fs::read_to_string(&path) {
                if let Some(app) = parse_desktop_file(&path, &content) {
                    if seen_names.insert(app.name.to_lowercase()) {
                        result.push(app);
                    }
                }
            }
        }
    }

    result
}

fn parse_desktop_file(path: &PathBuf, content: &str) -> Option<DesktopApp> {
    let mut in_desktop_entry = false;
    let mut name = String::new();
    let mut comment = String::new();
    let mut exec = String::new();
    let mut icon = String::new();
    let mut categories = String::new();
    let mut startup_wm_class = String::new();
    let mut no_display = false;
    let mut app_type = String::from("Application");

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_desktop_entry = trimmed == "[Desktop Entry]";
            continue;
        }
        if !in_desktop_entry || trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }

        if let Some((k, v)) = trimmed.split_once('=') {
            match k.trim() {
                "Name" if name.is_empty() => name = v.trim().to_string(),
                "Comment" | "GenericName" if comment.is_empty() => comment = v.trim().to_string(),
                "Exec" if exec.is_empty() => exec = v.trim().to_string(),
                "Icon" if icon.is_empty() => icon = v.trim().to_string(),
                "StartupWMClass" if startup_wm_class.is_empty() => {
                    startup_wm_class = v.trim().to_string()
                }
                "Categories" => categories = v.trim().to_string(),
                "NoDisplay" => no_display = v.trim().eq_ignore_ascii_case("true"),
                "Hidden" if v.trim().eq_ignore_ascii_case("true") => no_display = true,
                "Type" => app_type = v.trim().to_string(),
                _ => {}
            }
        }
    }

    if no_display || app_type != "Application" || name.is_empty() || exec.is_empty() {
        return None;
    }

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&name)
        .to_string();

    let app_id = if !startup_wm_class.is_empty() {
        startup_wm_class
    } else {
        stem.clone()
    };

    let icon_name = if !icon.is_empty() {
        icon
    } else {
        stem.clone()
    };

    let icon_path = resolve_icon_path(&icon_name).or_else(|| resolve_icon_path(&app_id));
    let glyph = pick_app_glyph(&name, &categories, &exec);

    Some(DesktopApp {
        name,
        description: if comment.is_empty() {
            clean_exec_command(&exec)
        } else {
            comment
        },
        exec,
        app_id,
        icon_name,
        icon_path,
        categories,
        glyph,
    })
}

pub fn pick_app_glyph(name: &str, categories: &str, exec: &str) -> String {
    let lower = format!("{} {} {}", name, categories, exec).to_lowercase();
    if lower.contains("term")
        || lower.contains("foot")
        || lower.contains("kitty")
        || lower.contains("alacritty")
    {
        "🖥".to_string()
    } else if lower.contains("firefox")
        || lower.contains("chrom")
        || lower.contains("browser")
        || lower.contains("web")
    {
        "🌐".to_string()
    } else if lower.contains("file")
        || lower.contains("nautilus")
        || lower.contains("thunar")
        || lower.contains("dolphin")
    {
        "📁".to_string()
    } else if lower.contains("code")
        || lower.contains("zed")
        || lower.contains("nvim")
        || lower.contains("vim")
        || lower.contains("devel")
    {
        "⚡".to_string()
    } else if lower.contains("signal")
        || lower.contains("discord")
        || lower.contains("telegram")
        || lower.contains("chat")
    {
        "💬".to_string()
    } else if lower.contains("audio")
        || lower.contains("music")
        || lower.contains("spotify")
        || lower.contains("pavucontrol")
    {
        "🎵".to_string()
    } else if lower.contains("setting") || lower.contains("config") || lower.contains("control") {
        "⚙".to_string()
    } else if lower.contains("game") || lower.contains("steam") {
        "🎮".to_string()
    } else {
        "🚀".to_string()
    }
}

/// Evaluates simple math expressions inside the Launchpad (e.g., `25 * 4 + 10`)
pub fn try_eval_math(input: &str) -> Option<f64> {
    let expr = input.trim().trim_start_matches('=').trim();
    if expr.is_empty() || !expr.chars().any(|c| "+-*/^".contains(c)) {
        return None;
    }
    if !expr
        .chars()
        .all(|c| c.is_ascii_digit() || "+-*/().^ ".contains(c))
    {
        return None;
    }
    eval_add_sub(expr)
}

fn eval_add_sub(s: &str) -> Option<f64> {
    let s = s.trim();
    let mut depth = 0i32;
    let bytes = s.as_bytes();
    for i in (0..bytes.len()).rev() {
        match bytes[i] {
            b')' => depth += 1,
            b'(' => depth -= 1,
            b'+' if depth == 0 && i > 0 => {
                return Some(eval_add_sub(&s[..i])? + eval_mul_div(&s[i + 1..])?);
            }
            b'-' if depth == 0 && i > 0 => {
                let prev = s[..i].trim().chars().last().unwrap_or('+');
                if !"+-*/(".contains(prev) {
                    return Some(eval_add_sub(&s[..i])? - eval_mul_div(&s[i + 1..])?);
                }
            }
            _ => {}
        }
    }
    eval_mul_div(s)
}

fn eval_mul_div(s: &str) -> Option<f64> {
    let s = s.trim();
    let mut depth = 0i32;
    let bytes = s.as_bytes();
    for i in (0..bytes.len()).rev() {
        match bytes[i] {
            b')' => depth += 1,
            b'(' => depth -= 1,
            b'*' if depth == 0 => {
                return Some(eval_mul_div(&s[..i])? * eval_atom(&s[i + 1..])?);
            }
            b'/' if depth == 0 => {
                let denom = eval_atom(&s[i + 1..])?;
                if denom.abs() < f64::EPSILON {
                    return None;
                }
                return Some(eval_mul_div(&s[..i])? / denom);
            }
            _ => {}
        }
    }
    eval_atom(s)
}

fn eval_atom(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.starts_with('(') && s.ends_with(')') && s.len() >= 2 {
        return eval_add_sub(&s[1..s.len() - 1]);
    }
    s.parse::<f64>().ok()
}

fn fallback_apps() -> Vec<DesktopApp> {
    vec![
        DesktopApp {
            name: "Foot".to_string(),
            description: "Terminal Wayland ultrarrápido".to_string(),
            exec: "foot".to_string(),
            app_id: "foot".to_string(),
            icon_name: "foot".to_string(),
            icon_path: resolve_icon_path("foot"),
            categories: "System;TerminalEmulator;".to_string(),
            glyph: "🖥".to_string(),
        },
        DesktopApp {
            name: "Firefox".to_string(),
            description: "Navegador de Internet".to_string(),
            exec: "firefox".to_string(),
            app_id: "firefox".to_string(),
            icon_name: "firefox".to_string(),
            icon_path: resolve_icon_path("firefox"),
            categories: "Network;WebBrowser;".to_string(),
            glyph: "🌐".to_string(),
        },
        DesktopApp {
            name: "Signal".to_string(),
            description: "Mensageiro Privado".to_string(),
            exec: "signal-desktop".to_string(),
            app_id: "signal-desktop".to_string(),
            icon_name: "signal-desktop".to_string(),
            icon_path: resolve_icon_path("signal-desktop"),
            categories: "Network;InstantMessaging;".to_string(),
            glyph: "💬".to_string(),
        },
    ]
}
