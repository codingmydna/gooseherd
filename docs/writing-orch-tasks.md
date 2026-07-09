# Writing good `/orch` tasks — the Fable 5 style

`/orch` hands your task to a **planner** and a cheaper **implementer**, then a
**reviewer** judges the result. None of them can read your mind, and the
implementer is deliberately not your smartest model. So the task text you write
is a *contract*: the more precisely it pins down what must be true, the less the
loop drifts.

This is the house style distilled from the model that bootstrapped gooseherd
(Fable 5). Every task it wrote followed the same shape and the same principles.
They are worth copying.

## The 6-part anatomy

| Part | What it does | Example fragment |
|---|---|---|
| **1. One-line imperative goal** | Open with the concrete deliverable. | "Add a mechanical gate step before review." |
| **2. Why / context / symptom** | One or two sentences of rationale or mechanism. For bugs: the observed symptom and a reproduction. | "Purpose: run cheap machine checks before spending reviewer tokens…" / "Symptom (observed): the planner deadlocks when…" |
| **3. Pointers to existing code** | Name the files, functions, and structs to touch or reuse. Say "reuse X, do not reimplement." | "Related parts: `live_input.rs`… `queued_inputs`. Reuse `cliclack::select`." |
| **4. Numbered requirements** | Each `(1) (2) (3)…` is one discrete spec: config knob names, defaults, per-branch behavior, and backward-compat constraints. Defer genuinely open design choices to the planner. | "`GOOSE_ORCH_GATES` list — empty means skip (same as today)… the exact split is decided during planning." |
| **5. Completion criteria** | Testable acceptance: named unit tests, an invariance/regression test, and the exact gate commands. For visual/interactive work: a reproduction procedure. | "…unit tests for X and Y; `cargo fmt` and `cargo clippy --all-targets -- -D warnings` and `cargo test -p goose-cli` pass." |
| **6. Planner directive footer** | Keep the planner from calling plan-mode approval tools (which deadlock a headless run). | "[Planner: present the plan as your final text only. Never call EnterPlanMode/ExitPlanMode — a separate model implements.]" |

## The 5 principles

1. **Pin the contract, delegate the method.** Nail down config names, defaults,
   edge behavior, and tests — but let the planner read the code and decide the
   real design ("the exact cut line is decided during planning").
2. **Bake the verification into the prompt.** Part 5 *is* the test plan. If the
   loop can't check it, it can't converge on it.
3. **State backward-compatibility every time.** Default values preserve current
   behavior; every feature is opt-in ("unset → unchanged").
4. **Reuse over reinvention.** Point at the existing helper and say "generalize
   into a shared module, no duplication."
5. **Enumerate the edges.** headless / non-tty / empty store / parse failure /
   path escape — give each a specified behavior ("silently skip; orch must not
   die").

Keep it tight. Fable 5's tasks packed all six parts into ~1,000–1,400
characters. Length is not thoroughness; precision is.

For a hard problem, offer the planner a couple of candidate approaches and let
it choose, rather than dictating one: "(a) … (b) … (a)+(b) is ideal."

## A worked example

```text
Add a mechanical gate step before the review phase.

Purpose: before calling the reviewer (an expensive model), have orch run quality
commands itself, so mechanically-detectable defects (format/lint/test failures)
are bounced straight back to the implementer without spending reviewer tokens.

Requirements: (1) Gates are defined by the config list GOOSE_ORCH_GATES (e.g.
["cargo fmt --check", "cargo clippy --all-targets -- -D warnings",
"cargo test -p goose-cli"]) — empty means skip (identical to today). Run in the
implementation workspace. (2) After each implement phase, run gates in order,
stop at the first failure, and send a bounce instruction (with a truncated
stdout/stderr tail) back to the implementer for another cycle — do not call the
reviewer. (3) Gate bounces are bounded by a separate counter
GOOSE_ORCH_MAX_GATE_RETRIES (default 2); exceeding it ends orch in a gate-failed
state (headless exit 1). (4) Only on gate pass does the review phase run, and
the review request states "gates passed: <commands>" so the reviewer need not
re-run them. (5) Record gate runs in the phase banner and ledger. (6) Repos with
no gates configured, existing exit-code meanings, and the interactive /orch path
must not regress. Done when: the fail → bounce → fix → pass → review flow and the
retry-exceeded exit are proven by tests (mock a failing gate), an unset-gates
no-op test, and cargo fmt / clippy -D warnings / cargo test -p goose-cli pass.

[Planner: present the plan as your final text only. Never call
EnterPlanMode/ExitPlanMode — a separate model implements.]
```

## When *not* to use `/orch`

The loop verifies through mechanical gates, and the implementer runs headless —
it never *sees* the terminal. So a purely **visual bug** (a misaligned TUI, a
spinner painting over streamed text) is a poor fit: the implementer can't
observe the defect and the gates won't catch it. Either fix it directly, or
first add a **golden-snapshot test** that captures the rendered output as a
string so the gate *can* catch it — then hand it to `/orch`.
