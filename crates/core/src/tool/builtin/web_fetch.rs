//! Read web pages through the shared OxiBrowser engine.

use async_trait::async_trait;
use oxibrowser_core::BrowseResult;
use serde_json::{json, Value};
use tokio::sync::OnceCell;
use url::Url;

use super::web_browser::{validate_url, BrowserRuntime};
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

pub struct WebFetchTool {
    // Keep even the runtime wrapper unconstructed until a valid page-open
    // request arrives. Registering the tool must have no browser lifecycle or
    // networking side effect.
    browser: OnceCell<BrowserRuntime>,
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self {
            browser: OnceCell::new(),
        }
    }
}

impl WebFetchTool {
    async fn browser(&self) -> &BrowserRuntime {
        self.browser
            .get_or_init(|| async { BrowserRuntime::default() })
            .await
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Open a public web page with OxiBrowser and return clean Markdown content"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The http(s) URL to open"
                },
            },
            "required": ["url"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let raw_url = params["url"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParams {
                tool: self.name().into(),
                message: "Missing required parameter: url".into(),
            })?;
        let url = validate_url(self.name(), raw_url)?;

        tracing::info!(tool = self.name(), url = %url, "opening web page with OxiBrowser");
        let page = self.browser().await.browse(&url).await?;

        Ok(format_page_result(page))
    }
}

fn format_page_result(page: BrowseResult) -> ToolResult {
    let title = page.title.trim();
    let heading = if title.is_empty() {
        String::new()
    } else {
        format!("# {title}\n\n")
    };
    let body = if page.markdown.trim().is_empty() {
        "No readable Markdown content was found on this page.".to_string()
    } else {
        page.markdown.clone()
    };
    let output = format!(
        "{heading}URL: {}\nHTTP status: {}\n\n{body}",
        page.url, page.status
    );
    let favicon_url = page_favicon_url(&page);
    let metadata = json!({
        "browser": "oxibrowser",
        "url": page.url,
        "title": page.title,
        "favicon_url": favicon_url,
        "status_code": page.status
    });

    if (200..300).contains(&page.status) {
        ToolResult::ok(output).with_metadata(metadata)
    } else {
        ToolResult::error(output).with_metadata(metadata)
    }
}

/// Finds the document-declared icon rather than assuming `/favicon.ico`.
/// Framework sites commonly use a hashed SVG/PNG or `apple-touch-icon`, so a
/// first-party `<link rel=icon>` is both more accurate and more likely to load
/// in the native image cache.
fn page_favicon_url(page: &BrowseResult) -> Option<String> {
    let page_url = Url::parse(&page.url).ok()?;
    let mut offset = 0usize;
    while let Some(relative_start) = page.html[offset..]
        .as_bytes()
        .windows(5)
        .position(|candidate| candidate.eq_ignore_ascii_case(b"<link"))
    {
        let start = offset + relative_start;
        let relative_end = page.html[start..].find('>')?;
        let end = start + relative_end + 1;
        let tag = &page.html[start..end];
        offset = end;

        let Some(rel) = html_tag_attribute(tag, "rel") else {
            continue;
        };
        let is_icon = rel.split_ascii_whitespace().any(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "icon" | "shortcut" | "apple-touch-icon" | "mask-icon"
            )
        });
        if !is_icon {
            continue;
        }
        let Some(href) = html_tag_attribute(tag, "href") else {
            continue;
        };
        let icon_url = page_url.join(href.trim()).ok()?;
        if matches!(icon_url.scheme(), "http" | "https") {
            return Some(icon_url.to_string());
        }
    }
    None
}

fn html_tag_attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    let mut search_from = 0usize;
    while let Some(relative) = lower[search_from..].find(&name) {
        let start = search_from + relative;
        let before = lower.as_bytes().get(start.wrapping_sub(1)).copied();
        let after = lower.as_bytes().get(start + name.len()).copied();
        let valid_boundary = before.is_none_or(|byte| byte.is_ascii_whitespace() || byte == b'<')
            && after.is_some_and(|byte| byte.is_ascii_whitespace() || byte == b'=');
        if !valid_boundary {
            search_from = start + name.len();
            continue;
        }

        let mut value_start = start + name.len();
        while lower
            .as_bytes()
            .get(value_start)
            .is_some_and(u8::is_ascii_whitespace)
        {
            value_start += 1;
        }
        if lower.as_bytes().get(value_start) != Some(&b'=') {
            search_from = start + name.len();
            continue;
        }
        value_start += 1;
        while lower
            .as_bytes()
            .get(value_start)
            .is_some_and(u8::is_ascii_whitespace)
        {
            value_start += 1;
        }
        let quote = tag.as_bytes().get(value_start).copied();
        let (value_start, value_end) = match quote {
            Some(b'\"' | b'\'') => {
                let value_start = value_start + 1;
                let value_end = tag[value_start..]
                    .find(quote? as char)
                    .map(|relative_end| value_start + relative_end)?;
                (value_start, value_end)
            }
            _ => {
                let value_end = tag[value_start..]
                    .find(|character: char| character.is_ascii_whitespace() || character == '>')
                    .map(|relative_end| value_start + relative_end)
                    .unwrap_or(tag.len());
                (value_start, value_end)
            }
        };
        return tag
            .get(value_start..value_end)
            .filter(|value| !value.is_empty());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_only_http_urls() {
        assert!(validate_url("web_fetch", "https://example.com").is_ok());
        assert!(validate_url("web_fetch", "http://example.com/path").is_ok());
        assert!(validate_url("web_fetch", "file:///tmp/secrets").is_err());
        assert!(validate_url("web_fetch", "javascript:alert(1)").is_err());
    }

    #[test]
    fn formats_markdown_and_metadata() {
        let result = format_page_result(BrowseResult {
            url: "https://example.com/".into(),
            title: "Example".into(),
            status: 200,
            markdown: "Hello".into(),
            html: "<p>Hello</p>".into(),
        });

        assert!(result.success);
        assert!(result.content.contains("# Example"));
        assert!(result.content.contains("Hello"));
        assert_eq!(result.metadata.as_ref().unwrap()["browser"], "oxibrowser");
    }

    #[test]
    fn captures_a_document_declared_favicon() {
        let result = format_page_result(BrowseResult {
            url: "https://example.com/docs/page".into(),
            title: "Example".into(),
            status: 200,
            markdown: "Hello".into(),
            html: r#"<link rel="icon" href="/assets/brand.svg">"#.into(),
        });

        assert_eq!(
            result.metadata.as_ref().unwrap()["favicon_url"],
            "https://example.com/assets/brand.svg"
        );
    }

    #[tokio::test]
    async fn does_not_construct_browser_runtime_without_a_valid_request() {
        let tool = WebFetchTool::default();
        assert!(tool.browser.get().is_none());

        assert!(tool.execute(&test_context(), &json!({})).await.is_err());
        assert!(tool.browser.get().is_none());
    }

    fn test_context() -> ToolContext {
        ToolContext {
            working_dir: std::path::PathBuf::from("/tmp"),
            session_id: "session".into(),
            agent_id: "agent".into(),
            enabled_tools: Vec::new(),
            available_tools: Vec::new(),
            tool_activation: std::sync::Arc::new(crate::tool::ToolActivation::default()),
            conversation_context: Vec::new(),
            agent_runner: None,
            memory_search_backend: None,
            agent_event_sink: None,
        }
    }
}
