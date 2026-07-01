# Foxy Agentic GUI Harness Design

## Goal

Give coding agents a local, scriptable way to drive Foxy's real egui/eframe desktop UI alongside MCP inspection. The harness should feel close to Playwright's interaction loop: act, observe structured state, optionally take a screenshot, then act again.

This file is a design and implementation contract.

Foxy also supports eframe's upstream inspection protocol through the `eframe/inspection` feature. Run it together with this Foxy-specific harness when MCP access is available: `egui_mcp` provides generic MCP access to the AccessKit tree, input injection, screenshots, and window resize; Foxy's `agent-gui` harness provides token-gated control, Foxy state fetches, fixtures, app intents, performance counters, and destructive-action safeguards.

## Implemented status

The harness is **implemented**. Live map:

- App-side driver: `src/ui/app/agent_driver.rs` (TCP loopback server on the UI thread, token-gated, session file `agent-gui-session.json` in the config dir).
- CLI client: `src/cli/commands/agent_gui.rs`; args in `src/cli/args.rs` (`UiArgs` flags + `AgentGuiCommand` group).
- Launch: `Foxy.exe ui --agent-gui --agent-port 0`; desktop-integration (Start Menu / `.desktop`) is auto-skipped while `--agent-gui` is set (`src/main.rs::launch_ui`).
- Isolation: `--config-dir` / `FOXY_CONFIG_DIR` redirects config + DB + data (`src/core/utils/app_paths.rs`).
- Upstream MCP inspection: `Cargo.toml` enables eframe's `inspection` feature. Set `EGUI_INSPECTION=1` (default `127.0.0.1:5719`) or `EGUI_INSPECTION=<host:port>` at launch, then connect `egui_mcp`.

Implemented commands: `status`, `open-view` (with `--repo-index` and `--tab`), `snapshot` (with `--fields`/`--since-frame`), `text`, `find`, `click`, `scroll`, `hover`, `mouse-down`, `mouse-up`, `key`, `type`, `screenshot` (with `--annotate`), `fps`, `wait`, `logs`, `repositories`, `addons`, `settings`, `progress`, `scale`, `resize`, `profiles`, `missions`, `spaces`, `download-summary`, `toasts`, `set-setting`, `close`. Input commands (`key`/`click`/`scroll`/`mouse-*`) accept `--ctrl/--shift/--alt/--command` modifiers. The structured-state reads (`repositories`, `addons`, `settings`, `progress`, `profiles`, `missions`, `spaces`, `download-summary`, `toasts`) let an agent observe app state that has no semantic nodes - addon rows especially. `set-setting` is the only state-mutating read/write fetch; it stays loopback + token-gated and clamps like the offline `settings set` CLI.

**Extended commands** (see [SKILL.md](../SKILL.md) "Extended commands"): efficiency - `--flat`/`--field` client projection, `snapshot --fields/--since-frame`, `settle`, `exec --stdin` (one persistent connection, NDJSON from stdin); interaction - `focus`/`nav`/`fill`, `filters`/`set-filter`, `select`, `window`, `context-menu`/`menu-select`; fetches - `inventory`, `pending-updates`, `app-update`, `memory`, `arma-profiles`, `backups`; diagnostics/determinism - `health`, `stable-render`, `screenshot --annotate`, `assert`, and the `scenario`/`fixture` client-side runners. Snapshot gained a `focused` field. State-mutating commands stay loopback + token-gated; `fixture` only ever writes the small JSON config (allowlisted: `settings.json`/`repositories.json`/`repository_spaces.json`) into the isolated `FOXY_CONFIG_DIR`, never the database; fetches redact absolute paths to basenames. See [SKILL.md](../SKILL.md) for the operating playbook and recipes.

**Semantic / pipelined / determinism commands** (see [SKILL.md](../SKILL.md) "Semantic / pipelined / determinism commands"): `invoke` (named app-action registry + `list-actions`, destructive intents gated behind `--allow-destructive`), `batch` (server-side pipeline that resumes across frames for parking steps), `diff` (field-level delta vs a stored `last`/`frame:<n>` baseline), `drag` (one-call multi-frame drag), `query` (JMESPath over the union app-state document), `checkpoint`/`restore` (UI-state save & rollback - UI state only), `element` (deep single-node introspection + coordinate hit-test), `events` (semantic UI-event ring with incremental tailing), `clock` (virtual UI-timer time control), `dialog` (native picker automation). The virtual clock and dialog-interception slot live in [`src/ui/app/agent_support.rs`](../../../src/ui/app/agent_support.rs), consulted by ordinary UI code (toast expiry; the `rfd` picker call sites) only while the driver is running. `query` adds the `jmespath` crate. All security invariants hold: destructive intents need `allow_destructive`, paths redact to basenames, and `restore` never rolls back core/DB/disk.

