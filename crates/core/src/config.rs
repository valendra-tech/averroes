use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

use crate::provider::{Provider, anthropic::AnthropicProvider, openai::OpenAiProvider};

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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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

impl AppConfig {
    pub fn load() -> Result<Self, ConfigError> {
        let config_path = config_path()?;
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)
                .map_err(|e| ConfigError::Io { path: config_path.clone(), source: e })?;
            toml::from_str(&content)
                .map_err(|e| ConfigError::Parse(e.to_string()))
        } else {
            Ok(AppConfig::default())
        }
    }

    pub fn needs_setup(&self) -> bool {
        match self.provider.default.as_deref() {
            None => true,
            Some("anthropic") => self.provider.anthropic.is_none(),
            Some("openai") => self.provider.openai.is_none(),
            Some(_) => false,
        }
    }

    pub fn save(&self) -> Result<(), ConfigError> {
        let config_path = config_path()?;
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ConfigError::Io { path: parent.to_path_buf(), source: e })?;
        }
        let content = toml::to_string_pretty(self)
            .map_err(|e| ConfigError::Parse(e.to_string()))?;
        std::fs::write(&config_path, content)
            .map_err(|e| ConfigError::Io { path: config_path, source: e })?;
        Ok(())
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            provider: ProviderSection::default(),
            runtime: RuntimeSection::default(),
            compaction: CompactionSection::default(),
            skills: SkillsSection::default(),
        }
    }
}

fn config_path() -> Result<PathBuf, ConfigError> {
    Ok(dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("averroes")
        .join("config.toml"))
}

pub struct SetupWizard {
    pub provider: String,
    pub api_key: String,
    pub api_key_env: String,
    pub model: String,
}

impl SetupWizard {
    pub fn new() -> Self {
        Self {
            provider: "anthropic".into(),
            api_key: String::new(),
            api_key_env: String::new(),
            model: String::new(),
        }
    }

    pub fn default_model(&self) -> &str {
        if self.model.is_empty() {
            match self.provider.as_str() {
                "openai" => "gpt-4o",
                _ => "claude-sonnet-4-20250514",
            }
        } else {
            &self.model
        }
    }

    pub fn to_config(&self) -> AppConfig {
        let mut config = AppConfig::default();
        config.provider.default = Some(self.provider.clone());

        match self.provider.as_str() {
            "openai" => {
                config.provider.openai = Some(OpenAiConfig {
                    api_key_env: Some(if self.api_key_env.is_empty() {
                        "OPENAI_API_KEY".into()
                    } else {
                        self.api_key_env.clone()
                    }),
                    default_model: Some(self.default_model().into()),
                });
            }
            _ => {
                config.provider.anthropic = Some(AnthropicConfig {
                    api_key_env: Some(if self.api_key_env.is_empty() {
                        "ANTHROPIC_API_KEY".into()
                    } else {
                        self.api_key_env.clone()
                    }),
                    default_model: Some(self.default_model().into()),
                });
            }
        }

        config
    }

    pub fn save_config(&self) -> Result<(), ConfigError> {
        self.to_config().save()
    }
}

impl Default for SetupWizard {
    fn default() -> Self {
        Self::new()
    }
}

pub fn create_provider(config: &AppConfig) -> Result<Arc<dyn Provider>, ConfigError> {
    let default = config.provider.default.as_deref().unwrap_or("anthropic");

    match default {
        "openai" => {
            let openai = config.provider.openai.as_ref()
                .ok_or_else(|| ConfigError::Parse("OpenAI config missing".into()))?;
            let env_key = openai.api_key_env.as_deref().unwrap_or("OPENAI_API_KEY");
            let api_key = std::env::var(env_key)
                .map_err(|_| ConfigError::MissingApiKey { api_key_env: env_key.into() })?;
            let mut provider = OpenAiProvider::new(api_key);
            if let Some(ref model) = openai.default_model {
                provider = provider.with_default_model(model);
            }
            Ok(Arc::new(provider))
        }
        "anthropic" => {
            let anthropic = config.provider.anthropic.as_ref()
                .ok_or_else(|| ConfigError::Parse("Anthropic config missing".into()))?;
            let env_key = anthropic.api_key_env.as_deref().unwrap_or("ANTHROPIC_API_KEY");
            let api_key = std::env::var(env_key)
                .map_err(|_| ConfigError::MissingApiKey { api_key_env: env_key.into() })?;
            let mut provider = AnthropicProvider::new(api_key);
            if let Some(ref model) = anthropic.default_model {
                provider = provider.with_default_model(model);
            }
            Ok(Arc::new(provider))
        }
        _ => Err(ConfigError::UnknownProvider {
            provider: default.into(),
        }),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("IO error at {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("Unknown provider: {provider}")]
    UnknownProvider { provider: String },
    #[error("API key not found: {api_key_env}")]
    MissingApiKey { api_key_env: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = AppConfig::default();
        assert!(config.runtime.max_concurrent_calls == Some(10));
        assert!(config.compaction.strategy.as_deref() == Some("hybrid"));
    }

    #[test]
    fn test_config_serialization() {
        let config = AppConfig::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: AppConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.runtime.max_concurrent_calls, Some(10));
    }

    #[test]
    fn unknown_provider_is_not_treated_as_anthropic() {
        let mut config = AppConfig::default();
        config.provider.default = Some("unknown".into());

        assert!(!config.needs_setup());
        let error = match create_provider(&config) {
            Ok(_) => panic!("unknown provider unexpectedly created a provider"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            ConfigError::UnknownProvider { provider } if provider == "unknown"
        ));
    }

    #[test]
    fn missing_api_key_error_preserves_configured_environment_variable() {
        let env_key = "AVERROES_TEST_MISSING_PROVIDER_KEY";
        let mut config = AppConfig::default();
        config.provider.default = Some("openai".into());
        config.provider.openai = Some(OpenAiConfig {
            api_key_env: Some(env_key.into()),
            default_model: None,
        });

        let error = match create_provider(&config) {
            Ok(_) => panic!("missing test key unexpectedly created a provider"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            ConfigError::MissingApiKey { api_key_env } if api_key_env == env_key
        ));
    }
}
