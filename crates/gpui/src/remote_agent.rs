//! Telegram transport for the Remote Agent relay.
//!
//! This module deliberately knows nothing about GPUI sessions or providers.
//! It only translates Telegram updates into a small transport API. The app
//! remains responsible for authorization, session routing, and agent runs.

use crate::telegram_markdown::{markdown_to_telegram_html, telegram_html_chunks};
use reqwest::multipart::{Form, Part};
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::process::Command;
use uuid::Uuid;

const TELEGRAM_API: &str = "https://api.telegram.org";
const POLL_TIMEOUT_SECONDS: u64 = 25;
const MAX_TELEGRAM_TEXT: usize = 4096;

#[derive(Clone)]
pub(crate) struct TelegramClient {
    http: Client,
    token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TelegramUpdate {
    pub update_id: i64,
    pub message: Option<TelegramMessage>,
    pub callback_query: Option<TelegramCallbackQuery>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TelegramMessage {
    pub message_id: i64,
    pub from: Option<TelegramUser>,
    pub chat: TelegramChat,
    pub text: Option<String>,
    pub caption: Option<String>,
    pub document: Option<TelegramDocument>,
    #[serde(default)]
    pub photo: Vec<TelegramPhotoSize>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TelegramDocument {
    pub file_id: String,
    pub file_name: Option<String>,
    pub file_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TelegramPhotoSize {
    pub file_id: String,
    pub file_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TelegramUser {
    pub id: i64,
    #[serde(default)]
    pub first_name: String,
    pub username: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TelegramChat {
    pub id: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TelegramCallbackQuery {
    pub id: String,
    pub from: TelegramUser,
    pub message: Option<TelegramMessage>,
    pub data: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TelegramBot {
    pub username: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TelegramSentMessage {
    pub message_id: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct TelegramFile {
    file_path: String,
}

impl TelegramClient {
    pub(crate) fn new(token: impl Into<String>) -> Self {
        Self {
            http: Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(40))
                .build()
                .unwrap_or_else(|_| Client::new()),
            token: token.into(),
        }
    }

    pub(crate) async fn bot_username(&self) -> Result<Option<String>, String> {
        self.call("getMe", json!({}))
            .await
            .map(|bot: TelegramBot| bot.username)
    }

    pub(crate) async fn delete_webhook(&self) -> Result<(), String> {
        self.call::<Value>(
            "deleteWebhook",
            json!({
                "drop_pending_updates": false,
            }),
        )
        .await
        .map(|_| ())
    }

    pub(crate) async fn get_updates(
        &self,
        offset: Option<i64>,
    ) -> Result<Vec<TelegramUpdate>, String> {
        let mut body = json!({ "timeout": POLL_TIMEOUT_SECONDS });
        if let Some(offset) = offset {
            body["offset"] = json!(offset);
        }
        self.call("getUpdates", body).await
    }

    pub(crate) async fn send_chat_action(&self, chat_id: i64, action: &str) -> Result<(), String> {
        self.call::<Value>(
            "sendChatAction",
            json!({
                "chat_id": chat_id,
                "action": action,
            }),
        )
        .await
        .map(|_| ())
    }

    pub(crate) async fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        reply_markup: Option<Value>,
    ) -> Result<i64, String> {
        self.send_html_message(chat_id, &markdown_to_telegram_html(text), reply_markup)
            .await
    }

    async fn send_html_message(
        &self,
        chat_id: i64,
        html: &str,
        reply_markup: Option<Value>,
    ) -> Result<i64, String> {
        let mut body = json!({
            "chat_id": chat_id,
            "text": html,
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        });
        if let Some(reply_markup) = reply_markup {
            body["reply_markup"] = reply_markup;
        }
        self.call::<TelegramSentMessage>("sendMessage", body)
            .await
            .map(|message| message.message_id)
    }

    pub(crate) async fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
    ) -> Result<(), String> {
        self.call::<Value>(
            "editMessageText",
            json!({
                "chat_id": chat_id,
                "message_id": message_id,
                "text": markdown_to_telegram_html(text),
                "parse_mode": "HTML",
                "disable_web_page_preview": true,
            }),
        )
        .await
        .map(|_| ())
    }

    pub(crate) async fn answer_callback_query(&self, callback_id: &str) -> Result<(), String> {
        self.call::<Value>(
            "answerCallbackQuery",
            json!({
                "callback_query_id": callback_id,
            }),
        )
        .await
        .map(|_| ())
    }

    pub(crate) async fn send_photo(
        &self,
        chat_id: i64,
        bytes: Vec<u8>,
        caption: &str,
    ) -> Result<i64, String> {
        let url = self.endpoint("sendPhoto");
        let photo = Part::bytes(bytes)
            .file_name("averroes-screenshot.png")
            .mime_str("image/png")
            .map_err(|error| format!("could not prepare screenshot upload: {error}"))?;
        let form = Form::new()
            .text("chat_id", chat_id.to_string())
            .text("caption", markdown_to_telegram_html(caption))
            .text("parse_mode", "HTML")
            .part("photo", photo);
        let response = self
            .http
            .post(url)
            .multipart(form)
            .send()
            .await
            .map_err(|error| format!("Telegram upload failed: {error}"))?;
        let status = response.status();
        let payload = response
            .json::<TelegramResponse<TelegramMessage>>()
            .await
            .map_err(|error| format!("Telegram returned invalid upload data: {error}"))?;
        if !status.is_success() || !payload.ok {
            return Err(payload
                .description
                .unwrap_or_else(|| format!("Telegram upload failed with HTTP {status}")));
        }
        payload
            .result
            .map(|message| message.message_id)
            .ok_or_else(|| "Telegram did not return the uploaded photo message".into())
    }

    pub(crate) async fn download_file(
        &self,
        file_id: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, String> {
        let file = self
            .call::<TelegramFile>("getFile", json!({ "file_id": file_id }))
            .await?;
        let response = self
            .http
            .get(format!(
                "{TELEGRAM_API}/file/bot{}/{}",
                self.token, file.file_path
            ))
            .send()
            .await
            .map_err(|error| format!("Telegram file download failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "Telegram file download failed with HTTP {}",
                response.status()
            ));
        }
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes as u64)
        {
            return Err(format!("Telegram file is larger than {max_bytes} bytes"));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| format!("Telegram file download could not be read: {error}"))?
            .to_vec();
        if bytes.len() > max_bytes {
            return Err(format!("Telegram file is larger than {max_bytes} bytes"));
        }
        Ok(bytes)
    }

    pub(crate) async fn send_text_chunks(
        &self,
        chat_id: i64,
        text: &str,
        reply_markup: Option<Value>,
    ) -> Result<(), String> {
        let chunks = telegram_html_chunks(text, MAX_TELEGRAM_TEXT);
        if chunks.is_empty() {
            self.send_html_message(chat_id, "…", reply_markup).await?;
            return Ok(());
        }
        for (index, chunk) in chunks.iter().enumerate() {
            self.send_html_message(
                chat_id,
                chunk,
                (index + 1 == chunks.len())
                    .then(|| reply_markup.clone())
                    .flatten(),
            )
            .await?;
        }
        Ok(())
    }

    async fn call<T: DeserializeOwned>(&self, method: &str, body: Value) -> Result<T, String> {
        let response = self
            .http
            .post(self.endpoint(method))
            .json(&body)
            .send()
            .await
            .map_err(|error| format!("Telegram request failed: {error}"))?;
        let status = response.status();
        let payload = response
            .json::<TelegramResponse<T>>()
            .await
            .map_err(|error| format!("Telegram returned invalid data: {error}"))?;
        if !status.is_success() || !payload.ok {
            return Err(payload
                .description
                .unwrap_or_else(|| format!("Telegram returned HTTP {status}")));
        }
        payload
            .result
            .ok_or_else(|| format!("Telegram returned no result for {method}"))
    }

    fn endpoint(&self, method: &str) -> String {
        format!("{TELEGRAM_API}/bot{}/{method}", self.token)
    }
}

#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

pub(crate) async fn capture_desktop_screenshot() -> Result<Vec<u8>, String> {
    if !cfg!(target_os = "macos") {
        return Err("remote screenshots are only available on macOS".into());
    }

    let path = std::env::temp_dir().join(format!("averroes-remote-{}.png", Uuid::new_v4()));
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        Command::new("/usr/sbin/screencapture")
            .kill_on_drop(true)
            .arg("-x")
            .arg("-t")
            .arg("png")
            .arg(&path)
            .output(),
    )
    .await
    .map_err(|_| "screenshot capture timed out".to_owned())?
    .map_err(|error| format!("could not start screenshot capture: {error}"))?;

    if !output.status.success() {
        let _ = tokio::fs::remove_file(&path).await;
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            "macOS could not capture the screen; check Screen Recording permission".into()
        } else {
            format!("macOS could not capture the screen: {stderr}")
        });
    }

    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|error| format!("could not read screenshot: {error}"));
    let _ = tokio::fs::remove_file(&path).await;
    bytes
}

fn split_text(text: &str, max_chars: usize) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if current.chars().count() >= max_chars {
            chunks.push(std::mem::take(&mut current));
        }
        current.push(character);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::{split_text, TelegramMessage};

    #[test]
    fn splits_unicode_text_without_breaking_characters() {
        let chunks = split_text("aé界", 2);
        assert_eq!(chunks, vec!["aé", "界"]);
    }

    #[test]
    fn deserializes_documents_and_captioned_photos() {
        let document = serde_json::json!({
            "message_id": 1,
            "chat": { "id": 10 },
            "document": {
                "file_id": "document-file",
                "file_name": "notes.txt",
                "file_size": 123
            }
        });
        let photo = serde_json::json!({
            "message_id": 2,
            "chat": { "id": 10 },
            "caption": "Review this image",
            "photo": [{ "file_id": "photo-file", "file_size": 456 }]
        });

        let document = serde_json::from_value::<TelegramMessage>(document).unwrap();
        let photo = serde_json::from_value::<TelegramMessage>(photo).unwrap();

        assert_eq!(
            document.document.unwrap().file_name.as_deref(),
            Some("notes.txt")
        );
        assert_eq!(photo.caption.as_deref(), Some("Review this image"));
        assert_eq!(photo.photo[0].file_id, "photo-file");
    }
}
