# Upstream Parity Matrix

This document tracks agentic-coding patterns published first-hand by the **Claude Code** and **Codex** teams, and how gooseherd covers them. It exists so that "did we keep up?" is a diff, not a feeling.

- Rows come from first-party sources only (posts, docs, changelogs written by the two teams — see [Watch list](#watch-list)).
- A weekly upstream-watch routine scans the sources, and new patterns land here as new rows or status changes.
- Status legend: ✅ covered · 🔶 partial (note says what's missing) · ❌ gap · ➖ intentionally out of scope

Last reviewed: **2026-09-14** (this review: [Auto mode becomes the default](https://code.claude.com/docs/en/whats-new/2026-w32), 2026-08-14, and [Test plugins with evals](https://code.claude.com/docs/en/plugin-evals), ~2026-09-10)

## 1. Loop primitives

Source: [Getting started with loops](https://claude.com/blog/getting-started-with-loops) — the Claude Code team's taxonomy of agent loops.

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Turn-based agentic loop (prompt → work → verify → respond) | ✅ | ✅ | ✅ | Core goose session loop |
| Goal-based loop (run until a stated condition holds) | ✅ `/goal` | 🔶 (prompt-level) | 🔶 `/orch` | `/orch` loops plan→implement→review until reviewer VERDICT or `--max-cycles`. Gap: CC uses a **separate small model** as the condition judge; gooseherd's expensive reviewer doubles as judge. A cheap dedicated judge role would cut cost per cycle. |
| Time-based local loop (interval re-run) | ✅ `/loop 5m …` | ❌ | ✅ `/loop`, `goose loop -t --every` | Corrected this review: `docs/loops.md` and `goose loop -t "…" --every 5m --max N` already ship (verified in repo docs and `docs/reference.md` exit-code table). Previous ❌ was stale. |
| Scheduled cloud loop | ✅ `/schedule` | ✅ cloud tasks, now **Automations** (scheduled + event-triggered, same-thread continuity, review queue) | ➖ | gooseherd is local-first; document the cron + headless recipe instead. Codex Automations (OpenAI, ~2026-09) add cross-service triggers (Gmail/Slack/GitHub) and let a run resume its own thread later — worth a mention in the cron recipe doc even though gooseherd won't host this itself. |
| Proactive loop (event/schedule-triggered, no human in the loop) | ✅ (composition of primitives) | ✅ **Automations** (schedule or Gmail/Slack/GitHub trigger → Codex works in the background → lands in a review queue) | ❌ | Both upstreams now ship this natively; Codex moved from 🔶 to ✅ this review. gooseherd equivalent would compose cron + headless `goose orch`/`goose loop` + a goal condition — still requires the user to wire the trigger themselves. |
| Small-model routing inside loops (strong model only for judgment calls) | ✅ | ✅ | ✅ | This is gooseherd's founding premise: expensive planner/reviewer, cheap implementer. |

## 2. Verification and quality

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Verification skills — a SKILL.md defines what "done" means (start server, exercise UI, check console) | ✅ (`/run`, `/verify` bundled skills) | 🔶 (AGENTS.md conventions) | 🔶 | Playbooks exist (`profiles/fable5-playbook.md`, personal playbooks via `GOOSE_PLAYBOOK_PATH`) but there is no standardized "verification" section the roles are told to honor. |
| Skill/plugin eval harness — graded test cases, a no-plugin baseline (Δ), judge-scored graders, CI gate on score | ✅ `claude plugin eval` (new, ~2026-09-10, requires CC v2.1.269+) | ❌ (needs verification — not found in scanned sources) | ❌ | **New row.** CC's `claude plugin eval init` proposes cases/graders, runs each 3× with and without the plugin, and reports `WITH`/`W/OUT`/`Δ` so you can tell whether a skill actually helped. gooseherd's exemplar/playbook uplift has no equivalent ablation — `/stats` reports approval rate and cycles but never measures "with playbook vs. without" on the same task. Candidate: an `/arena`-style ablation mode that runs a task once with `GOOSE_ORCH_PLAYBOOK=always` and once with `never` and diffs the outcome. |
| Independent second-pass review by a separate agent | ✅ `/code-review` (now runs as a background subagent) | ✅ (Codex reviews 100% of OpenAI PRs) | ✅ | Structural: the reviewer role in `/orch` is a different model with read-only powers. |
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
| Session resume / fork / import | ✅ | ✅ | ✅ | Includes importing CC/Codex jsonl transcripts |
| Message queueing while a turn streams | ✅ | ❌ | ✅ | |
| Live status/slash commands mid-turn | 🔶 | ❌ | ✅ | `/status /stats /usage /roles /btw` during streaming |
| Plan mode | ✅ | 🔶 | ✅ `/plan` | |
| Skills | ✅ | 🔶 prompts | 🔶 `/skills` | Coverage vs CC's skill triggers/frontmatter unverified — needs a pass. |
| Hooks (pre/post tool-use automation) | ✅ (config-level, `settings.json`) | ❌ | 🔶 | Corrected this review: hooks are implemented (`crates/goose/src/hooks/mod.rs` — PreToolUse, PostToolUse, SessionStart/End, BeforeShellExecution, etc.), but they ship as a plugin's `hooks/hooks.json`, discovered via `discover_enabled_plugins`. The README's "same CC-compatible events, configured in config.yaml" oversells this: there's no session-level hook config outside a plugin. Doc/code pass needed to decide if that's the intended model or a gap. |
| Permission allowlists | ✅ (`/permissions`, per-command/path glob rules) | ✅ | 🔶 | Corrected this review: `permission.yaml` (`crates/goose/src/config/permission.rs`) persists `always_allow`/`ask_before`/`never_allow` per tool, surfaced via `configure`. Gap vs. CC: gooseherd's rules are tool-level, not command/path-pattern level (no `Bash(npm run *)` or `Read(./.env)` equivalent), and there's no CC-style deny-first `Tool(param:value)` matching. |
| Classifier-based auto-approval (a background model reviews risky actions instead of manual per-action approval, or a static allowlist) | ✅ **auto mode**, default for new Pro/Max/Team sessions since 2026-08-14 | 🔶 (needs verification — not found in scanned sources) | ❌ | **New row.** CC's auto mode runs a second, cheaper model as a real-time classifier over every risky tool call (writes, shell, network); Anthropic reports it catches ~89% of planted dangerous commands vs. ~14% for humans rubber-stamping prompts. gooseherd's closest analog is the static `GOOSE_ORCH_IMPLEMENT_POLICY=allowlist` (command allowlist, orch-only) plus the `permission.yaml` tool-level rules above — neither is a per-action judgment call by a model. This is the most direct instance of "small-model routing" applied to safety rather than cost, and gooseherd has no equivalent for interactive sessions. |
| Onboarding doctor | 🔶 `/doctor` (now `/checkup` alias, full fix-it checkup) | ❌ | ✅ `goose herd` | Check-up with fixes and role auto-config |

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
- https://code.claude.com/docs/en/whats-new (dated weekly feature digest — more reliable than the raw CHANGELOG for "what changed and when"; added this review)
- https://developers.openai.com/codex/changelog and https://developers.openai.com/codex/workflows
- https://developers.openai.com/codex/app/automations (Codex Automations — added this review)
- https://openai.com/news/ (Codex-tagged posts)

Note (2026-09-14): this review's environment could reach `code.claude.com` directly but not `claude.com`, `www.anthropic.com`, `openai.com`, or `developers.openai.com` (network egress policy blocked those domains). The Codex Automations and Agents API items below are corroborated by web search across multiple independent outlets rather than a direct fetch of the OpenAI page — re-verify against the primary URL once this routine (or a human) has access to those domains.

## Process

1. Weekly routine produces a digest: *new post/change → pattern → row here (new or status change) → gap ticket if ❌/🔶*.
2. One gap per week gets picked into the `/orch` self-improvement loop, alongside friction-log items from daily-driver use.
3. Status only moves to ✅ when the feature passes the quality bar: a CC/Codex user hits no dead end and no raw error.
