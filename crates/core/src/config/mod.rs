mod fs;
mod types;
pub(crate) use fs::{atomic_private_write, create_private_dir};
pub use types::*;

use crate::connection::ConnectionProfile;
use std::path::PathBuf;

impl AppConfig {
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(&ConfigPaths::discover()?)
    }

    pub fn load_from(paths: &ConfigPaths) -> Result<Self, ConfigError> {
        if !paths.settings.exists() {
            return Ok(Self::default());
        }
        let source =
            std::fs::read_to_string(&paths.settings).map_err(|source| ConfigError::Io {
                path: paths.settings.clone(),
                source,
            })?;
        let config: Self =
            toml::from_str(&source).map_err(|source| ConfigError::Parse(source.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self) -> Result<(), ConfigError> {
        self.save_to(&ConfigPaths::discover()?)
    }

    pub fn save_to(&self, paths: &ConfigPaths) -> Result<(), ConfigError> {
        self.validate()?;
        create_private_dir(&paths.root)?;
        let source = toml::to_string_pretty(self)
            .map_err(|source| ConfigError::Parse(source.to_string()))?;
        atomic_private_write(&paths.settings, source.as_bytes())
    }

    pub fn reset() -> Result<(), ConfigError> {
        let paths = ConfigPaths::discover()?;
        match std::fs::remove_file(&paths.settings) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(ConfigError::Io {
                path: paths.settings,
                source,
            }),
        }
    }

    pub fn connection(&self, id: &crate::connection::ConnectionId) -> Option<&ConnectionProfile> {
        self.connections.iter().find(|profile| &profile.id == id)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.version != types::CONFIG_VERSION {
            return Err(ConfigError::UnsupportedVersion(self.version));
        }
        let mut ids = std::collections::HashSet::new();
        for profile in &self.connections {
            profile
                .validate()
                .map_err(|error| ConfigError::InvalidConnection(error.to_string()))?;
            if !ids.insert(profile.id.clone()) {
                return Err(ConfigError::DuplicateConnection(profile.id.0.clone()));
            }
        }
        let mut agent_ids = std::collections::HashSet::new();
        for agent in &self.agents {
            if agent.id.trim().is_empty() {
                return Err(ConfigError::InvalidAgent("blank agent identifier".into()));
            }
            if agent.name.trim().is_empty() {
                return Err(ConfigError::InvalidAgent(format!(
                    "agent {} has a blank name",
                    agent.id
                )));
            }
            if agent.connection_id.trim().is_empty() || agent.model_id.trim().is_empty() {
                return Err(ConfigError::InvalidAgent(format!(
                    "agent {} must select a connection and model",
                    agent.id
                )));
            }
            if !agent_ids.insert(agent.id.clone()) {
                return Err(ConfigError::DuplicateAgent(agent.id.clone()));
            }
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("home directory is unavailable")]
    HomeDirectoryUnavailable,
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("settings parse error: {0}")]
    Parse(String),
    #[error("unsupported settings version {0}")]
    UnsupportedVersion(u32),
    #[error("invalid connection: {0}")]
    InvalidConnection(String),
    #[error("duplicate connection identifier: {0}")]
    DuplicateConnection(String),
    #[error("invalid agent: {0}")]
    InvalidAgent(String),
    #[error("duplicate agent identifier: {0}")]
    DuplicateAgent(String),
    #[error("invalid configuration path: {0}")]
    InvalidPath(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{ConnectionKind, ConnectionProfile};
    use std::path::Path;

    #[test]
    fn global_paths_live_under_dot_averroes() {
        let paths = ConfigPaths::for_home(Path::new("/Users/example"));
        assert_eq!(
            paths.settings,
            PathBuf::from("/Users/example/.averroes/config/settings.toml")
        );
        assert_eq!(
            paths.vault,
            PathBuf::from("/Users/example/.averroes/config/providers.enc")
        );
        assert_eq!(
            paths.default_workspace_root(),
            PathBuf::from("/Users/example/.averroes/default-workspace")
        );
    }

    #[test]
    fn default_settings_have_no_connection_or_model_default() {
        let settings = AppConfig::default();
        assert!(settings.connections.is_empty());
        let source = toml::to_string(&settings).unwrap();
        assert!(!source.contains("api_key"));
        assert!(!source.contains("default_provider"));
        assert!(!source.contains("default_model"));
    }

    #[test]
    fn load_accepts_the_legacy_qdivzero_kind_name() {
        let source = r#"
version = 1

[[connections]]
id = "qdivzero"
name = "QDivZero"
kind = "q_div_zero"
"#;

        let settings: AppConfig = toml::from_str(source).unwrap();
        assert_eq!(settings.connections[0].kind, ConnectionKind::QDivZero);
    }

    #[test]
    fn save_and_load_preserve_multiple_profiles() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ConfigPaths::for_home(temp.path());
        let mut settings = AppConfig::default();
        settings.connections.push(ConnectionProfile::api(
            "a",
            "Personal",
            ConnectionKind::OpenAi,
            None,
        ));
        settings.connections.push(ConnectionProfile::api(
            "b",
            "Work",
            ConnectionKind::OpenAi,
            None,
        ));
        settings.save_to(&paths).unwrap();

        let loaded = AppConfig::load_from(&paths).unwrap();
        assert_eq!(loaded.connections.len(), 2);
        let source = std::fs::read_to_string(&paths.settings).unwrap();
        assert!(!source.contains("secret"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&paths.root).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&paths.settings)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn save_and_load_preserve_delegated_agents() {
        let temp = tempfile::tempdir().unwrap();
        let paths = ConfigPaths::for_home(temp.path());
        let mut settings = AppConfig::default();
        settings.connections.push(ConnectionProfile::api(
            "work",
            "Work",
            ConnectionKind::OpenAi,
            None,
        ));
        settings.agents.push(AgentProfile {
            id: "researcher".into(),
            name: "Researcher".into(),
            description: "Investigates source material".into(),
            connection_id: "work".into(),
            model_id: "gpt-5".into(),
        });
        settings.save_to(&paths).unwrap();

        let loaded = AppConfig::load_from(&paths).unwrap();
        assert_eq!(loaded.agents, settings.agents);
    }

    #[test]
    fn duplicate_profile_ids_are_rejected() {
        let mut settings = AppConfig::default();
        settings.connections.push(ConnectionProfile::api(
            "same",
            "First",
            ConnectionKind::OpenAi,
            None,
        ));
        settings.connections.push(ConnectionProfile::api(
            "same",
            "Second",
            ConnectionKind::Anthropic,
            None,
        ));
        assert!(matches!(
            settings.validate(),
            Err(ConfigError::DuplicateConnection(id)) if id == "same"
        ));
    }
}
