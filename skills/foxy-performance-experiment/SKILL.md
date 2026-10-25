---
name: foxy-performance-experiment
description: "Design controlled Foxy performance experiments with frozen scope, comparable runs, correctness guards, and evidence-based stopping criteria."
---

# Foxy performance experiments

Use this workflow to answer one bounded performance question. Read [speed-of-light conventions](../../conventions/SPEED_OF_LIGHT.md), [measurement archive](../../conventions/SPEED_OF_LIGHT_MEASUREMENTS.md), [optimization loop](../../testkit/OPTIMIZATION_LOOP.md), and [hypotheses](../../testkit/HYPOTHESES.md). For actual cases also read [testkit instructions](../foxy-testkit/SKILL.md), [case format](../../testkit/CASE_FORMAT.md), and [testkit README](../../testkit/README.md).

## Frame the experiment

- State the user action, suspected stage, expected resource or critical path, and a measurable outcome.
- Name the operation and timer boundary. Keep service time, action wall time, and occupied wall time distinct.
- Freeze repository scope, payload and manifest checksums, local state, build kind, diagnostics mode, storage, origin/protocol, concurrency, and cache preparation.
- Choose work counters that expose correctness and work amplification, not only elapsed time or throughput.
- Write the hypothesis before the run. State the change expected, evidence that would support it, and evidence that would reject it.
- Keep a control and candidate on the same case/profile. Alternate order when practical and collect enough repetitions to show spread; a few runs do not establish a stable effect.
- Include failure, timeout, partial completion, and oracle outcomes. Do not filter them out when reporting reliability.

For each candidate, record:

- The exact code revision and whether the worktree had local changes.
- The case identifier, case fingerprint, and origin checksum.
- The operation's selected work scope and expected correctness result.
- The calibration identifiers used, or why no physical reference applies.
- Any changed condition that forces a separate comparison lane.
- The exact command and output directory for each run.

## Run and interpret

- Use the existing `foxy-testkit` case, suite, sweep, compare, report, and calibration commands documented by its skill. Do not invent flags or edit ignored case data without a task that owns it.
- Use `--validate-only` to check a case definition before a run. Use `--no-build` only after the required binaries are built for that intended run.
- A profiled run has overhead. Keep profile-enabled and unprofiled rows under separate case identities and use the unprofiled twin to confirm the result.
- Match calibration references to the measured machine, storage, origin, protocol, cache state, and work type. Missing counters or mismatched references are unknown, not zero.
- Compare outcome and work first, then elapsed time, distribution, and named resource demand. A faster result with changed scope or weaker verification is not an optimization claim.
- Record process/per-thread CPU and peak/retained private bytes when the lane exposes them. Follow the resource trade policy: footprint movement is advisory, and a time regression cannot be accepted to save memory or disk.
- Use the testkit oracle when a payload correctness claim depends on published part checksums. Structure-only checks prove presence and length, not content.
- Do not treat archived machine results or old throughput values as current expectations. Re-measure after relevant environment changes.
- Do not compare rows with different payloads, cache preparation, diagnostics, or timer boundaries as if they were paired.
- Do not accept a candidate because its best single run beats the control median.

## Evidence and stopping

Report the exact case and run identifiers, frozen conditions, repetitions, median and spread, absolute change, correctness/oracle result, work counters, and cited calibration lane IDs. Keep raw artifacts in their designated testkit locations and put only decision-changing summaries in the measurement archive.

Stop when the hypothesis is supported or rejected, correctness is established at the stated level, and the effect is either reproducible or within observed noise. If scope or environment drifted, mark the comparison inconclusive and redesign it. Stop before any run that would use an unapproved live repository, network target, GUI session, or user profile.

For code changes, follow the domain skill and [testing conventions](../../conventions/TESTING_CONVENTIONS.md). A benchmark result does not replace required checks or a runtime verification appropriate to the change.
