---
name: foxy-progress-correctness
description: "Audit Foxy progress and status reporting for truthful phases, monotonic work, terminal outcomes, cancellation, and retry behavior."
---

# Foxy progress correctness

Use this workflow when progress stalls, jumps, completes early, reports the wrong total, or fails to reflect cancellation and retries. Read [UI conventions](../../conventions/UI_CONVENTIONS.md), [accessibility conventions](../../conventions/ACCESSIBILITY_CONVENTIONS.md), [sync guardrails](../../conventions/SYNC_ALGO_CONVENTION.md), and `src/ui/AGENTS.md` for UI changes.

Trace the producer and consumer together. Relevant paths include `src/core/tasks/download_files/progress.rs`, `src/ui/app/downloads/progress.rs`, `src/ui/app/runtime/progress_events.rs`, and the view that renders the status. Confirm current event types and callers.

## Audit the contract

- Define each phase's unit, denominator, start/end event, and whether it measures completed work, estimated work, or activity only.
- Follow progress from work scheduling through task events and UI state to the displayed label/bar. Find dropped, duplicated, reordered, or stale events.
- Check monotonicity within a phase and deliberate reset when a new phase or retry begins. Never sum values with different denominators.
- Verify unknown progress remains indeterminate; zero remaining work is not proof of success until validation and promotion finish.
- For patches, compare planned transfer bytes with changed-part details and final checksum rollup. For fallback, ensure UI and estimates reflect full-file transfer.
- Distinguish network bytes received, disk bytes written, files promoted, verified files, and overall terminal completion.
- Trace cancellation, retry, failure, rollback, and late task completion. A timeout or canceled future does not undo filesystem or DB writes.
- Check that events carry repository instance and game-space context where needed. A stale completion must not update another install or active space.
- Verify accessible status text communicates phase, completion, failure, and cancellation without relying on color or animation.
- Reuse shared activity widgets and frame scheduling; do not add a repaint loop solely to animate an idle bar.

For each progress phase, capture:

- The phase name and the source event that enters it.
- The numerator and denominator, including any unknown state.
- Whether retry work resets, accumulates, or is shown separately.
- The event that marks success and the distinct failure/cancel events.
- The repository instance and active space associated with the event.
- The UI label shown before and after the terminal event.
- Whether a late event can overwrite a newer operation's status.
- Whether paused state is distinct from no progress or completion.
- Whether byte and file-count fallbacks are clearly identified.

## Evidence and stopping

Create a phase/event table from producer to display, with sample values for normal completion, retry, cancellation, and failure. Separate static tracing from runtime interaction evidence. If a UI change affects keyboard operation, also use [keyboard accessibility audit](../foxy-keyboard-accessibility-audit/SKILL.md); for live input tools, follow the [GUI driver skill](../foxy-gui-driver/SKILL.md).

Stop when every visible number and terminal label maps to a named producer contract, and a failure scenario can no longer produce a false success. If a phase lacks a defined unit or terminal event, report the contract gap instead of patching only its rendering.

Do not call a percentage monotonic across phase changes unless its denominator and completed work have the same meaning in both phases.

Check empty and zero-size work separately from an unknown total. A valid empty operation may complete immediately, while a missing total should remain indeterminate.

Keep logs and UI evidence free of real repository names, server URLs, and user filesystem paths.
