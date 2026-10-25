---
name: foxy-egui-responsiveness
description: "Investigate Foxy egui frame, input, and relayout responsiveness with controlled views, frame metrics, and accessible interaction evidence."
---

# Foxy egui responsiveness

Use this workflow for UI stalls, frame pacing, scrolling, delayed input, or relayout problems. Read [UI conventions](../../conventions/UI_CONVENTIONS.md), [accessibility conventions](../../conventions/ACCESSIBILITY_CONVENTIONS.md), `src/ui/AGENTS.md`, and [GUI driver skill](../foxy-gui-driver/SKILL.md). For quantitative cases also read the [testkit skill](../foxy-testkit/SKILL.md) and UI measurement section of [testkit README](../../testkit/README.md).

Inspect the relevant view and helpers under `src/ui/views/`, `src/ui/app/runtime/frame_cost.rs`, and `src/ui/app/ui_helpers/`. Keep immediate-mode drawing cheap and dispatch long work through the existing task/state flow.

## Capture a reproducible interaction

- Fix view, row count, scroll path, window size, renderer, scale, display, motion setting, and background workload. Report actual values for this run, not values from historical examples.
- Use the supported `foxy-testkit` UX case or GUI driver commands in their skills. An `agent-gui fps` request opens a live repaint window, so record probe cadence. The CLI round trip includes process startup and IPC; it is separate from the app's frame/FPS samples and must not be treated as UI frame cost.
- Record frame interval max/p95, lowest smoothed FPS, UI-thread CPU, relayout/pass counters when exposed, and action latency. Distinguish time to first frame from settled interaction.
- Compare the same path and capture duration. A single screenshot or average FPS does not identify input latency or first-reveal cost.
- For long lists, consider first-reveal text shaping and existing galley-cache behavior before changing data filtering. Preserve cache invalidation for text/style changes and use stable domain-based widget IDs.
- Check repaint causes, paced spinner use, delayed repaint scheduling, off-screen animation behavior, and whether work is incorrectly running on the UI thread.
- Exercise keyboard and focus behavior when the interaction changes. Include visible focus and activation, not only mouse paths.
- Keep animation and reduced-motion settings fixed across comparisons. Idle UI should not animate or request continuous repaint.

When the symptom is a visual stall, separate:

- Frame production delay from event-to-frame delay.
- UI-thread work from worker completion and result polling.
- First reveal from repeat scrolling over already visible rows.
- A relayout pass from the normal frame that paints the new layout.
- A blocked frame from a missing repaint request after state changed.
- App frame cost from OS compositor or driver presentation latency.
- Whether the observed work was visible or off-screen.
- Whether the app requested another frame after state changed.
- Whether the measurement began before or after the view settled.

## Evidence and stopping

Report build and renderer, fixed interaction, sample count/duration, frame and CPU metrics, relayout evidence, and whether the UI remained operable. Label static inspection separately from live sampled evidence. Use screenshots only to support visual claims; frame probes support timing claims.

Stop when the trigger and affected stage are reproducible, the relevant interaction remains usable, and comparable samples show the change or its uncertainty. Stop if the harness or renderer differs from the report; isolate it before making a performance claim. For actual launches, use the GUI driver's isolated configuration and cleanup rules.

If only static code evidence is available, identify the likely main-thread work and its triggering view/state. Do not attach a numeric FPS or latency estimate without sampled runtime data.

Record whether performance counters were collected while the pointer was over the app, since focus and input state can affect the reproduced path.

Do not infer an accessibility improvement from higher frame rate. Record keyboard path, focus visibility, and status clarity as separate outcomes when they are in scope.
