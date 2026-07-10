# gooseherd Overhaul — July 2026

Plan of record for the CLI-first overhaul. Produced from an 8-domain audit of the full
codebase (core, CLI/orch, providers, periphery, build, UX, security, uplift engine)
plus a cross-domain conflict/blindspot review. Decisions below are canonical; where
audit domains disagreed, the resolution is stated.

## Product thesis

gooseherd is a **CLI-first multi-model orchestration harness**: an expensive frontier
model plans and reviews, cheap models implement, machine gates verify — over ACP-driven
vendor CLIs (claude-acp, codex-acp, any agent via one config entry) and native
OpenAI/Anthropic-compatible APIs. Target users are Claude Code / Codex CLI users; the
quality bar is that they onboard with zero friction and never hit a "this feels
unfinished" moment. Desktop app, web UI, and hosted-service surfaces are non-goals.

## Canonical decisions (conflict resolutions)

| Question | Decision |
|---|---|
| CLI-wrapper providers (claude-code, codex, gemini-cli, cursor-agent, chatgpt-codex) | **Delete.** Upstream already deprecates them in favor of ACP. Add friendly config-migration errors ("claude-code → claude-acp; run `goose herd`"). |
| litellm / tetrate / nanogpt / avian / google-native / gemini-oauth | **Delete as Rust providers.** All reachable via the declarative custom-provider engine (openai-compatible endpoints) or ACP. Tetrate cut includes rewiring the `goose configure` first-run wizard away from Tetrate signup. |
| Niche ACP presets (copilot-acp, amp-acp, pi-acp) | **Keep.** ~400 LOC total, they are config presets over the ACP client and serve the multi-agent story. Candidates for the adapters catalog later. |
| Bundled declarative provider JSONs (35 defs: deepseek, groq, zai, …) | **Keep all.** They ARE the cheap-model story. Fix picker grouping later instead of deleting definitions. |
| goose-cli default features | **Canonical set: `[rustls-tls, system-keyring]`.** Features deleted outright: code-mode (+vendor/v8 + icu pins), local-inference (+cuda/vulkan/mlx), aws-providers, telemetry, nostr, tui, update. Kept as opt-in: otel, native-tls. |
| ACP server surface (`goose acp`, `goose serve`, acp/server\*, transport, inventory, goose-acp-macros) | **Delete entirely**, including the two CLI commands. The ACP *client* (acp/provider.rs) is the product core and is independent. A minimal stdio ACP server (Zed/JetBrains embedding) is a future milestone to be rebuilt cleanly on the `agent-client-protocol` crate, not carved out of the 25-submodule desktop server. |
| Recipes | **Keep the core** (`goose run --recipe`, validate/list — summon subagents depend on it). **Delete** desktop-only `goose recipe deeplink/open` + recipe_deeplink.rs. |
| goose-mcp builtins | **Keep `memory` only.** Delete autovisualiser (4MB embedded JS), tutorial, computercontroller (+peekaboo, lopdf/docx/umya/image codec tree). Unknown builtin names in existing user configs must warn-and-skip, not fail. |
| goose-server, goose-sdk crates | **Delete.** Verified: no crate depends on them; Justfile/CI references rewritten in the same change. goose-sdk-types, goose-test, goose-test-support, goose-acp-macros→(dies with ACP server). |
| security/ (remote ML prompt-injection stack) | **Delete all of it** (needs Block-internal endpoint; wired only into the native loop, not orch). A future offline heuristic is backlog. |
| posthog telemetry | **Delete outright** (hardcoded upstream PostHog key phones home to Block; consent friction for zero owner benefit). |
| scheduler / gateway (Telegram) / dictation / goose_apps / nostr / plan-mode (/plan) / project tracker / `goose update` / `goose tui` / `goose term` | **Delete.** `/loop` covers recurring runs; `goose update` currently *replaces gooseherd with upstream goose* — worst-case footgun. Keep sessions.db `schedule_id` column + 'scheduled'/'gateway' session-type strings as inert for DB back-compat. |
| plugins / hooks / skills / slash_commands | **Keep.** Hooks are CC-compatible (PreToolUse/PostToolUse/Stop…) and are a harness-uplift building block; plugins/skills are the extensibility story. |
| bat dependency | **Keep for now** (syntax highlighting serves CC-parity rendering). Backlog: replace with tree-sitter-highlight (grammars already compiled in core). |
| Windows | **macOS + Linux only, declared.** release.yml already builds 3 targets; delete Windows matrix legs, download_cli.ps1, build-windows.ps1. live_input is unix-only by design. |
| Binary / config-dir name | **Keep `goose`** binary name and `~/.config/goose` this cycle (owner's live setup + muscle memory). The upstream-collision trap is closed by deleting `goose update`. Full `gooseherd` rename is an open product question — revisit before 1.0. |
| Session import (CC/Codex/Pi .jsonl) | **Keep** — it is a migration asset for the target users. |
| Exemplar corpus | Local state is personal (Korean tasks, private repos) — **never ship**. The framework is generic; add repo scoping + export/import + repo-committed seed packs. |

## Workstream A — Trim (staged, each stage verified green)

1. **Zero-coupling deletions**: documentation/ (310MB), ui/ (21MB), services/, oidc-proxy/,
   recipe-scanner/, workflow_recipes/, examples/, evals/, bin/ (hermit), flake.nix/lock,
   Dockerfile, goose-self-test.yaml, test_acp_client.py, download_cli.sh/.ps1, upstream
   root *.md (GOVERNANCE, MAINTAINERS, I18N, CUSTOM_DISTROS, RELEASE_CHECKLIST,
   BUILDING_*, CONTRIBUTING_RECIPES, README.upstream). Workflows 46 → ~5 (ci, release,
   build-cli, + cargo-deny/machete un-guarded). Justfile rewritten CLI-first. AGENTS.md
   rewritten for the post-cut architecture (doubles as agent context — harness uplift).
2. **Crate deletions**: goose-server, goose-sdk (+ scripts the workflows called).
3. **Build surgery**: default features → canonical set; delete code-mode/vendor/v8/icu pins,
   local-inference + goose-local-inference + goose-download-manager, dictation, aws,
   telemetry/posthog, nostr, tui, update/sigstore; add `[profile.release]`
   strip=symbols + thin LTO + codegen-units=1; workspace default-members = goose-cli.
4. **Core module cuts**: gateway, goose_apps + apps ext + mcp_app_proxy, scheduler,
   recipe_deeplink, security/, session/legacy.rs, ACP server surface + Serve/Acp CLI
   commands + providers/inventory + goose-acp-macros; goose-mcp → memory only.
5. **Provider long tail**: databricks*, gcp*, snowflake, azure*, huggingface*, xai_oauth,
   kimicode, githubcopilot, oauth.rs, oauth_device_flow, instance_id, bedrock/sagemaker,
   tetrate (+signup), nanogpt, avian, litellm, google native, gemini_oauth, CLI-wrapper
   providers, create_with_named_model, stray utils-to-move.md. Config back-compat shims:
   removed-provider names → actionable error; unknown builtins → warn-and-skip.
6. **CLI command cuts** + configure first-run rewired to ACP-first (detect claude/codex
   CLIs → herd flow; API keys secondary).

Measured expectations: 963 → ~550 crates in the default graph; release binary 260MB →
25–50MB (strip alone recovers ~45MB); clean build time roughly halved; repo ~330MB lighter.

## Workstream B — Uplift engine ("any cheap model performs like a frontier model")

Mechanism today: plan/review exemplar stores (Jaccard retrieval) + fable5 playbook
injection + machine gates + ledger fingerprinting. Gaps, in leverage order:

1. **Playbook delivery is broken over ACP** — AcpProvider::stream drops the system
   prompt, so the centerpiece never reaches ACP planners/reviewers. Fix: fold non-empty
   system into the first user block for ACP; make the injection banner honest.
2. **Verdict protocol**: exact-token parse of the LAST `VERDICT:` line ("NOT APPROVED"
   currently parses as approval); NO_VERDICT → one bounded reprompt; one shared verdict
   module for orch reviewer / goal evaluator / arena judge.
3. **Structured plan schema**: required sections (Files / Steps / Acceptance criteria /
   Verification commands); structural plan gate on every finalize path (not only
   idle-timeout), one reprompt on failure. Replaces the char-count heuristic.
4. **Implementer context pack** (implementer uplift is ~zero today): structured
   self-verification block (criterion → evidence) demanded before review, reprompt once
   if missing, forwarded to reviewer as a checklist; distilled REVISE-defect patterns
   injected as "known failure modes"; playbook gating made configurable.
5. **Repo context pack**: cached per-repo orientation block (tree skeleton, detected
   manifests/gates, conventions files) injected for non-frontier models; refreshed on
   HEAD change. Cheap models fail on repo orientation first.
6. **Exemplar store v2**: repo-root scoping (fallback to cross-repo), skip-not-abort on
   corrupt index lines, tail-preserving truncation (acceptance criteria live at the END
   of plans), near-dup suppression, shared generic store for plan/review.
7. **Uplift gating**: replace personal `is_fable_model` with explicit configurable
   skip patterns; a claude-acp user with unset model must not silently lose all uplift.
8. **Evidence upgrades**: untracked file contents in orch git evidence (arena already
   does this), gate outputs + diffstat to the reviewer, git evidence to the goal
   evaluator, tail-keeping report truncation.
9. **Blind arena, actually blind**: bare-letter labels, shuffled order, sealed mapping
   revealed after verdict; arena writes ledger rows; winner plan/diff archived as
   exemplar candidates (arena → hill-climbing feedback).
10. **Safety-as-trust**: kill config.yaml-as-IPC (in-memory scoped overrides for
    GOOSE_MODE / PLAN_EXPLORE / IMPLEMENT_ACTIVE — crash can no longer leave a
    permissive downgrade; arena concurrency becomes safe); allowlist matcher rejects
    shell-chaining metacharacters; gates get timeouts + scrubbed env + printed commands;
    headless orch/arena default to the workspace allowlist policy.
11. **Measurement**: /stats shows approval-rate and mean-cycles with vs without
    exemplar injection per model (data already in the ledger).

## Workstream C — UX / onboarding / product

1. First run with no config → ACP-first onboarding (detect claude/codex CLIs, offer
   subscription-driven setup, run adapter installs), not the OpenRouter wizard; builder
   errors point to `goose herd`; `goose doctor` must work with zero config.
2. Live input default-on (tty+unix) with a visible affordance; Esc interrupts a turn.
3. Single slash-command registry driving completion, dispatch, /help (grouped), and
   nearest-match suggestions on typos.
4. Fix known frictions: /toggle→/r notice, /help duplicates, resume hint (`goose s -r`),
   codex-acp npm package-name mismatch, interactive resume picker.
5. Turn-end bell + documented GOOSE_STATUS_HOOK.
6. Contribution flywheel: adapters/ catalog (`goose herd add <name>`) + ADAPTERS.md +
   issue/PR templates — "add an agent with one YAML PR".
7. Docs: rewritten README (thesis, 2-min quickstart, comparison table), docs/reference.md
   (every GOOSE_* knob, exit codes, gates spec), repo-local .goose-gates.yaml (dogfood).
8. CI: binary-size regression gate; startup-time smoke (`goose --version` < 150ms).

## Sequencing

Workstream A lands first (staged commits, `cargo check/clippy/test` green per stage),
then B (each item test-covered), then C. A ships as one reviewed branch; B/C as focused
follow-ups. Nothing here is pushed to the public repo without the owner's go.

## Post-review backlog

Findings from the adversarial review deferred as backlog (not addressed in the
review-fix pass):

- **[11] Live steer echo vs spinner redraw** — self-echoed steer characters are
  overwritten by the cliclack spinner's steady-tick/elapsed-label redraw, so
  typing while a tool call streams looks invisible (the steer still submits).
- **[12] Termios restore on SIGTERM** — raw-mode/O_NONBLOCK restore relies only
  on `Drop`; a SIGTERM/SIGQUIT mid-turn leaves the tty in raw no-echo until
  `stty sane`. Needs a signal handler that restores the terminal.
- **[14] Partial steer line dropped at turn end** — an unsubmitted steer line is
  discarded when the turn ends instead of flowing to the next rustyline prompt
  as the old canonical-mode reader promised.
- **[22] Arena judge can read contestant logs** — `.goose-arena/<LABEL>.log`
  files carry the `goose run` banner with provider/model in cleartext in the
  judge's cwd; an exploring ACP judge could de-blind the lineup. Scrub/relocate
  logs before judging.
- **[26] Preset switch leaves stale effort keys** (pre-existing) — `apply_roles_spec`
  only sets keys present in the new spec and never clears prior
  `GOOSE_*_EFFORT` / `GOOSE_CODEX_REASONING_EFFORT`, producing a hybrid config.
- **[27] Fresh-DB vs migrated-DB schema divergence** (verified pre-existing
  upstream drift) — `create_schema` stamps version 14 without the
  `threads`/`thread_messages` tables and `sessions.thread_id` column that
  migration 10 adds on upgraded DBs. Confirmed present at the pre-overhaul base
  (`b156e2065`: same `CURRENT_SCHEMA_VERSION = 14`, same create/migration split),
  so it is not a regression of this branch. Latent until a future migration
  assumes the v10 path ran.
