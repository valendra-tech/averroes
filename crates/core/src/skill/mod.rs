pub mod loader;
pub mod index;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub triggers: Vec<String>,
    #[serde(skip)]
    pub path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("Skill '{0}' not found")]
    NotFound(String),
    #[error("I/O error reading {path}: {source}")]
    Io { path: PathBuf, #[source] source: std::io::Error },
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, SkillError>;
