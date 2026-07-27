# Upstream Parity Matrix

This document tracks agentic-coding patterns published first-hand by the **Claude Code** and **Codex** teams, and how gooseherd covers them. It exists so that "did we keep up?" is a diff, not a feeling.

- Rows come from first-party sources only (posts, docs, changelogs written by the two teams — see [Watch list](#watch-list)).
- A weekly upstream-watch routine scans the sources, and new patterns land here as new rows or status changes.
- Status legend: ✅ covered · 🔶 partial (note says what's missing) · ❌ gap · ➖ intentionally out of scope

Last reviewed: **2026-07-27** (baseline: [Building verification loops in Claude Code with skills](https://claude.com/blog/building-verification-loops-in-claude-code-with-skills), 2026-07-22; also covers [The new rules of context engineering for Claude 5 generation models](https://claude.com/blog/the-new-rules-of-context-engineering-for-claude-5-generation-models), 2026-07-24, and Claude Code CHANGELOG entries 2.1.204–2.1.220)

## 1. Loop primitives

Source: [Getting started with loops](https://claude.com/blog/getting-started-with-loops) — the Claude Code team's taxonomy of agent loops.

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Turn-based agentic loop (prompt → work → verify → respond) | ✅ | ✅ | ✅ | Core goose session loop |
| Goal-based loop (run until a stated condition holds) | ✅ `/goal` | 🔶 (prompt-level) | ✅ `/goal` | Shipped 2026-07-09. `/goal --check "<cmd>"` uses a deterministic exit-code check; without `--check`, a **dedicated** `GOOSE_EVALUATOR_PROVIDER`/`GOOSE_EVALUATOR_MODEL` role judges, falling back to the reviewer only if unset. Closes the 07-07 gap (reviewer doubling as judge). `/orch` itself still loops plan→implement→review until VERDICT or `--max-cycles`, which is the orch-specific case of this pattern. |
| Time-based local loop (interval re-run) | ✅ `/loop 5m …` | ❌ | ✅ `/loop` | Shipped 2026-07-09: `/loop` and headless `goose loop -t "…" --every 5m --max N` (`docs/loops.md`). |
| Scheduled cloud loop | ✅ `/schedule` | ✅ cloud tasks | ➖ | gooseherd is local-first; document the cron + headless recipe instead. |
| Proactive loop (event/schedule-triggered, no human in the loop) | ✅ (composition of primitives) | 🔶 | 🔶 | `docs/loops.md` (2026-07-09/10) now documents a concrete cron + `goose loop`/`goose goal --check` recipe ("Event-Driven Scheduling") for schedule-triggered, human-out-of-the-loop runs. Gap: no native `/schedule`-equivalent — still roadmap, users hand-roll the cron wrapper themselves. |
| Small-model routing inside loops (strong model only for judgment calls) | ✅ | ✅ | ✅ | This is gooseherd's founding premise: expensive planner/reviewer, cheap implementer. |

## 2. Verification and quality

Source: [Building verification loops in Claude Code with skills](https://claude.com/blog/building-verification-loops-in-claude-code-with-skills), 2026-07-22 — verification loops can be standalone (deliberately invoked, for cross-cutting checks like security/accessibility scans), embedded, chained, or PR-tied; a 6-phase `/verify` (build, typecheck, lint, test+coverage, security scan, diff review) yields a pass/fail report; spec validation checks a change against a markdown spec in the repo; Managed Agents (beta) can rubric-grade an outcome via a **separate grader agent**; the same checks wire into GitHub Actions to run on every push/PR.

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Verification skills — a SKILL.md defines what "done" means (start server, exercise UI, check console) | ✅ | 🔶 (AGENTS.md conventions) | 🔶 | `.goose-gates.yaml` now auto-derives from `package.json`/`go.mod` when absent (shipped 2026-07-10), and review rubric + exemplar injection landed for the reviewer role (2026-07-08) — closer than 07-07, still gap: no spec-validation-against-markdown-spec pattern, and no GH Actions recipe wiring gates/orch into CI (verified: `.github/workflows/` only runs gooseherd's own cargo fmt/clippy/test, nothing invokes `.goose-gates.yaml` or `orch` on a *target* repo). |
| Rubric-graded verification via an independent grader agent (separate from the main reviewer) | 🔶 (Managed Agents, beta) | ❌ (not found in scanned sources) | ❌ | `/orch`'s reviewer gets an injected rubric (2026-07-08) but is the *same* role/model that also holds plan/exemplar context — not a separate lightweight grader. New pattern from the 07-22 verification-loops post; worth tracking as CC's Managed Agents rubric grading matures out of beta. |
| Independent second-pass review by a separate agent | ✅ `/code-review` | ✅ (Codex reviews 100% of OpenAI PRs) | ✅ | Structural: the reviewer role in `/orch` is a different model with read-only powers. CC moved `/code-review` to run as a background subagent (2.1.218, 2026-07-22) — non-blocking; gooseherd's `/orch` review phase still blocks (see Daily-driver table). |
| Read-only enforcement for planning/review roles | ✅ plan mode | ✅ read-only sandbox | ✅ | Plan-Explore policy: read/search/think approved, writes rejected; double defense via claude plan mode / codex read-only sandbox. |
| Numeric success criteria work best (test counts, score thresholds) | ✅ (guidance) | ✅ (guidance) | ✅ | Shipped 2026-07-09: `docs/writing-orch-tasks.md` codifies numbered, testable "Completion criteria" as part 5 of its 6-part task anatomy, and `docs/loops.md` examples all pair `--check`/gates with concrete commands. Closes the 07-07 doc gap. |
| Context/prompt engineering discipline — judgment over exhaustive rule-lists, self-describing tool interfaces over few-shot examples | ✅ (guidance) | 🔶 (implicit in AGENTS.md conventions) | ✅ | New pattern from [The new rules of context engineering for Claude 5 generation models](https://claude.com/blog/the-new-rules-of-context-engineering-for-claude-5-generation-models), 2026-07-24 (CC trimmed 80%+ of its own system prompt for Opus 5/Fable 5 with no eval regression). Verified: `profiles/fable5-playbook.md` is already short (55 lines) and phrased as numbered principles/heuristics ("prefer the minimal edit," "diagnose before retrying"), not an imperative rule list — gooseherd already matches this pattern by construction; worth an explicit doc callout so it stays intentional as playbooks grow. |

## 3. Cost and token observability

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Per-session usage breakdown | ✅ `/usage` | 🔶 | ✅ | `/usage`, `/status` |
| Per-run/agent accounting with stop options | ✅ `/workflows` | 🔶 | ✅ | `orch_ledger.jsonl` + `/stats` (per role: model, tokens, time, verdict) |
| Pilot run before scaling (measure cost on a small slice first) | ✅ (guidance) | ✅ (guidance) | 🔶 | `/arena` is the natural pilot harness; not yet framed that way in docs. |
| Hard per-session spawn/tool-call caps (runaway-loop safety net) | ✅ | ❌ (not found in scanned sources) | 🔶 | New from CHANGELOG 2.1.212 (2026-07-17): `/subtask` replaces ad-hoc in-session subagent spawning, plus session-wide hard caps — `CLAUDE_CODE_MAX_SUBAGENTS_PER_SESSION` and `CLAUDE_CODE_MAX_WEB_SEARCHES_PER_SESSION`, both default 200 — specifically to stop runaway delegation/search loops. gooseherd has composable but not equivalent limits (verified): `GOOSE_MAX_TURNS` (session, default 1000, `agent.rs`), `GOOSE_SUBAGENT_MAX_TURNS` (per-subagent, default 25), `GOOSE_MAX_BACKGROUND_TASKS` (concurrent, default 5, `summon.rs`) — no single "total subagents/tool-calls this session" ceiling. See this week's pick. |

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
| Background/non-blocking execution for long-running agent work | ✅ | 🔶 (cloud tasks are inherently async; local CLI is foreground) | ❌ | New trend across CHANGELOG 2.1.198→2.1.218 (Jul 1–22): subagents background-by-default, `/fork` spins a background session, `/code-review` moved to a background subagent (2.1.218, 2026-07-22). Verified: gooseherd's `/orch` runs strictly synchronously — `handle_orchestrate` is a plain `.await` from both the REPL and headless CLI paths (`orchestrate/runner.rs`), no `tokio::spawn`; a long plan→implement→gate→review run blocks the terminal for its full duration. (The unrelated `handle_async_delegate` subagent-tool mechanism in `summon.rs` *is* async, but `/orch` doesn't use it.) |

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
