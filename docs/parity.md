# Upstream Parity Matrix

This document tracks agentic-coding patterns published first-hand by the **Claude Code** and **Codex** teams, and how gooseherd covers them. It exists so that "did we keep up?" is a diff, not a feeling.

- Rows come from first-party sources only (posts, docs, changelogs written by the two teams — see [Watch list](#watch-list)).
- A weekly upstream-watch routine scans the sources, and new patterns land here as new rows or status changes.
- Status legend: ✅ covered · 🔶 partial (note says what's missing) · ❌ gap · ➖ intentionally out of scope

Last reviewed: **2026-09-21** (baseline: [Projects redesigned: from folder to conversation](https://claude.com/blog/projects-redesigned), 2026-09-17)

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
| Project-level coordinator over parallel worker threads with shared memory/library | ✅ Claude Code Projects (beta) | ❌ | ❌ | **New this review.** [Projects redesigned](https://claude.com/blog/projects-redesigned) (2026-09-17): a coordinator scopes a request, delegates to parallel cloud threads (each its own branch/repo), reviews outputs, and assembles the result; threads share memory and a project library across repos and sessions. gooseherd's `/orch` and `/arena` each drive one coordinated run in one worktree — there is no persistent project layer that fans a request out across multiple repos/threads or carries shared memory between runs. Candidate: a `goose project` concept composing multiple `/orch` runs over a shared ledger/memory store. |

## 2. Verification and quality

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Verification skills — a SKILL.md defines what "done" means (start server, exercise UI, check console) | ✅ | 🔶 (AGENTS.md conventions) | 🔶 | Playbooks exist (`profiles/fable5-playbook.md`, personal playbooks via `GOOSE_PLAYBOOK_PATH`) but there is no standardized "verification" section the roles are told to honor. |
| Independent second-pass review by a separate agent | ✅ `/code-review` | ✅ (Codex reviews 100% of OpenAI PRs) | ✅ | Structural: the reviewer role in `/orch` is a different model with read-only powers. |
| Read-only enforcement for planning/review roles | ✅ plan mode | ✅ read-only sandbox | ✅ | Plan-Explore policy: read/search/think approved, writes rejected; double defense via claude plan mode / codex read-only sandbox. |
| Numeric success criteria work best (test counts, score thresholds) | ✅ (guidance) | ✅ (guidance) | 🔶 | Nothing stops it, but playbooks don't yet teach users to phrase orch tasks with numeric exit criteria. Doc fix. |

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
| Memory (`/remember`, CLAUDE.md/AGENTS.md) | ✅ | ✅ | ✅ | Claude Code now also reads a bare `AGENTS.md` when no `CLAUDE.md` is present (changelog v2.1.277) — convergence toward the convention gooseherd already standardized on. |
| Session resume / fork / import | ✅ | ✅ | ✅ | Includes importing CC/Codex jsonl transcripts |
| Message queueing while a turn streams | ✅ | 🔶 | ✅ | **Updated this review.** Codex shipped a `codex queue` command (~Aug 2026) to send messages into an existing local or remote session; unclear from the changelog whether it queues mid-turn the way CC's interactive queueing does, so marked partial pending a first-hand check — needs verification. |
| Live status/slash commands mid-turn | 🔶 | ❌ | ✅ | `/status /stats /usage /roles /btw` during streaming |
| Plan mode | ✅ | 🔶 | ✅ `/plan` | |
| Skills | ✅ | 🔶 prompts | 🔶 `/skills` | Coverage vs CC's skill triggers/frontmatter unverified — needs a pass. Claude Code also began syncing an account's claude.ai skills/plugins straight into terminal sessions (changelog v2.1.275, ~Aug 2026), widening the gap further. |
| Hooks (pre/post tool-use automation) | ✅ | ✅ (GA) | ❌ | Codex Hooks reached general availability 2026-05-14 (before our last review, noted here for completeness). Not requested yet in gooseherd; watch for demand from migrating users. |
| Permission allowlists | ✅ (Auto Mode) | ✅ | ❌ | **Updated this review.** Claude Code made Auto Mode the default permission mode for Pro/Max/Team (default flip 2026-08-14, announced 2026-08-07): a classifier auto-approves actions, with `permissions.deny`/`permissions.ask` written as plain-English rules taking priority over it. The bar moved from static allow-lists to classifier + natural-language rules. gooseherd has `GOOSE_ORCH_ALLOWED_COMMANDS`/`allowlist` policy, but that only gates the headless `/orch` implementer role, not general session permissions — still ❌ for a user-facing permission system. On the v1.43 list. |
| Task/agent dashboard (search, start, open, rename, stop running background tasks) | 🔶 (VS Code panel) | ✅ | ❌ | **New this review.** Codex added an interactive agents dashboard for managing local/remote tasks (~Aug 2026 changelog). gooseherd has `GOOSE_MAX_BACKGROUND_TASKS` for concurrent subagent tasks but no command or panel to list/open/rename/stop them individually — needs verification against the CLI, but no such surface was found in the codebase this pass. |
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
