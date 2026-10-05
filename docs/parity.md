# Upstream Parity Matrix

This document tracks agentic-coding patterns published first-hand by the **Claude Code** and **Codex** teams, and how gooseherd covers them. It exists so that "did we keep up?" is a diff, not a feeling.

- Rows come from first-party sources only (posts, docs, changelogs written by the two teams — see [Watch list](#watch-list)).
- A weekly upstream-watch routine scans the sources, and new patterns land here as new rows or status changes.
- Status legend: ✅ covered · 🔶 partial (note says what's missing) · ❌ gap · ➖ intentionally out of scope

Last reviewed: **2026-10-05** (new: [Customize Claude Code with mods](https://claude.com/blog/claude-code-mods), 2026-10-01; Claude Code changelog 2.1.283–2.1.289; Codex CLI 0.147–0.158 changelog entries)

## 1. Loop primitives

Source: [Getting started with loops](https://claude.com/blog/getting-started-with-loops) — the Claude Code team's taxonomy of agent loops.

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Turn-based agentic loop (prompt → work → verify → respond) | ✅ | ✅ | ✅ | Core goose session loop |
| Goal-based loop (run until a stated condition holds) | ✅ `/goal` | 🔶 (prompt-level) | 🔶 `/orch` | `/orch` loops plan→implement→review until reviewer VERDICT or `--max-cycles`. Gap: CC uses a **separate small model** as the condition judge; gooseherd's expensive reviewer doubles as judge. A cheap dedicated judge role would cut cost per cycle. |
| Time-based local loop (interval re-run) | ✅ `/loop 5m …` | ❌ | ❌ | Workaround today: cron + `goose orch -t --max-cycles`. Candidate: `goose orch --every 5m` or a `/loop` command. |
| Scheduled cloud loop | ✅ `/schedule` | ✅ cloud tasks | ➖ | gooseherd is local-first; document the cron + headless recipe instead. |
| Proactive loop (event/schedule-triggered, no human in the loop) | ✅ (composition of primitives) | 🔶 | ❌ | CC composes `/schedule` + `/goal` (e.g. hourly triage until every report is handled). gooseherd equivalent would compose cron + headless orch + a goal condition. |
| Small-model routing inside loops (strong model only for judgment calls) | ✅ | ✅ | ✅ | This is gooseherd's founding premise: expensive planner/reviewer, cheap implementer. |
| Scripted multi-agent fan-out (a JS/DAG script the model writes: `phase()`/`pipeline()`/`parallel()`, schema-validated subagent results, runs decoupled from the conversation) | ✅ Ultracode / Dynamic Workflows (`/effort ultracode`, up to 16 concurrent agents, 1000/run) | ❌ (not found in changelog/workflows docs) | 🔶 | `/orch` is a fixed plan→implement→gates→review pipeline; `/arena` fans N contestants out in parallel but only for blind comparison. Neither lets the planner write an arbitrary phased/parallel script with per-step schema validation. Highest-leverage gap this week — see digest. |
| Peer-to-peer agent teams (independent sibling sessions in their own panes, interruptible, message each other) | ✅ Agent Teams (`--agent-teams`) + `agent.spawn` for teammates, idle/waiting states (2.1.289) | ❌ | ❌ | `/orch` roles are sequential phases inside one harness-controlled run, not addressable peer sessions. |

## 2. Verification and quality

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Verification skills — a SKILL.md defines what "done" means (start server, exercise UI, check console) | ✅ (+ commit-time nudge: projects with a `verify` skill are told to run it before committing, 2.1.286) | 🔶 (AGENTS.md conventions) | 🔶 | Playbooks exist (`profiles/fable5-playbook.md`, personal playbooks via `GOOSE_PLAYBOOK_PATH`) but there is no standardized "verification" section the roles are told to honor. gooseherd's machine gates (`.goose-gates.yaml`) already *enforce* this before the reviewer runs, which is stricter than CC's nudge — worth stating explicitly in docs rather than marking 🔶. |
| Independent second-pass review by a separate agent | ✅ `/code-review` | ✅ (Codex reviews 100% of OpenAI PRs) | ✅ | Structural: the reviewer role in `/orch` is a different model with read-only powers. |
| Read-only enforcement for planning/review roles | ✅ plan mode | ✅ read-only sandbox | ✅ | Plan-Explore policy: read/search/think approved, writes rejected; double defense via claude plan mode / codex read-only sandbox. |
| Numeric success criteria work best (test counts, score thresholds) | ✅ (guidance) | ✅ (guidance) | 🔶 | Nothing stops it, but playbooks don't yet teach users to phrase orch tasks with numeric exit criteria. Doc fix. |

## 3. Cost and token observability

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Per-session usage breakdown | ✅ `/usage` | 🔶 | ✅ | `/usage`, `/status` |
| Per-run/agent accounting with stop options | ✅ `/workflows` | 🔶 | ✅ | `orch_ledger.jsonl` + `/stats` (per role: model, tokens, time, verdict) |
| Pilot run before scaling (measure cost on a small slice first) | ✅ (guidance) | ✅ (guidance) | 🔶 | `/arena` is the natural pilot harness; not yet framed that way in docs. |
| Context-budget-aware long sessions (model/harness co-designed so coding sessions can run longer without degrading) | ✅ [Opus 5.5 built for longer coding sessions](https://claude.com/blog/claude-opus-5-5-built-for-coding-sessions-that-use-more-context), 2026-09-24 | 🔶 | 🔶 needs verification | New row. Unclear whether gooseherd's compaction/summarization for long `/orch` cycles has been measured against degradation the way CC claims for Opus 5.5 — flagging rather than guessing at a status. |

## 4. Daily-driver table stakes

Habits CC/Codex users expect on day one (quality bar: no "wait, where is…?" moments).

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Shell passthrough (`!cmd`) | ✅ | ✅ | ✅ | |
| Project bootstrap (`/init`) | ✅ | ✅ | ✅ | |
| Memory (`/remember`, CLAUDE.md/AGENTS.md) | ✅ | ✅ | ✅ | |
| Session resume / fork / import | ✅ | ✅ | ✅ | Includes importing CC/Codex jsonl transcripts |
| Message queueing while a turn streams | ✅ | ❌ | ✅ | |
| Live status/slash commands mid-turn | 🔶 | ❌ | ✅ | `/status /stats /usage /roles /btw` during streaming |
| Plan mode | ✅ | 🔶 | ✅ `/plan` | |
| Skills | ✅ | 🔶 prompts | 🔶 `/skills` | Coverage vs CC's skill triggers/frontmatter unverified — needs a pass. |
| Hooks (pre/post tool-use automation) | ✅ (superseded by **Mods**: plugin-shipped TS functions that rewrite prompts/tool calls, draw new UI, redact secrets, stack sequentially — [Customize Claude Code with mods](https://claude.com/blog/claude-code-mods), 2026-10-01) | ❌ | ✅ | Corrected: `crates/goose/src/hooks/mod.rs` already implements the Open Plugins hooks spec (`PreToolUse`/`PostToolUse`/`SessionStart`/etc., `command`-type actions) — this was marked ❌ in error; it landed 2026-07-03. Gap is now one level up: gooseherd has hook-script automation but nothing like Mods' UI/behavior-replacement layer. |
| Permission allowlists | ✅ (+ model-level governance: `availableModelsMatch`/`deniedModels` managed settings, 2.1.283) | ✅ | ❌ | On the v1.43 list. CC's new model allow/deny-list is a related but distinct surface (which models may be selected, not just which tools run) — worth scoping separately when this is picked up. |
| Onboarding doctor | 🔶 `/doctor` (+ `prompt-audit` subcommand audits CLAUDE.md/AGENTS.md patterns, 2.1.283) | 🔶 `codex doctor` (`--feedback` flag added Aug 2026) | ✅ `goose herd` | Check-up with fixes and role auto-config. Gap: no equivalent of CC's `prompt-audit` — `goose herd`/`doctor` doesn't lint AGENTS.md/CLAUDE.md content quality, only environment/provider setup. Needs verification before claiming otherwise. |

## 5. gooseherd differentiators

Things neither upstream can do (single-vendor stacks), i.e. the reason to exist — kept here so parity work never crowds them out.

| Capability | Status |
|---|---|
| Cross-vendor multi-model loop: frontier planner/reviewer + cheap implementer, subscription auth on both sides, no API billing | ✅ `/orch` |
| Blind head-to-head model comparison in isolated worktrees | ✅ `/arena` |
| Cross-run cost/verdict ledger for model-mix decisions | ✅ `orch_ledger.jsonl` + `/stats` |
| In-session role/model/effort switching and presets | ✅ `/roles`, `/preset` |
| Model-identity verification (downgrade detection) | ✅ `EXPECT_MODEL` |

## Watch list

First-party sources scanned weekly:

- https://claude.com/blog (Claude Code team posts)
- https://www.anthropic.com/engineering
- https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md
- https://developers.openai.com/codex/changelog and https://developers.openai.com/codex/workflows
- https://openai.com/news/ (Codex-tagged posts)

## Process

1. Weekly routine produces a digest: *new post/change → pattern → row here (new or status change) → gap ticket if ❌/🔶*.
2. One gap per week gets picked into the `/orch` self-improvement loop, alongside friction-log items from daily-driver use.
3. Status only moves to ✅ when the feature passes the quality bar: a CC/Codex user hits no dead end and no raw error.
