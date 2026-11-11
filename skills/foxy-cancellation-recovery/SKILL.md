---
name: foxy-cancellation-recovery
description: "Trace cancellation through Foxy sync, downloads, delta patching, persistence, and rollback, verifying what state is restored or intentionally retained."
---

# Foxy cancellation and recovery

Use this workflow for cancellation, interrupted updates, partial files, stale patch plans, or recovery after a failed sync. Cancellation is a state transition with cleanup obligations, not just an error string.

## Workflow

1. Identify the operation and cancellation point: database wait, planning, transfer, hashing, patch apply, promotion, or final persistence.
2. Trace the `watch::Receiver<bool>` through the owning pipeline. Check whether child workers are joined before rollback or shared-state persistence is allowed to finish.
3. Inspect the rollback journal in `download_files/rollback.rs`: distinguish original files, staged replacements, promoted targets, and entries already restored.
4. For patch cancellation, inspect `delta_patch/orchestrator.rs`. Cancellation keeps a valid patch plan planned for a later run; it is not equivalent to patch validation failure or full-download fallback.
5. Verify the next-run recovery path and the focused tests. State explicitly whether evidence is code-only, a passing test, or a real isolated interruption run.

## Invariants

- Do not abort a task whose writes can race restoration. Wait for workers and relevant persistence to settle before rollback.
- A failed download must not leave a mixed target assembled from old and new bytes. Preserve rollback journals when restoration is incomplete.
- Do not delete temporary rollback evidence or mark the operation successful to make the UI appear clean.

## Boundaries

Never test interruption against a user's live repository. Use a disposable test fixture or an explicitly requested testkit case with a reviewed mutation journal. Stop if the cleanup owner or final state cannot be established from the code.

## References

- [Sync algorithm and cancellation invariants](../../conventions/SYNC_ALGO_CONVENTION.md)
- [Sync pipeline](../../src/core/api/sync_pipeline/pipeline.rs)
- [Download rollback journal](../../src/core/tasks/download_files/rollback.rs)
- [Delta patch orchestration](../../src/core/tasks/delta_patch/orchestrator.rs)
