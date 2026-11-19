use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct TabDoc {
    pub id: String,
    pub title: String,
    pub path: Option<PathBuf>,
    pub content: String,
    pub dirty: bool,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct SavedTabMeta {
    id: String,
    title: String,
    path: Option<PathBuf>,
    dirty: bool,
    has_draft: bool,
}

impl Default for SavedTabMeta {
    fn default() -> Self {
        Self {
            id: String::new(),
            title: "Sem título".to_string(),
            path: None,
            dirty: false,
            has_draft: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct SessionManifest {
    active_tab: usize,
    font_size: f32,
    next_untitled: usize,
    word_wrap: bool,
    tabs: Vec<SavedTabMeta>,
}

impl Default for SessionManifest {
    fn default() -> Self {
        Self {
            active_tab: 0,
            font_size: 14.0,
            next_untitled: 1,
            word_wrap: false,
            tabs: Vec::new(),
        }
    }
}

pub fn default_session_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("rocketde-editor")
        .join("session")
}

pub fn load_session_from(
    session_dir: &Path,
) -> (Vec<TabDoc>, usize, f32, usize, bool) {
    let manifest_path = session_dir.join("session.json");
    let Ok(raw) = fs::read_to_string(&manifest_path) else {
        return (Vec::new(), 0, 14.0, 1, false);
    };
    let manifest: SessionManifest = match serde_json::from_str(&raw) {
        Ok(m) => m,
        Err(err) => {
            eprintln!(
                "[rocketde-editor] Failed to parse {}: {err}",
                manifest_path.display()
            );
            return (Vec::new(), 0, 14.0, 1, false);
        }
    };

    let drafts_dir = session_dir.join("drafts");
    let mut tabs = Vec::with_capacity(manifest.tabs.len());
    for meta in manifest.tabs {
        if meta.id.is_empty() {
            continue;
        }
        let draft_path = drafts_dir.join(format!("{}.txt", meta.id));
        let content = if meta.has_draft && draft_path.exists() {
            match fs::read_to_string(&draft_path) {
                Ok(text) => text,
                Err(err) => {
                    eprintln!(
                        "[rocketde-editor] Failed to read draft {}: {err}",
                        draft_path.display()
                    );
                    continue;
                }
            }
        } else if let Some(ref p) = meta.path {
            match fs::read_to_string(p) {
                Ok(text) => text,
                Err(_) => continue,
            }
        } else {
            String::new()
        };

        let title = if let Some(ref p) = meta.path {
            p.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(&meta.title)
                .to_string()
        } else {
            meta.title
        };

        tabs.push(TabDoc {
            id: meta.id,
            title,
            path: meta.path,
            content,
            dirty: meta.dirty,
            cursor_line: 1,
            cursor_col: 1,
        });
    }

    let active = if tabs.is_empty() {
        0
    } else {
        manifest.active_tab.min(tabs.len() - 1)
    };
    let font_size = manifest.font_size.clamp(10.0, 32.0);
    let next_untitled = manifest.next_untitled.max(1);
    (tabs, active, font_size, next_untitled, manifest.word_wrap)
}

pub fn save_session_to(
    session_dir: &Path,
    tabs: &[TabDoc],
    active_tab: usize,
    font_size: f32,
    next_untitled: usize,
    word_wrap: bool,
) {
    let drafts_dir = session_dir.join("drafts");
    if let Err(err) = fs::create_dir_all(&drafts_dir) {
        eprintln!(
            "[rocketde-editor] Failed to create {}: {err}",
            drafts_dir.display()
        );
        return;
    }

    let mut active_ids = HashSet::with_capacity(tabs.len());
    let mut metas = Vec::with_capacity(tabs.len());

    for tab in tabs {
        active_ids.insert(tab.id.clone());
        let need_draft = tab.dirty || tab.path.is_none();
        let draft_path = drafts_dir.join(format!("{}.txt", tab.id));

        let has_draft = if need_draft {
            match fs::write(&draft_path, &tab.content) {
                Ok(()) => true,
                Err(err) => {
                    eprintln!(
                        "[rocketde-editor] Failed to write draft {}: {err}",
                        draft_path.display()
                    );
                    false
                }
            }
        } else {
            if draft_path.exists() {
                let _ = fs::remove_file(&draft_path);
            }
            false
        };

        metas.push(SavedTabMeta {
            id: tab.id.clone(),
            title: tab.title.clone(),
            path: tab.path.clone(),
            dirty: tab.dirty,
            has_draft,
        });
    }

    // Remove stale draft files belonging to closed tabs
    if let Ok(entries) = fs::read_dir(&drafts_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("txt") {
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                if !active_ids.contains(stem) {
                    let _ = fs::remove_file(p);
                }
            }
        }
    }

    let manifest = SessionManifest {
        active_tab: active_tab.min(tabs.len().saturating_sub(1)),
        font_size,
        next_untitled,
        word_wrap,
        tabs: metas,
    };

    let manifest_path = session_dir.join("session.json");
    match serde_json::to_string_pretty(&manifest) {
        Ok(json) => {
            if let Err(err) = fs::write(&manifest_path, json) {
                eprintln!(
                    "[rocketde-editor] Failed to write {}: {err}",
                    manifest_path.display()
                );
            }
        }
        Err(err) => {
            eprintln!("[rocketde-editor] Failed to serialize session: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_roundtrip_restores_unsaved_drafts() {
        let dir = std::env::temp_dir().join(format!(
            "rocketde_editor_test_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);

        let tabs = vec![
            TabDoc {
                id: "tab_1".to_string(),
                title: "Sem título 1".to_string(),
                path: None,
                content: "Rascunho não salvo\nlinha 2".to_string(),
                dirty: true,
                cursor_line: 2,
                cursor_col: 8,
            },
            TabDoc {
                id: "tab_2".to_string(),
                title: "notas.txt".to_string(),
                path: Some(PathBuf::from("/tmp/nao_existe_rocketde.txt")),
                content: "Alteração pendente em arquivo".to_string(),
                dirty: true,
                cursor_line: 1,
                cursor_col: 1,
            },
        ];

        save_session_to(&dir, &tabs, 1, 15.5, 3, true);
        let (loaded, active, font_size, next_u, word_wrap) = load_session_from(&dir);

        assert_eq!(loaded.len(), 2);
        assert_eq!(active, 1);
        assert!((font_size - 15.5).abs() < 0.01);
        assert_eq!(next_u, 3);
        assert!(word_wrap);
        assert_eq!(loaded[0].content, "Rascunho não salvo\nlinha 2");
        assert!(loaded[0].dirty);
        assert_eq!(loaded[1].content, "Alteração pendente em arquivo");
        assert!(loaded[1].dirty);

        let _ = fs::remove_dir_all(&dir);
    }
}
