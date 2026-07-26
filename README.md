# Averroes

High-performance AI harness for building code assistant applications in Rust.

**Design principles:**
- **Provider-agnostic** — trait-based abstraction over LLM providers (Anthropic, OpenAI, +)
- **Resource-optimal** — Tokio async + Rayon thread pool, single HTTP/2 connection pool, resource governor
- **Extensible** — trait-based core tools, dynamic tool registration, markdown-based skills
- **Hierarchical agents** — main agent spawns sub-agents with scoped tasks and tools

## Packages

| Crate | Description |
|-------|-------------|
| `averroes-core` | Shared harness — provider trait, agent runtime, tool system, skill loader, compaction, resource governor |
| `averroes-gpui` | Desktop frontend (GPUI) |

## Quickstart

```bash
# Build
cargo build --workspace

# Run tests
cargo test --workspace

# GPUI desktop workspace
cargo run -p averroes-gpui
```

## Architecture

```
User Input → GPUI → averroes-core::Agent::run()
  → Skill resolution (indexed markdown skills)
  → Provider::chat() (streaming)
  → Tool calls loop (agent decides which tools to invoke)
  → Compaction (context window management)
  → Sub-agent spawning (hierarchical delegation)
  → Result streamed back to frontend
```

## Configuration

Shared configuration at `~/.config/averroes/config.toml`:

```toml
[provider]
default = "anthropic"

[provider.anthropic]
api_key_env = "ANTHROPIC_API_KEY"
default_model = "claude-sonnet-4-20250514"

[provider.openai]
api_key_env = "OPENAI_API_KEY"
default_model = "gpt-4o"

[runtime]
max_concurrent_calls = 10
token_budget_per_minute = 200000

[compaction]
strategy = "hybrid"
threshold = 0.8

[skills]
paths = ["./skills", "~/.config/averroes/skills"]
```

## Skills

Skills are markdown files stored under `skills/` directories. They are indexed at startup and loaded on demand by the agent:

```markdown
# Example Skill

Description of what this skill does.

## Triggers
- keyword one
- keyword two
```

The agent uses `list_skills` and `load_skill` tools to discover and load relevant skills while keeping context lean.

## GPUI Workspace

The desktop frontend is a native GPUI application with a light TokenFactory-inspired visual system:

- Closable session tabs with `+` new-session behavior.
- A per-session composer with `+`, `Build`, provider/model, `Max`, and `Send` controls.
- Shared setup and settings for provider, model, and API-key environment variables.
- Runtime status and keyboard shortcuts: `cmd-n`, `cmd-w`, `cmd-l`, `cmd-enter`, and `cmd-,`.

The GPUI frontend and core runtime use the same configuration file; no GPUI-specific provider configuration is created.

## Builtin Tools

`bash`, `file_read`, `file_write`, `glob`, `grep`, `web_fetch`, `list_skills`, `load_skill` — plus dynamic tool registration and MCP client stub.

## License

MIT — see [LICENSE](LICENSE).
