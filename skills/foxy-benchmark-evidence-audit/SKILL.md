---
name: foxy-benchmark-evidence-audit
description: "Audit Foxy benchmark claims for comparable scope, valid baselines, complete counters, correctness, and reproducible evidence."
---

# Foxy benchmark evidence audit

Use this workflow to review an existing benchmark record, comparison, or performance claim. Read [speed-of-light conventions](../../conventions/SPEED_OF_LIGHT.md), especially collection/comparison rules and presentation, [measurement archive](../../conventions/SPEED_OF_LIGHT_MEASUREMENTS.md), and [testkit README](../../testkit/README.md). For runner field semantics consult [case format](../../testkit/CASE_FORMAT.md) and [ledger format](../../testkit/LEDGER_FORMAT.md).

## Audit the comparison

- Identify the user action, operation, emitter, timer boundary, build, harness, and what completion means.
- Verify work units and time units. `SOL` records use bytes and nanoseconds; do not silently normalize ambiguous legacy text.
- Compare case fingerprint, published origin checksum, selected scope, payload, profile, diagnostics mode, database mode/gate, cache preparation, environment, and calibration references.
- Confirm the baseline is accepted, valid, and tied to the same reference IDs and relevant environment. An expired, changed, legacy, or missing lane is not a current physical limit.
- Distinguish action wall time, operation service sums, occupied wall intervals, and makespan. Parallel service totals can exceed elapsed time.
- Confirm all required counters are present. Missing work, interval, outcome, or memory fields mean unknown, never zero.
- Check at least five comparable successful repetitions per condition for a claim about effect, with additional runs for noisy lanes or small effects. Report sample count, median, spread, and absolute difference.
- Keep failures and timeouts in reliability results. A summary of successes alone can hide a regression.
- Check correctness guards and oracle result. A structure-only oracle pass does not prove content equality; absent extra-file checks do not prove there are no extras.
- Treat profiled and unprofiled rows separately because instrumentation can change DB/filesystem-bound timing.

Check the record chain:

- Is the raw run artifact available and does it match the summarized row?
- Does the row identify the code build and harness version?
- Is the terminal outcome successful, partial, failed, or timed out?
- Are the reference IDs the same ones accepted with the baseline?
- Does the reported unit conversion preserve the source counter and precision?
- Can another reviewer reproduce the comparison from the stored artifact references?
- Are all compared rows from the intended operation and cache lane?
- Does the result retain the worktree's build commit for the measured binary?
- Are timeouts and incomplete runs visible in the reliability summary?

## State what the evidence supports

Classify evidence as static review, sampled runtime measurement, or correctness-verified runtime result. Static code inspection supports mechanism claims, not measured speedup. A sample supports only its captured interval and environment. A verified claim needs matching scope, successful correctness outcome, and credible repeated measurements.

Report each mismatch with record/run IDs, relevant field names, and the claim it invalidates or limits. Preserve raw values and label conversions. Do not import numbers from dated archive entries as universal budgets.

Audit presentation separately from validity. A correct ratio can still be mislabeled if it compares service time with action wall time, uses a mismatched work counter, or omits failures. A chart title or baseline label is part of the claim and must match the underlying lane.

Record the audit date and the archive revision inspected. Avoid silently replacing a historical claim with current measurements from a different environment.

Do not reconstruct missing raw intervals from rounded summaries. State which conclusion is unavailable when precision, sample count, or outcome detail is absent. Keep the audit read-only unless the task separately assigns new measurements or baseline maintenance.

Stop when the claim is supported, narrowed, or rejected with traceable evidence. If necessary identifiers or counters are absent, mark the result inconclusive and state the missing evidence. Do not alter ledgers, accept baselines, or initiate fresh runs as part of a read-only audit unless the task explicitly assigns those actions.
