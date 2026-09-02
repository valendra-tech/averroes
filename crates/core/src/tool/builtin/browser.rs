//! Stateful browser interaction exposed through one compact action schema.

use async_trait::async_trait;
use base64::Engine as _;
use oxibrowser_core::{BrowseResult, Tab};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;

use super::web_browser::{validate_url, BrowserRuntime, PAGE_OPEN_TIMEOUT};
use super::web_fetch::page_favicon_url;
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

const MAX_AUTOMATIC_SESSIONS: usize = 8;
const MAX_BROWSER_CONTENT_CHARS: usize = 16_000;
const MAX_INTERACTIVE_ELEMENTS: usize = 30;
const DEFAULT_SCROLL_Y: f64 = 600.0;

pub struct BrowserTool {
    runtime: BrowserRuntime,
    sessions: Mutex<HashMap<String, BrowserSession>>,
    session_open_lock: Mutex<()>,
    access_clock: AtomicU64,
}

struct BrowserSession {
    tab: Tab,
    last_used: u64,
}

#[derive(Debug, Deserialize)]
struct BrowserParams {
    action: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    delta_x: Option<f64>,
    #[serde(default)]
    delta_y: Option<f64>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    width: Option<u32>,
}

impl Default for BrowserTool {
    fn default() -> Self {
        Self {
            runtime: BrowserRuntime::default(),
            sessions: Mutex::new(HashMap::new()),
            session_open_lock: Mutex::new(()),
            access_clock: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str {
        "browser"
    }

    fn description(&self) -> &str {
        "Interact with one automatic browser session for this conversation. Use only when web_fetch is insufficient because JavaScript, cookies, clicks, forms, or navigation state are required."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "open", "inspect", "click", "fill", "type", "press",
                        "select", "check", "uncheck", "scroll", "wait", "back",
                        "forward", "reload", "screenshot", "close"
                    ],
                    "description": "Browser operation to perform"
                },
                "url": {
                    "type": "string",
                    "description": "HTTP(S) URL for open"
                },
                "target": {
                    "type": "string",
                    "description": "Element ref from inspect (for example e3) or CSS selector"
                },
                "value": {
                    "type": "string",
                    "description": "Text or option value for fill, type, or select"
                },
                "key": {
                    "type": "string",
                    "description": "Key or combo for press, for example Enter or Ctrl+A"
                },
                "delta_x": {
                    "type": "number",
                    "description": "Horizontal pixels for scroll"
                },
                "delta_y": {
                    "type": "number",
                    "description": "Vertical pixels for scroll; defaults to 600"
                },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 60000,
                    "description": "Wait timeout; defaults to 10000"
                },
                "width": {
                    "type": "integer",
                    "minimum": 320,
                    "maximum": 2000,
                    "description": "Screenshot viewport width; defaults to 1280"
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let params: BrowserParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let action = params.action.trim().to_ascii_lowercase();
        let session_id = ctx.session_id.trim();
        if session_id.is_empty() {
            return Err(invalid("The browser session requires a conversation id"));
        }

        tokio::time::timeout(
            PAGE_OPEN_TIMEOUT,
            self.execute_action(session_id, &action, params),
        )
        .await
        .map_err(|_| ToolError::Execution {
            tool: self.name().into(),
            message: format!(
                "Browser action '{action}' timed out after {} seconds",
                PAGE_OPEN_TIMEOUT.as_secs()
            ),
        })?
    }
}

