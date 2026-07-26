use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceIndex {
    pub workspaces: Vec<WorkspaceConfig>,
    pub active: Option<String>,
}

impl Default for WorkspaceIndex {
    fn default() -> Self {
        Self {
            workspaces: Vec::new(),
            active: None,
        }
    }
}

pub struct WorkspaceStore {
    index_path: PathBuf,
    index: WorkspaceIndex,
}

impl WorkspaceStore {
    pub fn new() -> Self {
        let config_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config")
            .join("averroes");
        let index_path = config_dir.join("workspaces.json");
        let index = Self::load_index(&index_path).unwrap_or_default();
        Self { index_path, index }
    }

    fn load_index(path: &Path) -> Option<WorkspaceIndex> {
        let json = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&json).ok()
    }

    fn save_index(&self) -> Result<(), std::io::Error> {
        if let Some(parent) = self.index_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.index)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&self.index_path, json)
    }

    pub fn workspaces(&self) -> &[WorkspaceConfig] {
        &self.index.workspaces
    }

    pub fn active_workspace(&self) -> Option<&WorkspaceConfig> {
        self.index.active.as_ref().and_then(|id| {
            self.index.workspaces.iter().find(|w| &w.id == id)
        })
    }

    pub fn add_workspace(&mut self, name: String, root: PathBuf) -> WorkspaceConfig {
        let ws = WorkspaceConfig {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.trim().to_string(),
            root,
        };
        self.index.workspaces.push(ws.clone());
        if self.index.active.is_none() {
            self.index.active = Some(ws.id.clone());
        }
        let _ = self.save_index();
        ws
    }

    pub fn set_active(&mut self, id: &str) -> bool {
        if self.index.workspaces.iter().any(|w| w.id == id) {
            self.index.active = Some(id.to_string());
            let _ = self.save_index();
            true
        } else {
            false
        }
    }

    pub fn remove_workspace(&mut self, id: &str) -> bool {
        let len_before = self.index.workspaces.len();
        self.index.workspaces.retain(|w| w.id != id);
        if self.index.active.as_deref() == Some(id) {
            self.index.active = self.index.workspaces.first().map(|w| w.id.clone());
        }
        if self.index.workspaces.len() != len_before {
            let _ = self.save_index();
            true
        } else {
            false
        }
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.active_workspace()
            .map(|ws| ws.root.join(".averroes").join("sessions"))
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".config")
                    .join("averroes")
                    .join("sessions")
            })
    }

    pub fn workspace_root(&self) -> PathBuf {
        self.active_workspace()
            .map(|ws| ws.root.clone())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    pub fn save_open_tabs(&self, session_ids: &[String]) -> Result<(), std::io::Error> {
        if let Some(ws) = self.active_workspace() {
            let dir = ws.root.join(".averroes");
            std::fs::create_dir_all(&dir)?;
            let json = serde_json::to_string_pretty(&session_ids)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            std::fs::write(dir.join("tabs.json"), json)
        } else {
            Ok(())
        }
    }

    pub fn load_open_tabs(&self) -> Vec<String> {
        self.active_workspace()
            .and_then(|ws| {
                let path = ws.root.join(".averroes").join("tabs.json");
                let json = std::fs::read_to_string(&path).ok()?;
                serde_json::from_str(&json).ok()
            })
            .unwrap_or_default()
    }
}

impl Default for WorkspaceStore {
    fn default() -> Self {
        Self::new()
    }
}
