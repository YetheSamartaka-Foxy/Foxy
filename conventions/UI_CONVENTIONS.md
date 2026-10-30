\## UI conventions (egui/eframe)

\- Keep UI code state-driven; store state in `Foxy` or view-specific structs.

\- Every new `Foxy` field is either space-scoped (reset in `reset_space_scoped_state`) or app-global (listed in `APP_GLOBAL_FOXY_FIELDS` with a reason). A guard test fails on anything unaccounted, because a space-scoped field left out of the reset leaks the previous game space's data into the next one. See `conventions/GAME_SPACES_CONVENTIONS.md`.

Settled R01-R24 decisions live in [reviewed contracts](REVIEWED_BEHAVIOR_CONTRACTS.md).
Render preset readiness with `repo_state_for_repository`, including its selected
profile and enabled addons; physical instance status alone cannot establish a
different preset's readiness. Closing may leave a saved changed selection unchecked.
Progress belongs to a phase and per-addon attempt generation; discard older
snapshots after fallback and label repeated work. Proxy restart retains the exact
approved instance, selection and modifiers after cancellation/recovery settles.
Restore arbitrates an automatic retry before accepting the restore action.
All review dialogs use ordinary focusable buttons and readable text; final Join
failure retains Retry/Cancel and shutdown requires a fresh post-save gameplay probe.

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

\- Preserve the established appearance across egui updates: use bundled monochrome icon fonts with native system-font fallback off, compact widget corners and padding from `base_visuals`/`apply_runtime_palette_visuals`, and full-width repository row fills regardless of label length or status. Compare populated sidebar, toolbar, settings and modal screenshots with the previous version before accepting a UI library update; passing interaction tests alone does not establish visual compatibility.

\- For long operations, show status: progress bar, label, or spinner.

