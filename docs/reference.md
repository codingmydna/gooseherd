# gooseherd reference

Configuration knobs, exit codes, the gates spec, and the adapter catalog. Every
knob here is read by the current code; defaults are the code-level defaults
(`goose herd` and `goose configure` may write explicit values that differ).

Config lives in `~/.config/goose/config.yaml`. Every `GOOSE_*` key below can be
set there or as an environment variable of the same name; the environment wins.

## Roles & models

The orchestration roles resolve through a fallback chain. `GOOSE_PROVIDER` /
`GOOSE_MODEL` are the session default; each role overrides it and falls back to
it (reviewer falls back to planner; judge falls back to reviewer → planner →
default).

| Knob | Default | Description |
|---|---|---|
| `GOOSE_PROVIDER` | required | Session/default provider name |
| `GOOSE_MODEL` | required | Session/default model name |
| `GOOSE_PLANNER_PROVIDER` / `GOOSE_PLANNER_MODEL` / `GOOSE_PLANNER_EFFORT` | session default | Planner role |
| `GOOSE_IMPLEMENTER_PROVIDER` / `GOOSE_IMPLEMENTER_MODEL` / `GOOSE_IMPLEMENTER_EFFORT` | session default | Implementer role |
| `GOOSE_REVIEWER_PROVIDER` / `GOOSE_REVIEWER_MODEL` / `GOOSE_REVIEWER_EFFORT` | planner → default | Reviewer role |
| `GOOSE_JUDGE_PROVIDER` / `GOOSE_JUDGE_MODEL` / `GOOSE_JUDGE_EFFORT` | reviewer → planner → default | Arena judge / goal evaluator |
| `GOOSE_EVALUATOR_PROVIDER` / `GOOSE_EVALUATOR_MODEL` / `GOOSE_EVALUATOR_EFFORT` | judge chain | `goose goal` evaluator (when no `--check`) |
| `GOOSE_ROLE_PRESETS` | unset | Saved role-config presets (`/preset` manages these) |
| `GOOSE_ACTIVE_PRESET` | unset | Currently active role preset |
| `GOOSE_FAST_MODEL` | provider's fast model | Model for lightweight tasks (session naming, compaction) |
| `GOOSE_MODE` | `auto` | Tool-approval mode: `auto`, `approve`, `smart_approve`, `chat` |
| `GOOSE_TEMPERATURE` | provider default | Sampling temperature |
| `GOOSE_MAX_TOKENS` | provider default | Max output tokens |
| `GOOSE_CONTEXT_LIMIT` | model default | Override the context-window size |
| `GOOSE_THINKING_EFFORT` | unset | Default reasoning effort |
| `GOOSE_MAX_TURNS` | 1000 | Max agent turns per response |
| `GOOSE_SUBAGENT_PROVIDER` / `GOOSE_SUBAGENT_MODEL` | session default | Provider/model for summoned subagents |
| `GOOSE_SUBAGENT_MAX_TURNS` | 25 | Max turns for a summoned subagent |
| `GOOSE_ACP_AGENTS` | unset | Map of custom ACP agent CLIs (see the adapter catalog) |

## Orchestration (`/orch`)

| Knob | Default | Description |
|---|---|---|
| `GOOSE_ORCH_MAX_CYCLES` | 3 | Max implement → review cycles |
| `GOOSE_ORCH_ASK` | on | Planner question rounds; `off`/`false`/`0` disables |
| `GOOSE_ORCH_MAX_QUESTIONS` | 2 | Max planner question rounds |
| `GOOSE_ORCH_MIN_PLAN_CHARS` | 3000 | Minimum plan length before it is accepted |
| `GOOSE_ORCH_PHASE_IDLE_TIMEOUT_SECS` | 600 | Per-phase idle timeout |
| `GOOSE_ORCH_PROGRESS_SECS` | 60 | Progress heartbeat cadence |
| `GOOSE_ORCH_PLAYBOOK` | `auto` | Playbook injection: `auto`, `always`, `never` |
| `GOOSE_PLAYBOOK_PATH` | embedded playbook | External playbook file override |
| `GOOSE_ORCH_AUTO_MERGE` | false | Merge the approved worktree branch automatically |
| `GOOSE_ORCH_IN_PLACE` | false | Run in the working directory instead of a worktree |
| `GOOSE_ORCH_LINK_ENV` | true | Symlink `.env*` into the orch worktree |
| `GOOSE_ORCH_IMPLEMENT_POLICY` | `auto` (interactive) / `allowlist` (headless) | ACP implementer permission policy |
| `GOOSE_ORCH_ALLOWED_COMMANDS` | seeded from repo | Implementer command allowlist (allowlist policy) |
| `GOOSE_REPO_PACK` | `auto` | Repo-orientation block injection: `auto`, `always`, `never` |
| `GOOSE_PLAN_ALLOW_EXEC` | false | Allow shell during the read-only plan phase |
| `GOOSE_ACP_PLAN_EXPLORE` | false | Read-only ACP explore sandbox for plan/eval roles |

