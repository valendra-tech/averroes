use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

const MAX_CONFIRMATION_STRING_BYTES: usize = 2_048;

/// A live question exposed by the UI while an agent waits for a human reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UserQuestion {
    pub id: String,
    pub question: String,
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskUserParams {
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
}

impl AskUserParams {
    pub fn parse(value: &serde_json::Value) -> std::result::Result<Self, String> {
        let mut params: Self =
            serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
        params.question = params.question.trim().to_owned();
        params.options = params
            .options
            .into_iter()
            .map(|option| option.trim().to_owned())
            .filter(|option| !option.is_empty())
            .fold(Vec::new(), |mut options, option| {
                if !options
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(&option))
                {
                    options.push(option);
                }
                options
            });
        if params.question.is_empty() {
            return Err("question cannot be empty".into());
        }
        if params.options.len() > 6 {
            return Err("options must contain at most 6 choices".into());
        }
        Ok(params)
    }
}

struct PendingQuestion {
    prompt: UserQuestion,
    answer: Mutex<Option<String>>,
    cancelled: AtomicBool,
    ready: Notify,
}

/// Per-conversation rendezvous between the agent runtime and the GPUI shell.
/// A tool call is intentionally suspended until the user answers; no answer is
/// fabricated and no provider request continues in the meantime.
#[derive(Default)]
pub struct AskUserBroker {
    pending: Mutex<HashMap<String, Arc<PendingQuestion>>>,
}

impl AskUserBroker {
    pub fn request(&self, session_id: &str, params: AskUserParams) -> UserQuestion {
        let mut pending = self.pending.lock();
        if let Some(existing) = pending.get(session_id) {
            return existing.prompt.clone();
        }
        let prompt = UserQuestion {
            id: format!(
                "question-{}",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ),
            question: params.question,
            options: params.options,
        };
        pending.insert(
            session_id.to_owned(),
            Arc::new(PendingQuestion {
                prompt: prompt.clone(),
                answer: Mutex::new(None),
                cancelled: AtomicBool::new(false),
                ready: Notify::new(),
            }),
        );
        prompt
    }

    pub fn pending(&self, session_id: &str) -> Option<UserQuestion> {
        self.pending
            .lock()
            .get(session_id)
            .map(|pending| pending.prompt.clone())
    }

    pub fn request_confirmation(
        &self,
        session_id: &str,
        tool_name: &str,
        params: &serde_json::Value,
    ) -> UserQuestion {
        let safe_params = redact_confirmation_params(params);
        let arguments = serde_json::to_string_pretty(&safe_params)
            .unwrap_or_else(|_| "<unavailable>".to_string());
        let arguments = truncate_confirmation_text(&arguments, 4_000);
        self.request(
            session_id,
            AskUserParams {
                question: format!(
                    "Allow the agent to execute tool '{tool_name}' with these arguments?\n\n{arguments}"
                ),
                options: vec!["Allow".into(), "Deny".into()],
            },
        )
    }

    pub async fn wait_for_answer(&self, session_id: &str, question_id: &str) -> Option<String> {
        let pending = self.pending.lock().get(session_id).cloned()?;
        if pending.prompt.id != question_id {
            return None;
        }
        loop {
            let notified = pending.ready.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if pending.cancelled.load(Ordering::Acquire) {
                return None;
            }
            if let Some(answer) = pending.answer.lock().clone() {
                self.pending.lock().remove(session_id);
                return Some(answer);
            }
            notified.await;
        }
    }

    pub fn cancel(&self, session_id: &str, question_id: &str) -> bool {
        let pending = self.pending.lock().get(session_id).cloned();
        let Some(pending) = pending else {
            return false;
        };
        if pending.prompt.id != question_id {
            return false;
        }
        pending.cancelled.store(true, Ordering::Release);
        pending.ready.notify_waiters();
        self.pending.lock().remove(session_id).is_some()
    }

    pub fn cancel_session(&self, session_id: &str) -> bool {
        let pending = self.pending.lock().remove(session_id);
        let Some(pending) = pending else {
            return false;
        };
        pending.cancelled.store(true, Ordering::Release);
        pending.ready.notify_waiters();
        true
    }

