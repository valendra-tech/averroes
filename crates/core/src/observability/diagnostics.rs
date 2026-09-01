use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use regex::Regex;

const MAX_ENTRIES: usize = 300;

static STARTED_AT: OnceLock<Instant> = OnceLock::new();
static NEXT_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static ENTRIES: LazyLock<RwLock<VecDeque<DiagnosticEntry>>> =
    LazyLock::new(|| RwLock::new(VecDeque::with_capacity(MAX_ENTRIES)));
static TOKEN_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:github_pat_[a-z0-9_]+|gh[opurs]_[a-z0-9]+|sk-[a-z0-9_-]{8,})")
        .expect("valid diagnostic token pattern")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl DiagnosticLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Success => "OK",
            Self::Warning => "WARN",
            Self::Error => "ERROR",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticEntry {
    pub sequence: u64,
    pub timestamp: u64,
    pub elapsed_ms: u64,
    pub level: DiagnosticLevel,
    pub component: String,
    pub message: String,
}

pub fn record(level: DiagnosticLevel, component: impl Into<String>, message: impl AsRef<str>) {
    let entry = DiagnosticEntry {
        sequence: NEXT_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        timestamp: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        elapsed_ms: STARTED_AT.get_or_init(Instant::now).elapsed().as_millis() as u64,
        level,
        component: component.into(),
        message: redact_secrets(message.as_ref()),
    };

    let mut entries = ENTRIES.write();
    if entries.len() == MAX_ENTRIES {
        entries.pop_front();
    }
    entries.push_back(entry);
}

pub fn entries() -> Vec<DiagnosticEntry> {
    ENTRIES.read().iter().cloned().collect()
}

pub fn clear() {
    ENTRIES.write().clear();
}

pub fn export_text() -> String {
    entries()
        .into_iter()
        .map(|entry| {
            format!(
                "#{:04} +{:>7.2}s {:<5} {:<14} {}",
                entry.sequence,
                entry.elapsed_ms as f64 / 1_000.0,
                entry.level.label(),
                entry.component,
                entry.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn redact_secrets(message: &str) -> String {
    TOKEN_PATTERN
        .replace_all(message, "[REDACTED]")
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_supported_provider_token_shapes() {
        let message = redact_secrets(
            "oauth=gho_abc123 app=ghu_DEF456 pat=github_pat_11_AAA openai=sk-secret123456",
        );

        assert_eq!(
            message,
            "oauth=[REDACTED] app=[REDACTED] pat=[REDACTED] openai=[REDACTED]"
        );
    }

    #[test]
    fn keeps_non_secret_diagnostic_context() {
        assert_eq!(
            redact_secrets("GET api.githubcopilot.com/models returned 403"),
            "GET api.githubcopilot.com/models returned 403"
        );
    }
}
