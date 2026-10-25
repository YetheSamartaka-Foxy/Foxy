---
name: foxy-sync-regression-check
description: "Check a Foxy repository sync change against quick-scan, remote-refresh, hashing, queue, summary, and recovery invariants before making a regression claim."
---

# Foxy sync regression check

Use this workflow for changes to repository checks, remote metadata, local hashing, pending updates, downloads, or post-sync propagation. Read the regression guardrails before proposing an optimization or accepting a result.

## Workflow

1. Read the relevant section of `SYNC_ALGO_CONVENTION.md`, especially the guardrails and acceptance cases for the changed stage.
2. Trace the decision from entry point through quick scan or remote refresh, queue construction, transfer, persistence, and final summary. Check that each stage uses the intended repository instance and operation scope.
3. For a correctness change, identify a minimal regression test. For a real repository profile claim, choose the matching `foxy-testkit` UX or performance lane from its skill and README; the ignored live cases never run from Cargo tests or CI.
4. Compare the same build, mode, cache state, origin, and profile. Verify work counters and the independent oracle where applicable, not only elapsed time or a success banner.
5. Give a bounded conclusion: static review, unit test, testkit fixture, or seeded live-profile evidence. Do not promote one evidence level into another.

## Inspect

Look for stale remote graphs, clean local content with changed manifests, path-scoped pending state, patch-first queue estimates, sibling propagation, and cancellation rollback. A recovery trigger should converge after the read it schedules; repeated verification on every startup is a regression.

## Boundaries

Do not edit ignored testkit cases, ledger rows, baselines, or runtime repositories unless explicitly asked. Follow the testkit skill before any real build or case run. Stop when the test profile does not match the claim.

## References

- [Sync algorithm and regression guardrails](../../conventions/SYNC_ALGO_CONVENTION.md)
- [Testkit case and evidence workflow](../foxy-testkit/SKILL.md)
- [Testkit case catalog and run rules](../../testkit/README.md)
- [Sync pipeline](../../src/core/api/sync_pipeline/pipeline.rs)