## Machine gates

Gates run after each implement phase, before the reviewer is called. A failing
gate re-dispatches the implementer (never spends reviewer tokens).

| Knob | Default | Description |
|---|---|---|
| `GOOSE_ORCH_GATES` | unset | Global fallback gate commands (list) |
| `GOOSE_ORCH_GATE_TIMEOUT_SECS` | 3600 | Per-gate timeout (a cold worktree build can exceed 15 min; raise if a real gate legitimately needs longer) |
| `GOOSE_ORCH_GATE_ENV` | `scrub` | `scrub` removes credential env vars; `inherit` passes them through |
| `GOOSE_ORCH_MAX_GATE_RETRIES` | 2 | Reimplement retries on gate failure before aborting |

### `.goose-gates.yaml`

A YAML list of shell command strings at the repo root:

```yaml
- cargo fmt --check
- cargo clippy --workspace --all-targets -- -D warnings
- cargo test -p goose-cli --lib
```

- Resolution order: repo-root `.goose-gates.yaml` → gates derived from
  `package.json` (test/build scripts) or `go.mod` → `GOOSE_ORCH_GATES`. Cargo
  repos are **not** auto-derived — commit a `.goose-gates.yaml` (`goose herd`
  offers a starter).
- An empty list (`[]`) is an explicit opt-out of gates for that repo.
- A gate that names a build tool whose manifest is absent from the target repo
  (a `cargo` gate in a repo with no `Cargo.toml`) is skipped, not failed.
- Commands with shell metacharacters (`| & ; < > ( ) $ \``) run under `sh -c`;
  others run directly.

## Exemplars & uplift

| Knob | Default | Description |
|---|---|---|
| `GOOSE_PLAN_EXEMPLARS` | true | Enable the plan exemplar store |
| `GOOSE_PLAN_EXEMPLARS_INJECT` | `auto` | Plan exemplar injection: `auto`, `always`, `never` |
| `GOOSE_REVIEW_EXEMPLARS` | true | Enable the review exemplar store |
| `GOOSE_REVIEW_EXEMPLARS_INJECT` | `auto` | Review exemplar injection: `auto`, `always`, `never` |
| `GOOSE_IMPL_FAILURE_MODES` | `auto` | Inject distilled REVISE failure modes to the implementer |
| `GOOSE_UPLIFT_FRONTIER_PATTERNS` | `fable` | Model-name substrings treated as frontier (uplift skipped) |

`auto` injects playbook/exemplars only into roles whose serving model is not a
frontier model (per `GOOSE_UPLIFT_FRONTIER_PATTERNS`); `always` forces injection
even for frontier models; `never` disables it.

## Arena / goal / loop

| Knob | Default | Description |
|---|---|---|
| `GOOSE_ARENA_LINEUP` | `codex-acp/gpt-5.5` | Default arena contestant lineup |
| `GOOSE_ARENA_TIMEOUT_SECS` | 900 | Per-contestant timeout |
| `GOOSE_GOAL_MAX_ATTEMPTS` | 5 | `goose goal` / `/goal` max attempts |
| `GOOSE_LIVE_INPUT` | on (unix tty) | Live stdin steering + bare-Esc interrupt while a turn streams; set `false` to disable |

## UX, rendering & misc

