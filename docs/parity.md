# Upstream Parity Matrix

This document tracks agentic-coding patterns published first-hand by the **Claude Code** and **Codex** teams, and how gooseherd covers them. It exists so that "did we keep up?" is a diff, not a feeling.

- Rows come from first-party sources only (posts, docs, changelogs written by the two teams — see [Watch list](#watch-list)).
- A weekly upstream-watch routine scans the sources, and new patterns land here as new rows or status changes.
- Status legend: ✅ covered · 🔶 partial (note says what's missing) · ❌ gap · ➖ intentionally out of scope

Last reviewed: **2026-10-19** (new: Claude Code changelog 2.1.297–2.1.303, notably the auto-mode classifier cluster; [Code Review is now generally available](https://claude.com/resources/articles/code-review-is-now-generally-available), 2026-10-15; [Agent observability on the Claude Platform](https://claude.com/resources/articles/agent-observability-on-the-claude-platform), 2026-10-14; Codex CLI 0.163.0, 2026-10-15)

## 1. Loop primitives

Source: [Getting started with loops](https://claude.com/blog/getting-started-with-loops) — the Claude Code team's taxonomy of agent loops.

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Turn-based agentic loop (prompt → work → verify → respond) | ✅ | ✅ | ✅ | Core goose session loop |
| Goal-based loop (run until a stated condition holds) | ✅ `/goal` | 🔶 (prompt-level) | 🔶 `/orch` | `/orch` loops plan→implement→review until reviewer VERDICT or `--max-cycles`. Gap: CC uses a **separate small model** as the condition judge; gooseherd's expensive reviewer doubles as judge. A cheap dedicated judge role would cut cost per cycle. |
| Time-based local loop (interval re-run) | ✅ `/loop 5m …` | ❌ | ❌ | Workaround today: cron + `goose orch -t --max-cycles`. Candidate: `goose orch --every 5m` or a `/loop` command. |
| Scheduled cloud loop | ✅ `/schedule` | ✅ cloud tasks | ➖ | gooseherd is local-first; document the cron + headless recipe instead. |
| Proactive loop (event/schedule-triggered, no human in the loop) | ✅ (composition of primitives) | 🔶 | ❌ | **This week's pick — see digest.** CC composes `/schedule` + `/goal` (e.g. hourly triage until every report is handled). gooseherd equivalent would compose cron + headless orch + a goal condition. Raised twice without being picked ([2026-W41](upstream-watch/2026-W41.md), [2026-W42](upstream-watch/2026-W42.md), both via the Block production-usage citation below) — this routine itself (a cron-fired agent session that reads state, decides, and writes a PR with no human in the loop) is a live instance of the exact pattern gooseherd can't yet offer its own users as a first-class primitive. |
| Small-model routing inside loops (strong model only for judgment calls) | ✅ | ✅ | ✅ | This is gooseherd's founding premise: expensive planner/reviewer, cheap implementer. |
| Scripted multi-agent fan-out (a JS/DAG script the model writes: `phase()`/`pipeline()`/`parallel()`, schema-validated subagent results, runs decoupled from the conversation) | ✅ Ultracode / Dynamic Workflows (`/effort ultracode`, up to 16 concurrent agents, 1000/run) + per-subagent model override (`CLAUDE_CODE_WORKFLOW_SUBAGENT_MODEL`, 2.1.296) and per-subagent compaction window (`autoCompactWindow`, 2.1.296) | ❌ (not found in changelog/workflows docs) | 🔶 | `/orch` is a fixed plan→implement→gates→review pipeline; `/arena` fans N contestants out in parallel but only for blind comparison. Neither lets the planner write an arbitrary phased/parallel script with per-step schema validation. CC narrowed its own cost gap this week by letting workflow subagents run on a cheaper model than the orchestrator — still gooseherd's premise, but now explicit on both sides. Still the highest-leverage structural gap — see prior digest ([2026-W41](upstream-watch/2026-W41.md)). |
| Peer-to-peer agent teams (independent sibling sessions in their own panes, interruptible, message each other) | ✅ Agent Teams (`--agent-teams`) + `agent.spawn` for teammates, idle/waiting states (2.1.289) | ❌ | ❌ | `/orch` roles are sequential phases inside one harness-controlled run, not addressable peer sessions. |
| Production-scale orchestrator + many cheap workers with human-gated merges (recurring scheduled codebase-wide agent passes) | ✅ [How Block orchestrates Claude Fable across thousands of pull requests](https://claude.com/blog/how-block-orchestrates-claude-fable-across-thousands-of-pull-requests), 2026-10-08 — Fable plans, "directs up to dozens of smaller, more cost-effective models" to implement; two humans approve every deploy; agents run daily/weekly codebase-wide passes | 🔶 | 🔶 | Validates gooseherd's founding premise (small-model routing row above) at production scale. The recurring-scheduled-pass half of this pattern is the same thing as the **Proactive loop** row above (still ❌ for gooseherd) — this citation is evidence that pattern is proven valuable in production, not just a CC feature nobody uses; raises the priority of that gap. |

## 2. Verification and quality

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Verification skills — a SKILL.md defines what "done" means (start server, exercise UI, check console) | ✅ (+ commit-time nudge: projects with a `verify` skill are told to run it before committing, 2.1.286) | 🔶 (AGENTS.md conventions) | 🔶 | Playbooks exist (`profiles/fable5-playbook.md`, personal playbooks via `GOOSE_PLAYBOOK_PATH`) but there is no standardized "verification" section the roles are told to honor. gooseherd's machine gates (`.goose-gates.yaml`) already *enforce* this before the reviewer runs, which is stricter than CC's nudge — worth stating explicitly in docs rather than marking 🔶. |
| Independent second-pass review by a separate agent | ✅ `/code-review` + hosted **Code Review** (GA 2026-10-15: auto-reviews every PR pre-human, Quick/Standard/Deep depth tiers, ~3x faster than preview at 6–10 min) | ✅ (Codex reviews 100% of OpenAI PRs) | ✅ | Structural: the reviewer role in `/orch` is a different model with read-only powers. Code Review's depth tiers are the same shape as gooseherd's own `/code-review` skill `--effort low|medium|high|xhigh|max` — validation, no status change. |
| Read-only enforcement for planning/review roles | ✅ plan mode | ✅ read-only sandbox | ✅ | Plan-Explore policy: read/search/think approved, writes rejected; double defense via claude plan mode / codex read-only sandbox. |
| Numeric success criteria work best (test counts, score thresholds) | ✅ (guidance) | ✅ (guidance) | 🔶 | Nothing stops it, but playbooks don't yet teach users to phrase orch tasks with numeric exit criteria. Doc fix. |
| Automated eval generation + hillclimb loop against a scored eval, with a held-out slice to catch overfitting (`/claude-api build-eval` then `/claude-api hillclimb`: generate a test set, iteratively change the target app/skill, keep a change only if its score improves, revert if the held-out score regresses) | ✅ (2026-10-07, [Automating eval design and hillclimbing with Claude](https://claude.com/blog/automating-eval-design-and-hillclimbing)) | ❌ (not found) | ❌ | Picked in [2026-W42](upstream-watch/2026-W42.md); not re-picked this week, still open. gooseherd's `exemplars.rs` hill-climbing (`crates/goose-cli/src/session/exemplars.rs`) only curates which past-transcript exemplars get injected into plan/review prompts, fed by `/arena` winners; it is not a general "generate an eval, iterate the implementation against it, guard with a held-out split" loop for `/orch` tasks. `/orch`'s only exit signal today is the reviewer's VERDICT — there is no numeric eval score and no anti-overfitting check against held-out cases. |

## 3. Cost and token observability

| Pattern | Claude Code | Codex | gooseherd | Notes |
|---|---|---|---|---|
| Per-session usage breakdown | ✅ `/usage` | 🔶 | ✅ | `/usage`, `/status` |
| Per-run/agent accounting with stop options | ✅ `/workflows` + [Agent observability on the Claude Platform](https://claude.com/resources/articles/agent-observability-on-the-claude-platform) (2026-10-14): every agent turn, model call, and tool/MCP call exported as a standard OpenTelemetry trace (GenAI semantic conventions) for Grafana/Honeycomb/Datadog/Langfuse | 🔶 | ✅ | `orch_ledger.jsonl` + `/stats` (per role: model, tokens, time, verdict), **and** generic OTLP trace/metric/log export already exists (`crates/goose/src/otel/otlp.rs`) — gooseherd isn't behind here. Needs verification: whether gooseherd's span/attribute names follow the same OTel **GenAI semantic conventions** CC cites, which would make its traces drop into the same dashboards without custom mapping. |
| Pilot run before scaling (measure cost on a small slice first) | ✅ (guidance) + effort/model-mix benchmarking grid in production ("Fable on low might just be better than another model on xhigh" — [Block/Fable post](https://claude.com/blog/how-block-orchestrates-claude-fable-across-thousands-of-pull-requests), 2026-10-08) | ✅ (guidance) | 🔶 | `/arena` is the natural pilot harness; not yet framed that way in docs. Block's point that a cheap model on low effort can beat an expensive one on high effort is a direct argument for `/arena`-driven model-mix selection — strengthens the case for a doc fix, not a code gap. |
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
| Per-task workspace isolation via managed git worktrees (create/list/clean up worktrees for parallel sessions without a by-hand `git worktree` dance) | ✅ `--worktree`/`-w` (shipped ~v2.1.49, Feb 2026; isolated branch under `.claude/worktrees/`) | ✅ new, 2026-10-08 (0.162.0): managed worktree tools in the TUI for trusted local projects, plus Command Center pinned tasks (`p` to pin, shared group, grouping persists across launches) | ✅ | `goose worktree new/list/prune` (general) and `/orch`'s own auto-created per-run worktree (`.goose/worktrees/orch-<id>`, named branch, `.env` symlinking, auto-commit/merge — `crates/goose-cli/src/session/orchestrate/workspace.rs`) already cover create/list/clean-up. Gap vs Codex: no equivalent of Command Center's pinned/shared task grouping across multiple concurrent runs. |
| Automatic model/effort routing per turn via a client-side classifier ("auto mode") — the harness, not the user, decides which model/effort handles a given turn | ✅ (2.1.299–2.1.302: `/effort`/`/fast` surfaced in agent view, OTel telemetry for auto-mode decisions, `CLAUDE_CODE_AUTO_MODE_MODEL` lets orgs pin the classifier's own model) | ❌ (not found) | ❌ | gooseherd has manual switching (`/roles`, `/preset`) and a related-but-distinct form of automation — `GOOSE_ORCH_PLAYBOOK=auto` picks a frontier-pattern *playbook*, not a per-turn model/effort decision — but nothing that classifies an incoming turn's complexity and auto-picks model/effort the way CC's auto mode does. New row. |

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
- Fallback used 2026-10-12 and again 2026-10-19: `developers.openai.com` and `openai.com` do not resolve from the watch environment (DNS failure, two weeks running — looks like a persistent network-policy gap, not a transient outage); https://github.com/openai/codex/releases is used instead (same first-party project, GitHub-hosted release notes).

## Process

1. Weekly routine produces a digest: *new post/change → pattern → row here (new or status change) → gap ticket if ❌/🔶*.
2. One gap per week gets picked into the `/orch` self-improvement loop, alongside friction-log items from daily-driver use.
3. Status only moves to ✅ when the feature passes the quality bar: a CC/Codex user hits no dead end and no raw error.
