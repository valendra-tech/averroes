use crate::connection::ConnectionProfile;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "config_version")]
    pub version: u32,
    #[serde(default)]
    pub connections: Vec<ConnectionProfile>,
    #[serde(default)]
    pub runtime: RuntimeSection,
    #[serde(default)]
    pub compaction: CompactionSection,
    #[serde(default)]
    pub skills: SkillsSection,
    #[serde(default)]
    pub agents: Vec<AgentProfile>,
}

const fn config_version() -> u32 {
    CONFIG_VERSION
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: config_version(),
            connections: Vec::new(),
            runtime: RuntimeSection::default(),
            compaction: CompactionSection::default(),
            skills: SkillsSection::default(),
            agents: Vec::new(),
        }
    }
}

/// A user-defined delegated agent. The connection and model are explicit so
/// delegation remains predictable even when the conversation changes model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub connection_id: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSection {
    pub max_concurrent_calls: Option<usize>,
    pub token_budget_per_minute: Option<u64>,
}

impl Default for RuntimeSection {
    fn default() -> Self {
        Self {
            max_concurrent_calls: Some(10),
            token_budget_per_minute: Some(200_000),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionSection {
    pub strategy: Option<String>,
    pub threshold: Option<f64>,
}

impl Default for CompactionSection {
    fn default() -> Self {
        Self {
            strategy: Some("hybrid".into()),
            threshold: Some(0.8),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillsSection {
    pub paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    pub root: PathBuf,
    pub settings: PathBuf,
    pub vault: PathBuf,
}

impl ConfigPaths {
    pub fn discover() -> Result<Self, super::ConfigError> {
        let home = dirs::home_dir().ok_or(super::ConfigError::HomeDirectoryUnavailable)?;
        Ok(Self::for_home(&home))
    }

    pub fn for_home(home: &Path) -> Self {
        let root = home.join(".averroes").join("config");
        Self {
            settings: root.join("settings.toml"),
            vault: root.join("providers.enc"),
            root,
        }
    }
}
