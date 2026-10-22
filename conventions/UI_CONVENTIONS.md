\## UI conventions (egui/eframe)

\- Keep UI code state-driven; store state in `Foxy` or view-specific structs.

\- Every new `Foxy` field is either space-scoped (reset in `reset_space_scoped_state`) or app-global (listed in `APP_GLOBAL_FOXY_FIELDS` with a reason). A guard test fails on anything unaccounted, because a space-scoped field left out of the reset leaks the previous game space's data into the next one. See `conventions/GAME_SPACES_CONVENTIONS.md`.

\- Gate game-varying UI on `GameCapabilities`, never on `module.id() == "arma3"`. A game that lacks a feature renders no control for it rather than a dead or disabled one.

\- Use `render\_\*` or `ui\_\*` for drawing functions; `on\_\*` for event handlers.

\- Avoid heavy work in `update`/draw; trigger work in background and update state when done.

\- Large views are organized as directory modules (`mod.rs` + sub-files). When adding substantial new UI to an existing view, add a new subfile in that view's directory rather than growing `mod.rs`. Keep individual `.rs` files under \~800 lines.

\- New screens belong in `src/ui/views/` with a focused module.
\- The Steam Workshop store of the active game space is the `Steam Workshop` tab of the game-space settings view (`src/ui/views/workshop/`), shown when the target space declares `steam_workshop`. A non-active space renders an explanation instead, because the store resolves through `active_game_space_dir()`. The tab reloads from disk whenever it is entered (`WorkshopViewState::loaded`), and every action that downloads, unsubscribes, or copies mod folders goes through `WorkshopTask` on a worker thread; only the small `workshop.json` writes (enable, load order) run inline.

\- Reuse shared UI helpers in `src/ui/app/ui\_helpers/` (via `Foxy` methods) before writing inline layout code. Key helpers include `render\_adaptive\_tab\_bar` (responsive tab bars), `modal\_icon\_button\_size`, `toolbar\_icon\_button\_size`, `adaptive\_button\_height`, and `ui\_state\_checkbox`.

\- Reuse palette/theme from `src/ui/palette.rs`; do not hardcode new UI colors in views.

\- Never use `gamma\_multiply` for UI colors. Define explicit named colors in `src/ui/palette.rs` and reference those constants instead.

\- Reuse centralized font sizes from `src/ui/fonts.rs`; do not hardcode new font sizes in views.

\- Preserve established UI margins, padding, and spacing in each view. When changing labels, button text, or control widths, adjust layout math so the existing gutters and alignment stay intact unless told explicitly otherwise.

\- For long operations, show status: progress bar, label, or spinner.

