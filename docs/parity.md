# Upstream Parity Matrix

This document tracks agentic-coding patterns published first-hand by the **Claude Code** and **Codex** teams, and how gooseherd covers them. It exists so that "did we keep up?" is a diff, not a feeling.

- Rows come from first-party sources only (posts, docs, changelogs written by the two teams — see [Watch list](#watch-list)).
- A weekly upstream-watch routine scans the sources, and new patterns land here as new rows or status changes.
- Status legend: ✅ covered · 🔶 partial (note says what's missing) · ❌ gap · ➖ intentionally out of scope

Last reviewed: **2026-08-03** (this week: Claude Code CHANGELOG v2.1.202–2.1.220, `whats-new` weekly notes for W28/W29, and the Codex/GPT-5.6 rollout — see [digest](upstream-watch/2026-W32.md))

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
| Session/loop primitive split: interactive fork vs. background-session fork | ✅ `/fork` now spins a real background session (`claude agents`); old behavior renamed `/subtask` (CHANGELOG v2.1.212–213, 2026-07-16/17) | 🔶 (Codex Remote background sessions, GA'd pre-cutoff) | 🔶 `/arena` worktrees are the closest analog | gooseherd's `/arena` already isolates parallel runs in worktrees; there's no lightweight "peel this session into a tracked background run and keep chatting in the original" primitive outside orch/arena. Needs verification against current `/fork`-equivalent (if any) before claiming ✅ or 🔶. |

## 2. Verification and quality

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Verification skills — a SKILL.md defines what "done" means (start server, exercise UI, check console) | ✅ | 🔶 (AGENTS.md conventions) | 🔶 | Playbooks exist (`profiles/fable5-playbook.md`, personal playbooks via `GOOSE_PLAYBOOK_PATH`) but there is no standardized "verification" section the roles are told to honor. |
| Independent second-pass review by a separate agent | ✅ `/code-review` (moved to a background subagent in v2.1.218, 2026-07-22, so review no longer fills the main thread) | ✅ (Codex reviews 100% of OpenAI PRs) | ✅ | Structural: the reviewer role in `/orch` is a different model with read-only powers. Note: CC also stopped auto-invoking `/verify`/`/code-review` as of v2.1.215 (2026-07-19) — reviews now require explicit invocation, which is already how gooseherd's machine gates + reviewer role work. No status change; logged as confirming gooseherd's existing explicit-invocation design is the right call, not a gap. |
| Read-only enforcement for planning/review roles | ✅ plan mode | ✅ read-only sandbox | ✅ | Plan-Explore policy: read/search/think approved, writes rejected; double defense via claude plan mode / codex read-only sandbox. |
| Numeric success criteria work best (test counts, score thresholds) | ✅ (guidance) | ✅ (guidance) | 🔶 | Nothing stops it, but playbooks don't yet teach users to phrase orch tasks with numeric exit criteria. Doc fix. See also `docs/writing-orch-tasks.md`. |
| AI-driven security-scanning verification gate (build a threat model, reproduce suspected vulns in an isolated env, track findings across runs, gate CI on a severity threshold) | 🔶 (`/security-review` exists as an interactive skill; no evidence of an equivalent standalone CI-gating CLI) | ✅ **new**: Codex Security CLI open-sourced 2026-07-29 (`@openai/codex-security`, Apache-2.0) — repo scanner + threat model + reproduction + fix verification + CI severity gate | 🔶 | gooseherd's `.goose-gates.yaml` already supports wiring **any** exit-code CLI as a gate (`docs/reference.md#goose-gatesyaml`), so a security-scan gate is mechanically possible today — but there's no documented recipe or bundled example, and no verification that an external scanner's exit-code contract (pass/fail on severity threshold) round-trips cleanly through gooseherd's gate runner. **This week's pick — see digest.** |

## 3. Cost and token observability

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Per-session usage breakdown | ✅ `/usage` | 🔶 | ✅ | `/usage`, `/status` |
| Per-run/agent accounting with stop options | ✅ `/workflows` | 🔶 | ✅ | `orch_ledger.jsonl` + `/stats` (per role: model, tokens, time, verdict) |
| Pilot run before scaling (measure cost on a small slice first) | ✅ (guidance) | ✅ (guidance) | 🔶 | `/arena` is the natural pilot harness; not yet framed that way in docs. |
| Session-wide runaway-loop safety caps (hard limit on total subagent spawns / search calls per session, independent of any user-set iteration count; long tool calls auto-backgrounded) | ✅ **new**: 200-per-session default caps on WebSearch calls and subagent spawns, tunable via env vars, reset by `/clear`; MCP tool calls over 2 min auto-background (CHANGELOG v2.1.212, 2026-07-16) | ❓ not confirmed either way this week | ❌ | gooseherd's `/loop` has a user-set `max_iterations` and `/orch` has `--max-cycles`/`DEFAULT_MAX_CYCLES`, but nothing bounds *total* subagent spawns or tool calls within a single run independent of those user-chosen limits — a misbehaving implementer role could still spin sub-tool-calls unboundedly within one cycle. Distinct gap from the existing cycle caps; candidate for a future pick. |
| Explicit per-request speed/cost knob (e.g. "fast mode" at a fixed price multiplier, no intelligence change) | 🔶 (fast mode exists for the CLI harness generally, not a per-request API toggle) | ✅ **new**: GPT-5.6 Sol "Fast mode" — 2.5x speed at 2x price, shipped 2026-07-30 alongside an ~80%/20% price cut for Luna/Terra | ➖ | gooseherd delegates to whatever speed/cost knobs a provider exposes; no gooseherd-side change needed unless a provider adapter needs updating to pass the flag through. |

## 4. Daily-driver table stakes

Habits CC/Codex users expect on day one (quality bar: no "wait, where is…?" moments).

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Shell passthrough (`!cmd`) | ✅ | ✅ | ✅ | |
| Project bootstrap (`/init`) | ✅ | ✅ | ✅ | |
| Memory (`/remember`, CLAUDE.md/AGENTS.md) | ✅ | ✅ | ✅ | |
| Session resume / fork / import | ✅ | ✅ | ✅ | Includes importing CC/Codex jsonl transcripts. CC's `/fork` reworked into a tracked background-session primitive (2026-07-16/17) — see loop-primitives table above; gooseherd's equivalent needs a verification pass, not claimed ✅/🔶 without checking current CLI surface. |
| Message queueing while a turn streams | ✅ | ❌ | ✅ | |
| Live status/slash commands mid-turn | 🔶 | ❌ | ✅ | `/status /stats /usage /roles /btw` during streaming |
| Plan mode | ✅ | 🔶 | ✅ `/plan` | |
| Skills | ✅ | 🔶 prompts | 🔶 `/skills` | Coverage vs CC's skill triggers/frontmatter unverified — needs a pass. |
| Hooks (pre/post tool-use automation) | ✅ | ❌ | ❌ | Not requested yet; watch for demand from migrating users. |
| Permission allowlists | ✅ | ✅ | ❌ | On the v1.43 list. |
| Onboarding doctor | 🔶 `/doctor` | ❌ | ✅ `goose herd` | Check-up with fixes and role auto-config |
| Non-visual/accessible TUI output (screen reader mode) | ✅ **new**: `--ax-screen-reader` / `axScreenReader` — linear plain-text output, no redraws (v2.1.208, 2026-07-13) | ❓ not confirmed either way this week | ❌ | Not evaluated before; low-traffic gap unless accessibility demand surfaces. Logged for completeness, not urgent. |
| Frontier/latest-model currency in the model registry (can the harness actually address the newest planner/reviewer-class models by name) | n/a (CC ships its own model access) | n/a (Codex ships its own model access) | ❌ **gap, verified against code** | `crates/goose-provider-types/src/canonical/data/canonical_models.json` has no `claude-opus-5` entries (newest present: `claude-opus-4.7`/`4.8` across vendors) and no GPT-5.6 tier (`sol`/`terra`/`luna`; newest present: `gpt-5.5`). Opus 5 became CC's default Opus model 2026-07-24; GPT-5.6 reached general availability 2026-07-09 and is Codex's now-recommended replacement for the GPT-5.4 line (deprecating 2026-08-31). Since gooseherd's whole premise is routing to the current frontier model for planner/reviewer roles, an unresolvable model ID silently defeats that premise. |

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

Note (2026-08-03): `claude.com/blog`, `developers.openai.com/codex/*`, and `openai.com/news` were unreachable directly this week (proxy/WAF blocks) and were covered via WebSearch instead; `anthropic.com/engineering` was reachable but had nothing published since the 2026-07-07 baseline. `developers.openai.com/codex/workflows` specifically could not be confirmed changed or unchanged — flagged for a manual check next week.

## Process

1. Weekly routine produces a digest: *new post/change → pattern → row here (new or status change) → gap ticket if ❌/🔶*.
2. One gap per week gets picked into the `/orch` self-improvement loop, alongside friction-log items from daily-driver use.
3. Status only moves to ✅ when the feature passes the quality bar: a CC/Codex user hits no dead end and no raw error.