impl BrowserTool {
    async fn execute_action(
        &self,
        session_id: &str,
        action: &str,
        params: BrowserParams,
    ) -> Result<ToolResult> {
        if action == "close" {
            return self.close_session(session_id).await;
        }

        if action == "open" {
            let raw_url = required(params.url.as_deref(), "url", action)?;
            let url = validate_url("browser", raw_url)?;
            let tab = self.open_tab(session_id).await?;
            let page = tab
                .goto(&url)
                .await
                .map_err(|error| browser_error(action, error))?;
            return self.inspect_page(&tab, page).await;
        }

        let tab = self.active_tab(session_id).await?;
        match action {
            "inspect" => {
                let page = tab
                    .content()
                    .await
                    .map_err(|error| browser_error(action, error))?;
                self.inspect_page(&tab, page).await
            }
            "click" => {
                let target = target_selector(required(
                    params.target.as_deref(),
                    "target",
                    action,
                )?)?;
                tab.click_and_stabilize(&target)
                    .await
                    .map_err(|error| browser_error(action, error))?;
                self.action_result(&tab, action, Some(&target)).await
            }
            "fill" => {
                let target = target_selector(required(
                    params.target.as_deref(),
                    "target",
                    action,
                )?)?;
                let value = required(params.value.as_deref(), "value", action)?;
                tab.fill(&target, value)
                    .await
                    .map_err(|error| browser_error(action, error))?;
                self.action_result(&tab, action, Some(&target)).await
            }
            "type" => {
                let target = target_selector(required(
                    params.target.as_deref(),
                    "target",
                    action,
                )?)?;
                let value = required(params.value.as_deref(), "value", action)?;
                tab.r#type(&target, value)
                    .await
                    .map_err(|error| browser_error(action, error))?;
                self.action_result(&tab, action, Some(&target)).await
            }
            "press" => {
                let key = required(params.key.as_deref(), "key", action)?;
                tab.press(key)
                    .await
                    .map_err(|error| browser_error(action, error))?;
                self.action_result(&tab, action, Some(key)).await
            }
            "select" => {
                let target = target_selector(required(
                    params.target.as_deref(),
                    "target",
                    action,
                )?)?;
                let value = required(params.value.as_deref(), "value", action)?;
                tab.select_option(&target, value)
                    .await
                    .map_err(|error| browser_error(action, error))?;
                self.action_result(&tab, action, Some(&target)).await
            }
            "check" | "uncheck" => {
                let target = target_selector(required(
                    params.target.as_deref(),
                    "target",
                    action,
                )?)?;
                if action == "check" {
                    tab.check(&target).await
                } else {
                    tab.uncheck(&target).await
                }
                .map_err(|error| browser_error(action, error))?;
                self.action_result(&tab, action, Some(&target)).await
            }
            "scroll" => {
                tab.scroll(
                    params.delta_x.unwrap_or_default(),
                    params.delta_y.unwrap_or(DEFAULT_SCROLL_Y),
                )
                .await
                .map_err(|error| browser_error(action, error))?;
                self.action_result(&tab, action, None).await
            }
            "wait" => {
                let target = target_selector(required(
                    params.target.as_deref(),
                    "target",
                    action,
                )?)?;
                let timeout_ms = params.timeout_ms.unwrap_or(10_000).clamp(1, 60_000);
                tab.wait_for(&target, timeout_ms)
                    .await
                    .map_err(|error| browser_error(action, error))?;
                self.action_result(&tab, action, Some(&target)).await
            }
            "back" => {
                tab.back()
                    .await
                    .map_err(|error| browser_error(action, error))?;
                self.action_result(&tab, action, None).await
            }
            "forward" => {
                tab.forward()
                    .await
                    .map_err(|error| browser_error(action, error))?;
                self.action_result(&tab, action, None).await
            }
            "reload" => {
                tab.reload()
                    .await
                    .map_err(|error| browser_error(action, error))?;
                self.action_result(&tab, action, None).await
            }
            "screenshot" => {
                let width = params.width.unwrap_or(1_280).clamp(320, 2_000);
                let png = tab
                    .screenshot(width)
                    .await
                    .map_err(|error| browser_error(action, error))?;
                Ok(ToolResult::ok(format!(
                    "Captured the current page as a {width}px-wide PNG screenshot."
                ))
                .with_image(
                    "image/png",
                    base64::engine::general_purpose::STANDARD.encode(png),
                )
                .with_metadata(json!({ "width": width, "media_type": "image/png" })))
            }
            _ => Err(invalid(format!(
                "Unknown browser action '{action}'. Use open, inspect, click, fill, type, press, select, check, uncheck, scroll, wait, back, forward, reload, screenshot, or close"
            ))),
        }
    }

