use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Notify;

/// A live question exposed by the UI while an agent waits for a human reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UserQuestion {
    pub id: String,
    pub question: String,
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
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

    pub fn answer(&self, session_id: &str, question_id: &str, answer: String) -> bool {
        let pending = self.pending.lock().get(session_id).cloned();
        let Some(pending) = pending else {
            return false;
        };
        if pending.prompt.id != question_id {
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
        let pending = self
            .pending
            .lock()
            .get(session_id)
            .cloned()
            .expect("question is inserted before waiting");
        loop {
            let notified = pending.ready.notified();
            if let Some(answer) = pending.answer.lock().clone() {
                self.pending.lock().remove(session_id);
                return (prompt, answer);
            }
            notified.await;
        }
    }
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
}
