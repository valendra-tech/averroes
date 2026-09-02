# Compact Web Tools Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace browser-backed `web_fetch` with a bounded 60-second direct HTTP reader and add one compact, stateful `browser` tool for interactive work.

**Architecture:** `web_fetch` owns an independent Reqwest client and parses fetched HTML through `oxibrowser_core::Page` without executing JavaScript. `browser` is a single action-dispatch tool backed by OxiBrowser `Tab` instances stored by conversation ID with an eight-entry LRU cap. Only page-open and inspect actions return bounded page content; interaction actions return concise state.

**Tech Stack:** Rust, Tokio, Reqwest, OxiBrowser Core, Serde JSON, GPUI localization.

---

### Task 1: Lock the direct-fetch contract

**Files:**
- Modify: `crates/core/src/tool/builtin/web_fetch.rs`
- Modify: `crates/core/src/tool/builtin/web_browser.rs`

- [ ] **Step 1: Write failing tests**

Add tests asserting a 60-second `FETCH_TIMEOUT`, a 60-second `PAGE_OPEN_TIMEOUT`, textual/binary content classification, UTF-8-safe output truncation, and HTTP metadata using `transport = "http"`.

- [ ] **Step 2: Run the tests and verify RED**

Run `cargo test -p averroes-core tool::builtin::web_fetch::tests tool::builtin::web_browser::tests::page_script_limits_stay_within_the_safe_boa_range` and confirm failures reference the old browser metadata and 10-second timeout.

- [ ] **Step 3: Implement direct HTTP fetching**

Build `WebFetchTool` around Reqwest. Validate HTTP(S), apply one 60-second deadline, follow redirects, cap downloaded bytes, reject binary content, decode HTML with `oxibrowser_core::encoding::decode_html`, parse it with `oxibrowser_core::page::Page::from_html`, and cap model-facing output before creating `ToolResult`.

- [ ] **Step 4: Verify GREEN**

Run the tests from Step 2 and confirm they pass.

### Task 2: Add the compact stateful browser tool

**Files:**
- Create: `crates/core/src/tool/builtin/browser.rs`
- Modify: `crates/core/src/tool/builtin/web_browser.rs`
- Modify: `crates/core/src/tool/builtin/mod.rs`

- [ ] **Step 1: Write failing contract tests**

Add tests for the single flat action schema, automatic session-key reuse, `e1` reference conversion, validation of action-specific arguments, compact action output, and LRU victim selection.

- [ ] **Step 2: Run the tests and verify RED**

Run `cargo test -p averroes-core tool::builtin::browser` and confirm the tool and helpers do not yet exist.

- [ ] **Step 3: Implement session and action dispatch**

Create `BrowserTool` with an OxiBrowser runtime and `tokio::sync::Mutex<HashMap<String, BrowserSession>>`. Resolve each session from `ToolContext.session_id`, reuse its tab, evict and close the least-recently-used tab above eight entries, and implement `open`, `inspect`, `click`, `fill`, `type`, `press`, `select`, `check`, `uncheck`, `scroll`, `wait`, `back`, `forward`, `reload`, and `close`.

- [ ] **Step 4: Implement compact inspection**

Use one browser-side expression to annotate at most 30 interactive elements with `data-averroes-ref`, return short `eN` references, and format a page snapshot capped independently from the global tool-output guard. Resolve `eN` to its data selector while accepting explicit CSS selectors unchanged.

- [ ] **Step 5: Register and verify GREEN**

Register one `browser` tool in `register_all`, assert both `web_fetch` and `browser` are discoverable, and run `cargo test -p averroes-core tool::builtin::browser tool::builtin::tests`.

### Task 3: Prioritize direct fetch and update presentation

**Files:**
- Modify: `crates/core/src/prompt/templates/system.md`
- Modify: `crates/core/src/prompt/mod.rs`
- Modify: `crates/core/src/agent/tools.rs`
- Modify: `crates/gpui/src/app.rs`
- Modify: `crates/gpui/src/ui/tool_icon.rs`
- Modify: `crates/gpui/locales/en.json`
- Modify: `crates/gpui/locales/es.json`

- [ ] **Step 1: Write failing policy and presentation tests**

Assert the system prompt says to try `web_fetch` before `browser`, browser is excluded from parallel web execution, and the browser icon resolves to the web icon family.

- [ ] **Step 2: Run the tests and verify RED**

Run the targeted prompt, agent-tools, and GPUI icon tests and confirm the new policy assertion fails.

- [ ] **Step 3: Update policy, labels, and sources**

Describe `web_fetch` as direct HTTP and `browser` as interactive. Add localized browser labels, source extraction for browser metadata, and keep only `web_fetch` plus web search in the parallel read-only path.

- [ ] **Step 4: Verify GREEN**

Run the targeted tests and confirm all pass.

### Task 4: Regression verification

**Files:**
- Verify all modified files.

- [ ] **Step 1: Format and inspect**

Run `cargo fmt --all -- --check` and `git diff --check`.

- [ ] **Step 2: Run crate suites**

Run `cargo test -p averroes-core` and `cargo test -p averroes-gpui`.

- [ ] **Step 3: Check the workspace**

Run `cargo check --workspace` and review the final diff for unrelated changes or unbounded browser output.