The CLI supports `--toon` for agent-facing machine output. TOON encodes the whole `CliEnvelope` at stdout and accepts TOON for whole-document inputs (`scenario`, `batch --steps`/`--stdin`, `invoke --params`) when enabled, with JSON fallback. `FOXY_AGENT_TOON=1` makes agent-gui CLI calls default to TOON. The app-side TCP wire remains newline-delimited JSON because no LLM reads that internal loopback protocol.

## Research Notes

- `egui_kittest` supports querying egui widgets by semantic labels and can do snapshot tests with its `snapshot` and `wgpu` features. Use it for headless component-level tests, not as the live desktop control channel.
- `egui_inspection` defines the upstream request/response inspection protocol and `InspectionPlugin`. With eframe's `inspection` feature, the plugin is attached from `EGUI_INSPECTION` at startup and can expose AccessKit tree reads, input injection, screenshots, and resize over TCP.
- `egui_mcp` is the first MCP consumer of `egui_inspection`. Use it in the same session as Foxy's harness when possible: `egui_mcp` has generic widget access, and `agent-gui` has Foxy-specific state and safeguards.
- `kittest` is powered by AccessKit, which makes semantic UI queries a good fit for Rust GUI testing when the UI exposes useful accessibility nodes.
- AccessKit is intended to expose a cross-platform accessibility tree for toolkits with custom-rendered UI. eframe integrates AccessKit by default, so Foxy should preserve accessibility names and roles as the semantic layer improves.
- egui exposes `ViewportCommand::Screenshot`, and the result is delivered back as `egui::Event::Screenshot`. This is the preferred in-app screenshot path because it captures the actual egui frame without external screen-grabbing tools.
- egui input is represented as events in `RawInput`; a live harness can inject pointer, scroll, text, and key events inside the app process rather than relying only on OS-level coordinate automation.

Useful upstream docs:

- `egui_kittest`: https://docs.rs/egui_kittest
- `eframe` inspection feature: https://docs.rs/eframe/latest/eframe/#feature-flags
- `egui_inspection`: https://docs.rs/egui_inspection
- `egui_mcp`: https://github.com/rerun-io/kittest_inspector/tree/main/crates/egui_mcp
- `egui::ViewportCommand::Screenshot`: https://docs.rs/egui/latest/egui/viewport/enum.ViewportCommand.html
- `egui::Event`: https://docs.rs/egui/latest/egui/enum.Event.html
- AccessKit overview: https://accesskit.dev/
- AccessKit repository: https://github.com/AccessKit/accesskit

## Architecture

Add two pieces:

1. App-side driver service, enabled only for test/dev sessions.
2. CLI client commands that send JSON requests and print JSON or TOON responses.

Recommended startup shape:

```powershell
$env:EGUI_INSPECTION = "1"
cargo run -- ui --debug-mode --agent-gui --agent-port 0 --config-dir temporary_files\agent-gui-run
```

`--agent-port 0` should bind to `127.0.0.1` on an OS-selected port, write a session file under the selected config root, and include a random token. The CLI client reads that session file unless the caller explicitly passes host/port/token.

Session file example:

```json
{
  "pid": 12345,
  "host": "127.0.0.1",
  "port": 49231,
  "token": "base64url-random-token",
  "created_at": "2026-06-14T14:00:00Z"
}
```

Prefer newline-delimited JSON over loopback TCP or named pipes. Keep the protocol small and synchronous from the client's point of view: every command has one response, with optional `request_id`.

TOON is intentionally not used on the loopback wire. It is a CLI boundary format for agent-read stdout and bulk agent-written inputs only.

## Security Rules

- The driver must be disabled by default.
- Bind only to loopback or use an OS-local pipe.
- Require an unpredictable token for every command.
- Store the token only in the run's temporary config root or a caller-specified token file.
- Reject destructive app/core commands unless the command carries an explicit `allow_destructive: true` flag and the underlying Foxy action already has its normal confirmation semantics.
- Never expose user paths in logs beyond what Foxy already logs; prefer basenames or redacted paths in harness responses unless the command explicitly asks for full paths.
- Do not let screenshots default to `%APPDATA%\Foxy`; write them to the caller's output path or the temporary config root.
- Driver sessions must not mutate the user's environment: under `--agent-gui` the app skips Start Menu / `.desktop` registration (`src/main.rs::launch_ui`). Always run with an isolated `FOXY_CONFIG_DIR`; to test with real data, copy only `settings.json` / `repositories.json` / `repository_spaces.json` into it (never the multi-GB `database.db` - addon lists rebuild from the on-disk inventory scan).

