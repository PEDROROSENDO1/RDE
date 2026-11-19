use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Configurações persistentes do Gerenciador de Tarefas em `~/.config/rocketde/tasks.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TasksConfig {
    pub refresh_ms: u64,
    pub history_samples: usize,
    pub show_kernel_threads: bool,
    pub only_current_user: bool,
    pub confirm_before_kill: bool,
    pub remember_sudo_session: bool,
    pub show_iec_units: bool,
}

impl Default for TasksConfig {
    fn default() -> Self {
        Self {
            refresh_ms: 1000,
            history_samples: 60,
            show_kernel_threads: false,
            only_current_user: false,
            confirm_before_kill: true,
            remember_sudo_session: true,
            show_iec_units: true,
        }
    }
}

impl TasksConfig {
    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from(".config"))
            .join("rocketde")
            .join("tasks.toml")
    }

    pub fn load_or_create() -> Self {
        let path = Self::config_path();
        if path.exists() {
            match fs::read_to_string(&path) {
                Ok(content) => match toml::from_str::<TasksConfig>(&content) {
                    Ok(cfg) => return cfg,
                    Err(err) => {
                        eprintln!(
                            "[rocketde-tasks] Falha ao interpretar {}: {err}",
                            path.display()
                        );
                        return Self::default();
                    }
                },
                Err(err) => {
                    eprintln!(
                        "[rocketde-tasks] Falha ao ler {}: {err}",
                        path.display()
                    );
                    return Self::default();
                }
            }
        }
        let cfg = Self::default();
        cfg.save();
        cfg
    }

    pub fn save(&self) {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            if let Err(err) = fs::create_dir_all(parent) {
                eprintln!(
                    "[rocketde-tasks] Falha ao criar pasta {}: {err}",
                    parent.display()
                );
                return;
            }
        }
        match toml::to_string_pretty(self) {
            Ok(serialized) => {
                if let Err(err) = fs::write(&path, serialized) {
                    eprintln!(
                        "[rocketde-tasks] Falha ao gravar {}: {err}",
                        path.display()
                    );
                }
            }
            Err(err) => {
                eprintln!("[rocketde-tasks] Falha ao serializar config: {err}");
            }
        }
    }
}
