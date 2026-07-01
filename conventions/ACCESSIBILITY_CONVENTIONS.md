\## Accessibility conventions (a11y)

\### Baseline

\- Treat accessibility as a feature, not polish; regressions are bugs.

\- Target `WCAG 2.2 AA` principles adapted for desktop egui UI and CLI output.

\- Definition of done for user-facing changes: keyboard-operable interactions, readable text at larger sizes, sufficient contrast, and clear status/error messaging.



\### UI accessibility

\- Every interactive element must be reachable and usable with keyboard-only navigation.

\- Provide keyboard parity for list/card/row navigation: support `ArrowUp`/`ArrowDown` (and `ArrowLeft`/`ArrowRight` where applicable).

\- `Tab`/`Shift+Tab` must traverse interactive controls in a predictable order.

\- `Enter` must activate the focused/default action in the current context.

\- Keep focus behavior predictable; visible focus state must always be obvious.

\- Do not rely on color alone to communicate state (error, warning, success, selected, disabled).

\- Use palette constants for all semantic states; do not hardcode ad-hoc colors in views.

\- Keep text scalable by using centralized font sizing (`src/ui/fonts.rs`) and avoid fixed tiny text.

\- For long-running operations, expose progress and current status text (not spinner-only).

\- Respect reduced cognitive load: prefer plain labels, clear grouping, and consistent action placement.



\### CLI accessibility

\- Keep CLI output readable and deterministic; avoid noisy or ambiguous phrasing.

\- Ensure `--json` output remains structured and stable for assistive tooling and automation.

\- Errors must include clear cause + actionable next step when possible.

\- Do not require color for understanding CLI output; text alone must carry meaning.



\### Localization and text

\- Keep user-facing copy plain-language and concise; avoid jargon where possible.

\- Preserve placeholders and meaning consistently across locales.

\- When adding labels/messages, verify both locales remain understandable and not truncated in common layouts.



\### PR and review gate

\- Any change touching UI text, controls, layout, CLI output, or interaction flow must include a short "Accessibility impact" note in change notes/PR description.

\- If accessibility parity is deferred, document why and list the follow-up scope.