    pub fn answer(&self, session_id: &str, question_id: &str, answer: String) -> bool {
        let pending = self.pending.lock().get(session_id).cloned();
        let Some(pending) = pending else {
            return false;
        };
        if pending.prompt.id != question_id {
            return false;
        }
        if pending.cancelled.load(Ordering::Acquire) {
            return false;
        }
        let answer = answer.trim();
        if answer.is_empty() {
            return false;
        }
        *pending.answer.lock() = Some(answer.to_owned());
        pending.ready.notify_waiters();
        true
    }

    pub async fn ask(&self, session_id: &str, params: AskUserParams) -> (UserQuestion, String) {
        let prompt = self.request(session_id, params);
        let answer = self
            .wait_for_answer(session_id, &prompt.id)
            .await
            .unwrap_or_default();
        (prompt, answer)
    }
}

pub(crate) fn redact_confirmation_params(value: &serde_json::Value) -> serde_json::Value {
    redact_confirmation_value(None, value)
}

fn redact_confirmation_value(key: Option<&str>, value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let value = if is_sensitive_key(key) {
                        serde_json::Value::String("[redacted]".into())
                    } else {
                        redact_confirmation_value(Some(key), value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .map(|value| redact_confirmation_value(key, value))
                .collect(),
        ),
        serde_json::Value::String(value) if is_semantically_sensitive_key(key) => {
            serde_json::Value::String(truncate_confirmation_text(
                &redact_inline_secrets(value),
                MAX_CONFIRMATION_STRING_BYTES,
            ))
        }
        serde_json::Value::String(value) => serde_json::Value::String(truncate_confirmation_text(
            value,
            MAX_CONFIRMATION_STRING_BYTES,
        )),
        _ => value.clone(),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        key.as_str(),
        "password"
            | "passphrase"
            | "secret"
            | "token"
            | "apikey"
            | "accesstoken"
            | "refreshtoken"
            | "authorization"
            | "cookie"
            | "setcookie"
            | "credential"
            | "privatekey"
            | "keyfile"
            | "clientsecret"
    ) || key.ends_with("token")
        || key.ends_with("secret")
        || key.ends_with("password")
        || key.ends_with("passphrase")
        || key.ends_with("apikey")
        || key.ends_with("privatekey")
        || key.ends_with("credential")
}

fn is_semantically_sensitive_key(key: Option<&str>) -> bool {
    let Some(key) = key else {
        return false;
    };
    matches!(
        key,
        "command" | "input" | "content" | "patch" | "text" | "value" | "url"
    )
}

fn redact_inline_secrets(value: &str) -> String {
    let patterns = [
        r"(?i)(\bBearer\s+)[^\s,;}\]]+",
        r"(?i)(\b(?:password|passphrase|secret|(?:[a-z0-9][a-z0-9_-]*)?token|api[_-]?key|authorization|cookie|private[_-]?key)\b\s*[:=]\s*)[^\s,;}\]]+",
    ];
    patterns.iter().fold(value.to_owned(), |value, pattern| {
        regex::Regex::new(pattern)
            .map(|regex| regex.replace_all(&value, "$1[redacted]").into_owned())
            .unwrap_or(value)
    })
}

fn truncate_confirmation_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes.saturating_sub(3);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &value[..end])
}

pub(crate) fn confirmation_approved(answer: &str) -> bool {
    matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "allow" | "allowed" | "approve" | "approved" | "yes" | "y"
    )
}

pub struct AskUserTool {
    broker: Arc<AskUserBroker>,
}

