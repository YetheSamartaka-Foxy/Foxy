---
name: foxy-download-tail-analysis
description: "Analyze Foxy download ramp, plateau, and tail behavior using transfer intervals, queue state, chunk waves, and verified completion outcomes."
---

# Foxy download tail analysis

Use this workflow for download ramp, plateau, final-wave, and completion delay questions. Read [speed-of-light conventions](../../conventions/SPEED_OF_LIGHT.md) O1/O2 and E10, [measurement archive](../../conventions/SPEED_OF_LIGHT_MEASUREMENTS.md), and the download sections of [sync guardrails](../../conventions/SYNC_ALGO_CONVENTION.md). For execution mechanics and case commands, use the [testkit skill](../foxy-testkit/SKILL.md).

Trace the current implementation in `src/core/tasks/download_files/`: `orchestrator.rs`, `metrics.rs`, `batching.rs`, `range_scheduler.rs`, `progress.rs`, and `rollback.rs`. Check actual emitters and interval definitions before assuming a log field's meaning.

## Diagnose the tail

- Freeze file list, sizes, range grid, origin/protocol, limiter, cache state, build, and profile. Record whether patches or full-file transfers ran.
- Distinguish `tail_s` from `tail_deficit_bytes`, last network byte, last disk byte, queue-empty time, file promotion, and user-visible terminal completion.
- Use `Download shape` and final-wave evidence alongside interval series. Do not infer a tail from a throughput average alone.
- Compare the measured residual work with justified network, disk, request-latency, and dependency ceilings. Keep units and scope aligned.
- Check whether one large file or straggling wave holds the action after most work completes. Inspect per-file chunk waves and queue draining.
- Separate concurrency ramp from the last residual transfer. Chunk size, fair scheduling, request preparation, connection reuse, server behavior, and local finalization can all matter.
- Verify expected transfer bytes and changed-part counts. A patch plan changes the amount of useful transfer work and its checksum contract.
- Include failed/partial transfers, retries, rollback, and fallback-to-full outcomes; success throughput alone can hide an execution regression.
- Do not use old connection ceilings, chunk sizes, or archived throughput as universal constants. Recalibrate for the same origin and protocol.

For a tail claim, identify:

- Whether the observation begins at the last plateau interval or at the last queued request.
- Whether the final task is network-bound, disk-bound, verification-bound, or awaiting cleanup.
- Whether the remaining-byte count is observed or estimated from progress state.
- Whether retry work belongs to the same attempt or a later attempt.
- Whether the scheduler still had queued work when the apparent tail began.
- Whether patch assembly or rollback contributes to terminal time.
- Whether the transfer limiter changed during the measured interval.
- Whether queue-empty time is distinct from task completion time.
- Whether the final disk write was durable or only buffered.

## Report and stop

Report case/run IDs, fixed conditions, interval and terminal timestamps, sample count, work bytes, tail duration, deficit, file/wave distribution, correctness outcome, and the bound used. Classify evidence as static, interval-sampled, or payload-verified. A profile can locate stages but changes their cost, so confirm claims with an unprofiled twin.

Stop when the tail has a supported cause, the action reaches a verified terminal outcome, and comparable repetitions show the effect or its uncertainty. If the queue or terminal timestamp is missing, do not claim the tail is caused by a particular stage. For code edits, retain patch fallback, rollback, and pending-state invariants from the sync conventions.

Do not recommend raising concurrency from tail duration alone; check the measured aggregate curve and whether the tail is a serial dependency.

Retain the original interval samples and units with any derived ramp/plateau/tail split so another reviewer can recalculate the boundary.

When investigating resumed ranges, use the chunk grid recorded in the sidecar. A current grid can make a valid resumed transfer appear to have an anomalous final wave.
