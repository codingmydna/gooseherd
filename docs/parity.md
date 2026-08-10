# Upstream Parity Matrix

This document tracks agentic-coding patterns published first-hand by the **Claude Code** and **Codex** teams, and how gooseherd covers them. It exists so that "did we keep up?" is a diff, not a feeling.

- Rows come from first-party sources only (posts, docs, changelogs written by the two teams — see [Watch list](#watch-list)).
- A weekly upstream-watch routine scans the sources, and new patterns land here as new rows or status changes.
- Status legend: ✅ covered · 🔶 partial (note says what's missing) · ❌ gap · ➖ intentionally out of scope

Last reviewed: **2026-08-10** (baseline: [Auto mode is now the default in Claude Code for Pro, Max, and Team plans](https://claude.com/blog/auto-mode-default-in-claude-code), 2026-08-07)

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
| Per-run/agent accounting with stop options | ✅ `/workflows` | 🔶 | ✅ | `orch_ledger.jsonl` + `/stats` (per role: model, tokens, time, verdict) |
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
| Skills | ✅ | 🔶 prompts | 🔶 `/skills` | Coverage vs CC's skill triggers/frontmatter unverified — needs a pass. |
| Hooks (pre/post tool-use automation) | ✅ | ❌ | ✅ | Shipped (`crates/goose/src/hooks/mod.rs`): the same CC-compatible lifecycle events (`PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `Stop`, plus a few extras), configured in `config.yaml`, documented in [README.md](../README.md). This row was stale — verified from code, no new upstream post prompted it. |
| Permission allowlists | ✅ | ✅ | 🔶 | `GOOSE_ORCH_IMPLEMENT_POLICY=allowlist` + `GOOSE_ORCH_ALLOWED_COMMANDS` (see [reference.md](reference.md), [SECURITY.md](../SECURITY.md)) confines the headless `/orch` implementer to the workspace and an approved command list, rejecting shell-chaining metacharacters. Narrower than CC's general per-tool `settings.json` allow/deny/ask rules that apply across any interactive or headless tool call — gooseherd's is orch-implementer-specific, not a session-wide user-facing allowlist. Also stale from code inspection, not a new post. |
| Onboarding doctor | 🔶 `/doctor` | ❌ | ✅ `goose herd` | Check-up with fixes and role auto-config |
| Learned permission classifier (replaces static allow/deny rules with a model call judging each tool use for irreversible/destructive/out-of-bounds risk) | ✅ auto mode | ➖ (sandboxed exec instead) | ❌ | New this cycle: [Auto mode is now the default in Claude Code for Pro, Max, and Team plans](https://claude.com/blog/auto-mode-default-in-claude-code) (2026-08-07) — CC's classifier reportedly catches 89% of dangerous commands in testing vs. 13.6% for manual human review, and Anthropic stopped charging the classifier's token overhead. gooseherd's permission layers (Plan-Explore tool-kind policy, orch implement allowlist) are both static rule sets, not a learned judge. Gap, not urgent — Codex explicitly doesn't do this either, favoring OS-level sandboxing — but worth a bench before dismissing. |

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