## Command Contract

App-side driver responses should be JSON objects on the loopback wire. The CLI may wrap and serialize them as JSON (`--json`) or TOON (`--toon`) at stdout:

```json
{
  "ok": true,
  "command": "snapshot",
  "view": "repository-list",
  "elapsed_ms": 7,
  "errors": []
}
```

On failure:

```json
{
  "ok": false,
  "command": "click",
  "view": "settings",
  "elapsed_ms": 3,
  "errors": [
    {
      "code": "not_found",
      "message": "No visible node matched text 'Download speed'"
    }
  ]
}
```

### `status`

Return process health and session metadata.

Fields: `pid`, `view`, `renderer`, `debug_mode`, `uptime_ms`, `startup_frame_rendered`, `busy`, `active_modal_count`.

### `open-view`

Switch `Foxy.current_view`.

Supported values map directly to `FoxyView`: `repository-list`, `settings`, `repository-settings`, `repository-space-settings`, `help`, `changelog`, `about`, `app-update`, `version-browser`, `swifty-migration`.

For views that require selected state, reject with a clear error unless selectors are provided. `repository-settings` needs `--repo-index` (or a selected repository).

`--tab configuration|addons|optional-addons|external-addons` selects the repository-settings sub-tab (parsed by `parse_agent_gui_repository_settings_tab`). Addon rows are not semantic nodes, so this is the only way to reach an addon list for scroll/perf testing.

### `snapshot`

Return a structured observation of the current UI.

Minimum fields:

```json
{
  "view": "repository-list",
  "update_modal_open": false,
  "fps": 59.8,
  "texts": ["Repositories", "Settings"],
  "nodes": [
    {
      "id": "footer.settings",
      "role": "button",
      "text": "Settings",
      "enabled": true,
      "focused": false,
      "rect": { "x": 920.0, "y": 734.0, "w": 36.0, "h": 28.0 }
    }
  ]
}
```

Include node rectangles in egui logical points. If AccessKit data is not sufficient, add explicit Foxy instrumentation IDs at widget creation sites.

Implemented snapshot adds Foxy-specific state useful to agents: `repository_settings_tab` (current tab name when in repository-settings), `repositories_count`, `selected_repository`, `frame` (cumulative frames), `cumulative_pass_nr` (cumulative egui passes), `busy`, `busy_reasons` (stable kebab-case names of the background-work flags set - *why* `busy` is true), `active_modal_count`, `active_modals` (stable kebab-case names of open dialogs), `pointer` (latest cursor position or `null`), `pixels_per_point`, `zoom_factor` (UI-scale multiplier, drive with `scale`), `startup_frame_rendered`.

> Note the CLI envelope: client output is `CommandSuccess` whose `.data` is the `AgentGuiResponse` whose `.data` is this snapshot - i.e. fields are at `.data.data.<field>`.

### `text`

Return only visible text and optional context. This is the cheap command an agent should use before a screenshot.

Options: `--contains`, `--regex`, `--limit`.

### `find`

Return nodes matching semantic criteria.

Options: `--text`, `--role`, `--id`, `--visible-only`.

### `click`

Prefer semantic selectors:

```powershell
cargo run -- agent-gui click --text "Settings" --json
cargo run -- agent-gui click --id "footer.settings" --json
```

Coordinate fallback:

```powershell
cargo run -- agent-gui click --x 480 --y 320 --json
```

Coordinate clicks should use egui logical points, not physical pixels. `click` also accepts `--button primary|secondary|middle`, `--double`, and `--ctrl/--shift/--alt/--command` modifiers. `--id <node>` with no semantic handler clicks the node's rect center.

### `hover`, `mouse-down`, `mouse-up`

Pointer primitives for tooltips, hover states, and frame-by-frame drags.

```powershell
cargo run -- agent-gui hover --x 990 --y 400 --json
cargo run -- agent-gui mouse-down --x 100 --y 200 --json
cargo run -- agent-gui hover --x 250 --y 200 --json
cargo run -- agent-gui mouse-up --x 400 --y 200 --json
```

`hover` injects a `PointerMoved`. `mouse-down`/`mouse-up` inject a single `PointerButton` press/release (with `--button` and modifiers). Compose a drag across separate calls (= separate frames); a single in-frame down→move→up is not reliably classified as a drag by egui.