\- Animated progress visuals live in `src/ui/app/ui_helpers/activity/` and are re-exported from `crate::ui::app`: `ActivityBar` (drop-in for `egui::ProgressBar`: up to four pixel-snapped rows of triangles side by side, pointing up and down by turns (`TriangleGrid` in `triangles.rs`, the verification map's tiling too), that fill with mana: a blink per triangle, climbing bubbles, twinkles and a glowing frontier, with a sweeping beam for unknown progress; a short bar drops rows, and `rows(1)` asks for a single line), `VerificationMap` (hashing and checks), `ProgressOrb` (sidebar rows, a round cluster of triangles), `ActivityIcon` (an animated glyph naming the work), `StepTrack` (bulk runs as one triangle block per item), and `paint_border_beam`/`paint_border_flare` (a running or just-finished banner). Progress fills are mana triangles, not squares, hatched stripes or smooth liquid. Bars that sit where the map can also appear use `VerificationMap::HEIGHT`, so the two match. Text over a bar goes through `paint_bar_label`, which adds a feathered backing and a glyph outline in the shade that contrasts with the text. Reuse these instead of new ad-hoc animations, and keep their colors flat: every triangle takes one of four `Tones` (empty, the fill, and two steps of the fill toward `ON_STRONG_FILL`), never a darker shade or a gradient. They move only while work runs and their rect is on screen, ask for frames through `Motion::keep_alive` at `PROGRESS_FRAME_INTERVAL`, and draw a still frame when the **Reduce motion** setting zeroes egui's `animation_time`. Idle UI never animates. One-shot completion effects use `rise_effect`, which does not replay when a finished view is reopened.

\- Spinners are `crate::ui::app::PacedSpinner`, never `egui::Spinner` or `ui.spinner()`: egui's spinner requests a repaint on every paint and keeps the whole UI drawing at the display rate (223 fps measured through a repository check). A repaint that only needs to come soon goes through `crate::ui::app::request_frame_after`, because egui starts a delayed frame one predicted frame early and a short `request\_repaint\_after` otherwise fires at once. The `UI frame cost during sync` log line reports frames, UI-thread CPU and the top repaint causes for every check.

\- Any clickable card/row/surface (not just buttons) must set pointer cursor on hover (`CursorIcon::PointingHand`) so interactivity is obvious.

\- Context menus should use separators between logical actions.

\- Give repeated egui widgets stable IDs/salts. For stateful widgets, derive salts from durable domain identity, not translated labels or per-frame/generated strings. Good salts include repository IDs, file paths, database IDs, and stable enum variants.

\- For custom fully-painted rows/cards with no child widget state, prefer a stable row-slot interaction ID such as `ui.interact(rect, ui.make\_persistent\_id(("view\_row", scope\_id, row\_idx)), Sense::click())` plus `ui.advance\_cursor\_after\_rect(rect)`. Keep selected/action data tied to the underlying domain item separately. This avoids egui warnings like "Widget rect changed id between passes" when filtering, sorting, or toggles swap the visible row set inside one frame.

\- Keep `ScrollArea::id\_salt`, `push\_id`, collapsers, text inputs, and context-menu anchors stable across egui sizing/paint passes. If a row contains stateful child widgets, do not use the row index as their only salt; use domain identity for those children.

\### Long-list scroll performance

\- Scroll FPS in long virtualized lists (addon tabs, mission list) is bound by egui **first-reveal text shaping**, not by the cache/filter layer. epaint's `GalleyCache` keeps only galleys laid out in the current frame, so idle = all cache hits but scrolling reveals never-before-shaped text = full shaping + font-atlas growth every frame. More filtering/caching at the data layer cannot beat this; the only levers are shaping fewer glyphs/galleys per row, fewer rows, or caching the shaped galleys.

\- Use the shared persistent galley cache in `src/ui/views/galley\_cache.rs` (`lazy\_galley`, `truncated\_galley`, `paint\_centered`, `paint\_overlay\_left`, `paint\_anchored`) for hot scrolling rows. Shape each row once with `Color32::PLACEHOLDER`, store `Arc<Galley>`, and recolor at paint time via the fallback-color arg of `Painter::galley` so state/color changes never re-shape. Width-independent text uses `layout\_no\_wrap`; width-dependent text is truncated and rebuilt on resize. Fill lazily (only rows actually viewed). Do not cache short non-scrolling sections (e.g. the repository-view server list) - egui's per-frame cache already covers them and a galley cache is dead weight.

\- The settings FPS toggle (footer readout left of the log-export button) is the tool to measure these regressions.

\### Per-instance repository status

\- Repository status/pending-update maps in the UI are keyed per *instance* via `repo\_instance\_key(url, local\_path)` (= `normalize\_repo\_url(url) + U+001F + content\_hash::normalize\_path(path)`), not by URL alone, so two installs of one URL in different folders show independent status. Use the `\*\_for\_address` helpers in `src/ui/app/repository/list\_cache.rs` and thread `local\_path` through results/events. `repo\_foxy\_modes` is the deliberate exception - foxy mode is a URL-level property. See the identity invariant in root `AGENTS.md` and `conventions/CORE\_CONVENTIONS.md`.

\### Launch and Join readiness

An instance that cannot claim its game-space database renders only the already-running notice. Claim before loading settings or honoring a pending wipe marker, both at startup and on a space switch. Do not run background tasks, configuration persistence, or normal close persistence while the lock-conflict notice is active.

Launch and Join use the same readiness modal. An enabled Steam check blocks game-launch actions until Steam is running; TeamSpeak, missing external addons, and addon/DLC selections remain advisory. Standalone addon downloads do not require Steam. Join readiness confirmation resumes server status and addon queries before dispatch, and Steam is checked again at dispatch for games with a Steam App ID. Generic games without a Steam App ID do not require Steam.

Readiness process polling runs on one background worker at a time, with its cooldown measured from result receipt. Rendering applies only returned process flags to the current selections. Closing or replacing the modal, starting a client, and switching game spaces drop the result receiver so an old check cannot update another action. Never wait for process probes during modal rendering; retain the final Steam dispatch check.

The readiness modal scrolls its complete body within the viewport and keeps launch/cancel actions outside that scroll area. Addon matching retains folder aliases even when database display names are available. Display-name snapshots are keyed by normalized `(URL, local path)` so separate installs of the same repository cannot overwrite one another's addon metadata.

Database repair-required state is separate from prompt visibility and stays set until a wipe succeeds. The startup warning is dismissible so Foxy remains usable; dismissing it leaves repository/mod checks, updates, and background startup scans blocked. Wipe actions use green buttons in both the startup warning and the launch reminder. Launch and Join remain available. Their addon matching must not query an incompatible database.

After Steam, TeamSpeak, external-addon, server-addon, and DLC readiness steps, each final Launch or Join shows a separate database reminder while repair is required. Do not insert it at the initial Join readiness continuation that still needs to query the server. Offer **Wipe database and continue** as a green action, plus **Continue without wiping** and **Cancel**. Explain in text that wiping rebuilds database records while keeping downloaded mods, that checks and updates remain blocked until repair succeeds, and that continuing without wiping applies only to this action. Use clear labels and visible keyboard focus so color is not the only cue. Keep actions reachable outside scrolling content.

A live wipe stays marked in flight until its result is received, preventing another wipe or game-space switch during the rebuild. Successful repair resumes the exact saved Launch or Join continuation with its addon/DLC selections. Failure retains the repair requirement and pending action; cancel stops that action. Continuing without wiping resumes only that saved action and leaves the next Launch or Join eligible for the reminder.

\### Renderer startup

\- `src/ui/launcher.rs` owns graphics startup. A crash inside a Vulkan ICD or an injected overlay layer kills the process before any panic hook runs, so the only evidence is the launch-attempt marker (`renderer_fallback::record_launch_attempt`) written before `eframe::run_native` and cleared in the app-creator closure. `next_launch_stage` moves a launch that finds the marker one rung down `GraphicsLaunchStage`: `Full` (every wgpu backend, or the one remembered in the graphics-backend record), `SafeBackend` (DX12 on Windows, GL on Linux), then `Glow`, which is persisted into the renderer setting and shows the existing fallback notice.

\- Clear the marker only once the app is constructed, never earlier: an error returned by eframe is logged and is not a crash. A narrowed launch that fails before construction drops the remembered backend and retries on every backend, so a driver or GPU change never costs a working launch, and a retry never opens a second window over a session that already started.

\- Keep `MemoryHints::MemoryUsage` for wgpu (`configure_graphics_memory_hints`): Foxy uploads a font atlas and a few images, and the default performance hints only reserve commit.

\- `foxy ui --debug-modal <app-update|db-schema-wipe|storage-check>` previews a startup modal with placeholder data; register a new startup modal there and with the agent driver so it stays testable.

