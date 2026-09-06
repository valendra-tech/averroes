use super::ContextUsage;
use crate::storage::work::{WorkHistoryEntry, WorkHistoryKind};
use parking_lot::{Mutex, RwLock};
use std::fmt;

/// Absolute character bound for a handoff carried into a fresh context.
pub const MAX_HANDOFF_CHARS: usize = 20_000;
/// Bound used by later recovery builders for one persisted recovery record.
pub const MAX_RECOVERY_RECORD_CHARS: usize = 4_000;
/// Tokens kept free when sizing a page for a provider request.
pub const PAGE_MARGIN_TOKENS: usize = 1_000;
/// Reads smaller than this are not useful enough to return automatically.
pub const MIN_PAGE_CHARS: usize = 1_000;
/// Automatic rollover needs enough usable context to leave a meaningful band.
pub const MIN_USABLE_TOKENS: usize = 10_000;
/// Reminders occupy at most this many tokens before the rollover line.
pub const REMINDER_BUFFER_TOKENS: usize = 32_000;
/// Tokens kept free from the provider window for agent rollover safety.
pub const CONTEXT_RESERVE_TOKENS: usize = 1_000;
/// Maximum size of the automatic reminder inserted into provider context.
pub const MAX_REMINDER_CHARS: usize = 512;

/// Worst-case UTF-8 width used when converting a token budget to a character
/// budget without seeing the page contents.
const MAX_UTF8_BYTES_PER_CHAR: u64 = 4;
const IMAGE_ALLOWANCE_TOKENS: u64 = 1_024;

#[derive(Debug, Clone, Copy, Default)]
struct RequestOverhead {
    system_prompt_tokens: u64,
    active_tool_schema_tokens: u64,
    pending_user_tokens: u64,
    image_count: u64,
}

