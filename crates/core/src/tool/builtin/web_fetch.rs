//! Read public web pages with a compact direct HTTP request.

use async_trait::async_trait;
use futures::StreamExt;
use oxibrowser_core::network::IpFilter;
use oxibrowser_core::page::Page;
use reqwest::redirect::Policy;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;
use url::Url;

use super::web_browser::validate_url;
use crate::tool::{Result, Tool, ToolContext, ToolError, ToolResult};

pub(crate) const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_DOWNLOAD_BYTES: usize = 2 * 1024 * 1024;
const MAX_FETCH_OUTPUT_CHARS: usize = 24_000;

pub struct WebFetchTool {
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WebFetchParams {
    url: String,
}

impl Default for WebFetchTool {
    fn default() -> Self {
        let redirect_filter = IpFilter::block_private();
        let client = reqwest::Client::builder()
            .redirect(Policy::custom(move |attempt| {
                if redirect_filter.is_hostname_allowed(
                    attempt.url().host_str().unwrap_or_default(),
                ) {
                    attempt.follow()
                } else {
                    tracing::warn!(url = %attempt.url(), "Blocked web_fetch redirect to a private or unresolved host");
                    attempt.stop()
                }
            }))
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(format!("Averroes/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { client }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch an HTTP(S) URL directly without a browser. Use this first for fast, read-only page and API access; use browser only for JavaScript or interaction."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The http(s) URL to fetch"
                }
            },
            "required": ["url"],
            "additionalProperties": false
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let params: WebFetchParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let url = validate_url(self.name(), &params.url)?;

        tracing::info!(tool = self.name(), url = %url, "fetching URL over direct HTTP");
        tokio::time::timeout(FETCH_TIMEOUT, self.fetch(&url))
            .await
            .map_err(|_| ToolError::Execution {
                tool: self.name().into(),
                message: format!(
                    "Direct HTTP fetch timed out after {} seconds while opening {url}",
                    FETCH_TIMEOUT.as_secs()
                ),
            })?
    }
}

impl WebFetchTool {
    async fn fetch(&self, requested_url: &str) -> Result<ToolResult> {
        let response = self
            .client
            .get(requested_url)
            .send()
            .await
            .map_err(|error| ToolError::Execution {
                tool: "web_fetch".into(),
                message: format!("Direct HTTP request failed for {requested_url}: {error}"),
            })?;
        let status = response.status().as_u16();
        let final_url = response.url().to_string();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();

        if !content_type.is_empty() && !is_textual_content_type(&content_type) {
            let metadata = http_metadata(
                requested_url,
                &final_url,
                status,
                &content_type,
                "",
                None,
                false,
            );
            return Ok(ToolResult::error(format!(
                "web_fetch only returns textual responses; {final_url} returned {content_type}"
            ))
            .with_metadata(metadata));
        }

        let mut body = Vec::new();
        let mut response_truncated = false;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| ToolError::Execution {
                tool: "web_fetch".into(),
                message: format!("Failed reading the response from {final_url}: {error}"),
            })?;
            let remaining = MAX_DOWNLOAD_BYTES.saturating_sub(body.len());
            if chunk.len() > remaining {
                body.extend_from_slice(&chunk[..remaining]);
                response_truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
        }

        format_fetched_response(
            requested_url,
            &final_url,
            status,
            &content_type,
            &body,
            response_truncated,
        )
        .await
    }
}

async fn format_fetched_response(
    requested_url: &str,
    final_url: &str,
    status: u16,
    content_type: &str,
    bytes: &[u8],
    response_truncated: bool,
) -> Result<ToolResult> {
    let is_html = content_type.to_ascii_lowercase().contains("html")
        || (content_type.is_empty()
            && String::from_utf8_lossy(bytes)
                .trim_start()
                .starts_with(['<', '!']));
    let (title, favicon_url, readable) = if is_html {
        let html = oxibrowser_core::encoding::decode_html(bytes, Some(content_type));
        let parsed_url = Url::parse(final_url).map_err(|error| ToolError::Execution {
            tool: "web_fetch".into(),
            message: format!("The final response URL is invalid: {error}"),
        })?;
        let page = Page::from_html(parsed_url, &html, status, content_type.to_owned())
            .await
            .map_err(|error| ToolError::Execution {
                tool: "web_fetch".into(),
                message: format!("Failed to parse HTML from {final_url}: {error}"),
            })?;
        (
            page.title().unwrap_or_default().trim().to_owned(),
            page_favicon_url(final_url, &html),
            page.to_markdown(),
        )
    } else {
        (
            String::new(),
            None,
            String::from_utf8_lossy(bytes).into_owned(),
        )
    };

    let body = if readable.trim().is_empty() {
        "No readable text content was found in this response.".to_owned()
    } else {
        bound_fetch_output(readable.trim())
    };
    let title_heading = (!title.is_empty())
        .then(|| format!("# {title}\n\n"))
        .unwrap_or_default();
    let redirect = (requested_url != final_url)
        .then(|| format!("Requested URL: {requested_url}\n"))
        .unwrap_or_default();
    let truncation = response_truncated
        .then_some("Response body was truncated before parsing.\n")
        .unwrap_or_default();
    let output = format!(
        "{title_heading}URL: {final_url}\n{redirect}HTTP status: {status}\nContent-Type: {}\n{truncation}\n{body}",
        if content_type.is_empty() {
            "unknown"
        } else {
            content_type
        }
    );
    let metadata = http_metadata(
        requested_url,
        final_url,
        status,
        content_type,
        &title,
        favicon_url,
        response_truncated,
    );

    if (200..300).contains(&status) {
        Ok(ToolResult::ok(output).with_metadata(metadata))
    } else {
        Ok(ToolResult::error(output).with_metadata(metadata))
    }
}

