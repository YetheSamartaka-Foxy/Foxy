# UI agent notes

- Load `../../conventions/UI_CONVENTIONS.md` before changing egui layout, view state, widgets, palette, fonts, or interaction behavior.
- Load `../../conventions/i18n_CONVENTIONS.md` before changing user-facing text or `src/ui/locales/`.
- Load `../../conventions/ACCESSIBILITY_CONVENTIONS.md` for user-facing controls, navigation, status text, or CLI-visible copy.
- Keep UI draw code state-driven and avoid blocking work in immediate-mode rendering.
