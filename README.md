# Averroes

Averroes is a local-first AI workspace and agent harness written in Rust. It
combines a high-performance core with a native GPUI desktop application for
conversations, coding workspaces, provider integrations, tools, skills, and
delegated agents.

The project is named after Ibn Rushd (Averroes), the Andalusian philosopher,
physician, and jurist from Córdoba whose work connected rigorous reasoning with
practical knowledge. Averroes is developed by [Valendra.tech](https://valendra.tech)
with care.

## Highlights

- Streaming conversations with provider-independent agent orchestration.
- A central model registry with provider hooks, live catalog refresh, and
  manually registered models for providers without a model-list endpoint.
- Multiple connections to the same provider, with credentials kept outside the
  conversation data.
- Workspace-aware `AGENTS.md` instructions and on-demand Markdown skills.
- Built-in tools for files, shell sessions, web research, tasks, memory,
  checkpoints, questions, and delegated agents.
- Global memory for durable user-approved facts and deep memory for searching
  older conversation context through the local embedding index.
- SQLite persistence for conversations, messages, usage, sources, tool events,
  tasks, checkpoints, and embedding metadata.
- Live tool and reasoning activity in the UI, grouped without hiding the
  underlying event order.
- macOS update checks and release DMGs generated automatically from GitHub
  Releases.

## Repository layout

| Path | Responsibility |
| --- | --- |
| `crates/core` | Provider, model, agent, tool, skill, memory, compaction, storage, and task domains |
| `crates/gpui` | Native desktop application, settings, conversation UI, diagnostics, and updates |
| `crates/vendor` | Small local patches for dependencies used by the browser/rendering stack |
| `scripts/bundle-macos.sh` | Builds the release binary, `.app`, and drag-to-Applications DMG |
| `.github/workflows` | macOS bundle and GitHub Release automation |
| `assets` | Application and brand assets |

## Supported providers

Providers are selected and configured from the desktop Settings screen. The
model picker is populated from the connection's catalog; provider defaults are
not silently inserted.

| Provider | Authentication / discovery |
| --- | --- |
| Codex | ChatGPT/Codex sign-in flow and authenticated account catalog |
| GitHub Copilot | GitHub authentication and authenticated Copilot catalog |
| QDivZero | API token and `/serving-endpoints` catalog for running chat workloads |
| OpenAI | API token and OpenAI-compatible `/v1/models` catalog |
| Anthropic | API token and Anthropic provider integration |
| DeepSeek | API token and OpenAI-compatible API |
| Groq | API token and OpenAI-compatible API |
| Ollama | Local Ollama server; models can be refreshed or added manually |
| Ollama Cloud | API token and Ollama Cloud's OpenAI-compatible API |
| Compatible API | User-provided OpenAI-compatible base URL and API token |

Connections that expose embeddings can also be selected for conversation
search. Models with embedding capability are discovered from the live catalog;
manual model metadata can declare `embeddings = true` when required.

## Requirements

- Rust stable with Cargo.
- macOS for the `averroes-gpui` desktop application and DMG packaging.
- A configured provider connection for model requests. Ollama can be used
  locally without an API key.

## Development

```bash
# Build every workspace crate
cargo build --workspace

# Run the test suite
cargo test --workspace

# Format and lint-style checks used by the project
cargo fmt --all -- --check
git diff --check

# Start the desktop application
cargo run -p averroes-gpui
```

Credentials are entered in the application and stored using the platform
Keychain-backed vault where available. Do not commit API keys, provider tokens,
or generated session data.

## Configuration and storage

The shared configuration is created under `~/.averroes/config`:

- `settings.toml` stores connection metadata, agent profiles, runtime settings,
  compaction settings, and skill paths.
- `providers.enc` stores encrypted provider credentials.
- `averroes.db` is a private SQLite database containing persistent work data.

Workspace-specific session and tab state lives under the active workspace's
`.averroes/` directory. The UI is the recommended way to edit configuration so
that credentials, model catalogs, and connection metadata remain consistent.

## Tools

The registry is extensible and the agent discovers the complete registered
catalog before enabling tools for a task. The built-in groups are:

- Workspace: `bash`, `file_read`, `file_write`, `glob`, `grep`, `checkpoint`.
- Web: `web_search`, `web_fetch` (the latter uses OxiBrowser and contributes
  opened pages to conversation sources).
- Discovery: `discover_tools`, `enable_tools`, `list_tools`.
- Skills: `list_skills`, `load_skill`.
- Tasks: `task_list`, `add_task`, `mark_task_as_done`.
- Memory: `create_global_memory`, `delete_global_memory`,
  `search_deep_memory`, `get_deep_memory`.
- Agents and interaction: `list_agents`, `call_agents`, `ask_user`.

Delegated agents receive a focused objective, an explicit connection/model
binding, and a stable thread identifier so their complete read-only execution
can be inspected from the parent conversation. A delegated agent cannot spawn
another delegated agent.

## Skills and workspace instructions

Before working in a project, Averroes loads applicable `AGENTS.md` instructions
from the active workspace. Skills are discovered from workspace-local
locations, including:

```text
.averroes/skills/
.agents/skills/
.codex/skills/
.claude/skills/
skills/
```

Skills are indexed at startup and loaded on demand, keeping the active prompt
small while allowing the agent to use project-specific workflows.

## Memory, search, and compaction

The conversation index is compiled with the selected embedding connection and
stored in SQLite. Indexing can continue in the background while the application
is idle, and semantic retrieval is used for deep-memory queries and relevant
conversation context. If the optional `sqlite-vector-rs` extension is not
available, the same SQLite-backed index uses the built-in search path.

Context compaction is internal runtime behavior. It uses provider-reported
usage, preserves important objective and decision context, and is surfaced in
the conversation when it occurs; there is no compaction tool for the agent to
invoke.

## macOS releases

The tag-triggered release pipeline has `validate`, `build`, and `publish` jobs.
A pushed valid SemVer tag `vX.Y.Z` on a commit reachable from `main` starts the
pipeline. Publishing a GitHub Release manually does not start it.

The pipeline builds arm64 and x86_64 DMGs. Both are Developer ID signed with
the hardened runtime and secure timestamps, notarized, stapled, and Gatekeeper
validated before publishing.

A public GitHub Release is created only after both architecture builds succeed.
A failure creates no public Release, although a failure after draft creation can
leave a draft Release for maintainers.

Each release includes `SHA256SUMS.txt`. From the directory containing the
downloaded assets, verify them with:

```bash
shasum -a 256 -c SHA256SUMS.txt
```

macOS 13.0 is the supported runtime minimum.

Create a release tag from current `main` with:

```bash
git checkout main
git pull --ff-only
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin vX.Y.Z
```

GitHub Actions requires these secrets:

- `APPLE_CERTIFICATE_BASE64`
- `APPLE_CERTIFICATE_PASSWORD`
- `APPLE_CODESIGN_IDENTITY`
- `APPLE_API_KEY_BASE64`
- `APPLE_API_KEY_ID`
- `APPLE_API_ISSUER_ID`

Retain the legacy `APPLE_ID`, `APPLE_TEAM_ID`, and
`APPLE_APP_SPECIFIC_PASSWORD` secrets only until the first API-key release
succeeds, then remove them.

The application checks for newer releases at startup. When an update is
available, the user can download and open the verified DMG from the update
dialog. The current version is shown in Settings → About and in the sidebar.

To build a DMG locally on macOS:

```bash
VERSION=1.2.3 scripts/bundle-macos.sh
```

The script requires `cargo`, `hdiutil`, `plutil`, `ditto`, `vtool`, and `lipo`.
Packaging verifies the macOS 13.0 target and binary architecture.
`CODESIGN_IDENTITY` is optional, and `CODESIGN_KEYCHAIN` can select an explicit
isolated signing keychain. Local packaging does not notarize DMGs.

## License

MIT — see [LICENSE](LICENSE).