impl RequestOverhead {
    fn total_tokens(self) -> u64 {
        self.system_prompt_tokens
            .saturating_add(self.active_tool_schema_tokens)
            .saturating_add(self.pending_user_tokens)
            .saturating_add(self.image_count.saturating_mul(IMAGE_ALLOWANCE_TOKENS))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextBudget {
    pub context_window: u64,
    pub reserve_tokens: u64,
    pub enabled: bool,
    pub usable: i64,
    pub rollover_at: u64,
    pub supported: bool,
}

impl ContextBudget {
    pub fn new(context_window: usize, reserve_tokens: usize, enabled: bool) -> Self {
        let usable = context_window as i64 - reserve_tokens as i64;
        let supported = !enabled || usable >= MIN_USABLE_TOKENS as i64;
        Self {
            context_window: context_window as u64,
            reserve_tokens: reserve_tokens as u64,
            enabled,
            usable,
            rollover_at: usable.max(0) as u64 + 1,
            supported,
        }
    }

    pub fn automatic_enabled(&self) -> bool {
        self.enabled && self.supported && self.usable > 0
    }

    pub fn remind_at(&self) -> u64 {
        self.rollover_at
            .saturating_sub((REMINDER_BUFFER_TOKENS as u64).min(self.usable.max(0) as u64 / 10))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRequest {
    pub handoff: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderClaim {
    pub fingerprint: String,
    pub remaining_tokens: u64,
    pub rollover_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomaticHandoffError {
    reason: String,
}

impl AutomaticHandoffError {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl fmt::Display for AutomaticHandoffError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.reason)
    }
}

impl std::error::Error for AutomaticHandoffError {}

#[derive(Debug)]
pub struct ContextController {
    budget: RwLock<ContextBudget>,
    window_id: RwLock<String>,
    generation: RwLock<u64>,
    usage: RwLock<Option<ContextUsage>>,
    pending: Mutex<Option<ContextRequest>>,
    reminder_fingerprints: RwLock<std::collections::HashSet<String>>,
    request_overhead: RwLock<RequestOverhead>,
}

impl ContextController {
    pub fn new(budget: ContextBudget) -> Self {
        Self {
            budget: RwLock::new(budget),
            window_id: RwLock::new("initial".into()),
            generation: RwLock::new(0),
            usage: RwLock::new(None),
            pending: Mutex::new(None),
            reminder_fingerprints: RwLock::new(std::collections::HashSet::new()),
            request_overhead: RwLock::new(RequestOverhead::default()),
        }
    }

    pub fn for_test(context_window: usize, reserve_tokens: usize) -> Self {
        Self::new(ContextBudget::new(context_window, reserve_tokens, true))
    }

    pub fn budget(&self) -> ContextBudget {
        *self.budget.read()
    }

    pub fn record_usage(&self, usage: ContextUsage) {
        *self.usage.write() = Some(usage);
    }

    pub fn current_generation(&self) -> u64 {
        *self.generation.read()
    }

    pub fn record_usage_for_generation(&self, generation: u64, usage: ContextUsage) -> bool {
        let current_generation = self.generation.read();
        if *current_generation != generation {
            return false;
        }
        *self.usage.write() = Some(usage);
        true
    }

    pub fn usage(&self) -> Option<ContextUsage> {
        *self.usage.read()
    }

    pub fn context_usage(&self) -> ContextUsage {
        self.usage()
            .unwrap_or_else(|| ContextUsage::unknown(self.budget().context_window as usize))
    }

    pub fn clear_usage(&self) {
        *self.usage.write() = None;
    }

    pub fn clear_usage_for_generation(&self, generation: u64) -> bool {
        let current_generation = self.generation.read();
        if *current_generation != generation {
            return false;
        }
        self.clear_usage();
        true
    }

    pub fn replace_budget(&self, budget: ContextBudget) {
        let mut generation = self.generation.write();
        *generation = generation.saturating_add(1);
        *self.budget.write() = budget;
        self.clear_usage();
        *self.pending.lock() = None;
        self.reminder_fingerprints.write().clear();
        *self.request_overhead.write() = RequestOverhead::default();
    }

    pub fn window_id(&self) -> String {
        self.window_id.read().clone()
    }

    /// Restores a persisted window identity without resetting usage or
    /// pending context actions. A caller restoring a full snapshot may apply
    /// the provider usage separately after this identity is restored.
    pub fn set_window_id(&self, window_id: impl Into<String>) {
        *self.window_id.write() = window_id.into();
    }

    pub fn set_request_overhead(
        &self,
        system_prompt_tokens: usize,
        active_tool_schema_tokens: usize,
        pending_user_tokens: usize,
        image_count: usize,
    ) {
        *self.request_overhead.write() = RequestOverhead {
            system_prompt_tokens: system_prompt_tokens as u64,
            active_tool_schema_tokens: active_tool_schema_tokens as u64,
            pending_user_tokens: pending_user_tokens as u64,
            image_count: image_count as u64,
        };
    }

    pub(crate) fn request_overhead(&self) -> (usize, usize, usize, usize) {
        let overhead = *self.request_overhead.read();
        (
            overhead.system_prompt_tokens as usize,
            overhead.active_tool_schema_tokens as usize,
            overhead.pending_user_tokens as usize,
            overhead.image_count as usize,
        )
    }

    pub fn request_context(&self, handoff: Option<String>) -> Result<(), String> {
        let handoff = handoff
            .filter(|handoff| !handoff.trim().is_empty())
            .map(|handoff| {
                if handoff.chars().count() > self.handoff_limit() {
                    Err(format!(
                        "handoff exceeds the {} character limit",
                        self.handoff_limit()
                    ))
                } else {
                    Ok(truncate_utf8(&handoff, self.handoff_limit()))
                }
            })
            .transpose()?;

        *self.pending.lock() = Some(ContextRequest { handoff });
        Ok(())
    }

    pub fn pending_request(&self) -> Option<ContextRequest> {
        self.pending.lock().clone()
    }

    pub fn clear_pending_request(&self) {
        *self.pending.lock() = None;
    }

    pub(crate) fn take_pending_request(&self) -> Option<ContextRequest> {
        self.pending.lock().take()
    }

    pub(crate) fn restore_pending_request(&self, request: Option<ContextRequest>) {
        *self.pending.lock() = request;
    }

    pub fn handoff_limit(&self) -> usize {
        let overhead = *self.request_overhead.read();
        MAX_HANDOFF_CHARS.min(self.page_capacity_chars(overhead))
    }

    /// Returns the safe capacity available in a fresh provider window. At the
    /// old window's rollover line, the remaining capacity is exhausted and
    /// cannot be used to size the handoff that will be sent to the new one.
    pub fn fresh_handoff_limit(&self) -> usize {
        let overhead = *self.request_overhead.read();
        MAX_HANDOFF_CHARS.min(self.fresh_page_capacity_chars(overhead))
    }

    /// Returns a safe character budget for a paged text result.
    ///
    /// When provider usage is not known, only half of the fresh operational
    /// capacity is exposed so the first request has room for its system
    /// prompt, schemas, pending input, image allowance, and response overhead.
    pub fn safe_page_chars(
        &self,
        offset: usize,
        system_prompt_tokens: usize,
        active_tool_schema_tokens: usize,
        pending_user_tokens: usize,
        image_count: usize,
    ) -> Result<usize, String> {
        self.set_request_overhead(
            system_prompt_tokens,
            active_tool_schema_tokens,
            pending_user_tokens,
            image_count,
        );
        let overhead = *self.request_overhead.read();
        let chars = self.page_capacity_chars(overhead) as u64;
        if chars < MIN_PAGE_CHARS as u64 {
            return Err(format!(
                "safe page at offset {offset} is only {chars} characters; minimum is {MIN_PAGE_CHARS}"
            ));
        }
        Ok(chars.min(usize::MAX as u64) as usize)
    }

    fn page_capacity_chars(&self, overhead: RequestOverhead) -> usize {
        let budget = self.budget();
        let request_overhead = overhead.total_tokens();
        let fresh_capacity = (budget.usable.max(0) as u64)
            .saturating_sub(PAGE_MARGIN_TOKENS as u64)
            .saturating_sub(request_overhead);
        let available_tokens = match self.usage().and_then(|usage| usage.input_tokens) {
            Some(input_tokens) => (budget.usable.max(0) as u64)
                .saturating_sub(input_tokens)
                .saturating_sub(PAGE_MARGIN_TOKENS as u64)
                .saturating_sub(request_overhead),
            None => fresh_capacity / 2,
        };
        available_tokens
            .saturating_div(MAX_UTF8_BYTES_PER_CHAR)
            .min(usize::MAX as u64) as usize
    }

    fn fresh_page_capacity_chars(&self, overhead: RequestOverhead) -> usize {
        let budget = self.budget();
        let available_tokens = (budget.usable.max(0) as u64)
            .saturating_sub(PAGE_MARGIN_TOKENS as u64)
            .saturating_sub(overhead.total_tokens());
        available_tokens
            .saturating_div(MAX_UTF8_BYTES_PER_CHAR)
            .min(usize::MAX as u64) as usize
    }

    pub fn reminder_fingerprint(&self, model: &str) -> String {
        format!(
            "{}:{}:{}:{}",
            self.window_id(),
            model,
            self.budget().context_window,
            self.budget().reserve_tokens
        )
    }

    pub fn mark_reminder(&self, fingerprint: impl Into<String>) {
        self.reminder_fingerprints
            .write()
            .insert(fingerprint.into());
    }

    pub fn reminder_matches(&self, fingerprint: impl AsRef<str>) -> bool {
        self.reminder_fingerprints
            .read()
            .contains(fingerprint.as_ref())
    }

    /// Atomically claims the one automatic reminder allowed for this window
    /// and budget fingerprint. Explicit context actions do not use this path.
    pub fn claim_automatic_reminder(&self, model: &str) -> Option<ReminderClaim> {
        let budget = self.budget();
        if !budget.automatic_enabled() {
            return None;
        }
        let input_tokens = self.usage().and_then(|usage| usage.input_tokens)?;
        if input_tokens < budget.remind_at() || input_tokens >= budget.rollover_at {
            return None;
        }

        let fingerprint = self.reminder_fingerprint(model);
        let mut claimed = self.reminder_fingerprints.write();
        if claimed.contains(&fingerprint) {
            return None;
        }
        claimed.insert(fingerprint.clone());
        Some(ReminderClaim {
            fingerprint,
            remaining_tokens: budget.rollover_at.saturating_sub(input_tokens),
            rollover_at: budget.rollover_at,
        })
    }

    pub fn clear_reminder(&self, fingerprint: impl AsRef<str>) {
        self.reminder_fingerprints
            .write()
            .remove(fingerprint.as_ref());
    }

    /// Starts a new provider context and clears state that belongs only to the
    /// previous window. Returns the previous window id for lifecycle events.
    pub fn begin_window(&self, window_id: impl Into<String>) -> String {
        let mut generation = self.generation.write();
        *generation = generation.saturating_add(1);
        let previous = std::mem::replace(&mut *self.window_id.write(), window_id.into());
        *self.usage.write() = None;
        *self.pending.lock() = None;
        self.reminder_fingerprints.write().clear();
        *self.request_overhead.write() = RequestOverhead::default();
        previous
    }
}

/// Truncates by Unicode scalar values rather than bytes, so the returned text
/// can always be passed to a provider as valid UTF-8.
pub fn truncate_utf8(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

/// Builds a bounded, provider-facing recovery handoff for the active window.
/// Required owner/tool anchors are atomic: if they do not fit, returning an
/// error lets the caller preserve the existing compaction/error path instead
/// of silently dropping continuation state.
pub fn build_auto_handoff_for_window(
    entries: &[WorkHistoryEntry],
    current_window_id: &str,
    max_chars: usize,
) -> Result<String, AutomaticHandoffError> {
    let max_chars = max_chars.min(MAX_HANDOFF_CHARS);
    if max_chars == 0 {
        return Err(AutomaticHandoffError::new(
            "automatic handoff has no safe character capacity",
        ));
    }

    let current_entries = entries
        .iter()
        .filter(|entry| entry.window_id == current_window_id)
        .collect::<Vec<_>>();
    let owner = current_entries
        .iter()
        .rev()
        .copied()
        .find(|entry| entry.kind == WorkHistoryKind::User)
        .or_else(|| {
            entries
                .iter()
                .rev()
                .find(|entry| entry.kind == WorkHistoryKind::User)
        })
        .ok_or_else(|| {
            AutomaticHandoffError::new("automatic handoff is missing its owner anchor")
        })?;

    let mut handoff = String::new();
    append_handoff_section(
        &mut handoff,
        "[Current owner input]\n",
        &format_anchor(owner),
        max_chars,
        true,
        "owner anchor",
    )?;

    if let Some(latest_direct_input) = entries
        .iter()
        .rev()
        .find(|entry| entry.kind == WorkHistoryKind::User && entry.entry_id != owner.entry_id)
    {
        append_handoff_section(
            &mut handoff,
            "\n[Latest direct input]\n",
            &format_anchor(latest_direct_input),
            max_chars,
            true,
            "latest direct input",
        )?;
    }

    if let Some(answer) = current_entries.iter().rev().find(|entry| {
        entry.kind == WorkHistoryKind::ToolResult
            && entry.payload.get("name").and_then(|value| value.as_str()) == Some("ask_user")
            && entry
                .payload
                .get("success")
                .and_then(|value| value.as_bool())
                == Some(true)
    }) {
        append_handoff_section(
            &mut handoff,
            "\n[Latest successful ask_user answer]\n",
            &format_anchor(answer),
            max_chars,
            true,
            "successful ask_user answer",
        )?;
    }

    let trailing_batch = trailing_unconsumed_tool_batch(&current_entries);
    if !trailing_batch.is_empty() {
        let mut batch = String::new();
        for entry in trailing_batch {
            if !batch.is_empty() {
                batch.push('\n');
            }
            batch.push_str(&format_recovery_entry(entry));
        }
        append_handoff_section(
            &mut handoff,
            "\n[Trailing unconsumed tool batch]\n",
            &batch,
            max_chars,
            true,
            "trailing tool batch",
        )?;
    }

    if let Some(previous) = entries.iter().rev().find(|entry| {
        entry.kind == WorkHistoryKind::ContextWindow
            && entry
                .payload
                .get("handoff")
                .and_then(|value| value.as_str())
                .is_some_and(|handoff| !handoff.trim().is_empty())
    }) {
        let stale = previous
            .payload
            .get("handoff")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let _ = append_handoff_section(
            &mut handoff,
            "\n[Previous checkpoint/handoff — possibly stale]\n",
            &bounded_recovery_text(stale),
            max_chars,
            false,
            "stale checkpoint",
        );
    }

    if handoff.is_empty() {
        return Err(AutomaticHandoffError::new(
            "automatic handoff did not contain a safe recovery anchor",
        ));
    }
    Ok(handoff)
}

pub fn build_auto_handoff(
    entries: &[WorkHistoryEntry],
    max_chars: usize,
) -> Result<String, AutomaticHandoffError> {
    let current_window_id = entries
        .iter()
        .rev()
        .map(|entry| entry.window_id.as_str())
        .find(|window_id| !window_id.is_empty())
        .unwrap_or("initial");
    build_auto_handoff_for_window(entries, current_window_id, max_chars)
}

fn append_handoff_section(
    handoff: &mut String,
    heading: &str,
    body: &str,
    max_chars: usize,
    required: bool,
    label: &str,
) -> Result<(), AutomaticHandoffError> {
    let section = format!("{heading}{body}");
    if handoff
        .chars()
        .count()
        .saturating_add(section.chars().count())
        <= max_chars
    {
        handoff.push_str(&section);
        return Ok(());
    }
    if required {
        return Err(AutomaticHandoffError::new(format!(
            "automatic handoff {label} does not fit within the safe limit"
        )));
    }
    Ok(())
}

fn bounded_recovery_text(text: &str) -> String {
    if text.chars().count() <= MAX_RECOVERY_RECORD_CHARS {
        text.to_owned()
    } else {
        format!(
            "{}\n[… recovery record truncated …]",
            truncate_utf8(text, MAX_RECOVERY_RECORD_CHARS.saturating_sub(32))
        )
    }
}

fn format_anchor(entry: &WorkHistoryEntry) -> String {
    let mut text = bounded_recovery_text(&entry.text);
    let images = summarize_images(entry);
    if !images.is_empty() {
        text.push(' ');
        text.push_str(&images);
        text.push_str(" [history entry id: ");
        text.push_str(&entry.entry_id);
        text.push(']');
    }
    text
}

fn trailing_unconsumed_tool_batch<'a>(
    entries: &'a [&'a WorkHistoryEntry],
) -> Vec<&'a WorkHistoryEntry> {
    let start = entries
        .iter()
        .rposition(|entry| {
            !matches!(
                &entry.kind,
                WorkHistoryKind::ToolCall | WorkHistoryKind::ToolResult
            )
        })
        .map(|index| index + 1)
        .unwrap_or(0);
    entries[start..]
        .iter()
        .copied()
        .filter(|entry| {
            matches!(
                &entry.kind,
                WorkHistoryKind::ToolCall | WorkHistoryKind::ToolResult
            )
        })
        .collect()
}

fn format_recovery_entry(entry: &WorkHistoryEntry) -> String {
    let body = match &entry.kind {
        WorkHistoryKind::ToolCall => {
            let name = entry
                .payload
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("tool");
            let arguments = entry
                .payload
                .get("arguments")
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string())
                })
                .unwrap_or_else(|| entry.text.clone());
            format!("{name}({})", bounded_recovery_text(&arguments))
        }
        WorkHistoryKind::ToolResult => bounded_recovery_text(&entry.text),
        _ => bounded_recovery_text(&entry.text),
    };
    let images = summarize_images(entry);
    if images.is_empty() {
        format!("- {body} [history entry id: {}]", entry.entry_id)
    } else {
        format!("- {body} {images} [history entry id: {}]", entry.entry_id)
    }
}

