# Upstream Parity Matrix

This document tracks agentic-coding patterns published first-hand by the **Claude Code** and **Codex** teams, and how gooseherd covers them. It exists so that "did we keep up?" is a diff, not a feeling.

- Rows come from first-party sources only (posts, docs, changelogs written by the two teams — see [Watch list](#watch-list)).
- A weekly upstream-watch routine scans the sources, and new patterns land here as new rows or status changes.
- Status legend: ✅ covered · 🔶 partial (note says what's missing) · ❌ gap · ➖ intentionally out of scope

Last reviewed: **2026-08-24** (this week: [Message your other Claude Code sessions](https://code.claude.com/docs/en/cross-session-messaging), shipped incrementally as v2.1.224 (Aug 7), v2.1.232 (Aug 13), v2.1.236 (Aug 19))

## 1. Loop primitives

Source: [Getting started with loops](https://claude.com/blog/getting-started-with-loops) — the Claude Code team's taxonomy of agent loops.

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Turn-based agentic loop (prompt → work → verify → respond) | ✅ | ✅ | ✅ | Core goose session loop |
| Goal-based loop (run until a stated condition holds) | ✅ `/goal` | 🔶 (prompt-level) | 🔶 `/orch` | `/orch` loops plan→implement→review until reviewer VERDICT or `--max-cycles`. Gap: CC uses a **separate small model** as the condition judge; gooseherd's expensive reviewer doubles as judge. A cheap dedicated judge role would cut cost per cycle. |
| Time-based local loop (interval re-run) | ✅ `/loop 5m …` | ❌ | ✅ `/loop` | **Status corrected 2026-08-24**: `crates/goose-cli/src/session/looping.rs` implements `/loop <interval> [--max N] [--until-done] <prompt>` with a `LOOP_DONE` self-termination sentinel — this closes the gap the 2026-07-07 baseline recorded as ❌. Not yet verified against CC's dynamic-pacing variant (self-chosen delay vs. fixed interval). |
| Scheduled cloud loop | ✅ `/schedule` | ✅ cloud tasks | ➖ | gooseherd is local-first; document the cron + headless recipe instead. |
| Proactive loop (event/schedule-triggered, no human in the loop) | ✅ (composition of primitives) | 🔶 | ❌ | CC composes `/schedule` + `/goal` (e.g. hourly triage until every report is handled). gooseherd equivalent would compose cron + headless orch + a goal condition. |
| Small-model routing inside loops (strong model only for judgment calls) | ✅ | ✅ | ✅ | This is gooseherd's founding premise: expensive planner/reviewer, cheap implementer. |
| **Cross-session agent messaging** (independently-launched sessions discover each other by name and exchange messages; async notify-on-idle) | ✅ `SendMessage` / `ListAgents` / `notify_when_idle` | ❌ (not found in Codex changelog/workflows as of this review) | 🔶 (needs verification) | **New row, 2026-08-24.** CC shipped this as three incremental releases in August: cross-session `SendMessage`/`ListAgents` (v2.1.224, Aug 7), `@`-mention addressing (v2.1.232, Aug 13), `notify_when_idle` opt-in idle handoff (v2.1.236, Aug 19) — see [docs](https://code.claude.com/docs/en/cross-session-messaging). gooseherd already has a structurally similar surface: `crates/goose/src/agents/platform_extensions/orchestrator.rs` exposes `ListSessions`, `StartAgent`, `SendMessage`, `InterruptAgent`, `ViewSession` tools. **Unverified**: whether these discover/message sessions started as *separate* `goose` processes (CC's model) or only sub-agents spawned within one `AgentManager`; no `notify_when_idle`-equivalent exists. This is this week's pick — see the digest. |

## 2. Verification and quality

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Verification skills — a SKILL.md defines what "done" means (start server, exercise UI, check console) | ✅ | 🔶 (AGENTS.md conventions) | 🔶 | Playbooks exist (`profiles/fable5-playbook.md`, personal playbooks via `GOOSE_PLAYBOOK_PATH`) but there is no standardized "verification" section the roles are told to honor. |
| Independent second-pass review by a separate agent | ✅ `/code-review` | ✅ (Codex reviews 100% of OpenAI PRs) | ✅ | Structural: the reviewer role in `/orch` is a different model with read-only powers. |
| Read-only enforcement for planning/review roles | ✅ plan mode | ✅ read-only sandbox | ✅ | Plan-Explore policy: read/search/think approved, writes rejected; double defense via claude plan mode / codex read-only sandbox. |
| Numeric success criteria work best (test counts, score thresholds) | ✅ (guidance) | ✅ (guidance) | 🔶 | Nothing stops it, but playbooks don't yet teach users to phrase orch tasks with numeric exit criteria. Doc fix. |
| Safer automatic-review defaults for higher-risk (e.g. cyber-capable) models | ➖ | ✅ (Codex changelog, Aug 2026) | ➖ | Codex-specific safety-tier gating on its own model roster; not a gooseherd-applicable pattern today — flagged for awareness only, not a gap. |

## 3. Cost and token observability

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Per-session usage breakdown | ✅ `/usage` | 🔶 | ✅ | `/usage`, `/status` |
| Per-run/agent accounting with stop options | ✅ `/workflows` | 🔶 | ✅ | `orch_ledger.jsonl` + `/stats` (per role: model, tokens, time, verdict) |
| Pilot run before scaling (measure cost on a small slice first) | ✅ (guidance) | ✅ (guidance) | 🔶 | `/arena` is the natural pilot harness; not yet framed that way in docs. |

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
| Default-model env var that persists across restarts but yields to an in-session model pick | ✅ `ANTHROPIC_DEFAULT_MODEL` (Aug 2026) | 🔶 | ✅ `GOOSE_MODEL` | `crates/goose-cli/src/cli.rs` documents `GOOSE_MODEL` as the equivalent env-level default; behavior not re-verified against CC's exact override precedence this pass. |
| Low-noise output mode (lead with results, skip preamble/narration) | ✅ built-in "Concise" style (v2.1.237, Aug 2026) | 🔶 | ❌ (needs verification) | New CC daily-driver habit; not confirmed whether gooseherd's TUI has an equivalent terse mode. Small gap, not this week's pick. |

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
