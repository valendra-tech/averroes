# Compact Web Tools Design

## Goal

Make ordinary page reads fast and inexpensive while retaining a stateful browser for JavaScript, clicks, forms, and other interactive workflows.

## Public tool contract

The web surface consists of two tools:

- `web_fetch` performs a direct HTTP GET. It does not execute JavaScript or create a browser session. Its schema remains intentionally small: a required HTTP(S) `url` only.
- `browser` owns one automatic interactive tab per agent conversation. It exposes a single flat schema with an `action` discriminator instead of registering one tool per browser operation.

`browser` supports `open`, `inspect`, `click`, `fill`, `type`, `press`, `select`, `check`, `uncheck`, `scroll`, `wait`, `back`, `forward`, `reload`, and `close`. Relevant optional fields include `url`, `target`, `value`, `key`, and scroll offsets. A target may be a CSS selector or a short element reference returned by `open` or `inspect`.

The system prompt and tool descriptions direct agents to use `web_fetch` first. They should enable `browser` only when a page requires JavaScript, cookies, navigation state, or interaction.

## Direct HTTP fetch

`web_fetch` uses the existing Reqwest dependency with redirects and decompression enabled. The complete request, body stream, and formatting operation share a 60-second deadline. Only HTTP(S) URLs are accepted.

Text, JSON, XML, and HTML responses are returned as readable text with URL, final URL, status, and content type metadata. HTML is normalized into compact readable content without launching OxiBrowser. Binary content is rejected with a useful error. Downloaded bytes and model-facing output are bounded independently so a large response cannot consume the conversation context.

Non-success HTTP statuses return an unsuccessful tool result while retaining a bounded response excerpt and metadata for diagnosis.

## Interactive browser

The browser implementation reuses OxiBrowser's `Tab` API. A synchronized store maps `ToolContext.session_id` to a tab state containing the tab, recent-use counter, current page summary, and current element-reference map. Parent conversations and delegated agent threads therefore receive separate automatic sessions.

The store allows at most eight live tabs. Creating a ninth tab closes and removes the least recently used tab. `close` removes the current conversation's tab explicitly. Browser actions execute sequentially; unlike independent HTTP fetches, they are never included in the parallel-web-tool path.

`open` creates the tab lazily and navigates it. `inspect` reads the live DOM and returns a bounded text snapshot plus at most 30 useful interactive elements. Each element receives a short reference such as `e1`; the state maps that reference to a generated CSS selector until the next inspection. Actions accepting `target` resolve either these references or a caller-provided CSS selector.

Navigation and waits are bounded to 60 seconds. Action errors name the action and target and keep the tab available for recovery unless it was explicitly closed or evicted.

## Context budget

`web_fetch` and browser page snapshots are capped before entering agent history. `open` and `inspect` are the only browser actions that return page text and interactive controls. Mutating actions return a concise confirmation with current URL and title, so repeated form filling does not duplicate the page body. The existing global live-tool-output bound remains a final safety net.

The tool catalog gains only one additional schema (`browser`). Existing persisted `web_fetch` activations remain valid, but now resolve to the preferred direct HTTP implementation.

## Compatibility and presentation

`web_fetch` keeps its current public name and remains read-only. Its metadata identifies the transport as direct HTTP instead of OxiBrowser. The new `browser` tool is not marked read-only because clicks and form submissions may change remote state.

Tool labels and icons use the existing web icon family. Source extraction continues to receive final URL, title when available, favicon when discoverable, and status metadata from both paths.

## Verification

Tests cover the 60-second deadline, URL and content-type validation, bounded direct-fetch output, tool registration, automatic session keys, LRU eviction, element-reference resolution, compact interaction results, and sequential browser classification. Existing browser safety, web source, compaction, and workspace tests must remain green.