impl AskUserTool {
    pub fn new(broker: Arc<AskUserBroker>) -> Self {
        Self { broker }
    }
}

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        "Asks the user a focused question and waits for their answer. Provide up to six optional smart-button choices; the user can always send a free-text reply instead. Use this when a decision, preference, or confirmation is needed rather than guessing."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "A concise question for the user."
                },
                "options": {
                    "type": "array",
                    "description": "Optional smart-button labels. Include only meaningful choices; free text is always available.",
                    "items": { "type": "string" },
                    "maxItems": 6
                }
            },
            "required": ["question"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, params: &serde_json::Value) -> Result<ToolResult> {
        let params = AskUserParams::parse(params).map_err(|message| ToolError::InvalidParams {
            tool: self.name().into(),
            message,
        })?;
        let (prompt, answer) = self.broker.ask(&ctx.session_id, params).await;
        Ok(ToolResult::ok(format!("The user answered: {answer}"))
            .with_metadata(json!({ "question": prompt, "answer": answer })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn waits_for_the_matching_ui_answer() {
        let broker = Arc::new(AskUserBroker::default());
        let waiting = {
            let broker = broker.clone();
            tokio::spawn(async move {
                broker
                    .ask(
                        "session",
                        AskUserParams {
                            question: "Continue?".into(),
                            options: vec!["Yes".into(), "No".into()],
                        },
                    )
                    .await
            })
        };
        tokio::task::yield_now().await;
        let prompt = broker.pending("session").unwrap();
        assert!(broker.answer("session", &prompt.id, "Yes".into()));
        let (_, answer) = waiting.await.unwrap();
        assert_eq!(answer, "Yes");
    }

    #[tokio::test]
    async fn cancellation_wakes_a_waiting_question() {
        let broker = Arc::new(AskUserBroker::default());
        let prompt = broker.request(
            "session",
            AskUserParams {
                question: "Continue?".into(),
                options: Vec::new(),
            },
        );
        let waiting = {
            let broker = broker.clone();
            let question_id = prompt.id.clone();
            tokio::spawn(async move { broker.wait_for_answer("session", &question_id).await })
        };
        tokio::task::yield_now().await;

        assert!(broker.cancel("session", &prompt.id));
        assert_eq!(waiting.await.unwrap(), None);
        assert!(broker.pending("session").is_none());
    }

    #[test]
    fn redacts_sensitive_confirmation_fields_recursively() {
        let params = serde_json::json!({
            "token": "top-secret",
            "nested": { "api_key": "another-secret", "visible": "keep" },
            "items": [{ "password": "hidden" }]
        });

        let redacted = redact_confirmation_params(&params);

        assert_eq!(redacted["token"], "[redacted]");
        assert_eq!(redacted["nested"]["api_key"], "[redacted]");
        assert_eq!(redacted["nested"]["visible"], "keep");
        assert_eq!(redacted["items"][0]["password"], "[redacted]");
    }

    #[test]
    fn redacts_embedded_secrets_and_bounds_large_confirmation_values() {
        let params = serde_json::json!({
            "command": "curl -H 'Authorization: Bearer top-secret'",
            "patch": "x".repeat(MAX_CONFIRMATION_STRING_BYTES + 100)
        });

        let redacted = redact_confirmation_params(&params);
        let command = redacted["command"].as_str().unwrap();
        let patch = redacted["patch"].as_str().unwrap();

        assert!(!command.contains("top-secret"));
        assert!(command.contains("[redacted]"));
        assert!(patch.len() <= MAX_CONFIRMATION_STRING_BYTES);
        assert!(patch.ends_with("..."));
    }

    #[test]
    fn redacts_mutating_tool_and_mcp_payload_shapes() {
        let params = serde_json::json!({
            "patch": "token=patch-secret",
            "content": "api_key=file-secret",
            "text": "password: desktop-secret",
            "value": "oauth_token=value-secret",
            "url": "https://example.test/?access_token=url-secret",
            "headers": { "Authorization": "Bearer mcp-secret" },
            "api_token": "compound-secret"
        });

        let redacted = redact_confirmation_params(&params);

        let serialized = serde_json::to_string(&redacted).unwrap();
        for secret in [
            "patch-secret",
            "file-secret",
            "desktop-secret",
            "value-secret",
            "url-secret",
            "mcp-secret",
            "compound-secret",
        ] {
            assert!(
                !serialized.contains(secret),
                "secret leaked: {secret}; redacted={serialized}"
            );
        }
        assert_eq!(redacted["headers"]["Authorization"], "[redacted]");
    }
}