| Knob | Default | Description |
|---|---|---|
| `GOOSE_STATUS_HOOK` | unset | Shell command run on each status change, receiving the status word as its argument (`thinking` when a turn starts, `waiting` when it returns to the prompt). Runs detached; stdout/stderr are discarded. Use it for desktop notifications or tmux/status-bar integration. |
| `GOOSE_BELL` | true | Ring the terminal bell (BEL) when a turn that ran longer than 10s finishes on a tty |
| `GOOSE_CLI_THEME` | `ansi` | Syntax-highlight theme |
| `GOOSE_CLI_DARK_THEME` / `GOOSE_CLI_LIGHT_THEME` | built-in | Theme names |
| `GOOSE_CLI_SHOW_COST` | false | Show token cost in the status line |
| `GOOSE_CLI_SHOW_THINKING` | true | Render model thinking blocks |
| `GOOSE_CLI_NEWLINE_KEY` | `j` | Modifier-key char for newline insert |
| `GOOSE_SHOW_FULL_OUTPUT` | false | Show full (untruncated) tool output |
| `GOOSE_NO_CODE_TRUNCATION` | false | Disable code-block truncation |
| `GOOSE_MAX_CODE_BLOCK_LINES` | 50 | Max lines before a code block is truncated |
| `GOOSE_TRUNCATED_SHOW_LINES` | 20 | Lines shown around a truncation |
| `GOOSE_MAX_TOOL_RESPONSE_SIZE` | 200000 | Char threshold before tool output spills to a file |
| `GOOSE_AUTO_COMPACT_THRESHOLD` | 0.8 | Context fraction that triggers compaction |
| `GOOSE_TOOL_PAIR_SUMMARIZATION` | true | Summarize tool-call pairs on compaction |
| `GOOSE_STOP_HOOK_BLOCK_CAP` | 8 | Max consecutive Stop-hook blocks |
| `GOOSE_MAX_ACTIVE_AGENTS` | 100 | Max concurrent agent sessions |
| `GOOSE_MAX_BACKGROUND_TASKS` | 5 | Max concurrent background subagent tasks |
| `GOOSE_DEFAULT_EXTENSION_TIMEOUT` | 300 | Default MCP extension startup timeout (s) |
| `GOOSE_DISABLE_SESSION_NAMING` | false | Disable automatic session naming |
| `GOOSE_DISABLE_KEYRING` | keyring on | Store secrets in a file instead of the OS keyring |
| `GOOSE_SHELL` | `bash`/`sh`, `cmd` on Windows | Shell the shell tool invokes |
| `GOOSE_WORKING_DIR` | cwd | Working directory for extensions |
| `GOOSE_SYSTEM_PROMPT_FILE_PATH` | unset | Override the system prompt from a file |
| `GOOSE_PROMPT_EDITOR` | `$VISUAL`/`$EDITOR` | External editor for prompt input |
| `GOOSE_SEARCH_PATHS` | unset | Extra directories for tool/binary lookup |
| `GOOSE_DEBUG` | false | Debug rendering mode |
| `GOOSE_CA_CERT_PATH` / `GOOSE_CLIENT_CERT_PATH` / `GOOSE_CLIENT_KEY_PATH` | unset | Custom CA / client cert / client key for provider TLS |
| `GOOSE_ADDITIONAL_CONFIG_FILES` | unset | Extra config files to merge (PATH-split) |

## Exit codes

| Command | Codes |
|---|---|
| `goose orch -t` | `0` reviewer approved · `3` provider usage/quota/auth limit (with recovery guidance) · `1` otherwise |
| `goose goal -t` | `0` goal met · `1` otherwise. With `--check`, the shell command's exit 0 means success and no evaluator model is called |
| `goose loop -t` | Runs until stopped, the `--max` cap, or `--until-done` sees a `LOOP_DONE` marker; exits `0` on normal completion |

## Adapter catalog

The built-in ACP agent catalog lives in [`adapters/`](../adapters/). `goose herd`
shows install/config status for each; `goose herd add <name>` writes its config.
Adding an agent is a one-file pull request — see [ADAPTERS.md](../ADAPTERS.md).

## See also

- [ADAPTERS.md](../ADAPTERS.md) — contribute an ACP agent
- [docs/loops.md](loops.md) — `/loop` and `/goal` patterns
- [docs/writing-orch-tasks.md](writing-orch-tasks.md) — the `/orch` task house style
- [docs/overhaul-2026-07.md](overhaul-2026-07.md) — architecture and design decisions
