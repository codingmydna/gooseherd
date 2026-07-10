# gooseherd

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![GitHub release](https://img.shields.io/github/v/release/codingmydna/gooseherd)](https://github.com/codingmydna/gooseherd/releases)
[![Rust](https://img.shields.io/badge/rust-stable-orange)](https://www.rust-lang.org)

**Stop guessing which model is worth your money — your repo already knows.**

*A gooseherd tends geese. This one tends AI models.*

![A real /orch run: the planner writes a plan, gpt-5.6 implements, the reviewer sends it back once, then approves](docs/assets/orch-demo.gif)

*An unedited `/orch` run (idle time compressed): Claude plans, GPT-5.6 implements, the machine gates run, and the reviewer rejects cycle 1 before approving cycle 2.*

gooseherd is a CLI-first multi-model orchestration harness (a fork of
[goose](https://github.com/aaif-goose/goose)): an expensive frontier model plans
and reviews, a cheaper model implements, and machine gates verify — over
ACP-driven vendor CLIs (claude-acp, codex-acp, any agent via one config entry)
and native OpenAI/Anthropic-compatible APIs. Coding-agent subscriptions don't
mix — Claude Code drives Anthropic models, Codex drives OpenAI models — and
gooseherd is the layer that makes them work *together* on one task, with a paper
trail.

## 30-second quickstart

```sh
# 1. Install the release binary to ~/.local/bin/goose
curl -fsSL https://raw.githubusercontent.com/codingmydna/gooseherd/main/scripts/install.sh | bash

# 2. Check your setup (logins, adapters, catalog) and write the recommended role config
goose herd

# 3. Start a session and orchestrate
goose session
```

```
/orch add input validation to the /login handler and cover it with tests
```

`goose herd` verifies that `claude` and `codex` are installed and logged in,
installs/points you at the ACP adapters, and offers to write a split-role config
(planner/reviewer on your Claude subscription, implementer on Codex). If you
only have API keys, those work too — assign any provider to any role.

## What it does

**`/orch <task>` — plan → implement → review across models.** The planner
explores the repo read-only and writes a plan with acceptance criteria. The
implementer executes it with full tool access. The reviewer checks the diff
against the plan and either approves or sends it back, up to `GOOSE_ORCH_MAX_CYCLES`.

```
― phase: plan · claude-acp/default ―
  ⎿ plan done · model default · in 14993 / out 2485 · 59.1s
― phase: implement (cycle 1/3) · codex-acp/gpt-5.5 ―
  ⎿ implement done · model gpt-5.5 · in 915 / out 613 · 188.0s
― phase: review (cycle 1/3) · claude-acp/default ―
VERDICT: APPROVED
  ⎿ review done · model default · in 2 / out 224 · 6.7s · APPROVED
```

Each run isolates itself in a git worktree (`.goose/worktrees/orch-<run_id>`,
env files symlinked), so parallel runs on one repo don't contaminate each
other's evidence. On approval the run auto-commits to `orch/<run_id>` and prints
the merge command (`--merge` merges for you).

**Machine gates run before the reviewer.** Configured quality gates run after
each implement phase; mechanical failures bounce straight back to the
implementer without spending reviewer tokens. A repo-root `.goose-gates.yaml`
takes priority, safe `package.json`/`go.mod` gates are derived next, and
`GOOSE_ORCH_GATES` is the global fallback. (See
[reference](docs/reference.md#goose-gatesyaml) for the spec.)

**Exemplar hill-climbing.** Approved plans are archived; similar past plans are
injected as few-shot exemplars for future planners, so the expensive model's
planning shape survives into cheaper ones. The Fable 5 playbook and plan/review
exemplars are injected only into roles whose serving model is not a frontier
model — tune with `GOOSE_ORCH_PLAYBOOK` / `GOOSE_PLAN_EXEMPLARS_INJECT` /
`GOOSE_REVIEW_EXEMPLARS_INJECT` (`auto|always|never`).

**`/arena` — a blind, per-repo model tournament.** Run the same task on several
models at once, each in its own detached worktree, then have the reviewer
blind-judge the diffs (bare-letter labels, shuffled order, mapping revealed
after the verdict):

```
arena results
  A-codex-acp      codex-acp/gpt-5.5   failed/timeout  900s
  B-claude-acp     claude-acp/default  completed        48s  2 files changed, 29 insertions(+)

RANKING: B-claude-acp > A-codex-acp
```

Every phase — orch and arena — is appended to a run ledger
(`orch_ledger.jsonl`): configured model vs. the model the provider actually
reported, tokens, durations, and verdicts. `/stats` reads it back as an
approval-rate and mean-cycles table per model, with exemplar injection on vs
off.

**`/loop` and `/goal` — unattended runs.** `/loop <interval> <prompt>` re-runs a
prompt on a cadence; `/goal <goal>` retries with evaluator feedback until the
goal is met. Both have headless forms (`goose loop -t … --every …`,
`goose goal -t … --max N --check "<cmd>"`). See
[Loop engineering with gooseherd](docs/loops.md).

## Coming from Claude Code or Codex CLI

Most of what your hands already know keeps working. The mapping:

| You're used to | Here |
|---|---|
| `claude` / `codex` | `goose session` (alias `goose s`) |
| Importing your history | `goose session import <transcript>` reads Claude Code / Codex / Pi `.jsonl` |
| `claude --resume <id>` | `goose session --resume --session-id <id>` (printed on every exit) |
| `claude --continue` | `goose session -r` |
| Plan mode / read-only exploration | automatic for the planner and reviewer roles in `/orch` |
| `/compact`, `/clear`, `/model` | same commands |
| `/loop`, `/goal` | same commands; headless `goose loop` / `goose goal` |
| Shift+Tab mode cycling | Shift+Tab cycles role presets (`/preset save <name>` first) |
| `/cost`, `/context` | `/usage`, `/status`, `/stats` |
| Side questions without derailing the session | `/btw <question>` |
| `/terminal-setup` for Shift+Enter | same command |
| TodoWrite checklist rendering | automatic (`☐/◐/✔`) |
| `!command` shell passthrough | same — output joins the context |
| `/init` writing CLAUDE.md | `/init` writes AGENTS.md |
| `#note` quick memory | `/remember <note>` → `.goosehints` |
| CLAUDE.md / AGENTS.md project memory | `AGENTS.md` and `.goosehints` are read as project memory |
| Hooks (PreToolUse/PostToolUse/Stop/…) | same CC-compatible events, configured in `config.yaml` |
| Typing mid-turn to steer | opt-in — set `GOOSE_LIVE_INPUT: true`; text is injected at the next tool boundary |

Your existing vendor logins are reused as-is — if `claude` and `codex` work in
your terminal, gooseherd's ACP providers work too, because the actual vendor CLI
runs under the hood.

## Model lineup

**Subscriptions, over ACP.** Install the adapter for each subscription you want
to drive, then log in once with the vendor's own CLI — gooseherd inherits those
sessions and never needs API keys for them:

```sh
npm install -g @agentclientprotocol/claude-agent-acp   # Claude Code subscription
npm install -g @agentclientprotocol/codex-acp          # ChatGPT / Codex subscription
```

Built-in ACP presets: `claude-acp`, `codex-acp`, `copilot-acp`, `amp-acp`,
`pi-acp`. A minimal split-role `~/.config/goose/config.yaml`:

```yaml
GOOSE_PROVIDER: claude-acp
GOOSE_MODEL: default
GOOSE_PLANNER_PROVIDER: claude-acp
GOOSE_PLANNER_MODEL: default
GOOSE_REVIEWER_PROVIDER: claude-acp
GOOSE_REVIEWER_MODEL: default
GOOSE_IMPLEMENTER_PROVIDER: codex-acp
GOOSE_IMPLEMENTER_MODEL: gpt-5.5
GOOSE_ORCH_MAX_CYCLES: 3
```

**Cheap API models, as declarative providers.** OpenAI/Anthropic-compatible
endpoints (deepseek, groq, zai, openrouter, …) ship as bundled provider
definitions — set the API key and assign them to a role. Add your own by
dropping a JSON definition in `~/.config/goose/custom_providers/<id>.json`.
Cheap implementers are what most people come here for.

**The adapter catalog.** Any ACP-speaking CLI plugs in through one config entry:

```yaml
GOOSE_ACP_AGENTS:
  gemini: gemini --acp
  opencode: opencode acp
```

Each entry becomes a provider (`gemini-acp`, …) assignable to any role. The
built-in [`adapters/`](adapters/) catalog turns this into a one-liner —
`goose herd` shows install status and `goose herd add <name>` writes the config.
Adding a new agent is a one-file pull request: see [ADAPTERS.md](ADAPTERS.md).

## How it compares

| Project | Approach |
|---|---|
| [zeroshot](https://github.com/the-open-engine/zeroshot) | Plan/implement/verify loop on top of subscription CLIs, with blind validators. No goose extension ecosystem, arena, or exemplar learning. |
| [Qwen Code's Agent Arena](https://qwenlm.github.io/qwen-code-docs/en/users/features/arena/) | Built-in one-shot multi-model arena — same task, isolated worktrees, pick a winner. Results are not accumulated or fed back into future runs. |
| [upstream goose](https://github.com/aaif-goose/goose) | Model-driven orchestration is on the roadmap. No deterministic harness loop, fixed roles, or machine gates today. |
| gooseherd | Fixed-role loop (plan → implement → review) with machine gates, plus a persistent per-repo arena ledger and exemplar hill-climbing. |

## Security model

The planner and reviewer run as full agents but cannot write: permission
requests are judged by ACP tool kind, so reads, searches, and read-only subagent
exploration are approved while edits, deletes, and moves are rejected (the
agent's own restrictions — Claude Code plan mode, Codex read-only sandbox — stay
as a second barrier). Headless orchestration and arena default the implementer
to an allowlist policy that confines it to the workspace and an approved command
list and rejects shell-chaining metacharacters. Machine gates run with
credential environment variables scrubbed and a timeout. Runtime mode overrides
are held in memory, never written back to `config.yaml`, so a crash cannot leave
a permissive downgrade behind. Full details and the disclosure process are in
[SECURITY.md](SECURITY.md).

## Reference

- [docs/reference.md](docs/reference.md) — every `GOOSE_*` knob, exit codes, the gates spec
- [ADAPTERS.md](ADAPTERS.md) — contribute an ACP agent (one-file PR)
- [docs/loops.md](docs/loops.md) — `/loop` and `/goal` patterns
- [docs/writing-orch-tasks.md](docs/writing-orch-tasks.md) — the `/orch` task house style
- [docs/overhaul-2026-07.md](docs/overhaul-2026-07.md) — architecture and design decisions

Build from source instead of the installer:

```sh
cargo build --release -p goose-cli
cp target/release/goose ~/.local/bin/goose
```

## Credits and license

Built on [goose](https://github.com/aaif-goose/goose) by Block and the AAIF
community. Apache-2.0, same as upstream. Not affiliated with Block, AAIF,
Anthropic, or OpenAI. Model subscriptions are governed by their vendors' terms —
this project only drives the vendors' own CLIs.
