# The Ultracode Method

Distilled from Claude Fable 5's July 2026 gooseherd overhaul: a ~200K-LOC trim plus a
harness-engineering program executed in one session, ending green (1,831 tests, zero
warnings) with a 77% smaller binary. This is the operating procedure for running a
large plan-and-implement engagement at maximum quality with orchestrated subagents.
It is written for whatever model drives the orchestration next.

## 1. Map before deciding

Never plan a large system from one reading. Fan out parallel domain auditors — one per
concern (core, CLI, providers, periphery, build, UX, security, the product's crown
jewel) — each returning a STRUCTURED result: inventory, cut candidates with exact
coupling to sever and confidence, refactor targets, quality issues with file:line,
opportunities, and load-bearing key facts. Then run a completeness critic over the
combined output whose only job is to find (a) contradictions between auditors,
(b) blindspots no auditor owned, and (c) spot-verified safety of the boldest claims.
In this engagement the critic caught 8 cross-domain conflicts and 10 blindspots
(CI/build files nobody owned, config back-compat, fork-identity collisions) that
would each have broken the build or the users had they surfaced mid-implementation.

## 2. Decisions become a committed artifact

Resolve every conflict the critic found — explicitly, one line per decision, with the
rationale — in a plan-of-record committed to the repo before any code changes. Every
downstream agent reads it first. Ambiguity you leave here is re-litigated N times by
N agents later.

## 3. Stage the irreversible; stay green at every boundary

Order destructive work so the tree compiles and tests pass after EVERY stage: zero-
coupling deletions first, then leaf crates, then feature surgery, then core modules,
then the long tail. One agent per stage, one commit per stage, machine-readable
report per stage (what deviated and why, what was intentionally left, exact test
counts). The gates between stages (fmt, clippy -D warnings, targeted tests) are what
let a 15-commit rewrite proceed without ever entering a broken state you must dig
out of.

## 4. Audit claims are hypotheses, not facts

Each stage re-verifies the specific premise it is about to act on. Here, a stage
agent disproved the audit's claim that a 1,700-LOC subsystem was "consumed only by
the server surface" — it was woven into the registry core — and renegotiated scope
instead of forcing the cut. Empower agents to push back with evidence; require them
to report the false premise rather than absorb it silently.

## 5. Adversarial review AFTER green — tests measure what you encoded, not what you missed

When everything passes, you have verified your own assumptions, nothing more. Run
independent reviewers per risk dimension (the deepest structural change, the
terminal/state-machine change, the cross-module flow, and back-compat against the
real user's real config), each hunting concrete failure scenarios only. Then kill
findings by refutation, not by vibes. In this engagement the review surfaced 30
findings, including two HIGH integration defects — the flagship safety feature was
silently inert end-to-end — that had survived every stage gate because each unit
behaved as its own tests demanded.

## 6. Honesty is a protocol, not a virtue

Every agent report separates observed (command output, file contents) from inferred;
lists deviations from spec with reasons; names what was skipped. The orchestrator's
job is to read those deviations and re-decide, not to average them away. A partial
success reported as such is worth more than a confident-sounding guess — this is the
same rule as the operating playbook, applied at the fleet level.

## 7. Wall-clock is compile time; design around it

In a systems codebase the model is rarely the bottleneck — build/test cycles are.
Budget accordingly: share a warm target dir, run per-stage test subsets with ONE full
suite at the end, prepare disk headroom up front (build caches grow without garbage
collection), and disable incremental compilation for check/test-once workloads.
Parallelize agents only across disjoint file sets; a shared tree forces sequencing,
and merge conflicts cost more than the parallelism buys.

## What makes it "ultracode"

Three multipliers over a straight implementation pass, in order of value:
1. **Breadth of finding** — multi-perspective fan-out (audit domains, review
   dimensions) surfaces what any single pass misses.
2. **Layers of verification** — critic over auditors, gates between stages,
   adversarial review over the finished work. Each layer catches a class of defect
   the previous one structurally cannot.
3. **Structured hand-offs** — schemas for findings, committed decisions, per-stage
   reports. Free-text hand-offs decay; structured ones compound.

Spend tokens on 1 and 2 before spending them on more elaborate implementation.
