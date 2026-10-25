---
name: foxy-delta-patch-verification
description: "Verify Foxy's patch plan coverage, transfer operations, apply integrity, cancellation behavior, atomic promotion, and full-download fallback."
---

# Foxy delta patch verification

Use this workflow when a file update patches unexpected bytes, downloads too much, fails validation, or falls back to a full file. A patch plan is optional and transient; the full-file path remains the correctness fallback.

## Workflow

1. Read delta planning and patch-first sections of the sync convention. Confirm which local and remote checksums establish the plan's copy and download operations.
2. Trace `plan_file_patch` through transfer, apply, segment verification, and promotion. Confirm every source range and output span is covered, including zero-length archive entries.
3. Check that patch artifacts are validated before promotion and that invalid plans or apply failures fall back to full download. Cancellation must retain a valid planned state and flow through rollback separately.
4. Inspect the final tree checksum, changed-part estimate, and log reason for any skipped plan. A successful file hash alone does not prove the queue estimate or user-visible summary is correct.
5. Run focused delta-patch unit tests for the changed contract. Use the testkit delta-patch lane only for a requested real-build or performance claim.

## Evidence

State plan status, copy/download bytes, final checksum result, fallback reason, and whether the evidence came from tests or a live case. Compare work counters as well as pass/fail.

## Boundaries

Do not edit temporary patch files or user targets to force a desired plan. Stop if the source manifest or expected checksum is unavailable; do not infer patch correctness from a debug log alone.

## References

- [Patch planning and regression guardrails](../../conventions/SYNC_ALGO_CONVENTION.md)
- [Delta patch module](../../src/core/tasks/delta_patch/mod.rs)
- [Patch planner](../../src/core/tasks/delta_patch/planning.rs)
- [Patch orchestration](../../src/core/tasks/delta_patch/orchestrator.rs)
- [Download rollback journal](../../src/core/tasks/download_files/rollback.rs)