### `scroll`

Send scroll input to the current pointer target or an explicitly selected node.

```powershell
cargo run -- agent-gui scroll --dy 520 --json
cargo run -- agent-gui scroll --x 990 --y 400 --dy -800 --json
```

Direction follows egui's `MouseWheel` convention: negative `dy` scrolls content down (reveals lower rows). Numeric args use `allow_hyphen_values`, so `--dy -800` parses without the `=` workaround. The driver injects `PointerMoved` to the target before the wheel event, so pass `--x/--y` to hover a specific column (e.g. right-edge icon buttons) - default target is the content center. `scroll` also accepts `--ctrl/--shift/--alt/--command` (e.g. Ctrl+scroll for zoom-style handlers).

### `key` and `type`

Named keys (`Escape`, `Enter`, `Tab`, `ArrowUp/Down/Left/Right`, `PageUp`, `PageDown`, `Home`, `End`, …) plus the full egui set via `Key::from_name` fallback: letters (`a`), digits (`5`), function keys (`F5`), and punctuation. `key` accepts `--ctrl/--shift/--alt/--command`, e.g. `--key a --ctrl` for Ctrl+A.

`type` emits text input into the currently focused widget.

### `screenshot`

Request `egui::ViewportCommand::Screenshot`, wait for `egui::Event::Screenshot`, encode PNG, and write to the requested output path.

Response fields: `screenshot_path`, `width`, `height`, `scale_factor`, `view`.

Use a timeout so agents do not hang if the core cannot return a screenshot.

### `fps`

Returns `fps` (`Foxy.fps_ema`), `fps_counter_visible`, and the egui pass/frame counters `cumulative_frame_nr` + `cumulative_pass_nr`.

The pass/frame counters are the key perf-regression signal. egui normally runs one pass per frame; extra passes mean multi-pass relayout (`request_discard`) - the cost behind the "changed id between passes" warning and scroll FPS drops. Sample `fps` twice around a scroll burst and compute:

```
extra_passes = (pass_nr_after - pass_nr_before) - (frame_nr_after - frame_nr_before)
```

`extra_passes == 0` is healthy single-pass rendering; `> 0` flags the regression. Caveat: the driver's discrete event injection may not reproduce interactive multi-pass triggers (hover tooltips, scroll momentum, high-DPI), so a `0` reading is not proof the interactive path is clean.

### `wait`

Wait for state without polling in the agent:

```powershell
cargo run -- agent-gui wait --text "Repository settings" --timeout-ms 5000 --json
cargo run -- agent-gui wait --view settings --timeout-ms 2000 --json
cargo run -- agent-gui wait --idle --timeout-ms 10000 --json
cargo run -- agent-gui wait --modal-open --timeout-ms 5000 --json
cargo run -- agent-gui wait --modal-closed --timeout-ms 5000 --json
cargo run -- agent-gui wait --toast "Saved" --timeout-ms 5000 --json
cargo run -- agent-gui wait --busy-reason-cleared core-sync --timeout-ms 60000 --json
cargo run -- agent-gui wait --download-complete --timeout-ms 120000 --json
cargo run -- agent-gui wait --fps-above 45 --timeout-ms 5000 --json
cargo run -- agent-gui wait --node-visible footer.help --timeout-ms 2000 --json
```

Conditions beyond text/view/idle/modal: `--toast <text>` (a feedback toast whose
message contains the text is showing - pairs with `toasts`),
`--busy-reason-cleared <name>` (the named `busy_reasons` flag is no longer set,
so you can wait out a single `core-sync`/`quick-scan` without going fully
idle), `--download-complete` (`download_finished`), `--fps-above <n>`
(`fps_ema >= n`), and `--node-visible <id>` (a known semantic node is on-screen).
Each is a predicate over existing per-frame state reusing the pending-wait
machinery, so the agent never has to poll.

### `logs`

Read the in-memory activity-log buffer (the same data behind the footer activity log, egui/wgpu warnings included) so agents can assert on app output without tailing the redirected stdout file.

```powershell
cargo run -- agent-gui logs --level warn --limit 20 --json
cargo run -- agent-gui logs --contains "redownload" --json
cargo run -- agent-gui logs --since-generation 1234 --json
```

Options: `--level` (error/warn/info/debug/trace), `--contains` (matches message or source), `--limit` (most recent N), `--since-generation` (incremental tail). Response: `generation` (monotonic counter, one tick per appended entry), `count`, and `entries` (`timestamp`/`level`/`source`/`message`). Pass a prior `generation` back as `--since-generation` to fetch only what was logged since.

