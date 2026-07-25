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
| `averroes-cli` | Terminal frontend (ratatui + crossterm) |
| `averroes-gpui` | Desktop frontend (GPUI) |

## Quickstart

```bash
# Build
cargo build --workspace

# Run tests
cargo test --workspace

# CLI
cargo run -p averroes-cli -- --help
```

## Architecture

```
User Input → [CLI | GPUI] → averroes-core::Agent::run()
  → Skill resolution (indexed markdown skills)
  → Provider::chat() (streaming)
  → Tool calls loop (agent decides which tools to invoke)
  → Compaction (context window management)
  → Sub-agent spawning (hierarchical delegation)
  → Result streamed back to frontend
```

## Configuration

CLI config at `~/.config/averroes/config.toml`:

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

## Builtin Tools

`bash`, `file_read`, `file_write`, `glob`, `grep`, `web_fetch`, `list_skills`, `load_skill` — plus dynamic tool registration and MCP client stub.

## License

MIT — see [LICENSE](LICENSE).
