use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub provider: ProviderSection,
    #[serde(default)]
    pub runtime: RuntimeSection,
    #[serde(default)]
    pub compaction: CompactionSection,
    #[serde(default)]
    pub skills: SkillsSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSection {
    pub default: Option<String>,
    pub anthropic: Option<AnthropicConfig>,
    pub openai: Option<OpenAiConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicConfig {
    pub api_key_env: Option<String>,
    pub default_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiConfig {
    pub api_key_env: Option<String>,
    pub default_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSection {
    pub max_concurrent_calls: Option<usize>,
    pub token_budget_per_minute: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionSection {
    pub strategy: Option<String>,
    pub threshold: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillsSection {
    pub paths: Option<Vec<String>>,
}

impl AppConfig {
    pub fn load() -> anyhow::Result<Self> {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => {
                toml::from_str(&contents).with_context(|| {
                    format!("Failed to parse config at {}", path.display())
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self::default())
            }
            Err(e) => Err(e).with_context(|| {
                format!("Failed to read config at {}", path.display())
            }),
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config dir at {}", parent.display()))?;
        }
        let contents = toml::to_string_pretty(self).context("Failed to serialize config")?;
        std::fs::write(&path, contents)
            .with_context(|| format!("Failed to write config to {}", path.display()))?;
        Ok(())
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            provider: ProviderSection::default(),
            runtime: RuntimeSection {
                max_concurrent_calls: Some(10),
                token_budget_per_minute: Some(200_000),
            },
            compaction: CompactionSection {
                strategy: Some("hybrid".to_string()),
                threshold: Some(0.8),
            },
            skills: SkillsSection::default(),
        }
    }
}

impl Default for ProviderSection {
    fn default() -> Self {
        Self {
            default: None,
            anthropic: None,
            openai: None,
        }
    }
}

impl Default for CompactionSection {
    fn default() -> Self {
        Self {
            strategy: None,
            threshold: None,
        }
    }
}

impl Default for RuntimeSection {
    fn default() -> Self {
        Self {
            max_concurrent_calls: None,
            token_budget_per_minute: None,
        }
    }
}

impl Default for SkillsSection {
    fn default() -> Self {
        Self { paths: None }
    }
}

pub fn config_path() -> PathBuf {
    let base = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("averroes").join("config.toml")
}