### `repositories`

Structured list of the configured repositories - no UI navigation required. Fills the gap that the repository list is virtualized and rows aren't reliably semantic.

```powershell
cargo run -- agent-gui repositories --json
cargo run -- agent-gui repositories --contains tfr --limit 5 --json
```

Options: `--contains` (matches name or address, case-insensitive), `--limit`. Response: `total`, `returned`, and `repositories[]` with `index`, `name`, `address`, `path`, `state` (`synced`/`pending-update`/`updating`/`unknown` from `repo_states`), `selected`, `pending_update_count` (from the pending-update cache), `addon_count`, `optional_addon_count`, `external_addon_count`, `profile_count`, `selected_profile`, `space_id`.

### `addons`

Structured addon rows for one repository tab. Addon rows are **not** semantic nodes, so this is the only way to read their names/enabled/size state programmatically (previously: screenshot-diff only).

```powershell
cargo run -- agent-gui addons --repo-index 0 --tab external-addons --json
cargo run -- agent-gui addons --repo-index 0 --tab addons --enabled-only --contains ace --limit 20 --json
```

`--repo-index` defaults to the selected repository; `--tab` (`configuration`/`addons`, `optional-addons`, `external-addons`) defaults to the current repository-settings tab. Options: `--contains`, `--enabled-only`, `--limit`. Response: `repository_index`, `repository_name`, `tab`, `total`, `returned`, and `addons[]` with `name`, `enabled`, `kind` (`required`/`optional`/`external`), `size_bytes` (0 until the repo-settings view has loaded sizes in the background), plus `source` (external) and `favorite`/`client_side` (optional + external). Reading addons does not require `open-view` first; to *render* a tab for scroll/screenshot/perf work, still use `open-view --tab`.

### `settings`

Serialize the live, effective `SettingsViewState` the running app currently holds (same shape as the `settings show` CLI command, but from in-memory state). Lets an agent confirm a settings-screen interaction actually mutated app state.

```powershell
cargo run -- agent-gui settings --json
```

### `progress`

Live download/sync/recheck state plus the busy breakdown.

```powershell
cargo run -- agent-gui progress --json
```

Response: `busy`, `busy_reasons` (stable kebab-case flags), `syncing_repository`, `current_sync_mode`, `download_active`, `download_label`, `download_percent`, `download_paused`, `download_finished`, `download_speed_bps`, `download_eta_secs`, `total_downloaded_bytes`, `active_mod_downloads`, `recheck_stage_label`, `recheck_stage_percent`, `recheck_hash_counter` (`{done,total}`), `update_modal_open`.

### `scale` and `resize`

Reproduce the high-DPI and large-window relayout paths that discrete event injection can't otherwise trigger (the interactive multi-pass caveat under `fps`).

```powershell
cargo run -- agent-gui scale 150 --json                       # global UI scale percent (25-500, like the slider)
cargo run -- agent-gui resize --width 1100 --height 720 --json # window inner size in logical points
```

`scale` sets the global UI-scale setting (`ui_scale_percent`) and marks settings dirty; the next frame's `zoom_factor`/`pixels_per_point` reflect it (response echoes the clamped `ui_scale_percent`). `resize` sends `ViewportCommand::InnerSize`; confirm via the next `snapshot.content_rect`.

### `profiles`

Structured launch profiles for a repository (`RepositoryProfile`), which are not semantic nodes. `--repo-index` defaults to the selected repository; `--contains`/`--limit` filter by name.

```powershell
cargo run -- agent-gui profiles --repo-index 0 --json
```

Response: `repository_index`, `repository_name`, `selected_profile`, `total`, `returned`, and `profiles[]` (`name`, `selected`, `flags` = csla/ef/gm/rf/spe/vn/ws/skip_intro/no_splash/world_empty/load_mission_to_memory/enable_ht/huge_pages/no_logs/include_steam_addons, `additional_params`, and `addon_override_count`/`optional_addon_override_count`/`external_addon_override_count`).

### `missions`

The cached editor missions for the *currently viewed* repository (`cached_missions: CachedMissionList`). Reads in-memory state only - `loaded` is `false` until a repository view has populated it. Names/folders/terrain are exposed; absolute paths are not (harness security rule).

```powershell
cargo run -- agent-gui missions --contains altis --json
```

Response: `loaded`, `profile_name`, `scanned_age_ms`, `total`, `returned`, and `missions[]` (`display_name`, `folder_name`, `world_name`, `root_folder_name`, `is_multiplayer`, `author`, `game_type`, `max_players`). Filter with `--contains` (matches name/folder/terrain) and `--limit`.

