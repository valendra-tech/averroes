use crate::connection::SessionBinding;
use crate::provider::types::{ChatMessage, MessageContent, Role};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub binding: SessionBinding,
}

pub struct SessionStore {
    base_dir: PathBuf,
}

impl SessionStore {
    pub fn new() -> Self {
        let base_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config")
            .join("averroes")
            .join("sessions");
        Self { base_dir }
    }

    pub fn with_dir(dir: PathBuf) -> Self {
        Self { base_dir: dir }
    }

    pub fn dir(&self) -> &PathBuf {
        &self.base_dir
    }

    pub fn save(
        &self,
        session_id: &str,
        messages: &[ChatMessage],
        workspace_id: Option<&str>,
        reasoning_effort: Option<&str>,
        binding: &SessionBinding,
    ) -> Result<(), std::io::Error> {
        std::fs::create_dir_all(&self.base_dir)?;
        let snapshot = SessionSnapshot {
            session_id: session_id.to_string(),
            messages: messages.to_vec(),
            workspace_id: workspace_id.map(String::from),
            reasoning_effort: reasoning_effort.map(String::from),
            binding: binding.clone(),
        };
        let json = serde_json::to_string_pretty(&snapshot)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(self.session_path(session_id), json)
    }

    pub fn load_snapshot(&self, session_id: &str) -> Option<SessionSnapshot> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return None;
        }
        let json = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&json).ok()
    }

    pub fn load(&self, session_id: &str) -> Result<Vec<ChatMessage>, std::io::Error> {
        let path = self.session_path(session_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let json = std::fs::read_to_string(&path)?;
        let snapshot: SessionSnapshot = serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(snapshot.messages)
    }

    pub fn delete(&self, session_id: &str) -> Result<(), std::io::Error> {
        let path = self.session_path(session_id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn list_sessions(&self) -> Result<Vec<String>, std::io::Error> {
        if !self.base_dir.exists() {
            return Ok(Vec::new());
        }
        let mut sessions = Vec::new();
        for entry in std::fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                if let Some(name) = entry.path().file_stem() {
                    sessions.push(name.to_string_lossy().to_string());
                }
            }
        }
        Ok(sessions)
    }

    fn session_path(&self, session_id: &str) -> PathBuf {
        self.base_dir.join(format!("{session_id}.json"))
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

pub async fn generate_session_title(
    provider: &dyn crate::provider::Provider,
    model: &str,
    user_message: &str,
) -> Result<String, String> {
    let request = title_request(model, user_message);

    let response = provider.chat(request).await.map_err(|e| e.to_string())?;

    let text = response.message.text().to_string();
    Ok(title_text(&text))
}

fn title_request(model: &str, user_message: &str) -> crate::provider::ChatRequest {
    crate::provider::ChatRequest {
        model: model.to_string(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: MessageContent::Text(user_message.to_string()),
            tool_call_id: None,
            tool_calls: None,
        }],
        tools: vec![],
        // Title generation must work with reasoning models as well as classic
        // chat models. Leaving this unset avoids rejected temperature fields.
        temperature: None,
        system: Some(
            "Generate a concise title (3-5 words) for this conversation based on the \
             user's first message. Reply with only the title, no quotes, no punctuation."
                .to_string(),
        ),
        reasoning_effort: None,
    }
}

fn title_text(raw: &str) -> String {
    let normalized = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let title = normalized
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('`')
        .trim_matches('#')
        .trim();
    let title = title
        .strip_prefix("Title:")
        .or_else(|| title.strip_prefix("title:"))
        .unwrap_or(title)
        .trim()
        .trim_matches('*')
        .trim();
    let title: String = title
        .chars()
        .filter(|c| !matches!(c, '.' | '!' | '?' | ',' | ';' | ':'))
        .collect();
    let title = title.trim();
    if title.is_empty() {
        "New session".to_string()
    } else {
        let mut chars = title.chars();
        match chars.next() {
            None => "New session".to_string(),
            Some(first) => {
                let mut result = first.to_uppercase().to_string();
                result.push_str(chars.as_str());
                result.chars().take(48).collect()
            }
        }
    }
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: crate::provider::types::MessageContent::Text(content.into()),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: crate::provider::types::MessageContent::Text(content.into()),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: crate::provider::types::MessageContent::Text(content.into()),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    pub fn text(&self) -> &str {
        match &self.content {
            crate::provider::types::MessageContent::Text(text) => text,
            crate::provider::types::MessageContent::Parts(parts) => {
                for part in parts {
                    if let crate::provider::types::ContentPart::Text { text } = part {
                        return text;
                    }
                }
                ""
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_request_uses_selected_model_without_temperature() {
        let request = title_request("gpt-5.6-luna", "Review the latest customer emails");

        assert_eq!(request.model, "gpt-5.6-luna");
        assert!(request.temperature.is_none());
        assert!(request.tools.is_empty());
        assert_eq!(
            request.messages[0].text(),
            "Review the latest customer emails"
        );
    }

    #[test]
    fn title_text_removes_common_model_wrappers() {
        assert_eq!(
            title_text("`title: **Latest customer emails**`"),
            "Latest customer emails"
        );
    }

    fn test_store() -> (SessionStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("averroes-test-{}", uuid::Uuid::new_v4()));
        let store = SessionStore {
            base_dir: dir.clone(),
        };
        (store, dir)
    }

    #[test]
    fn save_and_load_messages() {
        let (store, dir) = test_store();
        let messages = vec![
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi there"),
        ];

        store
            .save(
                "test-session",
                &messages,
                None,
                None,
                &SessionBinding::default(),
            )
            .unwrap();
        let loaded = store.load("test-session").unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].text(), "hello");
        assert_eq!(loaded[1].text(), "hi there");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_session_returns_empty() {
        let (store, dir) = test_store();
        let messages = store.load("nonexistent").unwrap();
        assert!(messages.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_session_removes_file() {
        let (store, dir) = test_store();
        let messages = vec![ChatMessage::user("test")];
        store
            .save(
                "delete-me",
                &messages,
                None,
                None,
                &SessionBinding::default(),
            )
            .unwrap();
        assert!(store.load("delete-me").unwrap().len() == 1);

        store.delete("delete-me").unwrap();
        assert!(store.load("delete-me").unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_binding_round_trips_and_legacy_files_default_to_unbound() {
        use crate::connection::{ConnectionId, SessionBinding};

        let (store, dir) = test_store();
        let binding = SessionBinding {
            connection_id: Some(ConnectionId("work".into())),
            model_id: Some("model-1".into()),
            reasoning_effort: Some("high".into()),
            tools: vec!["file_read".into(), "grep".into()],
        };
        store.save("bound", &[], None, None, &binding).unwrap();
        assert_eq!(store.load_snapshot("bound").unwrap().binding, binding);

        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("legacy.json"),
            r#"{"session_id":"legacy","messages":[]}"#,
        )
        .unwrap();
        assert_eq!(
            store.load_snapshot("legacy").unwrap().binding,
            SessionBinding::default()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
