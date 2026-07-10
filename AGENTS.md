# AGENTS Instructions

gooseherd is a CLI-first multi-model orchestration harness (a fork of goose): an
expensive frontier model plans and reviews, cheaper models implement, and machine
gates verify — over ACP-driven vendor CLIs (claude-acp, codex-acp, any agent via
one config entry) and native OpenAI/Anthropic-compatible APIs. Desktop app, web
UI, and hosted services are non-goals.

For every config knob, the exit-code contracts, and the `.goose-gates.yaml`
spec, see [docs/reference.md](docs/reference.md). The architecture and the
decisions behind the current layout are recorded in
[docs/overhaul-2026-07.md](docs/overhaul-2026-07.md).

## Crate map

```
crates/
├── goose               # core: agents, providers, ACP client, sessions, extensions
├── goose-cli           # CLI entry, orchestrate/arena/goal/loop, TUI rendering
├── goose-providers     # provider implementations
├── goose-provider-types# shared provider/model types + canonical model registry
├── goose-mcp           # MCP extensions — memory only after the trim
├── goose-test          # test tooling (MCP replay recorder)
├── goose-test-support  # shared test helpers
└── goose-sdk-types     # shared serializable types
```

## Key entry points

- `crates/goose-cli/src/cli.rs` — CLI argument surface and command dispatch.
- `crates/goose-cli/src/session/orchestrate/` — the plan → implement → review loop:
  `runner` (drives a run), `phases`, `planner`, `roles`, `gates` (machine gates),
  `workspace` (per-run git worktree).
- `crates/goose-cli/src/session/{arena,goal,looping,exemplars}.rs` — blind
  multi-model arena, goal loop, `/loop`, and exemplar hill-climbing.
- `crates/goose/src/acp/provider.rs` — the ACP client (product core) plus the
  orch implement permission policy.
- `crates/goose/src/agents/agent.rs` — the agent loop.

## Build / test / lint

Plain cargo, no hermit. `just` wraps the common tasks (run `just` to list them).

```bash
cargo build                                                        # just build
cargo build --release -p goose-cli                                 # just release
cargo test                                                         # just test
cargo fmt                                                          # just fmt
cargo clippy --workspace --all-targets -- -D warnings # just lint
```

`just install` builds the release binary and copies it to `~/.local/bin/goose`.
Run build/test/clippy only when asked to verify a change.

## Machine gates

Configured quality gates run before the reviewer is called and bounce mechanical
failures straight back to the implementer. `.goose-gates.yaml` at the repo root
takes priority; safe `package.json`/`go.mod` gates are derived next; the
`GOOSE_ORCH_GATES` env var is the global fallback.

## Code Quality

- Comments: write self-documenting code; prefer clear names over comments. Never
  restate what the code does. Comment only complex algorithms, non-obvious
  business logic, or "why" — never "what". No comments on self-evident operations,
  getters/setters, constructors, or standard Rust idioms.
- Simplicity: don't make things optional that needn't be — let the compiler
  enforce. Booleans default to false, not optional. Avoid overly defensive code;
  trust Rust's type system.
- Errors: use `anyhow::Result`. Don't add context that restates the error
  (`.context("Failed to X")` when the error already says it failed).
- Logging: clean up stray logs; add them only for errors or security events.

## Never

- Never skip `cargo fmt`.
- Never merge without running clippy.
- For human-authored dependency changes, use `cargo add` rather than editing
  `Cargo.toml` by hand (automated bump PRs are exempt); keep `Cargo.lock`
  consistent.