    async fn open_tab(&self, session_id: &str) -> Result<Tab> {
        if let Some(tab) = self.touch_session(session_id).await {
            return Ok(tab);
        }

        // Serialize session creation so concurrent conversations cannot race
        // past the global cap while the browser is creating a new tab.
        let _open_guard = self.session_open_lock.lock().await;
        if let Some(tab) = self.touch_session(session_id).await {
            return Ok(tab);
        }

        let evicted = {
            let mut sessions = self.sessions.lock().await;
            if sessions.len() < MAX_AUTOMATIC_SESSIONS {
                None
            } else {
                let usage = sessions
                    .iter()
                    .map(|(id, session)| (id.clone(), session.last_used))
                    .collect::<HashMap<_, _>>();
                least_recently_used(&usage)
                    .map(str::to_owned)
                    .and_then(|id| sessions.remove(&id))
                    .map(|session| session.tab)
            }
        };
        if let Some(tab) = evicted {
            let _ = tab.close().await;
        }

        let browser = self.runtime.browser().await?;
        let created = browser
            .new_tab()
            .await
            .map_err(|error| browser_error("open", error))?;
        let mut sessions = self.sessions.lock().await;
        if let Some(existing) = sessions.get_mut(session_id) {
            existing.last_used = self.next_access();
            let existing = existing.tab.clone();
            drop(sessions);
            let _ = created.close().await;
            return Ok(existing);
        }
        sessions.insert(
            session_id.to_owned(),
            BrowserSession {
                tab: created.clone(),
                last_used: self.next_access(),
            },
        );
        Ok(created)
    }

    async fn active_tab(&self, session_id: &str) -> Result<Tab> {
        self.touch_session(session_id).await.ok_or_else(|| {
            invalid("No browser page is open for this conversation; call browser with action=open")
        })
    }

    async fn touch_session(&self, session_id: &str) -> Option<Tab> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions.get_mut(session_id)?;
        session.last_used = self.next_access();
        Some(session.tab.clone())
    }

    async fn close_session(&self, session_id: &str) -> Result<ToolResult> {
        let session = self.sessions.lock().await.remove(session_id);
        let Some(session) = session else {
            return Ok(ToolResult::ok("Browser session is already closed."));
        };
        session
            .tab
            .close()
            .await
            .map_err(|error| browser_error("close", error))?;
        Ok(ToolResult::ok(
            "Closed the browser session for this conversation.",
        ))
    }

    fn next_access(&self) -> u64 {
        self.access_clock.fetch_add(1, Ordering::Relaxed) + 1
    }

    async fn inspect_page(&self, tab: &Tab, page: BrowseResult) -> Result<ToolResult> {
        let controls = tab
            .evaluate(interactive_elements_script())
            .await
            .map_err(|error| browser_error("inspect", error))?;
        Ok(format_page_snapshot(page, &controls))
    }

    async fn action_result(
        &self,
        tab: &Tab,
        action: &str,
        target: Option<&str>,
    ) -> Result<ToolResult> {
        let page = tab
            .content()
            .await
            .map_err(|error| browser_error(action, error))?;
        let target = target
            .map(|target| format!(" on {target}"))
            .unwrap_or_default();
        let output = format!(
            "Browser {action}{target} completed.\nURL: {}\nTitle: {}",
            page.url,
            if page.title.trim().is_empty() {
                "(untitled)"
            } else {
                page.title.trim()
            }
        );
        Ok(ToolResult::ok(output).with_metadata(browser_metadata(&page)))
    }
}

