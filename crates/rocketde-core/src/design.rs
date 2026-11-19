use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Centralized visual design tokens for RocketDE, persisted in `~/.rocket/theme/theme.ron`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ThemeConfig {
    pub name: String,
    pub bar_bg_rgba: [u8; 4],
    pub dock_bg_rgba: [u8; 4],
    pub panel_bg_rgba: [u8; 4],
    pub border_rgba: [u8; 4],
    pub accent_rgb: [u8; 3],
    pub text_rgb: [u8; 3],
    pub text_muted_rgb: [u8; 3],
    pub surface: SurfaceColors,
    pub radii: DesignRadii,
    pub spacing: DesignSpacing,
    pub typography: DesignTypography,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SurfaceColors {
    pub card_bg_rgba: [u8; 4],
    pub elevated_bg_rgba: [u8; 4],
    pub inactive_rgba: [u8; 4],
    pub hover_rgba: [u8; 4],
    pub danger_rgb: [u8; 3],
    pub success_rgb: [u8; 3],
    pub warning_rgb: [u8; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DesignRadii {
    pub window: f32,
    pub panel: f32,
    pub card: f32,
    pub button: f32,
    pub pill: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DesignSpacing {
    pub header_height: f32,
    pub xs: f32,
    pub sm: f32,
    pub md: f32,
    pub lg: f32,
    pub item_x: f32,
    pub item_y: f32,
    pub button_x: f32,
    pub button_y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DesignTypography {
    pub body: f32,
    pub button: f32,
    pub heading: f32,
    pub small: f32,
}

impl Default for SurfaceColors {
    fn default() -> Self {
        Self {
            card_bg_rgba: [28, 31, 46, 220],
            elevated_bg_rgba: [36, 40, 58, 240],
            inactive_rgba: [255, 255, 255, 18],
            hover_rgba: [255, 255, 255, 38],
            danger_rgb: [255, 69, 58],
            success_rgb: [48, 209, 88],
            warning_rgb: [255, 159, 10],
        }
    }
}

impl Default for DesignRadii {
    fn default() -> Self {
        Self {
            window: 14.0,
            panel: 14.0,
            card: 10.0,
            button: 7.0,
            pill: 999.0,
        }
    }
}

impl Default for DesignSpacing {
    fn default() -> Self {
        Self {
            header_height: 40.0,
            xs: 4.0,
            sm: 6.0,
            md: 10.0,
            lg: 16.0,
            item_x: 8.0,
            item_y: 6.0,
            button_x: 8.0,
            button_y: 3.0,
        }
    }
}

impl Default for DesignTypography {
    fn default() -> Self {
        Self {
            body: 13.0,
            button: 13.0,
            heading: 18.0,
            small: 11.0,
        }
    }
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            name: "Rocket Dark".to_string(),
            bar_bg_rgba: [14, 15, 22, 225],
            dock_bg_rgba: [18, 20, 30, 210],
            panel_bg_rgba: [20, 22, 34, 242],
            border_rgba: [255, 255, 255, 36],
            accent_rgb: [10, 132, 255],
            text_rgb: [242, 244, 248],
            text_muted_rgb: [155, 162, 176],
            surface: SurfaceColors::default(),
            radii: DesignRadii::default(),
            spacing: DesignSpacing::default(),
            typography: DesignTypography::default(),
        }
    }
}

impl ThemeConfig {
    /// Directory where RocketDE `.ron` themes live: `~/.rocket/theme/`.
    pub fn theme_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".rocket")
            .join("theme")
    }

    /// Active theme file path: `~/.rocket/theme/theme.ron`.
    pub fn theme_path() -> PathBuf {
        Self::theme_dir().join("theme.ron")
    }

    /// Loads `~/.rocket/theme/theme.ron`, or creates it with defaults if missing.
    pub fn load_or_create() -> Self {
        let path = Self::theme_path();
        if path.exists() {
            match fs::read_to_string(&path) {
                Ok(content) => match ron::from_str::<ThemeConfig>(&content) {
                    Ok(theme) => return theme,
                    Err(err) => {
                        eprintln!(
                            "[rocketde-core] Failed to parse {}: {err}; using defaults",
                            path.display()
                        );
                        return Self::default();
                    }
                },
                Err(err) => {
                    eprintln!(
                        "[rocketde-core] Failed to read {}: {err}; using defaults",
                        path.display()
                    );
                    return Self::default();
                }
            }
        }

        let default_theme = Self::default();
        default_theme.save();
        default_theme
    }

    /// Persists the active theme to `~/.rocket/theme/theme.ron`, returning an error message on failure.
    pub fn try_save(&self) -> Result<(), String> {
        let path = Self::theme_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let pretty = ron::ser::PrettyConfig::new()
            .depth_limit(4)
            .separate_tuple_members(true)
            .enumerate_arrays(false);
        let serialized = ron::ser::to_string_pretty(self, pretty)
            .map_err(|e| format!("serialize theme: {e}"))?;
        fs::write(&path, serialized).map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(())
    }

    /// Persists the active theme to `~/.rocket/theme/theme.ron`.
    pub fn save(&self) {
        if let Err(err) = self.try_save() {
            eprintln!("[rocketde-core] Failed to save theme: {err}");
        }
    }

    /// Lists all `.ron` theme files inside `~/.rocket/theme/`.
    pub fn list_themes() -> Vec<PathBuf> {
        let dir = Self::theme_dir();
        if !dir.exists() {
            let _ = fs::create_dir_all(&dir);
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("ron"))
            })
            .collect();
        files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        files
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_path_is_under_dot_rocket_theme() {
        let path = ThemeConfig::theme_path();
        assert!(path.ends_with(".rocket/theme/theme.ron"));
    }

    #[test]
    fn ron_roundtrip_preserves_tokens() {
        let original = ThemeConfig::default();
        let serialized = ron::ser::to_string_pretty(&original, ron::ser::PrettyConfig::default())
            .expect("serialize theme to ron");
        let parsed: ThemeConfig = ron::from_str(&serialized).expect("deserialize theme from ron");
        assert_eq!(original, parsed);
    }

    #[test]
    fn partial_ron_uses_defaults() {
        let partial = r#"
(
    name: "Custom",
    accent_rgb: (255, 0, 128),
)
"#;
        let parsed: ThemeConfig = ron::from_str(partial).expect("parse partial ron");
        assert_eq!(parsed.name, "Custom");
        assert_eq!(parsed.accent_rgb, [255, 0, 128]);
        assert_eq!(parsed.radii, DesignRadii::default());
        assert_eq!(parsed.spacing, DesignSpacing::default());
        assert_eq!(parsed.typography, DesignTypography::default());
    }
}