### `spaces`

Repository spaces (`repository_spaces`) with attached-repo counts and bulk-action progress. Path/checksum fields are intentionally omitted.

```powershell
cargo run -- agent-gui spaces --json
```

Response: `selected_space`, `bulk_progress` (`null` unless a space bulk action is running: `space_id`, `mode`, `total_count`, `completed_count`, `succeeded_count`, `failed_count`, `updates_available_count`, `up_to_date_count`, `current_repo_name`), `total`, `returned`, and `spaces[]` (`id`, `name`, `local_name_override`, `collapsed`, `selected`, `manifest_entry_count`, `required_entry_count`, `attached_repository_count`). Filter with `--contains`/`--limit`.

### `download-summary`

The last completed `DownloadSummary` - the structured "did the sync do what I expected?" assertion, instead of log parsing. `present` is `false` until a download has finished.

```powershell
cargo run -- agent-gui download-summary --json
cargo run -- agent-gui download-summary --include-telemetry --json
```

Response: `present`, `mods_updated`, `files_updated`, `parts_updated`, `downloaded_bytes`, `planned_transfer_bytes`, `full_download_bytes`, `patch_savings_bytes`, `patched_files`, stage durations in ms (`download_stage_ms`, `hash_stage_ms`, `cumulative_hash_ms`, `after_download_hash_ms`, `total_ms`), `avg_speed_bps`, and `telemetry_sample_count`. `--include-telemetry` adds `telemetry_samples[]` (`elapsed_ms`, `download_bps`, `disk_write_bps`, `hash_files_per_sec`, `cpu_percent`, `memory_bytes`); omitted by default because the series can hold up to 180 samples.

### `toasts`

The current user-feedback toast (`ui_toast`), or `present: false` once it has outlived its display duration. Pairs with `wait --toast <text>`.

```powershell
cargo run -- agent-gui toasts --json
```

Response: `present`, `message`, `kind` (`success`/`error`), `age_ms`, `remaining_ms`, `duration_ms`.

### `set-setting`

Mutate one live setting on the running app and observe the UI react - the read/write complement to the read-only `settings` fetch. Clamps/validates the way the offline `settings set` CLI does, and reuses `mark_settings_dirty()` (the debounced save) or the dedicated helper for each field. Loopback + token-gated like every command.

```powershell
cargo run -- agent-gui set-setting show-fps-counter true --json
cargo run -- agent-gui set-setting ui-scale-percent 150 --json
cargo run -- agent-gui set-setting download-speed-limit-mbps unlimited --json
cargo run -- agent-gui set-setting locale en --json
```

Keys: `debug-mode`, `show-activity-log` (routes through `set_activity_log_visibility`), `show-fps-counter` (bool spellings: true/1/on/yes/false/0/off/no), `ui-scale-percent` (integer, reuses the `scale` clamp), `locale` (string; also calls `i18n.set_language`), `download-speed-limit-mbps` (integer ≥ 1, or `unlimited`/`none` to disable). Response echoes `{ key, value }` with `value` set to the *applied* (clamped) value so an agent can confirm clamping; an unknown key or unparseable value returns an `invalid-setting` error.

### `invoke`

Drive a named app-action by intent rather than by pixels. `AgentGuiCommand::Invoke { action, params, allow_destructive, list_actions }`. The `AGENT_ACTIONS` table maps kebab-case names → a branch over `&mut Foxy` that calls the *same* methods the buttons call (`start_core_sync`, `set_download_paused`, `cancel_sync`, `force_redownload_repository`, `present_launch_preflight`, `open_settings_view`, …). `list_actions` returns `{name, destructive, params, summary}` per action for runtime discovery. Destructive intents (anything that mutates core/disk: sync/recheck/force-redownload/launch) require `allow_destructive` and keep Foxy's confirmation/preflight semantics. The CLI merges `--repo-index`/`--profile` into `params`. Response: `{action, ran, destructive, snapshot}`.

### `batch`

Server-side pipeline. `AgentGuiCommand::Batch { steps, stop_on_error }`. `agent_gui_start_batch` drives steps in order on the UI thread; each step runs through `handle_agent_gui_request` with an **internal response channel**. A step that parks (`settle`/`wait`/`screenshot`/`drag`) leaves its channel empty, so the batch stores the receiver (`PendingBatch.step_rx`) and `poll_agent_gui` resumes it via `agent_gui_advance_batches` once the per-step completion pass fires. Reuses the existing `PendingSettle`/`PendingWait`/`PendingScreenshot`/`PendingDrag` machinery - never blocks the UI thread. Nested batches are rejected. Response: `{ ok, total, results:[ <AgentGuiResponse>, … ] }`. Distinct from `exec` (client-side, one-at-a-time) and `scenario` (client-side, reconnect per step).