\- Animated progress visuals live in `src/ui/app/ui_helpers/activity/` and are re-exported from `crate::ui::app`: `ActivityBar` (drop-in for `egui::ProgressBar`: up to four pixel-snapped rows of triangles side by side, pointing up and down by turns (`TriangleGrid` in `triangles.rs`, the verification map's tiling too), that fill with mana: a blink per triangle, climbing bubbles, twinkles and a glowing frontier, with a sweeping beam for unknown progress; a short bar drops rows, and `rows(1)` asks for a single line), `VerificationMap` (hashing and checks), `ProgressOrb` (sidebar rows, a round cluster of triangles), `ActivityIcon` (an animated glyph naming the work), `StepTrack` (bulk runs as one block per item), and `paint_border_beam`/`paint_border_flare` (a running or just-finished banner). `ActivityBar` uses triangles only for incomplete `Forward` or `Backward` work, including unknown progress. `Still`, `Held`, and completed bars use a solid track and continuous fill without completion flashes; completed downloads use the success color. Completed `StepTrack` blocks are solid and pending blocks are empty. Active progress fills are mana triangles, not squares, hatched stripes or smooth liquid. Bars that sit where the map can also appear use `VerificationMap::HEIGHT`, so the two match. Text over a bar goes through `paint_bar_label`, which adds a feathered backing and a glyph outline in the shade that contrasts with the text. Reuse these instead of new ad-hoc animations, and keep their colors flat: every triangle takes one of four `Tones` (empty, the fill, and two steps of the fill toward `ON_STRONG_FILL`), never a darker shade or a gradient. They move only while work runs and their rect is on screen, ask for frames through `Motion::keep_alive` at `PROGRESS_FRAME_INTERVAL`, and draw a still frame when the **Reduce motion** setting zeroes egui's `animation_time`. Idle UI never animates. Banner completion effects use `rise_effect`, which does not replay when a finished view is reopened.

\- Spinners are `crate::ui::app::PacedSpinner`, never `egui::Spinner` or `ui.spinner()`: egui's spinner requests a repaint on every paint and keeps the whole UI drawing at the display rate (223 fps measured through a repository check). A repaint that only needs to come soon goes through `crate::ui::app::request_frame_after`, which passes the requested delay directly to egui; do not add predicted frame time to it. The `UI frame cost during sync` log line reports frames, UI-thread CPU and the top repaint causes for every check. Progress pacing also follows where the window is (`set_window_presence`, set once a frame in `update`): at most 10 fps while another window has focus and 1 fps while minimized or hidden to the tray, through `progress_frame_interval_override`, so drawing never competes with a game in front; the FPS counter stops forcing continuous frames out of front, and a session with the GUI harness attached keeps foreground pacing so testkit frame metrics stay comparable. New progress pacing goes through `progress_frame_interval()`, never a fixed interval.

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

\- Fields and queues that name a repository by list position (`syncing\_repository`, `update\_ready\_repo`, `download\_finished\_repo`, the folder and space sync queues, the bulk dialog, the completed-check banner) are re-pointed in one place, `Foxy::remap\_repository\_indices`, which delete, reorder, and clone all call. A new index-keyed field must be added there; queued work for a removed repository is dropped, never left to start on the repository that moved into its slot. Background results resolve their target by `(url, local\_path)` on arrival (`repo\_index\_by\_instance`); the URL-only fallback of `repo\_index\_by\_url\_and\_path` applies only when exactly one instance has the URL, and scheduled jobs never use it.

\- Deleting a repository with "Delete files" scopes removal to its exact URL/path instance. Another same-URL installation in an independent physical folder does not prevent removal. Protect shared or overlapping physical folders using the same ownership helper for both confirmation and execution. See N02 in [accepted behavior contracts](BEHAVIOR_CONTRACTS.md).

\- A repository's Address and Local Path are read-only while that instance syncs, quick-scans, or has a database wipe pending (`Foxy::repository\_identity\_locked`): the running work would otherwise land its results on the new identity and the folder change would wipe rows the worker is still writing. A database wipe is refused while a quick scan of the instance runs, and a quick scan does not start while its repository is being purged.

\- The user's Cancel (`cancel\_sync\_and\_queued\_runs`) stops the folder, space, or scheduled run the sync belongs to. A space bulk member that is refused or loses its terminal event counts as failed, and one that is detached or deleted leaves the run, so the run always completes. A scheduled post-action never runs after a cancel, treats a target it could not start as not succeeded, and restarts its countdown while other work runs. A result dispatched before a game-space switch (repository metadata, a cached update load) is dropped because the switch clears the pending set it must still be in.

\- A scheduled job that falls due while other work runs is held (`scheduler\_held\_jobs`) and starts when the work finishes; waiting does not age it toward the 10-minute missed window (`ScheduledJob::due\_state\_with\_hold`). Only a gap in scheduler ticks longer than that window (the computer slept, Foxy was suspended) drops a held claim, so the scheduler repaints every 5 s while a job is held. A missed run logs its reason (`missed\_run\_reason`).

\- A changed addon selection (row toggles, enable or disable all, profile switch) queues a quick local check of that instance from the repositories save path (`queue\_checks\_for\_changed\_addon\_selections`), so background checks never keep a stale status. Any sync start records the selection as stored. The check never interrupts a running sync, queue, bulk run, scheduled job, armed benchmark, or open update modal.

\### Launch and Join readiness

An instance that cannot claim its game-space database renders only the already-running notice. Claim before loading settings or honoring a pending wipe marker, both at startup and on a space switch. Do not run background tasks, configuration persistence, or normal close persistence while the lock-conflict notice is active.

Launch and Join use the same readiness modal. An enabled Steam check blocks game-launch actions until Steam is running; TeamSpeak, missing external addons, and addon/DLC selections remain advisory. Standalone addon downloads do not require Steam. Join readiness confirmation resumes server status and addon queries before dispatch, and Steam is checked again at dispatch for games with a Steam App ID. Generic games without a Steam App ID do not require Steam.

Readiness process polling runs on one background worker at a time, with its cooldown measured from result receipt. Rendering applies only returned process flags to the current selections. Closing or replacing the modal, starting a client, and switching game spaces drop the result receiver so an old check cannot update another action. Never wait for process probes during modal rendering; retain the final Steam dispatch check.

The readiness modal scrolls its complete body within the viewport and keeps launch/cancel actions outside that scroll area. Addon matching retains folder aliases even when database display names are available. Display-name snapshots are keyed by normalized `(URL, local path)` so separate installs of the same repository cannot overwrite one another's addon metadata.

Database repair-required state is separate from prompt visibility and stays set until a wipe succeeds. The startup warning is dismissible so Foxy remains usable; dismissing it leaves repository/mod checks, updates, and background startup scans blocked. Wipe actions use green buttons in both the startup warning and the launch reminder. Launch and Join remain available. Their addon matching must not query an incompatible database.

After Steam, TeamSpeak, external-addon, server-addon, and DLC readiness steps, each final Launch or Join shows a separate database reminder while repair is required. Do not insert it at the initial Join readiness continuation that still needs to query the server. Offer **Wipe database and continue** as a green action, plus **Continue without wiping** and **Cancel**. Explain in text that wiping rebuilds database records while keeping downloaded mods, that checks and updates remain blocked until repair succeeds, and that continuing without wiping applies only to this action. Use clear labels and visible keyboard focus so color is not the only cue. Keep actions reachable outside scrolling content.

A live wipe stays marked in flight until its result is received, preventing another wipe or game-space switch during the rebuild. Successful repair resumes the exact saved Launch or Join continuation with its addon/DLC selections. Failure retains the repair requirement and pending action; cancel stops that action. Continuing without wiping resumes only that saved action and leaves the next Launch or Join eligible for the reminder.

### Agent GUI input

- Driver input enters the root viewport through `eframe::App::raw_input_hook`, before egui derives keyboard focus, held keys, and pointer interactions. Do not append synthetic events to the current `InputState` from UI polling.
- Keep native window and viewport focus unchanged. Inject modifier changes with the input and restore native modifier state on the following frame.
- Named focus requests open the owning view and container, wait for an enabled rendered widget, and confirm that focus persists through the following pass before replying. New windows may need a sizing pass before their widgets accept focus.
- Send repeated Tab navigation one step per rendered frame; multiple Tab presses in one raw pass collapse into one focus move. Navigation replies observe the final completed input frame.
- A batch pauses after a raw input step through egui's deferred focus pass before evaluating its next command. Drag replies wait for the release frame. An incomplete navigation, drag, or settle returns a timeout error, never a successful partial result.
- Verify keyboard flow through actual focus changes and activation outcomes. A queued-input acknowledgement or an accepted command alone does not prove the widget received it. Register decision overlays in the driver's modal list so waits and snapshots can identify them.

### Accepted status, queue, and launch contracts

The [accepted behavior contracts](BEHAVIOR_CONTRACTS.md) add settled N01-N18 for
preparation consent/recovery, launch handoff, imported selections and queued
intent. Enforce these choices and investigate new gaps rather than asking again.

- A locally verified repository with unknown remote freshness stays launchable. Show persistent inline status such as "Locally verified; server not checked" and the last successful remote-check time. Do not add a launch dialog for this state.
- A terminal retryable transfer error gets automatic retries by default while Foxy stays open, controlled by app-global `retry_failed_downloads` (default enabled). Show countdown and Stop. Explicit cancellation never retries. A multi-repository run retries a target once when the error may be transient, then continues unaffected targets and reports a partial result with failed-target retry available. Never report the overall run as successful when any target failed or was skipped.
- If a scheduled job definition is edited, disabled, or deleted while that job runs, ask whether the change affects future runs only or also stops the active run and its post-action. The active run uses its captured target and action snapshot unless explicitly stopped.
- Launch or Join for an instance whose files are being replaced is queued and cancellable against that exact `(URL, local path)` identity. Recheck readiness when the update ends; an update failure does not launch. An unrelated installation does not block it.
- Refresh server requirements and relevant local addon state immediately before Join dispatch. Preserve compatible choices; if facts changed, show the difference and ask for confirmation again. Do this even when the earlier checklist is recent.

\### Renderer startup

\- `src/ui/launcher.rs` owns graphics startup. A crash inside a Vulkan ICD or an injected overlay layer kills the process before any panic hook runs, so the only evidence is the launch-attempt marker (`renderer_fallback::record_launch_attempt`) written before `eframe::run_native` and cleared in the app-creator closure. `next_launch_stage` moves a launch that finds the marker one rung down `GraphicsLaunchStage`: `Full` (every wgpu backend, or the one remembered in the graphics-backend record), `SafeBackend` (DX12 on Windows, GL on Linux), then `Glow`, which is persisted into the renderer setting and shows the existing fallback notice.

\- Clear the marker only once the app is constructed, never earlier: an error returned by eframe is logged and is not a crash. A narrowed launch that fails before construction drops the remembered backend and retries on every backend, so a driver or GPU change never costs a working launch, and a retry never opens a second window over a session that already started.

\- Keep `MemoryHints::MemoryUsage` for wgpu (`configure_graphics_memory_hints`): Foxy uploads a font atlas and a few images, and the default performance hints only reserve commit.

\- `foxy ui --debug-modal <app-update|db-schema-wipe|storage-check>` previews a startup modal with placeholder data; register a new startup modal there and with the agent driver so it stays testable.

