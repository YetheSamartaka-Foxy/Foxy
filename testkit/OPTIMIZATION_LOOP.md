# Optimization loop

1. Validate the case, calibrate every required reference lane, establish the
   two warm samples required to store a baseline, and use at least five
   comparable successful repetitions for a performance claim. Accept only
   from a clean worktree.
2. Read the latest ledger rows, SOL records, and attribution breakdown.
3. Select one hypothesis from `HYPOTHESES.md`. Record the expected metric and
   rough effect before editing.
4. Make one small change.
5. Format only touched Rust files, then run workspace clippy and tests.
6. Rebuild the requested case build and run the same frozen case.
7. Repeat the complete run once when the first comparison is outside tolerance.
   The comparator only promotes `candidate-regression` / `candidate-improvement`
   to a confirmed verdict when the previous complete run of the same case in the
   same variant moved the same metric the same way.
8. Accept an improvement, revert a regression, or record an inconclusive result
   in `ledger/<case-id>.notes.md`.

Stop when the iteration budget is spent, the target is reached, three hypotheses
are inconclusive, or a correctness gate fails.

Hard rules:

- Never edit a case during a loop. A new case hash starts a new history.
- Never compare GUI with CLI, debug with release, WAL with MVCC, different write
  gates, different storage classes, or cold with warm.
- Never compare different diagnostics modes, cache preparation, environment or
  origin checksum, reference ids, or metric models. `rebaseline-required` and
  `profile-mismatch` mean incomparable evidence, not a performance verdict.
- Faster because fewer bytes, files, or parts were processed is invalid.
- Correctness counters and the independent oracle outrank timing.
- Instrumentation changes require a new baseline.
- Memory movement is advisory unless longer runs show unbounded growth.
- Never edit a runtime database, user config, logs, caches, or backups.
- Never commit or push without an explicit request.
- Restore every mutation journal, including after a failed measured operation.
- Do not retry rust-lld or sccache experiments already rejected by the repo.
- After accepted runs, render `foxy-testkit measurements` and use that output
  to refresh the curated measurement table.

Commands: `foxy-testkit run` for one case, `foxy-testkit suite` for several,
`foxy-testkit compare` for a deliberate variant A/B (database mode and
write-gate size, named explicitly on both sides), `foxy-testkit sweep` plus
`foxy-testkit report` for the whole mode/gate matrix.

For a change to a parser, a metric, a threshold, or a ledger field, verify with
`foxy-testkit replay --all` before running a case: it re-derives rows from every
recorded run in milliseconds, so a fresh run is only needed once the derivation
is right.
