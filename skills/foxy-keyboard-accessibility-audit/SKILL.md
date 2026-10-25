---
name: foxy-keyboard-accessibility-audit
description: "Audit Foxy UI keyboard navigation, focus visibility, activation, status clarity, and contrast against accessibility conventions."
---

# Foxy keyboard and accessibility audit

Use this workflow for keyboard flow, focus, contrast, error clarity, or accessible status changes. Read [accessibility conventions](../../conventions/ACCESSIBILITY_CONVENTIONS.md), [UI conventions](../../conventions/UI_CONVENTIONS.md), and `src/ui/AGENTS.md`. For desktop input simulation, read [GUI driver skill](../foxy-gui-driver/SKILL.md).

Inspect the changed view, shared widgets, focus handling in `src/ui/app/runtime/keyboard_focus.rs`, and any relevant state or localization. Trace actual egui response creation and focus order rather than assuming visual order is keyboard order.

## Walk the interaction

- Define the start focus, expected sequence, modal boundaries, and escape/cancel behavior.
- Validate `Arrow` navigation where list or directional selection applies.
- Validate forward `Tab` and reverse `Shift+Tab` traversal through every actionable control, including scrollable content and modal actions.
- Confirm focus is visibly distinguishable at the actual theme and contrast, and does not disappear behind clipped or scrolling regions.
- Confirm `Enter` activates the focused control once and does not accidentally trigger a default/destructive action.
- Check that disabled, advisory, blocking, loading, and error states are conveyed in text or accessible labels, not color alone.
- Check semantic labels for icon-only controls, progress, and custom-painted rows. Preserve stable IDs for stateful widgets.
- Verify keyboard behavior after filtering, sorting, opening/closing a modal, and returning from a nested view when those paths are affected.
- For game-specific UI, verify capability gating hides unsupported controls rather than leaving dead or misleading affordances.
- Prefer existing controls, helpers, palette, and font sizes. Do not change unrelated layout to make one path pass.

For modal changes, also verify:

- Focus enters the modal predictably and does not remain on hidden background content.
- Tab traversal stays within the modal while it is active.
- Closing or canceling restores focus to a sensible invoking control.
- Scrollable content does not hide the focused item or the final actions.
- Escape behavior is explicit and safe for the current operation.
- Repeated activation cannot submit a destructive action twice.
- Focus order follows the task's decision order rather than incidental paint order.
- The selected item remains visible after keyboard navigation scrolls a list.
- Focus remains visible in both light and dark themes where supported.

## Evidence and stopping

Report the exact view and navigation path, each input tested, focus visibility, activation result, and any screen reader/status evidence available. Separate code inspection from simulated or manual UI evidence. A screenshot can show focus appearance but does not establish keyboard traversal.

Stop when every in-scope action can be reached, understood, activated, and exited with the documented keyboard path, or report the specific blocker and consequence. If the runtime or GUI harness is unavailable, give a static review and mark runtime keyboard validation unverified. Record an accessibility impact note for UI or CLI changes.

Keep evidence limited to the changed flow. Do not infer screen-reader support from a visible label or keyboard traversal alone.

For CLI output changes, inspect plain and `--json` contracts separately. Preserve stable machine-readable fields while improving human-readable status.

Describe any untested input path explicitly so a static review cannot be mistaken for a completed interaction audit.