fn http_metadata(
    requested_url: &str,
    final_url: &str,
    status: u16,
    content_type: &str,
    title: &str,
    favicon_url: Option<String>,
    response_truncated: bool,
) -> Value {
    json!({
        "transport": "http",
        "url": final_url,
        "requested_url": requested_url,
        "title": title,
        "favicon_url": favicon_url,
        "status_code": status,
        "content_type": content_type,
        "response_truncated": response_truncated
    })
}

fn is_textual_content_type(content_type: &str) -> bool {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    mime.starts_with("text/")
        || matches!(
            mime.as_str(),
            "application/json"
                | "application/ld+json"
                | "application/javascript"
                | "application/x-javascript"
                | "application/xml"
                | "application/xhtml+xml"
                | "application/x-www-form-urlencoded"
                | "application/graphql"
        )
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
}

fn bound_fetch_output(content: &str) -> String {
    let mut chars = content.chars();
    let bounded = chars
        .by_ref()
        .take(MAX_FETCH_OUTPUT_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}\n\n[web_fetch output truncated]")
    } else {
        bounded
    }
}

/// Finds the document-declared icon rather than assuming `/favicon.ico`.
pub(crate) fn page_favicon_url(page_url: &str, html: &str) -> Option<String> {
    let page_url = Url::parse(page_url).ok()?;
    let mut offset = 0usize;
    while let Some(relative_start) = html[offset..]
        .as_bytes()
        .windows(5)
        .position(|candidate| candidate.eq_ignore_ascii_case(b"<link"))
    {
        let start = offset + relative_start;
        let relative_end = html[start..].find('>')?;
        let end = start + relative_end + 1;
        let tag = &html[start..end];
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
            Some(b'"' | b'\'') => {
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
    fn direct_fetch_is_bounded_to_sixty_seconds() {
        assert_eq!(FETCH_TIMEOUT, Duration::from_secs(60));
    }

    #[test]
    fn validates_only_http_urls() {
        assert!(validate_url("web_fetch", "https://example.com").is_ok());
        assert!(validate_url("web_fetch", "http://example.com/path").is_ok());
        assert!(validate_url("web_fetch", "file:///tmp/secrets").is_err());
        assert!(validate_url("web_fetch", "javascript:alert(1)").is_err());
        assert!(validate_url("web_fetch", "http://127.0.0.1").is_err());
    }

    #[test]
    fn accepts_readable_content_types_and_rejects_binary_content() {
        assert!(is_textual_content_type("text/html; charset=utf-8"));
        assert!(is_textual_content_type("application/json"));
        assert!(is_textual_content_type("application/rss+xml"));
        assert!(!is_textual_content_type("image/png"));
        assert!(!is_textual_content_type("application/octet-stream"));
    }

    #[test]
    fn output_bound_does_not_split_unicode() {
        let output = bound_fetch_output(&"é".repeat(MAX_FETCH_OUTPUT_CHARS + 1));

        assert!(output.ends_with("[web_fetch output truncated]"));
        assert_eq!(output.matches('é').count(), MAX_FETCH_OUTPUT_CHARS);
    }

    #[tokio::test]
    async fn formats_html_as_markdown_with_direct_http_metadata() {
        let result = format_fetched_response(
            "https://example.com/",
            "https://example.com/",
            200,
            "text/html; charset=utf-8",
            b"<title>Example</title><h1>Hello</h1>",
            false,
        )
        .await
        .unwrap();

        assert!(result.success);
        assert!(result.content.contains("# Example"));
        assert!(result.content.contains("Hello"));
        assert_eq!(result.metadata.as_ref().unwrap()["transport"], "http");
    }

    #[tokio::test]
    async fn captures_a_document_declared_favicon() {
        let result = format_fetched_response(
            "https://example.com/docs/page",
            "https://example.com/docs/page",
            200,
            "text/html",
            br#"<link rel="icon" href="/assets/brand.svg"><p>Hello</p>"#,
            false,
        )
        .await
        .unwrap();

        assert_eq!(
            result.metadata.as_ref().unwrap()["favicon_url"],
            "https://example.com/assets/brand.svg"
        );
    }

    #[tokio::test]
    async fn invalid_params_do_not_start_a_request() {
        let tool = WebFetchTool::default();

        assert!(tool.execute(&test_context(), &json!({})).await.is_err());
    }

    #[tokio::test]
    async fn fetches_html_over_direct_http_without_browser_metadata() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 2048];
            let _ = socket.read(&mut request).await;
            let body = "<title>Direct</title><main>Fast response</main>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let result = WebFetchTool::default()
            .fetch(&format!("http://{address}/page"))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.content.contains("Fast response"));
        assert_eq!(result.metadata.as_ref().unwrap()["transport"], "http");
        assert!(result.metadata.as_ref().unwrap().get("browser").is_none());
        server.await.unwrap();
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
