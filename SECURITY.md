# Security

## Reporting a vulnerability

Please report security vulnerabilities privately through GitHub Security
Advisories: https://github.com/codingmydna/gooseherd/security/advisories/new.
Do not open a public issue for a security problem.

## The orchestration permission model

gooseherd runs cheaper models under a frontier model's plan and does not trust
any single agent with unrestricted access. Planner and reviewer roles run
read-only: permission requests are judged by ACP tool kind, so reads, searches,
and read-only subagent exploration are approved while edits, deletes, and moves
are rejected (the vendor CLI's own restrictions — Claude Code plan mode, Codex
read-only sandbox — remain a second barrier). The implementer runs with write
access confined to its own git worktree, and with
`GOOSE_ORCH_IMPLEMENT_POLICY: allowlist` it is further restricted to an approved
command list. Mechanical gates run before the reviewer sees any diff. As with
any developer agent, review generated code and run untrusted tasks in an
isolated environment.

## Security model

The posture is **safe by default, permissive by choice**. Concretely:

- **No config-file IPC.** Permission steering during a run (planner read-only
  mode, the implementer's mode/policy) is applied as process-local, in-memory
  overrides — never written to `config.yaml`. A crash mid-run therefore cannot
  leave your on-disk permission mode downgraded, and concurrent runs (arena)
  never race on the file. A one-time self-heal clears the stale
  `GOOSE_ORCH_IMPLEMENT_ACTIVE` / `GOOSE_ACP_PLAN_EXPLORE` flags an interrupted
  older build could have persisted.

- **Implement policy defaults.** Headless runs (`goose orch`/`goal`/`loop`
  without a TTY, and arena contestants) default to the workspace `allowlist`
  policy: file writes are confined to the implementation worktree and commands
  must match `GOOSE_ORCH_ALLOWED_COMMANDS` (seeded with `git` plus the repo's
  detected build tools). Interactive `goose orch` keeps `auto`, because a human
  is present to approve. Set `GOOSE_ORCH_IMPLEMENT_POLICY=auto` to restore the
  old permissive default; native (non-ACP) implementers have no allowlist
  enforcement path and always run `auto`.

- **Allowlist matcher.** A command that matches the allowlist by first token is
  still rejected if it contains shell-chaining metacharacters
  (`;`, `&&`, `|`, `` ` ``, `$( )`, `<`, `>`, newline). Only an allowlist entry
  that itself contains a metacharacter (an explicit shell-form entry) opts into
  that exact chained command.

- **Gate execution.** Machine gates run with a per-gate timeout
  (`GOOSE_ORCH_GATE_TIMEOUT_SECS`, default 900s; the process group is killed on
  timeout) and, by default, a scrubbed environment (`GOOSE_ORCH_GATE_ENV=scrub`)
  that removes credential-shaped variables (`*_API_KEY`, `*_TOKEN`, `*_SECRET`,
  `ANTHROPIC_*`, `OPENAI_*`, `AWS_*`, …) while keeping `PATH`, `HOME`, `CI`, and
  build-tool vars. Set `GOOSE_ORCH_GATE_ENV=inherit` to pass the full
  environment through. Derived (non-`.goose-gates.yaml`, non-global) gate
  commands are printed before they run.

- **Worktree isolation is git-level, not an OS sandbox.** The implementer works
  in a dedicated git worktree, but that is a filesystem convention, not a
  security boundary: absolute paths and allowlisted commands can still reach
  outside it, and gates execute real processes. By default ignored `.env*` files
  are symlinked into the worktree for convenience
  (`GOOSE_ORCH_LINK_ENV=false` disables this; the start-of-run security banner
  shows the effective state). For untrusted tasks, run gooseherd inside a
  container or VM.
