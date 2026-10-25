---
name: foxy-startup-crash-diagnosis
description: "Diagnose Foxy startup failures from launch markers, logs, renderer fallback state, and startup gates without confusing a native crash with an ordinary error."
---

# Foxy startup crash diagnosis

Use this workflow for failures before the main window appears, early exits, or repeated startup fallback. First distinguish a process crash before app construction from a returned error after startup.

## Workflow

1. Establish the exact executable build and whether a Foxy process remained running. Read only logs and marker evidence the user supplied or explicitly authorized.
2. Inspect `src/ui/launcher.rs` and `renderer_fallback.rs`. Foxy writes a launch-attempt marker before eframe starts and clears it when the app is constructed. A marker left behind advances the next attempt through `Full`, `SafeBackend`, then persisted `Glow` fallback.
3. Correlate the marker stage with startup log lines. A returned eframe error is logged and exits with a failure status; a native graphics-stack crash can leave no panic or app log.
4. Check startup gates separately: database process lock, schema compatibility, game-space initialization, and renderer selection can fail at different points.
5. If the user wants a live reproduction, use an isolated config root and the GUI driver instructions. Do not copy, remove, or reset the user's marker or settings as a diagnostic shortcut.

## Findings

Report the build identity, last completed startup stage, marker state, and the evidence that separates renderer failure from lock, schema, or initialization failure. Label an inferred graphics cause as an inference until a fallback launch or matching runtime log confirms it.

## Boundaries

- A `Glow` fallback notice is evidence that a prior launch did not construct the app, not proof of a specific driver defect.
- The modal preview command uses placeholder data and does not reproduce a native startup crash.
- Stop at static diagnosis when no isolated environment is available. Do not repeatedly launch against user state.

## References

- [Renderer startup contract](../../conventions/UI_CONVENTIONS.md)
- [UI launcher](../../src/ui/launcher.rs)
- [Renderer fallback state](../../src/core/utils/renderer_fallback.rs)
- [GUI driver workflow](../foxy-gui-driver/SKILL.md)
