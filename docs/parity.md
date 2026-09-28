# Upstream Parity Matrix

This document tracks agentic-coding patterns published first-hand by the **Claude Code** and **Codex** teams, and how gooseherd covers them. It exists so that "did we keep up?" is a diff, not a feeling.

- Rows come from first-party sources only (posts, docs, changelogs written by the two teams — see [Watch list](#watch-list)).
- A weekly upstream-watch routine scans the sources, and new patterns land here as new rows or status changes.
- Status legend: ✅ covered · 🔶 partial (note says what's missing) · ❌ gap · ➖ intentionally out of scope

Last reviewed: **2026-09-28** (baseline: [Claude Opus 5.5 is built for coding sessions that use more context](https://claude.com/blog/claude-opus-5-5-built-for-coding-sessions-that-use-more-context), 2026-09-24)

## 1. Loop primitives

Source: [Getting started with loops](https://claude.com/blog/getting-started-with-loops) — the Claude Code team's taxonomy of agent loops.

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Turn-based agentic loop (prompt → work → verify → respond) | ✅ | ✅ | ✅ | Core goose session loop |
| Goal-based loop (run until a stated condition holds) | ✅ `/goal` | 🔶 (prompt-level) | 🔶 `/orch` | `/orch` loops plan→implement→review until reviewer VERDICT or `--max-cycles`. Gap: CC uses a **separate small model** as the condition judge; gooseherd's expensive reviewer doubles as judge. A cheap dedicated judge role would cut cost per cycle. |
| Time-based local loop (interval re-run) | ✅ `/loop 5m …` | ❌ | ❌ | Workaround today: cron + `goose orch -t --max-cycles`. Candidate: `goose orch --every 5m` or a `/loop` command. |
| Scheduled cloud loop | ✅ `/schedule` | ✅ cloud tasks | ➖ | gooseherd is local-first; document the cron + headless recipe instead. |
| Proactive loop (event/schedule-triggered, no human in the loop) | ✅ (composition of primitives) | ✅ Codex Automations | ❌ | **Correction.** Last week's edit reverted Codex to 🔶; that was a regression from re-deriving the file instead of carrying the prior review forward (see [Process](#process)). Codex Automations (schedule- and event-triggered background runs with thread continuity and a review queue, shipped ~Aug 2026) earns ✅, matching CC's `/schedule` + `/goal` composition. gooseherd equivalent would still compose cron + headless orch + a goal condition — no such recipe is documented yet. |
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
| Skills | ✅ | 🔶 prompts | 🔶 `/skills` | Coverage vs CC's skill triggers/frontmatter unverified — needs a pass. Claude Code also began syncing an account's claude.ai skills/plugins straight into terminal sessions (changelog v2.1.275, ~Aug 2026), widening the gap further. **Updated this review:** [Build plugins for Claude](https://claude.com/blog/build-plugins-for-claude) (2026-09-25) documents an authoring path from a Claude Code skill to a published plugin — CC skills now have a packaging story gooseherd's `/skills` has no analog for. |
| Frontier model currency (planner/reviewer roles can target the vendors' current top model by canonical name) | n/a | n/a | ❌ | **Restored + updated this review** — this row was raised in an earlier weekly PR (#21, 2026-08-03) flagging missing `claude-opus-5`/GPT-5.6 entries, then lost when a later week rebuilt this file from the stale `docs/upstream-parity` baseline instead of carrying it forward (see [Process](#process)). Now further behind: CC shipped **Claude Opus 5.5** as its new default coding model (changelog v2.1.280; [launch post](https://claude.com/blog/claude-opus-5-5-built-for-coding-sessions-that-use-more-context), 2026-09-24 — 1M context, $4/$20 per Mtok, positioned for long, high-context coding sessions). Verified directly against `crates/goose-provider-types/src/canonical/data/canonical_models.json`: it stops at `claude-opus-4.8`/`claude-sonnet-4.6` plus a bare `claude-sonnet-5`, with **no `claude-opus-5` or `claude-opus-5-5` entry at all**. `/orch`'s planner/reviewer roles cannot address the model upstream now treats as default. Secondary reports (openai.com blocked to direct fetch this week, so unverified first-hand) also describe Codex model-picker additions (GPT-6 "Sol"/"Luna"); worth the same registry check once corroborated. |
| Plugin marketplace (discover/install third-party agents, skills, connectors from a central catalog) | ✅ Claude Marketplace | 🔶 (Agent Plugins, aggregated app plugins) | ❌ | **Restored + updated this review** — also raised previously (PR #22, 2026-08-17) and lost in the same rebuild. Claude Code's plugin/skill catalog graduated to [**Claude Marketplace**](https://claude.com/blog/claude-marketplace) (2026-09-23): one surface for plugins, agents and partner services (Snowflake/Vercel case study same day). Verified via repo search: no marketplace, plugin-catalog, or extension-discovery code exists under `crates/` — extensions are still config-file-declared one at a time (`crates/goose/src/config/extensions.rs`). |
| Hooks (pre/post tool-use automation) | ✅ | ✅ (GA) | 🔶 | **Correction** — verified directly against `crates/goose/src/hooks/mod.rs`: gooseherd does ship lifecycle hooks (an earlier week correctly marked this 🔶/✅; a later rebuild wrongly reverted it to ❌, see [Process](#process)). Scope is narrower than CC/Codex: hooks live in `<plugin-root>/hooks/hooks.json` and only fire for events a plugin subscribes to — there's no session-wide, non-plugin hook surface. Codex Hooks reached GA 2026-05-14. |
| Permission allowlists | ✅ (Auto Mode) | ✅ | 🔶 | **Correction** — verified directly against `crates/goose/src/config/permission.rs`: a general, session-wide `permission.yaml` (`always_allow`/`ask_before`/`never_allow`) already exists, plus `GOOSE_ORCH_ALLOWED_COMMANDS`/`allowlist` scoped to the headless `/orch` implementer. A prior week correctly marked this 🔶; a later rebuild wrongly dropped it to ❌ (see [Process](#process)). Bar still moved further away, though: Claude Code's Auto Mode (default for Pro/Max/Team, flip 2026-08-14) is a **classifier** over natural-language `permissions.deny`/`permissions.ask` rules, not a static tool-name list — gooseherd's `permission.yaml` is tool-name-level only, no command/path-pattern matching and no learned classifier. On the v1.43 list. |
| Task/agent dashboard (search, start, open, rename, stop running background tasks) | 🔶 (VS Code panel) | ✅ | ❌ | **New this review.** Codex added an interactive agents dashboard for managing local/remote tasks (~Aug 2026 changelog). gooseherd has `GOOSE_MAX_BACKGROUND_TASKS` for concurrent subagent tasks but no command or panel to list/open/rename/stop them individually — needs verification against the CLI, but no such surface was found in the codebase this pass. |
| Onboarding doctor | 🔶 `/doctor` | ❌ | ✅ `goose herd` | Check-up with fixes and role auto-config |

## 5. Multi-agent coordination

**Restored this review** — this section was added by an earlier weekly PR (#24, 2026-08-24), then dropped when a later week rebuilt the file from the stale `docs/upstream-parity` baseline (see [Process](#process)).

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Cross-session agent messaging (sessions discover and message each other while running) | ✅ `SendMessage`/`ListAgents`/`notify_when_idle` | ❌ | 🔶 | gooseherd has a structurally similar surface — `ListSessions`/`StartAgent`/`SendMessage`/`InterruptAgent`/`ViewSession` in `crates/goose/src/agents/platform_extensions/orchestrator.rs` (verified present) — but it's unverified whether it addresses independently-running `goose` processes the way CC's does, or has a `notify_when_idle` equivalent. Marked needs-verification, not claimed as parity. |
| Subagent forking with full conversation inheritance by default | ✅ | ➖ | 🔶 | gooseherd's `summon.rs` spawns subagents synchronously within one run rather than forking a live session with inherited history; `/orch` roles instead coordinate via the shared workspace + ledger. Different mechanism, comparable outcome for the orch use case — not a clean ✅/❌. |

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
- https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md
- https://developers.openai.com/codex/changelog and https://developers.openai.com/codex/workflows
- https://openai.com/news/ (Codex-tagged posts)

## Process

1. Weekly routine produces a digest: *new post/change → pattern → row here (new or status change) → gap ticket if ❌/🔶*.
2. One gap per week gets picked into the `/orch` self-improvement loop, alongside friction-log items from daily-driver use.
3. Status only moves to ✅ when the feature passes the quality bar: a CC/Codex user hits no dead end and no raw error.
4. **Before touching this file, check for open `upstream-watch/*` PRs and branches and read the most recent one.** `docs/parity.md` has repeatedly failed to land on `main` (11 open, unmerged `upstream-watch/*` PRs as of 2026-09-28 — #18 through #28); several of those weeks re-derived the whole file from the stale `docs/upstream-parity` branch (frozen at the 2026-07-07 baseline) instead of stacking on the latest open PR, which silently reverted corrections and dropped whole rows (the "Frontier model currency" and "Plugin marketplace" rows above were each lost this way at least once). Branch from the most recent `upstream-watch/*` branch, not from `main` or `docs/upstream-parity`, whichever is more current — and flag it loudly in the PR description if that means superseding open PRs. A human still needs to actually merge one of these and close the rest; that hasn't happened yet.
