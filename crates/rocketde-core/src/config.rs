pub use crate::design::ThemeConfig;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RocketConfig {
    pub top_bar: TopBarConfig,
    pub dock: DockConfig,
    pub launcher: LauncherConfig,
    pub overview: OverviewConfig,
    #[serde(skip)]
    pub theme: ThemeConfig,
    pub capture: CaptureConfig,
    pub desktop: DesktopConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DesktopConfig {
    pub wallpaper_path: Option<String>,
    pub icon_pack: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OverviewPreviewMode {
    /// Captura 1 snapshot leve por janela ao abrir (padrão, ~0% CPU em repouso)
    #[default]
    Snapshot,
    /// Atualiza previews periodicamente enquanto o Overview estiver aberto
    Live,
    /// Ultra-leve: não captura frames do compositor, exibe cards estilizados com ícone do app
    Icons,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OverviewConfig {
    pub preview_mode: OverviewPreviewMode,
    pub compositor_blur: bool,
    pub animations: bool,
}

impl Default for OverviewConfig {
    fn default() -> Self {
        Self {
            preview_mode: OverviewPreviewMode::Snapshot,
            compositor_blur: false,
            animations: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TopBarConfig {
    pub height: u32,
    pub show_global_menu: bool,
    pub show_media: bool,
    pub show_cpu_ram: bool,
    pub clock_format: String,
    pub workspace_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DockConfig {
    pub enabled: bool,
    pub icon_size: f32,
    pub magnification: f32,
    pub margin_bottom: i32,
    pub pinned_apps: Vec<PinnedAppConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinnedAppConfig {
    pub name: String,
    pub app_id: String,
    pub exec: String,
    pub icon_glyph: String,
    pub accent_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LauncherConfig {
    pub width: f32,
    pub height: f32,
    pub max_results: usize,
    pub hotkey_hint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureConfig {
    pub screenshots_dir: Option<String>,
    pub recordings_dir: Option<String>,
    pub default_fps: u32,
    pub include_cursor: bool,
    pub copy_to_clipboard: bool,
    pub save_to_disk: bool,
    pub prefer_vaapi: bool,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            screenshots_dir: None,
            recordings_dir: None,
            default_fps: 30,
            include_cursor: true,
            copy_to_clipboard: true,
            save_to_disk: true,
            prefer_vaapi: true,
        }
    }
}

impl Default for RocketConfig {
    fn default() -> Self {
        Self {
            top_bar: TopBarConfig::default(),
            dock: DockConfig::default(),
            launcher: LauncherConfig::default(),
            overview: OverviewConfig::default(),
            theme: ThemeConfig::default(),
            capture: CaptureConfig::default(),
            desktop: DesktopConfig::default(),
        }
    }
}

impl Default for TopBarConfig {
    fn default() -> Self {
        Self {
            height: 30,
            show_global_menu: true,
            show_media: true,
            show_cpu_ram: true,
            clock_format: "%a %d %b  %H:%M".to_string(),
            workspace_count: 6,
        }
    }
}

impl Default for DockConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            icon_size: 48.0,
            magnification: 1.42,
            margin_bottom: 8,
            pinned_apps: vec![
                PinnedAppConfig {
                    name: "Launchpad".to_string(),
                    app_id: "rocketde-launcher".to_string(),
                    exec: "@launcher".to_string(),
                    icon_glyph: "🚀".to_string(),
                    accent_hex: "#FF5E3A".to_string(),
                },
                PinnedAppConfig {
                    name: "Foot".to_string(),
                    app_id: "foot".to_string(),
                    exec: "foot".to_string(),
                    icon_glyph: "🖥".to_string(),
                    accent_hex: "#2ECC71".to_string(),
                },
                PinnedAppConfig {
                    name: "Firefox".to_string(),
                    app_id: "firefox".to_string(),
                    exec: "firefox".to_string(),
                    icon_glyph: "🌐".to_string(),
                    accent_hex: "#3498DB".to_string(),
                },
                PinnedAppConfig {
                    name: "Signal".to_string(),
                    app_id: "signal-desktop".to_string(),
                    exec: "signal-desktop".to_string(),
                    icon_glyph: "💬".to_string(),
                    accent_hex: "#2980B9".to_string(),
                },
                PinnedAppConfig {
                    name: "btop++".to_string(),
                    app_id: "btop".to_string(),
                    exec: "foot btop".to_string(),
                    icon_glyph: "📊".to_string(),
                    accent_hex: "#E67E22".to_string(),
                },
            ],
        }
    }
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            width: 680.0,
            height: 440.0,
            max_results: 35,
            hotkey_hint: "Super + Space".to_string(),
        }
    }
}

impl RocketConfig {
    pub fn config_path() -> PathBuf {
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        base.join("rocketde").join("config.toml")
    }

    pub fn load_or_create() -> Self {
        let path = Self::config_path();
        if path.exists() {
            match fs::read_to_string(&path) {
                Ok(content) => match toml::from_str::<RocketConfig>(&content) {
                    Ok(mut cfg) => {
                        cfg.theme = ThemeConfig::load_or_create();
                        return cfg;
                    }
                    Err(err) => {
                        eprintln!(
                            "[rocketde-core] Failed to parse {}: {err}; using defaults without overwriting",
                            path.display()
                        );
                        let mut default_cfg = Self::default();
                        default_cfg.theme = ThemeConfig::load_or_create();
                        return default_cfg;
                    }
                },
                Err(err) => {
                    eprintln!(
                        "[rocketde-core] Failed to read {}: {err}; using defaults",
                        path.display()
                    );
                    let mut default_cfg = Self::default();
                    default_cfg.theme = ThemeConfig::load_or_create();
                    return default_cfg;
                }
            }
        }

        let mut default_cfg = Self::default();
        default_cfg.theme = ThemeConfig::load_or_create();
        default_cfg.save();
        default_cfg
    }

    pub fn try_save(&self) -> Result<(), String> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let serialized = toml::to_string_pretty(self).map_err(|e| format!("serialize config: {e}"))?;
        fs::write(&path, serialized).map_err(|e| format!("write {}: {e}", path.display()))?;
        self.theme.try_save()?;
        Ok(())
    }

    pub fn save(&self) {
        if let Err(err) = self.try_save() {
            eprintln!("[rocketde-core] Failed to save config: {err}");
        }
    }

    pub fn is_app_pinned(&self, app_id: &str, name: &str) -> bool {
        self.dock.pinned_apps.iter().any(|p| {
            (!app_id.is_empty() && !p.app_id.is_empty() && p.app_id.eq_ignore_ascii_case(app_id))
                || (!name.is_empty() && !p.name.is_empty() && p.name.eq_ignore_ascii_case(name))
        })
    }

    pub fn add_pinned_app(
        &mut self,
        name: &str,
        app_id: &str,
        exec: &str,
        glyph: &str,
    ) {
        if self.is_app_pinned(app_id, name) {
            return;
        }
        self.dock.pinned_apps.push(PinnedAppConfig {
            name: name.to_string(),
            app_id: app_id.to_string(),
            exec: exec.to_string(),
            icon_glyph: glyph.to_string(),
            accent_hex: "#3498DB".to_string(),
        });
        self.save();
    }

    pub fn remove_pinned_app_at(&mut self, index: usize) {
        if index < self.dock.pinned_apps.len() {
            self.dock.pinned_apps.remove(index);
            self.save();
        }
    }

    pub fn remove_pinned_by_id(&mut self, app_id: &str, name: &str) {
        let before = self.dock.pinned_apps.len();
        self.dock.pinned_apps.retain(|p| {
            let match_id = !app_id.is_empty()
                && !p.app_id.is_empty()
                && p.app_id.eq_ignore_ascii_case(app_id);
            let match_name =
                !name.is_empty() && !p.name.is_empty() && p.name.eq_ignore_ascii_case(name);
            !match_id && !match_name
        });
        if self.dock.pinned_apps.len() != before {
            self.save();
        }
    }

    /// Diretório de papéis de parede em `~/.rocket/backgrounds/`.
    pub fn backgrounds_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".rocket")
            .join("backgrounds")
    }

    /// Lista imagens suportadas em `~/.rocket/backgrounds/`, criando a pasta se não existir.
    pub fn list_backgrounds() -> Vec<PathBuf> {
        let dir = Self::backgrounds_dir();
        if !dir.exists() {
            let _ = fs::create_dir_all(&dir);
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file() && is_supported_image(p))
            .collect();
        files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        files
    }

    /// Diretório de pacotes de ícones em `~/.rocket/icons/`.
    pub fn icons_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".rocket")
            .join("icons")
    }

    /// Lista pacotes de ícones (subpastas) em `~/.rocket/icons/`, criando a pasta se não existir.
    pub fn list_icon_packs() -> Vec<String> {
        let dir = Self::icons_dir();
        if !dir.exists() {
            let _ = fs::create_dir_all(&dir);
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut packs: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .filter_map(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .filter(|name| !name.starts_with('.'))
                    .map(|name| name.to_string())
            })
            .collect();
        packs.sort_by_key(|a| a.to_lowercase());
        packs
    }
}

fn is_supported_image(path: &std::path::Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_overview_config_is_lightweight() {
        let cfg = RocketConfig::default();
        assert_eq!(cfg.overview.preview_mode, OverviewPreviewMode::Snapshot);
        assert!(!cfg.overview.compositor_blur);
        assert!(cfg.overview.animations);
    }

    #[test]
    fn config_deserializes_without_overview_section() {
        let partial = r#"
[top_bar]
height = 32
"#;
        let cfg: RocketConfig = toml::from_str(partial).expect("valid partial config");
        assert_eq!(cfg.top_bar.height, 32);
        assert_eq!(cfg.overview.preview_mode, OverviewPreviewMode::Snapshot);
        assert_eq!(cfg.desktop.icon_pack, None);
    }

    #[test]
    fn default_icon_pack_is_none() {
        let cfg = RocketConfig::default();
        assert_eq!(cfg.desktop.icon_pack, None);
        assert!(RocketConfig::icons_dir().ends_with(".rocket/icons"));
    }
}