### `diff`

Field-level delta between two observations. `AgentGuiCommand::Diff { baseline }`. The runtime keeps a capped ring of recent `AgentGuiSnapshot`s (pushed by every `snapshot`/`diff`); `baseline` is `last` or `frame:<n>` (exact frame, else most recent ≤ n). Node-set diff is keyed on the stable node `id`; scalars compared directly (high-noise fields `fps`/`frame`/`cumulative_pass_nr`/`pointer`/`content_rect` excluded). Response: `{ added_nodes[], removed_nodes[], changed_fields:{field:{from,to}}, text_added[], text_removed[], baseline_frame, current_frame }`. Pairs with `batch`.

### `drag`

First-class drag gesture. `AgentGuiCommand::Drag { from_*, to_*, steps, button }`. Enqueues a `PendingDrag` that emits `PointerButton(down)` on frame *n*, `steps` interpolated `PointerMoved` events on frames *n+1..*, and `PointerButton(up)` on the last - one event per **real** rendered frame (a single in-frame down→move→up is not classified as a drag by egui), driven by the same per-frame resume pattern as `PendingSettle`. Returns the post-drag snapshot.

### `query`

One JMESPath query over the union of the structured fetches. `AgentGuiCommand::Query { expr }`. `agent_gui_state_document` composes the existing `agent_gui_*_value` builders into one `serde_json::Value` (only the sub-documents the expression names are built; array sections are exposed as their inner arrays), then the `jmespath` crate evaluates `expr`. Read-only. Response: `{ expr, result }`.

### `checkpoint` / `restore`

UI-state save & rollback for safe exploration. `Checkpoint { name, list }` / `Restore { name }`. Captures the serializable UI subset (`current_view`, selection indices, `repository_settings_tab`, space id, list filters, the simple modal flag, `SettingsViewState`) into a `HashMap<String, Value>` on the runtime; restore writes it back and repaints. **UI state only** - it does not roll back core/DB/disk; the response carries `ui_state_only: true`. For true data isolation use `FOXY_CONFIG_DIR` + `fixture`.

### `element`

Deep single-node introspection. `AgentGuiCommand::Element { id, x, y }`. Returns one enriched node: `role`, `text`, `enabled`/`focused`/`hovered`, `rect`, `sibling_index`, `has_click`/`has_drag`/`has_scroll`, `tooltip`, and `maps_to_action` (the `invoke` name when known). With `--x/--y` it returns the hit-test winner (smallest rect containing the point) - what is actually under a misfiring coordinate click. Foxy's node model is flat, so `parent_id`/`child_ids` are null/empty for now.

### `events`

The causal counterpart to `logs`: a ring buffer of semantic UI events on the runtime with a monotonic `generation`. `AgentGuiCommand::Events { kinds, since, limit }`. Injected-input events (`click`/`key`/`scroll`/`type`/`invoke`/`drag`) are recorded in `handle_agent_gui_request`; state-transition events (`view-change`, `modal-open`/`modal-close`, `toast-shown`, `focus-change`, `download-state`) are derived each frame in `agent_gui_record_state_events` by diffing the observed frame against the previously recorded state - so no app-wide instrumentation is required. Reuses the `--since-generation` incremental-tail idiom from `logs`.

### `clock`

Virtual time control for determinism. `AgentGuiCommand::Clock { action, ms }` (`advance`/`freeze`/`resume`/`status`). A process-global offset in `agent_support` is added to the elapsed time the UI timers read (currently the toast `shown_at.elapsed()` site in `render_ui_toast` and the harness toast reads), so advancing the clock fires toast expiry on demand without wall-clock waiting; with no driver running the offset is zero and behavior is unchanged. Complements `stable-render` (which freezes animation time for stable pixels). Scoped to UI-side timers first; wiring it through core timers can come later.

### `dialog`

Native file/folder picker automation. `AgentGuiCommand::Dialog { action, path, cancel }` (`expect`/`pending`/`clear`). A driver-owned slot in `agent_support` holds the response for the next picker; the `rfd` call sites consult it through `agent_support::pick_folder`/`pick_file`/`save_file` (which fall back to the real native picker) *before* spawning the OS dialog when agent-gui mode is active. `expect --path`/`--cancel` queues the one-shot response; `pending` reports `{dialog_open, queued, intercepted_count}`. Echoed paths are redacted to basenames; the queue is honored only under agent-gui mode and only for the next picker.