fn interactive_elements_script() -> &'static str {
    r#"(function() {
        var old = document.querySelectorAll('[data-averroes-ref]');
        for (var j = 0; j < old.length; j++) old[j].removeAttribute('data-averroes-ref');
        var nodes = document.querySelectorAll(
            'a[href],button,input,textarea,select,[role="button"],[role="link"],[contenteditable="true"]'
        );
        var result = [];
        for (var i = 0; i < nodes.length && result.length < 30; i++) {
            var el = nodes[i];
            if (el.disabled || el.getAttribute('aria-hidden') === 'true') continue;
            var ref = 'e' + (result.length + 1);
            el.setAttribute('data-averroes-ref', ref);
            var text = (el.innerText || el.textContent || el.value || '').replace(/\s+/g, ' ').trim();
            result.push({
                ref: ref,
                tag: (el.tagName || '').toLowerCase(),
                type: el.getAttribute('type') || '',
                role: el.getAttribute('role') || '',
                name: el.getAttribute('name') || '',
                label: el.getAttribute('aria-label') || '',
                placeholder: el.getAttribute('placeholder') || '',
                text: text.slice(0, 120),
                href: (el.getAttribute('href') || '').slice(0, 240)
            });
        }
        return result;
    })()"#
}

fn format_page_snapshot(page: BrowseResult, controls: &Value) -> ToolResult {
    let title = page.title.trim();
    let heading = if title.is_empty() {
        String::new()
    } else {
        format!("# {title}\n\n")
    };
    let body = if page.markdown.trim().is_empty() {
        "No readable page content was found.".to_owned()
    } else {
        bound_browser_content(page.markdown.trim())
    };
    let controls = format_interactive_elements(controls);
    let output = format!(
        "{heading}URL: {}\nHTTP status: {}\n\n{body}\n\n## Interactive elements\n{controls}",
        page.url, page.status
    );
    let metadata = browser_metadata(&page);
    if (200..300).contains(&page.status) {
        ToolResult::ok(output).with_metadata(metadata)
    } else {
        ToolResult::error(output).with_metadata(metadata)
    }
}

