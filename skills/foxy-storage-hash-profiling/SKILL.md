---
name: foxy-storage-hash-profiling
description: "Profile Foxy local hashing and storage work while preserving checksum scope, cache state, verification semantics, and game-space ownership."
---

# Foxy storage and hash profiling

Use this workflow for local file enumeration, hashing, tree verification, and storage-bound behavior. Read [speed-of-light conventions](../../conventions/SPEED_OF_LIGHT.md) operation O3/O4 and the resource trade policy, [sync guardrails](../../conventions/SYNC_ALGO_CONVENTION.md), and [measurement archive](../../conventions/SPEED_OF_LIGHT_MEASUREMENTS.md). For live performance mechanics use [testkit skill](../foxy-testkit/SKILL.md) and [optimization loop](../../testkit/OPTIMIZATION_LOOP.md).

Relevant implementation includes `src/core/tasks/calculate_hashes/` (`scheduling.rs`, `storage_probe.rs`, `direct_read.rs`, `file_hashes.rs`, `part_hashes.rs`, `verified_record.rs`) and quick-scan stages under `src/core/api/quick_scan/`. Confirm current modules and callers before drawing conclusions.

## Preserve the work contract

- Name whether the path is content hashing, ordered tree verification, quick scan, or a combined action. These operations have different minimum work and timers.
- Preserve remote ordered rollups and local BLAKE3 content rollups. Content hashes do not replace tree-hash verification.
- Keep selected repository/addon/file scope identical across candidates. Repository identity includes normalized URL and local path.
- Record bytes read and hashed, metadata/stat calls, entries traversed, cache hit/miss reasons, worker concurrency, elapsed intervals, and verification outcome where available.
- Separate cold, warm, and evicted page-cache states. A warm read is not evidence of device throughput; use matching calibration lanes.
- Compare compute-only hash rate separately from read-and-hash work. Match algorithm and input size when using the BLAKE3 or MD5 compute reference.
- Check that cache keys and verified records remain safe across process restart, local drift, remote changes, same URL in separate folders, and space switching.
- Read the sync regression guardrails before changing verification triggers. A correctness read that repeats every launch is a performance regression even if the final verdict is right.
- Do not add per-launch deep scans, part-first drift detection, unscoped hash refresh, or redundant reads of data just tree-hashed.

For suspected cache effects, record:

- Whether the file identity/metadata cache hit or missed and why.
- Whether bytes came from a warm page cache or a cold/evicted read.
- Whether verification was restored from a record or recomputed.
- Which physical storage volume supplied the path.
- Whether concurrent work shares that volume and changes its demand.
- Whether the run includes hashing, persistence, or both in the timer.
- Whether bytes were read sequentially, by range, or through random access.
- Whether the hash algorithm and rollup order match the expected contract.
- Whether file identity changed between cache lookup and verification.

## Evidence and stopping

For testkit experiments, use a frozen matching case and report `hash_total_s`, bytes, tree verification runs, selected scope, cache state, memory, and oracle/outcome as applicable. Compare profiled and unprofiled cases separately. Use two consecutive seeded-profile startups when evaluating a recovery trigger: the second should show whether scheduled verification converged.

State whether evidence is static, sampled, or content-verified. Stop when the selected scope and verified result match, the observed work is explained by counters, and the change has a reproducible effect or is within measured noise. If cache state, scope, or reference is unknown, mark performance inconclusive. Never open runtime databases or change user profiles for profiling.

When reviewing a code proposal without runtime access, describe expected work removed or added and the invariant that bounds it. Mark the expected improvement as a hypothesis rather than a measurement.

Report local bytes and remote manifest work separately; they answer different cost questions and have different cache behavior.

Review cancellation and failure paths as well as the successful hash pass. A partial scan must not publish a clean checksum or restore verification state as though every selected file was checked.
