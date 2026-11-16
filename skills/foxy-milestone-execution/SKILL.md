---
name: foxy-milestone-execution
description: "Deliver a multi-step Foxy feature or existing milestone with fixed outcomes, acceptance mapping, scoped review and evidence reuse. Use for substantial end-to-end work, not small fixes or creating a new roadmap."
---

# Foxy milestone execution

Read the current private plan and applicable `AGENTS.md` files. Preserve the user's milestone count, sequence, completion boundary and commit/publish instructions. Keep one terse working note under `.local/`; update the existing ledger rather than accumulating competing plans.

## Fix the outcome

State the observable result, touched control surfaces, exclusions and highest-risk boundaries before implementation. Separate behavior to preserve from behavior intentionally changed. Map material acceptance to evidence in a compact table:

| Outcome or failure boundary | Existing seam/case | Needed proof | Reuse or rerun reason |
| --- | --- | --- | --- |
| One observable behavior | Caller and test/fixture | Unit, integration, GUI or native | Changed inputs or an actual evidence gap |

Use [testing conventions](../../conventions/TESTING_CONVENTIONS.md) to select checks. Keep required native/platform/provider proof explicit; earlier helper or engine-layout success cannot satisfy a new lifecycle or provider outcome. Discover environment prerequisites and external blockers early while continuing independent authorized work.

## Build a working slice

Trace the real caller, core operation, persistence/owner boundary and terminal result. Implement a thin complete path before expanding supporting helpers. Use [Rust changes](../foxy-rust-change/SKILL.md) for implementation and [agent workflows](../../conventions/AGENT_WORKFLOWS.md) for bounded delegation. Keep shared wiring and integration with the main agent; useful parallel work has disjoint ownership or a distinct read-only question.

Batch coherent edits, run the cheapest check that answers the next uncertainty, and group failures by cause. Record build time separately from test execution or environment waiting when investigating slowness. After a repeated failure, inspect its artifacts and correct the hypothesis before another expensive attempt. Avoid unrelated refactors and toolchain tuning during delivery.

## Integrate and finish

Use a fresh-context focused review for consequential changes; route material expectation changes through [test oracle review](../foxy-test-oracle-review/SKILL.md) and real external-process proof through [native acceptance](../foxy-native-acceptance/SKILL.md). Revisit fixed paths and affected callers rather than restarting every review. Run required final workspace checks once integration is stable, repeating relevant checks after subsequent source changes or failures.

Retain command, input/build scope, result and artifact references in the private note. Reuse evidence only when its source/environment inputs and claimed boundary remain valid; record why. Final GUI and CLI acceptance must exercise the integrated behavior. Finish when the agreed outcome, required checks and material findings are resolved. Report an actual blocker if acceptance is unavailable; do not shrink the milestone to get a completed count. Update progress and the next concrete action, then stop at the user's requested boundary.
