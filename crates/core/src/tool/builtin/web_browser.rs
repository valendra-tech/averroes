//! Shared OxiBrowser lifecycle and URL validation for internet tools.

use oxibrowser_core::{BrowseResult, Browser, BrowserConfig};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{OnceCell, Semaphore};
use url::Url;

use crate::tool::{Result, ToolError};

const MAX_BROWSER_SESSIONS: usize = 8;
pub(crate) const PAGE_OPEN_TIMEOUT: Duration = Duration::from_secs(10);
// The browser itself supports up to eight sessions, but page rendering and
// JavaScript execution are comparatively heavy. A shared gate leaves room for
// cleanup and prevents a burst of parallel tool calls from reaching its hard
// session ceiling.
const MAX_CONCURRENT_PAGE_OPENS: usize = 4;
const MAX_RESPONSE_BODY_BYTES: usize = 4 * 1024 * 1024;
// OxiBrowser 0.20 defaults these navigation limits to 4,096 calls and 16,384
// operand-stack entries. Modern framework bundles can exhaust the native JS
// thread stack before Boa turns that recursion into a JavaScript exception.
// Keep page scripts useful, but require Boa to stop a runaway bundle safely.
const MAX_NAV_SCRIPT_RECURSION: usize = 128;
const MAX_NAV_SCRIPT_STACK_SIZE: usize = 1_024;
const MAX_NAV_SCRIPT_LOOP_ITERATIONS: u64 = 5_000_000;
const NAV_SCRIPT_TIMEOUT_MS: u64 = 8_000;

/// Lazily owns one browser engine for the whole tool registry.
///
/// Creating a browser per tool call is expensive and loses the browser's
/// connection pool. The cell keeps startup cheap and reuses the engine for
/// later searches and page reads. Cookies remain in memory only.
#[derive(Clone)]
pub(crate) struct BrowserRuntime {
    browser: Arc<OnceCell<Arc<Browser>>>,
    page_open_permits: Arc<Semaphore>,
}

impl Default for BrowserRuntime {
    fn default() -> Self {
        Self {
            browser: Arc::new(OnceCell::new()),
            page_open_permits: Arc::new(Semaphore::new(MAX_CONCURRENT_PAGE_OPENS)),
        }
    }
}

impl BrowserRuntime {
    pub(crate) async fn browser(&self) -> Result<Arc<Browser>> {
        self.browser
            .get_or_try_init(|| async {
                Browser::new(browser_config())
                    .await
                    .map(Arc::new)
                    .map_err(|error| ToolError::Execution {
                        tool: "web_fetch".into(),
                        message: format!("Failed to start OxiBrowser: {error}"),
                    })
            })
            .await
            .map(Arc::clone)
    }

    pub(crate) async fn browse(&self, url: &str) -> Result<BrowseResult> {
        self.browse_with_timeout(url, PAGE_OPEN_TIMEOUT).await
    }

    async fn browse_with_timeout(&self, url: &str, timeout: Duration) -> Result<BrowseResult> {
        let deadline = Instant::now() + timeout;
        let _permit = tokio::time::timeout(remaining(deadline), self.page_open_permits.acquire())
            .await
            .map_err(|_| page_open_timeout(url, timeout))?
            .map_err(|_| ToolError::Execution {
                tool: "web_fetch".into(),
                message: "OxiBrowser page queue is unavailable".into(),
            })?;
        let browser = tokio::time::timeout(remaining(deadline), self.browser())
            .await
            .map_err(|_| page_open_timeout(url, timeout))??;
        let session = tokio::time::timeout(remaining(deadline), browser.new_session())
            .await
            .map_err(|_| page_open_timeout(url, timeout))?
            .map_err(|error| ToolError::Execution {
                tool: "web_fetch".into(),
                message: format!("OxiBrowser could not create a page for {url}: {error}"),
            })?;

        let navigation = tokio::time::timeout(remaining(deadline), async {
            let mut session = session.write().await;
            session.navigate(url).await
        })
        .await;

        let navigation = match navigation {
            Ok(result) => result,
            Err(_) => {
                close_session(&browser, &session).await;
                return Err(page_open_timeout(url, timeout));
            }
        };

        let result = match navigation {
            Ok(()) => {
                let mut session = session.write().await;
                let result = match session.page() {
                    Some(page) => Ok(BrowseResult::from_page(page)),
                    None => Ok(BrowseResult::empty()),
                };
                let _ = session.close().await;
                result
            }
            Err(error) => {
                close_session(&browser, &session).await;
                Err(error)
            }
        };
        browser.cleanup_closed_sessions();
        result.map_err(|error| ToolError::Execution {
            tool: "web_fetch".into(),
            message: format!("OxiBrowser could not open {url}: {error}"),
        })
    }
}

fn remaining(deadline: Instant) -> Duration {
    deadline
        .checked_duration_since(Instant::now())
        .unwrap_or_default()
}

fn page_open_timeout(url: &str, timeout: Duration) -> ToolError {
    let timeout = if timeout.as_secs() > 0 && timeout.subsec_nanos() == 0 {
        format!("{} seconds", timeout.as_secs())
    } else {
        format!("{} ms", timeout.as_millis())
    };
    ToolError::Execution {
        tool: "web_fetch".into(),
        message: format!("OxiBrowser timed out after {timeout} while opening {url}"),
    }
}