fn format_interactive_elements(value: &Value) -> String {
    let Some(elements) = value.as_array() else {
        return "None found.".into();
    };
    if elements.is_empty() {
        return "None found.".into();
    }
    elements
        .iter()
        .take(MAX_INTERACTIVE_ELEMENTS)
        .filter_map(|element| {
            let reference = compact_field(element, "ref", 12);
            let tag = compact_field(element, "tag", 24);
            if reference.is_empty() || tag.is_empty() {
                return None;
            }
            let mut details = [
                compact_field(element, "type", 40),
                compact_field(element, "role", 40),
                compact_field(element, "label", 120),
                compact_field(element, "placeholder", 120),
                compact_field(element, "text", 120),
                compact_field(element, "name", 80),
                compact_field(element, "href", 240),
            ]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
            details.dedup();
            Some(if details.is_empty() {
                format!("[{reference}] {tag}")
            } else {
                format!("[{reference}] {tag} — {}", details.join(" · "))
            })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn compact_field(element: &Value, field: &str, max_chars: usize) -> String {
    element
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

fn browser_metadata(page: &BrowseResult) -> Value {
    json!({
        "transport": "browser",
        "browser": "oxibrowser",
        "url": page.url,
        "title": page.title,
        "favicon_url": page_favicon_url(&page.url, &page.html),
        "status_code": page.status
    })
}

fn bound_browser_content(content: &str) -> String {
    let mut chars = content.chars();
    let bounded = chars
        .by_ref()
        .take(MAX_BROWSER_CONTENT_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}\n\n[browser content truncated]")
    } else {
        bounded
    }
}

fn target_selector(target: &str) -> Result<String> {
    let target = target.trim();
    if target.is_empty() {
        return Err(invalid("Browser target cannot be empty"));
    }
    if target.strip_prefix('e').is_some_and(|digits| {
        !digits.is_empty() && digits.chars().all(|digit| digit.is_ascii_digit())
    }) {
        Ok(format!(r#"[data-averroes-ref="{target}"]"#))
    } else {
        Ok(target.to_owned())
    }
}

fn required<'a>(value: Option<&'a str>, field: &str, action: &str) -> Result<&'a str> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid(format!("Browser action '{action}' requires {field}")))
}

fn least_recently_used(sessions: &HashMap<String, u64>) -> Option<&str> {
    sessions
        .iter()
        .min_by_key(|(_, last_used)| **last_used)
        .map(|(id, _)| id.as_str())
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidParams {
        tool: "browser".into(),
        message: message.into(),
    }
}

fn browser_error(action: &str, error: impl std::fmt::Display) -> ToolError {
    ToolError::Execution {
        tool: "browser".into(),
        message: format!("Browser action '{action}' failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolActivation;
    use std::sync::Arc;

    #[test]
    fn exposes_one_flat_action_schema() {
        let schema = BrowserTool::default().parameters();

        assert_eq!(schema["required"], json!(["action"]));
        assert!(schema["properties"]["action"]["enum"]
            .as_array()
            .is_some_and(|actions| actions.len() >= 15));
        assert!(schema.get("oneOf").is_none());
        assert!(schema["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("screenshot")));
    }

    #[test]
    fn short_refs_become_stable_data_selectors() {
        assert_eq!(
            target_selector("e12").unwrap(),
            r#"[data-averroes-ref="e12"]"#
        );
        assert_eq!(target_selector("#login").unwrap(), "#login");
        assert!(target_selector("").is_err());
    }

    #[test]
    fn least_recently_used_session_is_selected_for_eviction() {
        let sessions = HashMap::from([("one".into(), 8), ("two".into(), 2), ("three".into(), 5)]);

        assert_eq!(least_recently_used(&sessions), Some("two"));
    }

    #[test]
    fn interaction_output_is_compact_and_page_snapshots_are_bounded() {
        let long = "word ".repeat(MAX_BROWSER_CONTENT_CHARS);
        let output = bound_browser_content(&long);
        assert!(output.chars().count() < MAX_BROWSER_CONTENT_CHARS + 40);
        assert!(output.ends_with("[browser content truncated]"));

        let controls = json!([{
            "ref": "e1",
            "tag": "button",
            "label": "Submit",
            "text": "Submit the form"
        }]);
        assert_eq!(
            format_interactive_elements(&controls),
            "[e1] button — Submit · Submit the form"
        );
    }

    #[tokio::test]
    async fn automatic_session_reuses_the_page_for_forms_and_clicks() {
        let tool = BrowserTool::default();
        let context = test_context("conversation-1");
        let tab = tool.open_tab(&context.session_id).await.unwrap();
        let page = tab
            .goto(concat!(
                "data:text/html,%3Chtml%3E%3Ctitle%3EForm%3C/title%3E%3Cbody%3E",
                "%3Cinput%20id%3D%22name%22%20placeholder%3D%22Your%20name%22%3E",
                "%3Cbutton%20id%3D%22submit%22%20onclick%3D%22document.getElementById('out').textContent%3Ddocument.getElementById('name').value%22%3ESend%3C/button%3E",
                "%3Cp%20id%3D%22out%22%3E%3C/p%3E%3C/body%3E%3C/html%3E"
            ))
            .await
            .unwrap();
        let opened = tool.inspect_page(&tab, page).await.unwrap();
        assert!(opened.content.contains("[e1] input"));
        assert!(opened.content.contains("[e2] button"));

        tool.execute(
            &context,
            &json!({ "action": "fill", "target": "e1", "value": "Averroes" }),
        )
        .await
        .unwrap();
        tool.execute(&context, &json!({ "action": "click", "target": "e2" }))
            .await
            .unwrap();
        let inspected = tool
            .execute(&context, &json!({ "action": "inspect" }))
            .await
            .unwrap();

        assert!(inspected.content.contains("Averroes"));

        let screenshot = tool
            .execute(&context, &json!({ "action": "screenshot", "width": 640 }))
            .await
            .unwrap();
        let png = base64::engine::general_purpose::STANDARD
            .decode(&screenshot.images[0].data)
            .unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(screenshot.images[0].media_type, "image/png");
        assert_eq!(tool.sessions.lock().await.len(), 1);
    }

    fn test_context(session_id: &str) -> ToolContext {
        ToolContext {
            working_dir: std::path::PathBuf::from("/tmp"),
            session_id: session_id.into(),
            agent_id: "agent".into(),
            enabled_tools: Vec::new(),
            available_tools: Vec::new(),
            tool_activation: Arc::new(ToolActivation::default()),
            conversation_context: Vec::new(),
            agent_runner: None,
            memory_search_backend: None,
            agent_event_sink: None,
        }
    }
}
