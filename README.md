# gooseherd

A gooseherd tends geese. This one tends AI models.

gooseherd is a fork of [goose](https://github.com/aaif-goose/goose) that turns it
into a multi-model orchestrator: a frontier model plans and reviews, a cheaper
model does the implementation, and every step is measured so you can see who
actually did what.

The starting observation is simple. Coding-agent subscriptions don't mix —
Claude Code drives Anthropic models, Codex drives OpenAI models, and each one
is excellent inside its own harness. goose already speaks to both of them over
ACP. What was missing was a layer that makes them work *together* on one task,
with a paper trail.

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

**A full run lifecycle around the loop** — each `/orch` run isolates itself in
a git worktree (`.goose/worktrees/orch-<run_id>`, env files symlinked), so
parallel runs on one repo don't contaminate each other's evidence. Configured
quality gates (`GOOSE_ORCH_GATES`, e.g. fmt/lint/test) run *before* the
reviewer is called — mechanical failures bounce straight back to the
implementer without spending reviewer tokens. On approval the run auto-commits
to its branch and prints the merge command (`--merge` merges for you).
Approved plans are archived and similar past plans are injected as few-shot
exemplars for future planners (`GOOSE_PLAN_EXEMPLARS`), so the expensive
model's planning shape survives into cheaper ones. An allowlist permission
policy (`GOOSE_ORCH_IMPLEMENT_POLICY: allowlist`) confines the implementer to
the workspace and an approved command list — for running orchestration on
repos you actually care about.

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

**Plan-Explore permission policy** — the planner and reviewer run as full
agents but cannot write. Instead of goose's all-or-nothing modes, permission
requests are judged by ACP tool kind: reads, searches, and parallel subagent
exploration are approved; edits, deletes, and moves are rejected. This works
for any ACP agent, with the agent's own restrictions (Claude Code plan mode,
Codex read-only sandbox) kept as a second barrier.

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

## Setup

You need Rust, plus the ACP adapters for whichever subscriptions you want to
drive:

```sh
npm install -g @agentclientprotocol/claude-agent-acp   # Claude Code subscription
npm install -g @agentclientprotocol/codex-acp          # ChatGPT/Codex subscription

cargo build --release -p goose-cli
cp target/release/goose ~/.local/bin/goose
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

## Troubleshooting

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
