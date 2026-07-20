# Upstream Parity Matrix

This document tracks agentic-coding patterns published first-hand by the **Claude Code** and **Codex** teams, and how gooseherd covers them. It exists so that "did we keep up?" is a diff, not a feeling.

- Rows come from first-party sources only (posts, docs, changelogs written by the two teams — see [Watch list](#watch-list)).
- A weekly upstream-watch routine scans the sources, and new patterns land here as new rows or status changes.
- Status legend: ✅ covered · 🔶 partial (note says what's missing) · ❌ gap · ➖ intentionally out of scope

Last reviewed: **2026-07-20** (see [2026-W30 digest](upstream-watch/2026-W30.md))

## 1. Loop primitives

Source: [Getting started with loops](https://claude.com/blog/getting-started-with-loops) — the Claude Code team's taxonomy of agent loops.

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Turn-based agentic loop (prompt → work → verify → respond) | ✅ | ✅ | ✅ | Core goose session loop |
| Goal-based loop (run until a stated condition holds) | ✅ `/goal` | 🔶 (prompt-level) | 🔶 `/orch` | `/orch` loops plan→implement→review until reviewer VERDICT or `--max-cycles`. Gap: CC uses a **separate small model** as the condition judge; gooseherd's expensive reviewer doubles as judge. A cheap dedicated judge role would cut cost per cycle. |
| Time-based local loop (interval re-run) | ✅ `/loop 5m …` | ❌ | ❌ | Workaround today: cron + `goose orch -t --max-cycles`. Candidate: `goose orch --every 5m` or a `/loop` command. |
| Scheduled cloud loop | ✅ `/schedule` | ✅ cloud tasks, Codex Remote (GA July 2026) | ➖ | gooseherd is local-first; document the cron + headless recipe instead. |
| Proactive loop (event/schedule-triggered, no human in the loop) | ✅ (composition of primitives) | 🔶 | ❌ | CC composes `/schedule` + `/goal` (e.g. hourly triage until every report is handled). gooseherd equivalent would compose cron + headless orch + a goal condition. |
| Small-model routing inside loops (strong model only for judgment calls) | ✅ | ✅ | ✅ | This is gooseherd's founding premise: expensive planner/reviewer, cheap implementer. |
| Composable multi-agent workflow scripting (`agent()`/`parallel()`/`pipeline()` as code, with judge-panel, adversarial-verify, and loop-until-dry as reusable quality patterns) | ✅ Workflow tool (research preview → matured in the v2.1.210–2.1.215 patch train, Jul 14–19 2026) | ❌ (not found; PR Chat/Guardian auto-review are adjacent but not a general scripting primitive) | 🔶 needs verification | `/orch` and `/arena` are fixed-shape recipes (plan→implement→review; blind N-way + single judge) with no scriptable pipeline/parallel primitive, no multi-lens adversarial verification, no loop-until-dry, no budget-driven fan-out — confirmed by reading `orchestrate/runner.rs` and `arena.rs`. This is this week's pick (see digest). |

## 2. Verification and quality

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Verification skills — a SKILL.md defines what "done" means (start server, exercise UI, check console) | ✅ | 🔶 (AGENTS.md conventions) | 🔶 | Playbooks exist (`profiles/fable5-playbook.md`, personal playbooks via `GOOSE_PLAYBOOK_PATH`) but there is no standardized "verification" section the roles are told to honor. CC v2.1.215 (Jul 19 2026) made `/verify`/`/code-review` explicit-invoke only instead of auto-running — worth mirroring if gooseherd ever auto-fires playbooks. |
| Independent second-pass review by a separate agent | ✅ `/code-review` | ✅ (Codex reviews 100% of OpenAI PRs) | ✅ | Structural: the reviewer role in `/orch` is a different model with read-only powers. |
| Read-only enforcement for planning/review roles | ✅ plan mode | ✅ read-only sandbox | ✅ | Plan-Explore policy: read/search/think approved, writes rejected; double defense via claude plan mode / codex read-only sandbox. |
| Numeric success criteria work best (test counts, score thresholds) | ✅ (guidance) | ✅ (guidance) | 🔶 | Nothing stops it, but playbooks don't yet teach users to phrase orch tasks with numeric exit criteria. Doc fix. |

## 3. Cost and token observability

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Per-session usage breakdown | ✅ `/usage` | 🔶 | ✅ | `/usage`, `/status` |
| Per-run/agent accounting with stop options | ✅ `/workflows` | 🔶 | ✅ | `orch_ledger.jsonl` + `/stats` (per role: model, tokens, time, verdict) |
| Pilot run before scaling (measure cost on a small slice first) | ✅ (guidance) | ✅ (guidance) | 🔶 | `/arena` is the natural pilot harness; not yet framed that way in docs. See [Choosing a Claude model and effort level in Claude Code](https://claude.com/blog/claude-model-and-effort-level-in-claude-code) (Jul 7–8 2026) for the kind of cost/effort-routing guidance gooseherd's `/roles`/`/preset` docs are missing. |
| Runaway-loop cost caps (per-session limits on subagent spawns / tool calls, independent of the goal condition) | ✅ (v2.1.212, Jul 16 2026: default 200-subagent and 200-WebSearch-call session caps, both tunable via env var) | 🔶 needs verification | 🔶 needs verification | `/orch` bounds rounds via `--max-cycles`, but there is no cap on `/arena` lineup size or on total tool calls within a single role's turn — confirmed absent from `arena.rs`/`limits.rs`. Worth a cheap guard rail. |

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
| Hooks (pre/post tool-use automation) | ✅ | ❌ | ❌ | Not requested yet; watch for demand from migrating users. |
| Permission allowlists | ✅ | ✅ | ❌ | On the v1.43 list. |
| Onboarding doctor | 🔶 `/doctor` | ❌ | ✅ `goose herd` | Check-up with fixes and role auto-config |

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
