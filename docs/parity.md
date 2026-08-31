# Upstream Parity Matrix

This document tracks agentic-coding patterns published first-hand by the **Claude Code** and **Codex** teams, and how gooseherd covers them. It exists so that "did we keep up?" is a diff, not a feeling.

- Rows come from first-party sources only (posts, docs, changelogs written by the two teams — see [Watch list](#watch-list)).
- A weekly upstream-watch routine scans the sources, and new patterns land here as new rows or status changes.
- Status legend: ✅ covered · 🔶 partial (note says what's missing) · ❌ gap · ➖ intentionally out of scope

Last reviewed: **2026-08-31** (this week: Claude Code v2.1.243 "loops breakdown" in `/usage` (Aug 25), v2.1.248 `--restricted` hard-lockdown mode (Aug 27), v2.1.251 model-switch hooks + spend-limit tracking (Aug 28) — see [digest](upstream-watch/2026-W36.md)).

> **Note on this revision**: `docs/parity.md` has never been merged to the default branch — every weekly run through 2026-08-24 (PRs [#18](https://github.com/codingmydna/gooseherd/pull/18)–[#24](https://github.com/codingmydna/gooseherd/pull/24)) rebuilt it from the stale `docs/upstream-parity` branch (frozen 2026-07-07) instead of stacking on the prior week's PR, so six weeks of corrections and new rows never accumulated and some PRs actively regressed rows a later week had already fixed (see this week's digest for specifics). This revision reconciles the most accurate version of every row across PRs #18–#24 before layering in this week's findings. **Recommendation: merge this PR, then close #18–#24 as superseded.**

## 1. Loop primitives

Source: [Getting started with loops](https://claude.com/blog/getting-started-with-loops) — the Claude Code team's taxonomy of agent loops.

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Turn-based agentic loop (prompt → work → verify → respond) | ✅ | ✅ | ✅ | Core goose session loop |
| Goal-based loop (run until a stated condition holds) | ✅ `/goal` | 🔶 (prompt-level) | 🔶 `/orch` | `/orch` loops plan→implement→review until reviewer VERDICT or `--max-cycles`. Gap: CC uses a **separate small model** as the condition judge; gooseherd's expensive reviewer doubles as judge. A cheap dedicated judge role would cut cost per cycle. |
| Time-based local loop (interval re-run) | ✅ `/loop 5m …` | ❌ | ✅ `/loop` | Verified from code: `crates/goose-cli/src/session/looping.rs` implements `/loop <interval> [--max N] [--until-done] <prompt>` with a `LOOP_DONE` self-termination sentinel — closes the gap the 2026-07-07 baseline recorded as ❌ (status corrected 2026-08-24). New this cycle: CC's v2.1.243 (2026-08-25) added a **loops breakdown to `/usage`** (per-loop run count, tokens, tokens/run, last run) and folded consecutive no-op wake-ups into a single terminal line. gooseherd's equivalent gap is tracked under cost observability below — this week's pick. |
| Scheduled cloud loop | ✅ `/schedule` | ✅ cloud tasks | ➖ | gooseherd is local-first; document the cron + headless recipe instead. CC's self-hosted runner beta ([Run Claude Code sessions on your own compute](https://claude.com/blog/run-claude-code-sessions-on-your-own-compute), 2026-08-06) converges toward this stance (inference still round-trips to Anthropic). Still ➖ by design, not a gap. |
| Proactive loop (event/schedule-triggered, no human in the loop) | ✅ (composition of primitives) | 🔶 | ❌ | CC composes `/schedule` + `/goal` (e.g. hourly triage until every report is handled). gooseherd equivalent would compose cron + headless orch + a goal condition. |
| Small-model routing inside loops (strong model only for judgment calls) | ✅ | ✅ | ✅ | This is gooseherd's founding premise: expensive planner/reviewer, cheap implementer. |

## 2. Verification and quality

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Verification skills — a SKILL.md defines what "done" means (start server, exercise UI, check console) | ✅ | 🔶 (AGENTS.md conventions) | 🔶 | [Building verification loops in Claude Code with skills](https://claude.com/blog/building-verification-loops-in-claude-code-with-skills) (2026-07-22) names two shapes: *standalone* skills (deliberately invoked, cross-cutting — pre-commit scans, a11y audits) and *embedded* verification (fires automatically as part of the producing skill). gooseherd's `.goose-gates.yaml` machine gates (see [reference.md](reference.md#machine-gates)) are structurally the "embedded, automatic" half already — they run unconditionally before the reviewer. What's still missing is the "standalone, deliberately invoked" half: a named, reusable verification skill roles can be told to run mid-task, with numeric pass/fail criteria. Playbooks (`profiles/fable5-playbook.md`, `GOOSE_PLAYBOOK_PATH`) are the closest analog but aren't structured that way yet. |
| Independent second-pass review by a separate agent | ✅ `/code-review` | ✅ (Codex reviews 100% of OpenAI PRs) | ✅ | Structural: the reviewer role in `/orch` is a different model with read-only powers. |
| Read-only enforcement for planning/review roles | ✅ plan mode | ✅ read-only sandbox | ✅ | Plan-Explore policy: read/search/think approved, writes rejected; double defense via claude plan mode / codex read-only sandbox. |
| Numeric success criteria work best (test counts, score thresholds) | ✅ (guidance) | ✅ (guidance) | 🔶 | Nothing stops it, but playbooks don't yet teach users to phrase orch tasks with numeric exit criteria. Doc fix. |

## 3. Cost and token observability

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Per-session usage breakdown | ✅ `/usage` | 🔶 | ✅ | `/usage`, `/status` |
| Per-run/agent accounting with stop options | ✅ `/workflows`; new in v2.1.243 (2026-08-25): a **loops breakdown in `/usage`** — per-loop run count, total tokens, tokens/run, last-run timestamp | 🔶 | 🔶 | Verified from code: `orch_ledger.jsonl` already records every `/loop` iteration (`looping.rs::append_loop_ledger`, `phase: "loop"`, per-iteration token deltas, a truncated `task_preview`), and `/stats` (`session/ux.rs::handle_stats`) aggregates the ledger into run counts, tokens, and duration. But the aggregation key is `role · provider/model`, and every `/loop` record hardcodes `role: "session"` — so runs from *different* `/loop` tasks collapse into one bucket instead of breaking out per task the way CC's new view does. The raw per-record data needed for a real fix (`run_id`, `task_preview`) already exists in the ledger; this is a grouping/display change, not new instrumentation. **This week's pick** — see [digest](upstream-watch/2026-W36.md). |
| Hard spend limit / budget cap enforcement | ✅ spend-limit bar (v2.1.251, 2026-08-28, alongside Enterprise usage credits) | ❓ unverified | ❌ | New row, 2026-08-31. Verified from code: no spend-cap concept exists anywhere in the `goose` or `goose-cli` crates today — `/usage`/`/stats` report spend after the fact, nothing enforces a ceiling. Low urgency (gooseherd has no hosted billing surface to protect), but worth a mention if a "stop /orch when ledger cost crosses $N" flag ever gets requested. |
| Pilot run before scaling (measure cost on a small slice first) | ✅ (guidance) | ✅ (guidance) | 🔶 | `/arena` is the natural pilot harness; not yet framed that way in docs. |

## 4. Daily-driver table stakes

Habits CC/Codex users expect on day one (quality bar: no "wait, where is…?" moments).

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Shell passthrough (`!cmd`) | ✅ | ✅ | ✅ | |
| Project bootstrap (`/init`) | ✅ | ✅ | ✅ | |
| Memory (`/remember`, CLAUDE.md/AGENTS.md) | ✅ | ✅ | ✅ | |
| Session resume / fork / import | ✅ | ✅ | ✅ | Includes importing CC/Codex jsonl transcripts. Minor nuance from CC's `/fork` (v2.1.212, 2026-07-16): forking spins off a *background* session while the original keeps running in the foreground. gooseherd's `--fork` (`session/builder.rs`) copies and resumes, but needs verification on whether the original session stays live in parallel — flagging for confirmation, not downgrading. |
| Message queueing while a turn streams | ✅ | ❌ | ✅ | |
| Live status/slash commands mid-turn | 🔶 | ❌ | ✅ | `/status /stats /usage /roles /btw` during streaming |
| Plan mode | ✅ | 🔶 | ✅ `/plan` | |
| Skills | ✅ | 🔶 prompts, gaining ground | 🔶 `/skills` | Codex added a macOS "record a workflow, turn it into a reusable skill" capability (Record & Replay, rolled out to EU/UK/Switzerland, Aug 2026 per `developers.openai.com/codex/workflows`) — narrows the gap with CC's skill authoring, though it's demonstration-based rather than SKILL.md/frontmatter-based. gooseherd's own coverage vs. CC's skill triggers/frontmatter is still unverified — needs a pass. |
| Hooks (pre/post tool-use automation) | ✅, + new `PreModelSwitch`/`PostModelSwitch` events (v2.1.251, 2026-08-28, for blocking/confirming/annotating model switches) | ❌ | ✅ | Shipped (`crates/goose/src/hooks/mod.rs`): the same CC-compatible lifecycle events (`PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `Stop`, plus a few extras), configured in `config.yaml`, documented in [README.md](../README.md). No model-switch hook event exists yet — `/roles`/`/preset` switch models silently — small, low-urgency gap. |
| Permission allowlists | ✅ | ✅ | 🔶 | Verified from code: `GOOSE_ORCH_IMPLEMENT_POLICY=allowlist` + `GOOSE_ORCH_ALLOWED_COMMANDS` (`crates/goose/src/acp/provider.rs`; see [reference.md](reference.md), [SECURITY.md](../SECURITY.md)) confines the headless `/orch` implementer to the workspace and an approved command list, rejecting shell-chaining metacharacters. Narrower than CC's general per-tool `settings.json` allow/deny/ask rules that apply across any interactive or headless tool call — gooseherd's is orch-implementer-specific, not a session-wide user-facing allowlist. |
| Hard-lockdown session mode (one flag strips exec/code-run and WebFetch tools, confines file tools to the working directory, refuses `bypassPermissions`, and ignores user/project/local settings files — for untrusted code or freshly-cloned repos) | ✅ `--restricted` / `CLAUDE_CODE_RESTRICTED=1` (v2.1.248, 2026-08-27) | ❓ unverified (not found in Codex changelog this review) | ❌ | New row, 2026-08-31. gooseherd's closest analogs are narrower and don't compose into one switch: the Plan-Explore policy locks planning/review roles to read-only, and `GOOSE_ORCH_IMPLEMENT_POLICY=allowlist` locks the `/orch` implementer to a command allowlist — neither strips WebFetch/exec outright or ignores config files for a whole session. Relevant because gooseherd's headless `/orch` implementer already runs against arbitrary tasks and repos; a `--restricted`-equivalent would harden that path further. Not this week's pick (touches the ACP permission policy engine, bigger than a single cycle), but a strong candidate for a future one. |
| Onboarding doctor | 🔶 `/doctor` | ❌ | ✅ `goose herd` | Check-up with fixes and role auto-config |
| Learned permission classifier (replaces static allow/deny rules with a model call judging each tool use for irreversible/destructive/out-of-bounds risk) | ✅ auto mode | 🔶 `--approve-for-me` | ❌ | [Auto mode is now the default in Claude Code for Pro, Max, and Team plans](https://claude.com/blog/auto-mode-default-in-claude-code) (2026-08-07) — CC's classifier reportedly catches 89% of dangerous commands in testing vs. 13.6% for manual human review. Codex's `--approve-for-me` (`openai/codex#36373`) routes `on-request` approvals through "automatic review" under the `workspace-write` sandbox instead of a blanket allow — closer to a judged layer than plain sandboxing, but whether it's a learned classifier (like CC) or a simpler heuristic remains **unverified**. gooseherd's permission layers (Plan-Explore tool-kind policy, orch implement allowlist) remain static rule sets — still ❌, still this doc's oldest open gap (picked in [2026-W33](upstream-watch/2026-W33.md) and [2026-W34](upstream-watch/2026-W34.md)). |
| Plugin/extension marketplace with scoped catalogs (local/personal/workspace/remote discovery) | ✅ marketplaces | ✅ Agent Plugins | ❌ | Codex shipped "Agent Plugins" (Aug 2026) — portable plugins searchable across local, personal, workspace, and remote catalogs — joining CC's existing plugin marketplace support. Verified from code: gooseherd's extension system (`crates/goose/src/config/extensions.rs`, `crates/goose/src/builtin_extension.rs`) is flat config-entry MCP servers and builtins — no discovery, no scoping, no marketplace concept. Real gap, but low urgency: gooseherd's MCP-first design means "install a marketplace plugin" is mostly "add an MCP server entry" already; the missing piece is discoverability, not capability. |

## 5. Multi-agent coordination

Both vendors have shipped native primitives for *already-running* agents to coordinate with each other — a different thing from gooseherd's `/orch`/`/arena`, which spawn and coordinate roles within a single run.

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Live inter-agent messaging (a running session can address and message another running session directly, mid-task; async notify-on-idle) | ✅ `SendMessage` / `ListAgents` / `@`-mention addressing / `notify_when_idle` | ❌ (not found in Codex changelog/workflows as of this review) | 🔶 (needs verification) | Shipped across three incremental CC releases: cross-session `SendMessage`/`ListAgents` (v2.1.224, 2026-08-07), `@`-mention addressing (v2.1.232, 2026-08-13), `notify_when_idle` opt-in idle handoff (v2.1.236, 2026-08-19) — see [docs](https://code.claude.com/docs/en/cross-session-messaging). Verified from code: gooseherd has a structurally similar surface (`crates/goose/src/agents/platform_extensions/orchestrator.rs` exposes `ListSessions`, `ViewSession`, `StartAgent`, `SendMessage`, `InterruptAgent` tools). **Unverified**: whether these discover/message sessions started as *separate* `goose` processes (CC's model) or only sub-agents spawned within one `AgentManager`; no `notify_when_idle`-equivalent exists either way. Picked in [2026-W35](upstream-watch/2026-W35.md); still needs a code-tracing pass to resolve, not repeated as this week's pick. |
| Subagent execution forks the parent conversation with full inheritance by default | ✅ (v2.1.232) | ❓ unverified | 🔶 | CC's subagent forking now inherits the full parent conversation by default (previously scoped/summarized). gooseherd's `summon` subagents get task-scoped context via `TaskConfig`/recipe templates (`build_recipe_from_template`), not the full parent conversation automatically — closer to CC's old behavior than its new one. |

## 6. gooseherd differentiators

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
- https://raw.githubusercontent.com/anthropics/claude-code/main/CHANGELOG.md
- https://developers.openai.com/codex/changelog and https://developers.openai.com/codex/workflows
- https://openai.com/news/ (Codex-tagged posts)

## Process

1. Weekly routine produces a digest: *new post/change → pattern → row here (new or status change) → gap ticket if ❌/🔶*.
2. One gap per week gets picked into the `/orch` self-improvement loop, alongside friction-log items from daily-driver use.
3. Status only moves to ✅ when the feature passes the quality bar: a CC/Codex user hits no dead end and no raw error.
4. **Each week's PR stacks on the previous week's branch** (not on `docs/upstream-parity` or `main`) until a prior week's PR actually merges — see the note at the top of this document for what happens when that rule is skipped.