fn summarize_images(entry: &WorkHistoryEntry) -> String {
    if entry.images.is_empty() {
        return String::new();
    }
    let mut types = std::collections::BTreeMap::<&str, usize>::new();
    for image in &entry.images {
        *types.entry(image.media_type.as_str()).or_default() += 1;
    }
    let media = types
        .into_iter()
        .map(|(media_type, count)| format!("{count}×{media_type}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[images: {media}]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ContextUsage;
    use crate::storage::work::{WorkHistoryEntry, WorkHistoryKind};

    #[test]
    fn budget_uses_reserve_and_caps_the_reminder_band() {
        let budget = ContextBudget::new(400_000, 390_000, true);
        assert_eq!(budget.usable, 10_000);
        assert_eq!(budget.rollover_at, 10_001);
        assert!(budget.supported);
        assert_eq!(budget.remind_at(), 9_001);
    }

    #[test]
    fn disabled_compaction_keeps_explicit_support_but_disables_automatic_behavior() {
        let budget = ContextBudget::new(100_000, 16_384, false);
        assert!(budget.supported);
        assert!(!budget.automatic_enabled());
    }

    #[test]
    fn small_usable_windows_disable_automatic_behavior() {
        let budget = ContextBudget::new(32_000, 24_000, true);
        assert!(!budget.supported);
        assert!(!budget.automatic_enabled());
    }

    #[test]
    fn controller_tracks_usage_and_resets_it_for_a_new_window() {
        let controller = ContextController::for_test(100_000, 16_384);
        controller.record_usage(ContextUsage::from_usage(76_000, 500, 100_000));
        assert_eq!(controller.usage().unwrap().input_tokens, Some(76_000));

        controller
            .request_context(Some("continue the fix".into()))
            .unwrap();
        assert_eq!(
            controller.pending_request().unwrap().handoff.as_deref(),
            Some("continue the fix")
        );

        let previous = controller.begin_window("window-2");
        assert_eq!(previous, "initial");
        assert_eq!(controller.window_id(), "window-2");
        assert!(controller.usage().is_none());
        assert!(controller.pending_request().is_none());
    }

    #[test]
    fn known_usage_page_capacity_honors_the_reserved_tokens() {
        let controller = ContextController::for_test(100_000, 20_000);
        controller.record_usage(ContextUsage::from_usage(60_000, 0, 100_000));

        assert_eq!(
            controller.safe_page_chars(0, 0, 0, 0, 0).unwrap(),
            (20_000 - PAGE_MARGIN_TOKENS) / 4
        );
    }

    #[test]
    fn replacing_budget_fences_late_usage_from_the_previous_generation() {
        let controller = ContextController::for_test(100_000, 16_384);
        let previous_generation = controller.current_generation();
        controller.record_usage(ContextUsage::from_usage(50_000, 1, 100_000));

        controller.replace_budget(ContextBudget::new(200_000, 16_384, true));

        assert_eq!(controller.current_generation(), previous_generation + 1);
        assert!(controller.usage().is_none());
        assert!(!controller.record_usage_for_generation(
            previous_generation,
            ContextUsage::from_usage(75_000, 1, 100_000),
        ));
        assert!(controller.usage().is_none());
    }

    #[test]
    fn safe_page_chars_reserves_worst_case_utf8_bytes_per_character() {
        let controller = ContextController::for_test(100_000, 16_384);
        let page = controller.safe_page_chars(0, 0, 0, 0, 0).unwrap();
        let available_tokens = (100_000 - 16_384 - PAGE_MARGIN_TOKENS) / 2;

        assert_eq!(page, available_tokens / 4);
    }

    #[test]
    fn dynamic_fresh_capacity_accounts_for_tools_and_images() {
        let controller = ContextController::for_test(100_000, 16_384);
        let fresh = controller.safe_page_chars(0, 0, 0, 0, 0).unwrap();
        let loaded = controller
            .safe_page_chars(0, 8_000, 8_000, 8_000, 4)
            .unwrap();

        assert!(fresh > MIN_PAGE_CHARS);
        assert!(loaded < fresh);
    }

    #[test]
    fn request_overhead_snapshot_reduces_page_and_handoff_capacity() {
        let controller = ContextController::for_test(100_000, 16_384);
        let fresh_page = controller.safe_page_chars(0, 0, 0, 0, 0).unwrap();
        let fresh_handoff = controller.handoff_limit();

        let loaded_page = controller
            .safe_page_chars(0, 10_000, 10_000, 10_000, 0)
            .unwrap();
        let loaded_handoff = controller.handoff_limit();

        assert!(loaded_page < fresh_page);
        assert!(loaded_handoff < fresh_handoff);
    }

    #[test]
    fn budget_and_window_resets_clear_request_overhead() {
        let controller = ContextController::for_test(100_000, 16_384);
        controller.set_request_overhead(25_000, 25_000, 25_000, 0);
        controller
            .request_context(Some("pending handoff".into()))
            .unwrap();
        assert!(controller.handoff_limit() < MAX_HANDOFF_CHARS);

        controller.replace_budget(ContextBudget::new(100_000, 16_384, true));
        assert_eq!(
            controller.handoff_limit(),
            ContextController::for_test(100_000, 16_384).handoff_limit()
        );
        assert!(controller.pending_request().is_none());

        controller.set_request_overhead(25_000, 25_000, 25_000, 0);
        controller.begin_window("window-2");
        assert_eq!(
            controller.handoff_limit(),
            ContextController::for_test(100_000, 16_384).handoff_limit()
        );
    }

    #[test]
    fn usage_from_a_previous_generation_is_ignored() {
        let controller = ContextController::for_test(100_000, 16_384);
        let previous_generation = controller.current_generation();

        controller.begin_window("window-2");

        assert_eq!(controller.current_generation(), previous_generation + 1);
        assert!(!controller.record_usage_for_generation(
            previous_generation,
            ContextUsage::from_usage(50_000, 1, 100_000),
        ));
        assert!(controller.usage().is_none());
        assert!(controller.record_usage_for_generation(
            controller.current_generation(),
            ContextUsage::from_usage(25_000, 1, 100_000),
        ));
        assert_eq!(controller.usage().unwrap().input_tokens, Some(25_000));
    }

    #[test]
    fn handoff_limit_uses_current_capacity_below_the_fixed_maximum() {
        let controller = ContextController::for_test(100_000, 16_384);
        controller.record_usage(ContextUsage::from_usage(95_000, 0, 100_000));

        let limit = controller.handoff_limit();
        assert!(limit < MAX_HANDOFF_CHARS);
        assert!(controller
            .request_context(Some("x".repeat(limit + 1)))
            .is_err());
    }

    #[test]
    fn safe_page_limit_preserves_offsets_and_reports_unsupported_pages() {
        let controller = ContextController::for_test(10_000, 9_500);
        let error = controller.safe_page_chars(123, 0, 0, 0, 0).unwrap_err();
        assert!(error.contains("offset 123"));

        let limit = controller.handoff_limit();
        let unicode = "🙂".repeat(limit + 1);
        assert_eq!(unicode.chars().count(), limit + 1);
        assert!(controller.request_context(Some(unicode)).is_err());
        assert_eq!(truncate_utf8("a🙂b", 2), "a🙂");
    }

    #[test]
    fn reminder_fingerprint_is_scoped_to_the_active_window() {
        let controller = ContextController::for_test(100_000, 16_384);
        let fingerprint = controller.reminder_fingerprint("test-model");
        assert!(!controller.reminder_matches(&fingerprint));

        controller.mark_reminder(fingerprint.clone());
        assert!(controller.reminder_matches(&fingerprint));
        controller.begin_window("window-2");
        assert!(!controller.reminder_matches(&fingerprint));
    }

    #[test]
    fn automatic_reminder_is_claimed_once_per_window_budget_fingerprint() {
        let controller = ContextController::for_test(100_000, 16_384);
        controller.record_usage(ContextUsage::from_usage(78_000, 1, 100_000));

        let first = controller.claim_automatic_reminder("test-model");
        let second = controller.claim_automatic_reminder("test-model");

        assert!(first.is_some());
        assert!(second.is_none());
        assert_eq!(
            first.unwrap().fingerprint,
            controller.reminder_fingerprint("test-model")
        );
    }

    #[test]
    fn changing_model_allows_a_new_reminder_in_the_same_window() {
        let controller = ContextController::for_test(100_000, 16_384);
        controller.record_usage(ContextUsage::from_usage(78_000, 1, 100_000));

        let first = controller.claim_automatic_reminder("model-a").unwrap();
        assert!(controller.claim_automatic_reminder("model-a").is_none());
        let second = controller.claim_automatic_reminder("model-b");
        let third = controller.claim_automatic_reminder("model-a");

        assert!(second.is_some());
        assert!(third.is_none());
        assert_ne!(first.fingerprint, second.unwrap().fingerprint);
        assert!(first.fingerprint.contains("model-a"));
    }

    #[test]
    fn automatic_reminder_is_never_claimed_below_band_or_when_automatic_is_unsupported() {
        let below_band = ContextController::for_test(100_000, 16_384);
        below_band.record_usage(ContextUsage::from_usage(70_000, 1, 100_000));
        assert!(below_band.claim_automatic_reminder("test-model").is_none());

        let disabled = ContextController::new(ContextBudget::new(100_000, 16_384, false));
        disabled.record_usage(ContextUsage::from_usage(78_000, 1, 100_000));
        assert!(disabled.claim_automatic_reminder("test-model").is_none());

        let unsupported = ContextController::new(ContextBudget::new(32_000, 24_000, true));
        unsupported.record_usage(ContextUsage::from_usage(8_500, 1, 32_000));
        assert!(unsupported.claim_automatic_reminder("test-model").is_none());
    }

    #[test]
    fn automatic_handoff_preserves_owner_anchor_and_trailing_unconsumed_tool_results() {
        let entries = vec![
            WorkHistoryEntry::user("window-1", "user-1", "run the checks"),
            WorkHistoryEntry {
                entry_id: "call-1".into(),
                parent_id: None,
                thread_id: None,
                window_id: "window-1".into(),
                sequence: 2,
                timestamp: 2,
                kind: WorkHistoryKind::ToolCall,
                text: "bash({\"command\":\"cargo test\"})".into(),
                payload: serde_json::json!({
                    "call_id": "call-1",
                    "name": "bash",
                    "arguments": "{\"command\":\"cargo test\"}"
                }),
                images: Vec::new(),
            },
            WorkHistoryEntry {
                entry_id: "result-1".into(),
                parent_id: None,
                thread_id: None,
                window_id: "window-1".into(),
                sequence: 3,
                timestamp: 3,
                kind: WorkHistoryKind::ToolResult,
                text: "cargo test failed".into(),
                payload: serde_json::json!({
                    "call_id": "call-1",
                    "name": "bash",
                    "success": false
                }),
                images: Vec::new(),
            },
        ];

        let handoff = build_auto_handoff_for_window(&entries, "window-1", 20_000).unwrap();
        assert!(handoff.contains("run the checks"));
        assert!(handoff.contains("cargo test failed"));
        assert!(handoff.contains("cargo test"));
        assert!(handoff.chars().count() <= 20_000);
    }

    #[test]
    fn automatic_handoff_rejects_required_anchors_that_do_not_fit() {
        let entries = vec![WorkHistoryEntry::user(
            "window-1",
            "user-1",
            "user input that cannot fit safely",
        )];

        let error = build_auto_handoff_for_window(&entries, "window-1", 8).unwrap_err();
        assert!(error.to_string().contains("anchor"));
    }

    #[test]
    fn automatic_handoff_summarizes_images_without_copying_payloads() {
        let mut entry = WorkHistoryEntry::user("window-1", "image-entry", "inspect this image");
        entry.images.push(crate::provider::types::ImageSource {
            media_type: "image/png".into(),
            data: "base64-image-data".into(),
        });

        let handoff = build_auto_handoff_for_window(&[entry], "window-1", 20_000).unwrap();
        assert!(handoff.contains("1×image/png"));
        assert!(handoff.contains("image-entry"));
        assert!(!handoff.contains("base64-image-data"));
    }
}