## Implementation Pointers

- Add CLI args in `src/cli/args.rs` under `UiArgs` for starting the app-side service, and add an `agent-gui` command group for client commands.
- Keep command output contracts in or near `src/cli/output.rs`.
- Add app-side code under `src/ui/app/` or a narrow `src/ui/agent_driver/` module. Check `src/ui/AGENTS.md` and `conventions/UI_CONVENTIONS.md` before changing UI behavior.
- Route view changes through existing `FoxyView` state and helpers such as `open_reference_view` where appropriate.
- Use `Foxy::update` as the frame boundary for draining queued harness commands, applying actions, and publishing snapshots.
- Use a channel from the server thread to the UI thread; do not mutate `Foxy` from the server thread.
- Keep screenshot request state in the UI thread so the `Event::Screenshot` response is correlated with the pending CLI request.

## Extending the harness - implementation gotchas

Bugs that **passed `cargo fmt`, `clippy -D warnings`, and the unit tests** yet only surfaced when driving the real GUI. Always live-smoke a new command against an isolated `FOXY_CONFIG_DIR` before calling it done - the unit layer cannot see these:

- **Never `request_focus` on a widget that is not rendered this frame.** egui forwards the focused id to AccessKit, and `accesskit_consumer` panics the UI thread with `Focused ID #… is not in the node list` if no node claims it. So a `focus`/`fill` command that targets a widget must make its container visible in the *same* frame first. `poll_agent_gui` runs before the views draw (`update_loop.rs`), so setting e.g. `self.show_add_repository_modal = true` and then `request_focus` works; the widget renders and claims focus before the end-of-frame AccessKit tree is built. Registered focus targets live in `AGENT_FOCUS_TARGETS`, and each must have a matching "ensure visible" branch in `agent_gui_ensure_focus_target_visible`.
- **Do not name a subcommand arg the same as a `global = true` arg.** The global `--field` (the `--flat`/`--field` projection) and the `assert` positional `field` shared the clap id `field`; clap silently bound the positional value to the global `--field`, so `assert view --equals x` set `cli.field = "view"` and the projection nulled the payload (`data: null`) - no panic, no clippy warning. Give colliding args a distinct id (`#[arg(id = "assert_field")]`).
- **A positional `bool` defaults to a flag action and clap debug-asserts at runtime** (`positional … must take a value but action is SetTrue`). Use `#[arg(action = clap::ArgAction::Set)]` so `stable-render true|false` parses.
- **The agent-gui FPS probe keeps the app repainting continuously**, so `snapshot --since-frame N` rarely returns `{changed:false}` live (the frame number has already advanced). The delta gate is still correct; it just won't be observed under a continuously-repainting probe.
- **`stable-render` and other per-frame style state must be re-applied every frame** (egui resets `Style` each pass), which is why `poll_agent_gui` re-asserts it while the mode is on.

## Testing Strategy

- Unit test command parsing and JSON serialization.
- Unit test `FoxyView` string parsing.
- Add small snapshot serialization tests from synthetic node data.
- Use `egui_kittest` for specific view/component behavior when the app can be decomposed into testable egui functions.
- Add one ignored/manual smoke test or script that launches `ui --agent-gui`, runs `status`, `open-view settings`, `text`, `screenshot`, and `close`.
- **Live-smoke every new command** against an isolated `FOXY_CONFIG_DIR` (see [SKILL.md](../SKILL.md)). The gotchas above are invisible to fmt/clippy/unit tests.

## Agent Usage Pattern

1. Start the UI with an isolated `FOXY_CONFIG_DIR` (copy JSON config in for real data; skip the DB), `--agent-gui`, and `EGUI_INSPECTION=1` when MCP inspection is available.
2. Poll `status` until `ok`/`startup_frame_rendered`; both client and server must share `FOXY_CONFIG_DIR`.
3. Navigate with `open-view` (use `--tab` for repository-settings sub-tabs) or semantic clicks.
4. Assert Foxy state with `snapshot`/`text` (fields at `.data.data.*`) and use `egui_mcp` for generic AccessKit tree exploration or inspection screenshots.
5. For scroll/galley/perf work, bracket a scroll burst with `fps` and compute `extra_passes` from the pass/frame counters.
6. Loop event sequences inside one shell block (each CLI call is a separate ~100-300 ms process).
7. `close`, kill any stray debug process, and delete the temporary config root.
