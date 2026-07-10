# Loop engineering with gooseherd

gooseherd gives Claude Code and Codex CLI users a small set of loop primitives
that cover most agent automation patterns. Pick the loop whose exit condition
matches the work: deterministic checks beat model judgment whenever you can
write one.

## Agent Loop

Use plain sessions for normal agent work, and `/orch` when the work benefits
from separate planner, implementer, and reviewer models.

Example:

```sh
/orch add validation to the login handler and cover it with tests
```

The planner explores read-only, the implementer edits, and the reviewer judges
the resulting diff against the task and plan.

## Verification Loop

Use a repo-local `.goose-gates.yaml` for mechanical gates inside `/orch`, and
use `/goal --check` when a single deterministic success criterion should control
retries. The file is a YAML list such as `- pnpm run test`. Without it, orch
derives existing `test` and `build` scripts from `package.json` using the repo's
lockfile, or `go build ./...` and `go test ./...` from `go.mod`. It never derives
lint commands. `GOOSE_ORCH_GATES` remains the global fallback (and the existing
behavior for Cargo repos). Gates run before review, so failures bounce straight
back to the worker without spending reviewer tokens.

Example:

```sh
GOOSE_ORCH_GATES='["cargo fmt --check", "cargo test -p goose-cli"]' goose session
/goal make the CLI tests pass --max 4 --check "cargo test -p goose-cli"
```

## Time-Based Loop

Use `/loop` or headless `goose loop` when the watched state changes over time.
Choose an interval that matches how often the state can realistically change;
polling CI every five seconds just burns tokens.

Example:

```sh
goose loop -t "check whether the release workflow finished and summarize failures" --every 5m --max 12
```

## Goal-Based Loop

Use `/goal` or headless `goose goal` when the agent should keep attempting a
task until a success condition is confirmed or an attempt cap is reached. Prefer
`--check` for deterministic criteria; without it, gooseherd asks the evaluator
model configured by `GOOSE_EVALUATOR_PROVIDER` / `GOOSE_EVALUATOR_MODEL`,
falling back to the reviewer role.

Example:

```sh
/goal get the homepage Lighthouse score to 90 or above --max 5 --check "npm run lighthouse -- --min-score=90"
goose goal -t "reduce the failing test count to zero" --max 3 --check "cargo test"
```

## Hill-Climbing Loop

Use the plan and review exemplar archives plus the distillation playbook
workflow when production runs should improve future runs. Approved plans and
review verdicts become examples for later planner/reviewer prompts, turning
successful orchestration traces into a better harness.
By default the playbook and exemplars are injected unless the planner/reviewer
serving model is Fable; `GOOSE_ORCH_PLAYBOOK=auto|always|never`,
`GOOSE_PLAN_EXEMPLARS_INJECT=auto|always|never`, and
`GOOSE_REVIEW_EXEMPLARS_INJECT=auto|always|never` override that gate.

Example:

```sh
GOOSE_PLAN_EXEMPLARS=true GOOSE_REVIEW_EXEMPLARS=true goose session
/orch refactor the provider retry policy without changing behavior
```

After approval, inspect the archived exemplars and distill recurring guidance
into the playbook when it is stable enough to generalize.

## Event-Driven Scheduling

Cloud/event scheduling with `/schedule` is roadmap for gooseherd loop
engineering. Until then, use OS cron, launchd, or your CI scheduler with a
bounded headless command.

Example:

```sh
# cron-style wrapper
goose loop -t "check the nightly import and file an actionable summary" --every 1m --max 1
goose goal -t "repair the generated API docs" --max 2 --check "npm run docs:check"
```
