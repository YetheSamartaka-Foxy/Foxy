\## UI conventions (egui/eframe)

\- Keep UI code state-driven; store state in `Foxy` or view-specific structs.

\- Use `render\_\*` or `ui\_\*` for drawing functions; `on\_\*` for event handlers.

\- Avoid heavy work in `update`/draw; trigger work in background and update state when done.

\- Large views are organized as directory modules (`mod.rs` + sub-files). When adding substantial new UI to an existing view, add a new subfile in that view's directory rather than growing `mod.rs`. Keep individual `.rs` files under \~800 lines.

\- New screens belong in `src/ui/views/` with a focused module.

\- Reuse shared UI helpers in `src/ui/app/ui\_helpers/` (via `Foxy` methods) before writing inline layout code. Key helpers include `render\_adaptive\_tab\_bar` (responsive tab bars), `modal\_icon\_button\_size`, `toolbar\_icon\_button\_size`, `adaptive\_button\_height`, and `ui\_state\_checkbox`.

\- Reuse palette/theme from `src/ui/palette.rs`; do not hardcode new UI colors in views.

\- Never use `gamma\_multiply` for UI colors. Define explicit named colors in `src/ui/palette.rs` and reference those constants instead.

\- Reuse centralized font sizes from `src/ui/fonts.rs`; do not hardcode new font sizes in views.

\- Preserve established UI margins, padding, and spacing in each view. When changing labels, button text, or control widths, adjust layout math so the existing gutters and alignment stay intact unless told explicitly otherwise.

\- For long operations, show status: progress bar, label, or spinner.

\- Any clickable card/row/surface (not just buttons) must set pointer cursor on hover (`CursorIcon::PointingHand`) so interactivity is obvious.

\- Context menus should use separators between logical actions.

\- Give repeated egui widgets stable IDs/salts. For stateful widgets, derive salts from durable domain identity, not translated labels or per-frame/generated strings. Good salts include repository IDs, file paths, database IDs, and stable enum variants.

\- For custom fully-painted rows/cards with no child widget state, prefer a stable row-slot interaction ID such as `ui.interact(rect, ui.make\_persistent\_id(("view\_row", scope\_id, row\_idx)), Sense::click())` plus `ui.advance\_cursor\_after\_rect(rect)`. Keep selected/action data tied to the underlying domain item separately. This avoids egui warnings like "Widget rect changed id between passes" when filtering, sorting, or toggles swap the visible row set inside one frame.

\- Keep `ScrollArea::id\_salt`, `push\_id`, collapsers, text inputs, and context-menu anchors stable across egui sizing/paint passes. If a row contains stateful child widgets, do not use the row index as their only salt; use domain identity for those children.

\### Long-list scroll performance

\- Scroll FPS in long virtualized lists (addon tabs, mission list) is bound by egui **first-reveal text shaping**, not by the cache/filter layer. epaint's `GalleyCache` keeps only galleys laid out in the current frame, so idle = all cache hits but scrolling reveals never-before-shaped text = full shaping + font-atlas growth every frame. More filtering/caching at the data layer cannot beat this; the only levers are shaping fewer glyphs/galleys per row, fewer rows, or caching the shaped galleys.

\- Use the shared persistent galley cache in `src/ui/views/galley\_cache.rs` (`lazy\_galley`, `truncated\_galley`, `paint\_centered`, `paint\_overlay\_left`, `paint\_anchored`) for hot scrolling rows. Shape each row once with `Color32::PLACEHOLDER`, store `Arc<Galley>`, and recolor at paint time via the fallback-color arg of `Painter::galley` so state/color changes never re-shape. Width-independent text uses `layout\_no\_wrap`; width-dependent text is truncated and rebuilt on resize. Fill lazily (only rows actually viewed). Do not cache short non-scrolling sections (e.g. the repository-view server list) - egui's per-frame cache already covers them and a galley cache is dead weight.

\- The settings FPS toggle (footer readout left of the activity-log button) is the tool to measure these regressions.

\### Per-instance repository status

\- Repository status/pending-update maps in the UI are keyed per *instance* via `repo\_instance\_key(url, local\_path)` (= `normalize\_repo\_url(url) + U+001F + content\_hash::normalize\_path(path)`), not by URL alone, so two installs of one URL in different folders show independent status. Use the `\*\_for\_address` helpers in `src/ui/app/repository/list\_cache.rs` and thread `local\_path` through results/events. `repo\_foxy\_modes` is the deliberate exception - foxy mode is a URL-level property. See the identity invariant in root `AGENTS.md` and `conventions/BACKEND\_CONVENTIONS.md`.