async fn close_session(
    browser: &Browser,
    session: &Arc<tokio::sync::RwLock<oxibrowser_core::Session>>,
) {
    {
        let mut session = session.write().await;
        let _ = session.close().await;
    }
    browser.cleanup_closed_sessions();
}

fn browser_config() -> BrowserConfig {
    let mut config = BrowserConfig::headless();
    config.max_sessions = MAX_BROWSER_SESSIONS;
    config.max_response_body_bytes = MAX_RESPONSE_BODY_BYTES;
    config.nav_script_max_recursion = MAX_NAV_SCRIPT_RECURSION;
    config.nav_script_max_stack_size = MAX_NAV_SCRIPT_STACK_SIZE;
    config.nav_script_max_loop_iterations = MAX_NAV_SCRIPT_LOOP_ITERATIONS;
    config.nav_script_timeout_ms = NAV_SCRIPT_TIMEOUT_MS;
    config
}

pub(crate) fn validate_url(tool: &str, raw_url: &str) -> Result<String> {
    let raw_url = raw_url.trim();
    if raw_url.is_empty() {
        return Err(ToolError::InvalidParams {
            tool: tool.into(),
            message: "URL cannot be empty".into(),
        });
    }

    let url = Url::parse(raw_url).map_err(|error| ToolError::InvalidParams {
        tool: tool.into(),
        message: format!("Invalid URL: {error}"),
    })?;

    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(ToolError::InvalidParams {
            tool: tool.into(),
            message: "Only http:// and https:// URLs with a host are allowed".into(),
        });
    }

    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_script_limits_stay_within_the_safe_boa_range() {
        let config = browser_config();

        assert_eq!(PAGE_OPEN_TIMEOUT, Duration::from_secs(10));
        assert_eq!(config.nav_script_max_recursion, 128);
        assert_eq!(config.nav_script_max_stack_size, 1_024);
        assert_eq!(config.nav_script_max_loop_iterations, 5_000_000);
        assert_eq!(config.nav_script_timeout_ms, 8_000);
    }

    #[test]
    fn runtime_stays_off_until_a_page_is_requested() {
        let runtime = BrowserRuntime::default();
        assert!(runtime.browser.get().is_none());
    }

    #[tokio::test]
    async fn one_shot_browses_release_their_session_slot() {
        let mut config = browser_config();
        config.max_sessions = 1;
        let browser = Browser::new(config).await.unwrap();

        browser
            .browse("data:text/html,%3Cp%3Efirst%3C/p%3E")
            .await
            .unwrap();
        browser
            .browse("data:text/html,%3Cp%3Esecond%3C/p%3E")
            .await
            .unwrap();

        assert!(browser.sessions().read().is_empty());
    }

    #[tokio::test]
    async fn page_open_timeout_includes_time_waiting_for_a_slot() {
        let runtime = BrowserRuntime::default();
        let permits = futures::future::join_all(
            (0..MAX_CONCURRENT_PAGE_OPENS).map(|_| runtime.page_open_permits.acquire()),
        )
        .await;
        assert!(permits.iter().all(|permit| permit.is_ok()));
        let result = runtime
            .browse_with_timeout(
                "data:text/html,%3Cp%3Enever%20opened%3C/p%3E",
                Duration::from_millis(20),
            )
            .await;

        assert!(matches!(
            result,
            Err(ToolError::Execution { tool, message })
                if tool == "web_fetch" && message.contains("timed out after 20 ms")
        ));
    }

    #[tokio::test]
    async fn recursive_page_script_is_bounded_without_killing_the_browser() {
        let browser = Browser::new(browser_config()).await.unwrap();
        let session = browser.new_session().await.unwrap();
        let mut session = session.write().await;

        session
            .navigate(
                "data:text/html,%3Cscript%3Efunction%20loop()%7Bloop()%7Dloop()%3C/script%3E%3Cp%3Eready%3C/p%3E",
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn failed_page_script_leaves_the_browser_responsive() {
        let browser = Browser::new(browser_config()).await.unwrap();
        let session = browser.new_session().await.unwrap();
        let mut session = session.write().await;

        session
            .navigate(
                "data:text/html,%3Cscript%3Ethrow%20new%20Error('boom')%3C/script%3E%3Cscript%3Ewindow.__unsafe%20%3D%201%3C/script%3E%3Cp%3Esafe%3C/p%3E",
            )
            .await
            .unwrap();

        let value = session.evaluate_js("window.__unsafe").await.unwrap();
        assert_eq!(value.value, Some(serde_json::Value::Null));
        let text = session
            .evaluate_js("document.body.textContent")
            .await
            .unwrap();
        assert_eq!(text.value, Some(serde_json::json!("safe")));
    }

    #[tokio::test]
    #[ignore = "requires internet; regression coverage for a modern Next.js site"]
    async fn navigates_valendra_without_exhausting_the_js_thread_stack() {
        let browser = Browser::new(browser_config()).await.unwrap();

        let page = browser.browse("https://valendra.tech/").await.unwrap();

        assert!((200..400).contains(&page.status));
        assert!(!page.markdown.trim().is_empty());
    }
}
