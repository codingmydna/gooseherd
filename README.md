# gooseherd

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![GitHub release](https://img.shields.io/github/v/release/codingmydna/gooseherd)](https://github.com/codingmydna/gooseherd/releases)
[![Rust](https://img.shields.io/badge/rust-stable-orange)](https://www.rust-lang.org)

**Stop guessing which model is worth your money — your repo already knows.**

*A gooseherd tends geese. This one tends AI models.*

![A real /orch run: the planner writes a plan, gpt-5.6 implements, the reviewer sends it back once, then approves](docs/assets/orch-demo.gif)

*An unedited `/orch` run (idle time compressed): Claude plans, GPT-5.6 implements, the machine gates run, and the reviewer rejects cycle 1 before approving cycle 2.*

**When to reach for gooseherd:**

- You want your expensive subscription limits spent on planning and review,
  while a cheaper model does the implementation.
- You want to compare models blind on your repo's actual tasks, not on
  benchmarks — `/arena` runs them head-to-head and a ledger keeps score.
- You don't want the agent that wrote the code approving its own work —
  mechanical gates run before an independent reviewer sees the diff.

gooseherd is a fork of [goose](https://github.com/aaif-goose/goose) that turns it
into a multi-model orchestrator: a frontier model plans and reviews, a cheaper
model does the implementation, and every step is measured so you can see who
actually did what.

The starting observation is simple. Coding-agent subscriptions don't mix —
Claude Code drives Anthropic models, Codex drives OpenAI models, and each one
is excellent inside its own harness. goose already speaks to both of them over
ACP. What was missing was a layer that makes them work *together* on one task,
with a paper trail.

For the loop patterns behind these commands, see
[Loop engineering with gooseherd](docs/loops.md).

## What it adds on top of goose

**`/orch <task>`** — a plan → implement → review loop across different models.
The planner (say, Claude via your Claude Code subscription) explores the repo
read-only and writes a plan with acceptance criteria. The implementer (say,
GPT via your Codex subscription, or a local model) executes it with full tool
access. The reviewer checks the diff against the plan and either approves or
sends it back, up to N cycles.

```
― phase: plan · claude-acp/default ―
  ⎿ plan done · model default · in 14993 / out 2485 · 59.1s
― phase: implement (cycle 1/3) · codex-acp/gpt-5.5 ―
  ⎿ implement done · model gpt-5.5 · in 915 / out 613 · 188.0s
― phase: review (cycle 1/3) · claude-acp/default ―
VERDICT: APPROVED
  ⎿ review done · model default · in 2 / out 224 · 6.7s · APPROVED
```

### Your repo learns which model wins

Every run leaves evidence, and the evidence compounds.

**`/arena`** — run the same task on several models at once, each in its own
detached git worktree, then have the reviewer blind-judge the diffs:

```
arena results
  A-codex-acp      codex-acp/gpt-5.5   failed/timeout  900s
  B-claude-acp     claude-acp/default  completed        48s  2 files changed, 29 insertions(+)

RANKING: B-claude-acp > A-codex-acp
```

Worktrees are kept afterwards so you can inspect every attempt yourself.

**A run ledger and `/stats`** — every orchestration phase is appended to
`orch_ledger.jsonl`: configured model vs. the model the provider actually
reported, tokens, durations, verdicts, and the session's advertised context
limit (a useful fingerprint for catching silent model downgrades —
`GOOSE_<ROLE>_EXPECT_MODEL` warns when the reported model doesn't match).

**Exemplar hill-climbing** — approved plans are archived and similar past
plans are injected as few-shot exemplars for future planners
(`GOOSE_PLAN_EXEMPLARS`), so the expensive model's planning shape survives
into cheaper ones. Fable 5's playbook and plan/review exemplars are injected
into planner/reviewer roles whose serving model is not Fable; use
`GOOSE_ORCH_PLAYBOOK=auto|always|never`,
`GOOSE_PLAN_EXEMPLARS_INJECT=auto|always|never`, or
`GOOSE_REVIEW_EXEMPLARS_INJECT=auto|always|never` to override.

**A full run lifecycle around the loop** — each `/orch` run isolates itself in
a git worktree (`.goose/worktrees/orch-<run_id>`, env files symlinked), so
parallel runs on one repo don't contaminate each other's evidence. Configured
quality gates (`GOOSE_ORCH_GATES`, e.g. fmt/lint/test) run *before* the
reviewer is called — mechanical failures bounce straight back to the
implementer without spending reviewer tokens. On approval the run auto-commits
to its branch and prints the merge command (`--merge` merges for you).
An allowlist permission policy (`GOOSE_ORCH_IMPLEMENT_POLICY: allowlist`)
confines the implementer to the workspace and an approved command list — for
running orchestration on repos you actually care about.

**Bring any ACP agent** — besides the built-in claude/codex/copilot/amp/pi
adapters, any ACP-speaking CLI plugs in via config:

```yaml
GOOSE_ACP_AGENTS:
  gemini: gemini --acp
  opencode: opencode acp
  kimi: kimi acp
```

Each entry becomes a provider (`gemini-acp`, …) assignable to any role — handy
for cheap or free implementer models.

Entries may also use a map form with `command`, optional `env`, and optional
`env_remove`. `${VAR}` references in `env` are resolved from your shell or goose
secret store when the agent starts, so tokens do not need to live in shared
config:

```yaml
GOOSE_ACP_AGENTS:
  glm:
    command: claude-agent-acp
    env:
      ANTHROPIC_BASE_URL: https://api.z.ai/api/anthropic
      ANTHROPIC_AUTH_TOKEN: ${ZAI_API_KEY}
```

**Plan-Explore permission policy** — the planner and reviewer run as full
agents but cannot write. Instead of goose's all-or-nothing modes, permission
requests are judged by ACP tool kind: reads, searches, and parallel subagent
exploration are approved; edits, deletes, and moves are rejected. This works
for any ACP agent, with the agent's own restrictions (Claude Code plan mode,
Codex read-only sandbox) kept as a second barrier.

**Quality-of-life commands** — `/status` (roles, connection type, effort,
usage), `/usage`, `/roles` (change role assignments without leaving the
session), `/model provider/model` (switch the live session and persist it),
`/btw` (ask a side question in the background without touching the
session history), `goose worktree new/list/prune` (parallel-session worktrees
with env symlinks), `/terminal-setup` (make Shift+Enter insert a newline in
terminals that swallow the modifier), a copy-pasteable resume command on exit,
slash-command typing hints, per-role reasoning effort, and Claude-Code-style
rendering: diff coloring, edit previews, a bordered input box with a status
line, todo checklists (`☐/◐/✔`), role-colored response bullets during
orchestration (planner cyan, implementer yellow, reviewer magenta), a live
spinner with elapsed time and running tools, and mid-turn steering — type
while a turn is running and it's injected at the next tool boundary.

## How it compares

| Project | Approach |
|---|---|
| [zeroshot](https://github.com/the-open-engine/zeroshot) | Plan/implement/verify loop on top of subscription CLIs, with blind validators. No goose extension ecosystem, arena, or exemplar learning. |
| [Qwen Code's Agent Arena](https://qwenlm.github.io/qwen-code-docs/en/users/features/arena/) | Built-in one-shot multi-model arena — same task, isolated worktrees, pick a winner. Results are not accumulated or fed back into future runs. |
| [upstream goose](https://github.com/aaif-goose/goose) | Model-driven orchestration is on the roadmap. No deterministic harness loop, fixed roles, or machine gates today. |
| gooseherd | Fixed-role loop (plan → implement → review) with machine gates, plus a persistent per-repo arena ledger and exemplar hill-climbing. |

## Setup

### Install (recommended)

```sh
curl -fsSL https://raw.githubusercontent.com/codingmydna/gooseherd/main/scripts/install.sh | bash
```

Installs the latest release binary to `~/.local/bin/goose`.
`GOOSE_BIN_DIR`, `GOOSE_VERSION`, and `GOOSE_REPO` override the defaults.

### Manual download

Grab the tar.gz for your platform from the
[releases page](https://github.com/codingmydna/gooseherd/releases), verify the
`.sha256`, and copy `goose` somewhere on your `PATH`.

### Build from source

```sh
cargo build --release -p goose-cli
cp target/release/goose ~/.local/bin/goose
```

Whichever install path you choose, install the ACP adapters for the
subscriptions you want to drive:

```sh
npm install -g @agentclientprotocol/claude-agent-acp   # Claude Code subscription
npm install -g @agentclientprotocol/codex-acp          # ChatGPT/Codex subscription
```

Log in once with each vendor's own CLI (`claude`, `codex login`) — gooseherd
inherits those sessions and never needs API keys for them. API-key and local
providers (ollama, openrouter, …) work exactly as in upstream goose and can be
assigned to any role.

A minimal `~/.config/goose/config.yaml` for the split-role setup:

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

Then, inside `goose session`:

```
/orch add input validation to the /login handler and cover it with tests
```

The task text is a contract the cheaper implementer follows literally, so its
shape matters. gooseherd has a house style for writing them — a 6-part anatomy
(goal, why, code pointers, numbered requirements, completion criteria, planner
footer) and 5 principles, distilled from the model that bootstrapped this fork:
[Writing good `/orch` tasks — the Fable 5 style](docs/writing-orch-tasks.md).

## Coming from Claude Code or Codex CLI

Most of what your hands already know keeps working. The mapping:

| You're used to | Here |
|---|---|
| `claude` / `codex` | `goose session` |
| `claude --resume <id>` | `goose session --resume --session-id <id>` (printed on every exit) |
| `claude --continue` | `goose session -r` |
| Plan mode / read-only exploration | automatic for the planner and reviewer roles in `/orch` |
| `/compact`, `/clear`, `/model` | same commands |
| `/loop <interval> <prompt>` | same command; headless: `goose loop -t "<prompt>" --every <interval>` |
| `/goal <goal>` | same command; headless: `goose goal -t "<goal>" --max N --check "<cmd>"` |
| `/schedule` | roadmap; see [loop engineering](docs/loops.md) and use cron with `goose loop --max 1` / `goose goal` meanwhile |
| Shift+Tab mode cycling | Shift+Tab cycles role presets (`/preset save <name>` first) |
| `/cost`, `/context` | `/usage`, `/status`, `/stats` |
| Side questions without derailing the session | `/btw <question>` |
| Typing mid-turn to steer | same — injected at the next tool boundary |
| `/terminal-setup` for Shift+Enter | same command |
| TodoWrite checklist rendering | automatic (`☐/◐/✔`) |
| Background agents committing to a branch | `/orch` auto-worktree + auto-commit on approval |
| `!command` shell passthrough | same — `!command`, output joins the context |
| `/init` writing CLAUDE.md | `/init` writes AGENTS.md |
| `#note` quick memory | `/remember <note>` → .goosehints |
| CLAUDE.md / AGENTS.md project memory | AGENTS.md and .goosehints work as before (upstream goose behavior) |

Your existing vendor logins are reused as-is — if `claude` and `codex` work in
your terminal, gooseherd's ACP providers work too. Skills and plugins you
installed for those CLIs also load, because the actual vendor CLI is what runs
under the hood.

Run `goose herd` for a first-time checkup that verifies logins and adapters
and offers to write the recommended role config.

### Using GLM 5.2 / any Anthropic-compatible endpoint

GLM 5.2 (Zhipu / Z.ai) exposes an Anthropic-compatible endpoint, so it can run
through the `claude-agent-acp` adapter as a generic ACP provider:

```yaml
GOOSE_ACP_AGENTS:
  glm:
    command: claude-agent-acp
    env:
      ANTHROPIC_BASE_URL: https://api.z.ai/api/anthropic
      ANTHROPIC_AUTH_TOKEN: ${ZAI_API_KEY}

GOOSE_IMPLEMENTER_PROVIDER: glm-acp
GOOSE_IMPLEMENTER_MODEL: <z.ai model id>
```

Set `ZAI_API_KEY` in your shell (`export ZAI_API_KEY=...`) or store it with
`goose configure`; keep the config value as `${ZAI_API_KEY}`. Use `glm-acp` for
the planner, implementer, reviewer, or a saved role preset just like any other
provider. If you prefer not to use the ACP adapter, point the existing `openai`
provider at Z.ai's OpenAI-compatible endpoint with `OPENAI_BASE_URL` and
`OPENAI_API_KEY`.

## Troubleshooting

- macOS says it cannot verify the developer — the binary is not yet signed or
  notarized; for a manually downloaded binary, run
  `xattr -d com.apple.quarantine ~/.local/bin/goose`.
- `could not resolve command 'claude-agent-acp'` — the adapter isn't
  installed: `npm install -g @agentclientprotocol/claude-agent-acp` (and make
  sure `claude` itself is logged in). Same pattern for `codex-acp`.
- An /arena contestant times out with no changes — check its log at
  `.goose-arena/<label>.log`; vendor-CLI plugins that prompt interactively are
  the usual suspect in headless runs.
- The planner "can't do anything" — its session is read-only by design; it
  can read, search, and spawn read-only subagents, but edits and shell are
  denied (`GOOSE_PLAN_ALLOW_EXEC: true` relaxes shell).

## Caveats

This is a young fork, developed and tested on macOS. The orchestration loop,
arena, ledger, and permission policy all work end-to-end, but expect rough
edges — particularly around terminal rendering and headless runs of vendor
CLIs that ship their own plugins. Issues and PRs are welcome; so is telling me
the whole idea is wrong, if you can show your ledger.

## Credits and license

Built on [goose](https://github.com/aaif-goose/goose) by Block and the AAIF
community (upstream README: [README.upstream.md](README.upstream.md)).
Apache-2.0, same as upstream. Not affiliated with Block, AAIF, Anthropic, or
OpenAI. Model subscriptions are governed by their vendors' terms — this
project only drives the vendors' own CLIs.
