---
name: foxy-gui-driver
description: "Design, implement, or use Foxy's agentic GUI driver for Codex, Claude Code, or similar coding agents. Use when adding or debugging a Playwright-like desktop automation path for the egui/eframe UI: launching Foxy in GUI mode, opening FoxyView screens, inspecting visible text and accessibility state, reading FPS/performance status, simulating clicks/scroll/keyboard input, taking screenshots, detecting egui multi-pass/relayout regressions, or writing agent-driven UI smoke tests with Foxy's local harness and egui_mcp."
---

# Foxy GUI Driver

## Overview

Use this skill to drive Foxy's real desktop UI with both available agent channels. The Foxy harness is **implemented**: an app-side TCP driver (`src/ui/app/agent_driver.rs`) started by no-arg debug/IDE launches or with `ui --agent-gui` / `ui --agents`, plus an `agent-gui` CLI client (`src/cli/commands/agent_gui.rs`). The driver binds loopback, writes a token-protected session file into the config dir, and runs commands on the UI thread.

Foxy also compiles eframe's upstream `inspection` feature. When the app is launched with `EGUI_INSPECTION=1` or `EGUI_INSPECTION=<host:port>`, eframe attaches the `egui_inspection` plugin and exposes the standard inspection protocol for tools such as `egui_mcp`. In agentic GUI sessions, run `EGUI_INSPECTION` and `--agent-gui` together against the same isolated app instance. Use `egui_mcp` for generic MCP-driven AccessKit tree queries, input injection, screenshots, and resize checks; use `agent-gui` for Foxy-specific structured state (`repositories`, `addons`, `progress`, `download-summary`, etc.), token-gated commands, isolated fixtures, semantic app actions, and destructive-action safeguards.

For protocol/design details and command schemas, read [references/harness-design.md](references/harness-design.md).

## Confirm the harness exists

```powershell
rg -n "agent-gui|AgentGui|agent_gui" src Cargo.toml
```

If `src/ui/app/agent_driver.rs` and `src/cli/commands/agent_gui.rs` are present, follow this playbook. If not, treat the design file as the implementation spec.

## Safe launch (verified recipe)

The driver runs the **real app**. Two things to get right: isolate data, and don't mutate the desktop.

1. **Isolate config + DB + data** with `FOXY_CONFIG_DIR` (the `--config-dir` flag sets the same env). It redirects `app_paths::foxy_data_dir()`, so config, the SQLite DB, caches, and screenshots all live under the throwaway dir.
2. **Real data without the 1.3 GB DB**: copy *only* the small JSON files into the isolated dir; the DB rebuilds fresh and addon lists repopulate from a filesystem inventory scan (not the DB). Repos/external-addons appear because `repositories.json` still points at the real on-disk addon paths (scanned read-only).
3. **Desktop integration is auto-skipped under agent GUI mode**: the app will *not* repoint the Start Menu shortcut / `.desktop` entry. No-arg debug/IDE launches enable agent GUI mode automatically; release UI launches still need `ui --agent-gui` or `ui --agents`.

```powershell
$dir = "$PWD\temporary_files\agent-gui-run"
New-Item -ItemType Directory -Force $dir | Out-Null
foreach ($f in 'settings.json','repositories.json','repository_spaces.json') {
  Copy-Item "$env:APPDATA\Foxy\$f" "$dir\$f" -Force   # real repos, NOT the 1.3 GB database.db
}
$env:FOXY_CONFIG_DIR = $dir
$env:RUST_LOG = "warn"                                  # egui warnings -> the redirected stdout file
$exe = ".\target\x86_64-pc-windows-msvc\debug\Foxy.exe"
cargo build --bin Foxy
$p = Start-Process $exe -ArgumentList 'ui','--agent-gui','--agent-port','0' `
  -RedirectStandardError "$dir\app.log" -RedirectStandardOutput "$dir\app.out" -PassThru -WindowStyle Minimized
# poll until ready (both server and client need the SAME FOXY_CONFIG_DIR to share the session file)
do { Start-Sleep -Milliseconds 700; $ok = (& $exe agent-gui status --json 2>$null | ConvertFrom-Json).ok } until ($ok)
```

A debug build is fine for **logic/warning** checks (multi-pass is not perf-gated). Use `--release` only when absolute FPS numbers matter; debug FPS is not representative.

## egui_mcp alongside agent-gui

Use this when an agent has MCP access and you want the upstream egui inspection protocol available in the same run as Foxy's local `agent-gui` CLI. Foxy has `eframe = { features = ["glow", "inspection"] }`, so no extra app code is required.

Install the MCP server once:

```powershell
cargo install --git https://github.com/rerun-io/kittest_inspector egui_mcp
```

Register it with the agent you are using:

```powershell
claude mcp add egui egui-mcp
```

For Codex MCP configuration, add:

```toml
[mcp_servers.egui]
command = "egui-mcp"
args = []
```

Launch Foxy with an isolated config root, the Foxy agent driver, and upstream inspection enabled:

```powershell
$dir = "$PWD\temporary_files\egui-mcp-run"
New-Item -ItemType Directory -Force $dir | Out-Null
$env:FOXY_CONFIG_DIR = $dir
$env:EGUI_INSPECTION = "1"          # binds 127.0.0.1:5719
$env:RUST_LOG = "warn"
cargo run -- ui --agent-gui --agent-port 0
```

If port `5719` is busy, choose a loopback address explicitly:

```powershell
$env:EGUI_INSPECTION = "127.0.0.1:5720"
cargo run -- ui --agent-gui --agent-port 0
```

Notes:

- Keep the bind address on loopback unless you intentionally tunnel it. The upstream inspection protocol has no authentication and can read the UI tree, inject input, resize the window, and capture screenshots.
- Screenshots need a visible, rendered window. Tree reads and input can work while the app is in the background, but a minimized or fully occluded window can make screenshot requests time out.
- `EGUI_INSPECTION` accepts `1` for the default `127.0.0.1:5719`, a falsy value (`0` / `false`) to stay off, or a bind address such as `127.0.0.1:5720`.
- Keep both channels open when a task benefits from them: MCP can explore the generic widget tree and capture inspection screenshots while `agent-gui` asserts Foxy state, reads logs/progress, drives app intents, and performs cleanup.

## Output format

Use `--toon` for agent-read responses when you do not need PowerShell's `ConvertFrom-Json`. It emits the same machine-readable `CliEnvelope` as `--json`, but encoded as TOON to reduce repeated object keys in large payloads such as `addons`, `repositories`, `logs`, and `snapshot`. The app-side loopback wire remains newline-delimited JSON; TOON is only a CLI input/output boundary format.

Set `$env:FOXY_AGENT_TOON = "1"` to make agent-gui CLI calls default to TOON without adding `--toon` to every command. Keep `--json` for shell snippets that pipe into JSON tooling. The logical paths do not change: the default shape is still `.data.data.*`, `--flat` moves fields to `.data.*`, and `--field` returns the selected value.

TOON input is accepted for whole-document fields when `--toon` is active: `scenario` files, `batch --steps` / `batch --stdin`, and `invoke --params`. JSON remains supported as a fallback. `exec --stdin` input stays NDJSON because it is line-delimited streaming.

## Commands

```powershell
cargo run -- agent-gui status --json
cargo run -- agent-gui open-view repository-list --json
cargo run -- agent-gui open-view settings --tab customization --json
cargo run -- agent-gui open-view repository-settings --repo-index 0 --tab external-addons --json
cargo run -- agent-gui snapshot --json
cargo run -- agent-gui text --contains "External Addons" --json
cargo run -- agent-gui find --text "Settings" --visible-only --json
cargo run -- agent-gui click --text "Settings" --json
cargo run -- agent-gui click --x 480 --y 320 --json
cargo run -- agent-gui click --x 480 --y 320 --double --button secondary --json   # double / right click
cargo run -- agent-gui hover --x 990 --y 400 --json                                 # move pointer (tooltips)
cargo run -- agent-gui scroll --x 990 --y 400 --dy -600 --json
cargo run -- agent-gui key --key a --ctrl --json                                    # Ctrl+A; also F5, ArrowDown, etc.
cargo run -- agent-gui type "search text" --json
cargo run -- agent-gui mouse-down --x 100 --y 200 --json                            # press-hold (compose a drag)
cargo run -- agent-gui mouse-up --x 400 --y 200 --json                              # release at the end point
cargo run -- agent-gui screenshot --output temporary_files\agent-gui-run\shot.png --json
cargo run -- agent-gui fps --json
cargo run -- agent-gui wait --idle --timeout-ms 60000 --json
cargo run -- agent-gui wait --modal-open --timeout-ms 5000 --json                   # also --modal-closed
cargo run -- agent-gui logs --level warn --limit 20 --json                          # read in-memory app log
cargo run -- agent-gui repositories --contains tfr --limit 5 --json                 # structured repo list + sync state
cargo run -- agent-gui addons --repo-index 0 --tab external-addons --limit 50 --json # structured addon rows (no nodes!)
cargo run -- agent-gui addons --repo-index 0 --tab addons --enabled-only --contains ace --json
cargo run -- agent-gui settings --json                                              # live effective settings
cargo run -- agent-gui progress --json                                              # download/sync/recheck state
cargo run -- agent-gui scale 150 --json                                             # global UI scale % (reproduce high-DPI)
cargo run -- agent-gui resize --width 1100 --height 720 --json                      # window inner size (reproduce big-window)
cargo run -- agent-gui profiles --repo-index 0 --json                               # launch profiles + flags (no nodes!)
cargo run -- agent-gui missions --contains altis --json                             # cached editor missions for viewed repo
cargo run -- agent-gui spaces --json                                                # repository spaces + attached-repo counts
cargo run -- agent-gui download-summary --include-telemetry --json                  # last sync's bytes/patch-savings/telemetry
cargo run -- agent-gui toasts --json                                                # current user-feedback toast, if any
cargo run -- agent-gui set-setting show-fps-counter true --json                     # mutate one live setting + watch it react
cargo run -- agent-gui wait --toast "saved" --timeout-ms 5000 --json                # also --busy-reason-cleared/-download-complete/-fps-above/-node-visible
cargo run -- agent-gui health --json                                                # build/version + active renderer preflight (first call of a session)
cargo run -- agent-gui filters --json                                               # read every list filter value
cargo run -- agent-gui set-filter addons-filter ace --json                          # write one filter (string or bool: favorites-only/client-side-only/...)
cargo run -- agent-gui fill add-repository-input "http://repo" --json               # focus+clear+set a named text field (writes backing state)
cargo run -- agent-gui focus add-repository-input --json                            # focus a registered field (opens its container); --clear to drop focus
cargo run -- agent-gui nav --count 2 --json                                         # Tab traversal; --reverse for Shift+Tab; reports snapshot.focused
cargo run -- agent-gui select --repository 0 --json                                 # non-destructive selection (also --server/--mission/--space)
cargo run -- agent-gui window minimize --json                                       # restore|maximize|unmaximize|focus|hide-to-tray|show
cargo run -- agent-gui assert view --equals repository-list --json                  # assert snapshot/settings.*/progress.* field; --contains too
cargo run -- agent-gui inventory --contains ace --limit 50 --json                   # cross-folder addon inventory (shared-addon investigations)
cargo run -- agent-gui pending-updates --repo-index 0 --include-files --json        # planned update set BEFORE a sync (diff vs download-summary)
cargo run -- agent-gui app-update --json                                            # self-update flow status + mode/url
cargo run -- agent-gui memory --textures --json                                     # latest memory sample + texture-tracking totals/rows
cargo run -- agent-gui arma-profiles --json                                         # OS-level Arma 3 player profiles (-profiles launch arg)
cargo run -- agent-gui backups --json                                               # addon-backup records + per-addon counts
cargo run -- agent-gui screenshot --output shot.png --annotate --json               # overlay node rects/ids + write shot.png.nodes.json sidecar
cargo run -- agent-gui stable-render true --json                                    # freeze animation/blink for byte-stable screenshots
cargo run -- agent-gui settle --frames 2 --json                                     # park until N frames render, then return the post-input snapshot
cargo run -- agent-gui snapshot --fields view,fps,focused --json                    # project only these keys; --since-frame N gates {changed:false}
cargo run -- agent-gui snapshot --json --flat                                       # --flat drops the inner AgentGuiResponse envelope
cargo run -- agent-gui snapshot --json --field nodes.0.rect                         # --field extracts one dotted value from the payload
cargo run -- agent-gui context-menu --x 400 --y 300 --json                          # secondary-click to open an egui popup; then menu-select <label>
cargo run -- agent-gui fixture fixtures\base.json --config-dir $dir --json          # seed isolated config with known JSON (never the DB)
cargo run -- agent-gui scenario scenarios\smoke.json --json                         # run a committed step sequence; structured pass/fail transcript
'{"command":"health"}' | cargo run -- agent-gui exec --stdin --json                 # one persistent connection, NDJSON commands from stdin
cargo run -- agent-gui invoke list-actions --json                                   # enumerate the semantic-action registry
cargo run -- agent-gui invoke start-sync --repo-index 0 --allow-destructive --json  # drive by intent (gated for core/disk mutations)
cargo run -- agent-gui invoke apply-profile --repo-index 0 --profile Main --json    # non-destructive intents need no flag
echo '[{"command":"set-filter","name":"addons-filter","value":"ace"},{"command":"settle","frames":2},{"command":"addons","tab":"addons"}]' | cargo run -- agent-gui batch --stdin --json  # server-side pipeline, one round-trip
cargo run -- agent-gui diff --baseline last --json                                  # field-level delta vs the last snapshot/diff; also --baseline frame:<n>
cargo run -- agent-gui drag --from-id profile.row.0 --to-id profile.row.3 --json    # one-call drag (coords: --from-x/--from-y --to-x/--to-y --steps N)
cargo run -- agent-gui query "repositories[?state=='pending-update'].name" --json   # JMESPath over the union app-state document
cargo run -- agent-gui checkpoint base --json                                       # save UI state (also --list); restore with the next line
cargo run -- agent-gui restore base --json                                          # roll UI state back (UI state only, never core/disk)
cargo run -- agent-gui element --id footer.settings --json                          # deep node inspect; or --x/--y for the hit-test winner
cargo run -- agent-gui events --kind click,view-change --limit 50 --json            # recent semantic UI events; --since <gen> for incremental tail
cargo run -- agent-gui clock advance --ms 5000 --json                               # advance logical UI time (also freeze/resume/status)
cargo run -- agent-gui dialog expect --path "C:\\repos\\my-mod" --json              # queue the next native picker's result (also --cancel / dialog pending / dialog clear)
cargo run -- agent-gui close --json
```

- **`addons`** is the structured way into addon rows, which are **not** semantic nodes - previously you could only screenshot-diff them. Payload at `.data.data`: `repository_index`, `repository_name`, `tab`, `total`, `returned`, `addons[]` (`name`, `enabled`, `kind` = required/optional/external, `size_bytes`, plus `source`/`favorite`/`client_side` where applicable). Defaults to the current repository-settings tab; pass `--tab` to read another without navigating. `size_bytes` is `0` until the repo-settings addon view has loaded sizes in the background.
- **`repositories`** lists every configured repo without navigating: `index`, `name`, `address`, `path`, `state` (synced/pending-update/updating/unknown), `selected`, `pending_update_count`, addon counts, `profile_count`, `selected_profile`, `space_id`. Filter with `--contains` (name or address) and `--limit`.
- **`settings`** serializes the live in-memory `SettingsViewState` (same shape as `settings show`), so you can assert a settings-screen interaction actually mutated app state.
- **`progress`** reports `busy` + `busy_reasons` plus download/sync detail (`download_active`, `download_label`, `download_percent`, `download_paused`, `download_speed_bps`, `download_eta_secs`, `total_downloaded_bytes`, `active_mod_downloads`, `recheck_stage_label`/`recheck_stage_percent`/`recheck_hash_counter`, `current_sync_mode`, `syncing_repository`).
- **`scale <percent>`** drives the global UI-scale setting (clamped 25-500, like the slider): the next frame's `zoom_factor`/`pixels_per_point` reflect it. **`resize --width --height`** sends `ViewportCommand::InnerSize` in logical points. Together they reproduce the high-DPI / large-window relayout paths that discrete event injection otherwise can't trigger (see the multi-pass caveat below).
- **`profiles`** lists a repository's launch profiles (also not semantic nodes): payload at `.data.data` has `repository_index`, `repository_name`, `selected_profile`, and `profiles[]` (`name`, `selected`, `flags` = csla/ef/gm/rf/spe/vn/ws/skip_intro/…, `additional_params`, and `addon_override_count`/`optional_addon_override_count`/`external_addon_override_count`). `--repo-index` defaults to the selected repo; filter with `--contains`/`--limit`.
- **`missions`** returns the cached editor missions for the *currently viewed* repo (`cached_missions`): `loaded` (false until a repo view populated it), `profile_name`, `scanned_age_ms`, and `missions[]` (`display_name`, `folder_name`, `world_name` = terrain, `root_folder_name`, `is_multiplayer`, `author`, `game_type`, `max_players`). No absolute paths are exposed. Filter with `--contains` (name/folder/terrain) and `--limit`.
- **`spaces`** lists repository spaces with `attached_repository_count` (repos pointing at the space), `selected_space`, `manifest_entry_count`/`required_entry_count`, plus `bulk_progress` (`{space_id, mode, total/completed/succeeded/failed/updates_available/up_to_date counts, current_repo_name}`) while a space bulk action runs. Filter with `--contains`/`--limit`.
- **`download-summary`** returns the last completed `DownloadSummary` (`present:false` until one exists): `mods_updated`, `files_updated`, `parts_updated`, `downloaded_bytes`, `planned_transfer_bytes`, `full_download_bytes`, `patch_savings_bytes`, `patched_files`, stage durations (`download_stage_ms`, `hash_stage_ms`, `cumulative_hash_ms`, `after_download_hash_ms`, `total_ms`), `avg_speed_bps`, and `telemetry_sample_count`. Add `--include-telemetry` for the full per-sample series (`download_bps`/`disk_write_bps`/`hash_files_per_sec`/`cpu_percent`/`memory_bytes`). Structured assertion of "did the sync do what I expected?" instead of log parsing.
- **`toasts`** surfaces the current user-feedback toast (`present`, `message`, `kind` = success/error, `age_ms`, `remaining_ms`, `duration_ms`), or `present:false` once it has timed out. Pairs with `wait --toast <text>`.
- **`set-setting <key> <value>`** is the read/write complement to `settings`: mutate one live field and watch the UI react. Keys: `debug-mode`, `show-activity-log`, `show-fps-counter` (bool: true/1/on/yes), `ui-scale-percent` (int, reuses the `scale` clamp), `locale` (e.g. `en`, also reloads i18n), `download-speed-limit-mbps` (int ≥ 1, or `unlimited`). Clamps/validates like the offline `settings set` CLI and echoes the applied value at `.data.data.value`. Loopback + token-gated like every command.
- **More `wait` conditions** beyond text/view/idle/modal: `--toast <text>` (a feedback toast containing the text is showing), `--busy-reason-cleared <name>` (the named `busy_reasons` flag is gone - wait out one `core-sync` without going fully idle), `--download-complete`, `--fps-above <n>`, `--node-visible <id>`. Each reuses the pending-wait machinery, so no agent-side polling loop.
- `open-view settings` accepts `--tab application|backup-manager|additional-search-folders|cleanup|direct-download|ts3-plugin|customization` to land directly on a Settings tab. Snapshot responses include `settings_tab` while the Settings view is active, so assert it directly after opening.
- `open-view repository-settings` needs `--repo-index` (or a selected repo) and accepts `--tab configuration|addons|optional-addons|external-addons` to land directly on a tab. For *reading* addon rows prefer `addons` (above); `open-view --tab` is still how you put a tab on screen for scroll/screenshot/perf work.
- `scroll`/`click`/`hover`/`mouse-*` accept negative deltas directly (`--dy -600`); the `=` workaround is no longer required. Default pointer target is the content center, so pass `--x/--y` to aim at a specific column (e.g. the right-edge icon buttons).
- **Modifiers** (`--ctrl --shift --alt --command`) attach to `key`, `click`, `scroll`, `mouse-down`, `mouse-up`. On Windows `--ctrl` also sets egui's `command` flag, so shortcuts that check either field fire.
- **`key`** now accepts the full egui key set: named keys (`Escape`, `PageDown`, `ArrowDown`), letters (`a`), digits (`5`), and function keys (`F5`).
- **`click --id <node>`** with no semantic handler now clicks the node's rect center (was an error); a coordinate fallback still needs both `--x` and `--y`.
- **`logs`** reads the same in-memory buffer as the footer activity log (egui/wgpu warnings included). Filter with `--level`, `--contains`, `--limit`. It returns a monotonic `generation`; pass it back as `--since-generation <g>` for cheap incremental tailing (only entries added since are returned).
- **Drags** are reliable when composed across frames: `mouse-down` → one or more `hover`/`mouse-up` calls (each CLI call is a separate frame). A single in-frame down→move→up is not reliably classified as a drag by egui.

## Extended commands (efficiency, interaction, fetches, determinism)

- **`health`** is the first call of a session: `version`/`version_label`/`commit`/`build_kind`, `renderer` (the *active* backend - `wgpu` or the `glow` fallback, captured at startup), `renderer_preference`, `renderer_fallback_pending`, `agent_gui:true`, `stable_render`, `locale`, `uptime_ms`. CI can gate on a client/server version match before a run.
- **`filters`** reads every list filter; **`set-filter <name> <value>`** writes one. String filters: `addons-filter`, `optional-addons-filter`, `external-addons-filter`, `external-addons-origin-filter`, `addon-state-filter`, `mission-search`, `mission-terrain-filter`, `space-detail-filter`. Boolean: `favorites-only`, `client-side-only`, `group-by-origin`, `show-folders`. Setting a filter directly is the **correct** way to drive the addon-list scroll/galley recipes (filter to a known subset, then assert) - far more reliable than focusing a box and typing.
- **`fill <target> <value>`** focuses+clears+sets a named text field by writing its backing state directly (reliable, no per-key timing). Targets include `add-repository-input`, `profile-name`, the filter fields above, `direct-download-url`/`direct-download-destination`. **`focus <target>` / `focus --clear`** sets/clears keyboard focus; the only widget with a registered stable id today is `add-repository-input` (focusing it opens the add-repository modal so the field actually renders - focusing a widget that is not on screen would crash egui/AccessKit). The focused widget is reported in `snapshot.focused`. **`nav --count N [--reverse]`** sends Tab/Shift+Tab and parks one frame, then reports `snapshot.focused`.
- **`select`** is non-destructive UI selection (highlight/view only, never a core action): `--repository N`, `--server N` (needs a selected repo), `--mission N` (from the cached list), `--space <id>`.
- **`window <action>`**: `minimize`, `restore`, `maximize`, `unmaximize`, `focus`, `hide-to-tray`, `show`. Routed through `ViewportCommand` + the tray manager, so restore-from-tray and the post-launch hide path can be exercised.
- **`assert <field> [--equals v | --contains v] [--repository-index N]`** evaluates one observed field and returns `{ok, field, source, op, expected, observed}`. Sources: a bare key or `snapshot.<path>` reads the snapshot; `settings.<path>` and `progress.<path>` read those fetches. With neither `--equals`/`--contains` it is a presence check. Use it as scenario steps for fail-fast checks.
- **Fetches:** `inventory` (cross-folder addon inventory - `name`/`folder` basename/`source`/`size_bytes`, plus `total`/`total_size_bytes`; filter `--contains`/`--folder`/`--source`/`--limit`), `pending-updates` (the planned set *before* a sync from `pending_update_cache`: per-repo `mods[]` with `needs_update`/`total_bytes`/`changed_file_count`, `--include-files` for per-file diffs - diff this against `download-summary` after a sync), `app-update` (`status` idle/checking/available/downloading/…, plus `mode`/`url`/`github_repo`/`auto_check`), `memory` (latest sample working-set/private/tracked bytes + buckets, texture totals; `--history` for the series, `--textures` for per-texture rows), `arma-profiles` (OS-level Arma 3 player profiles - `name`/`is_default`/`folder`/`active`), `backups` (addon-backup records + `count_per_addon`/`total_size_bytes`/retention). All redact absolute paths to basenames.
- **`screenshot --annotate`** overlays each known node's rect + id and the pointer crosshair onto the PNG and writes a `<output>.nodes.json` sidecar (rect→id, scale factor) - the fastest way to debug a misfiring coordinate click.
- **`stable-render true|false`** zeroes egui's animation time and disables caret blink so screenshots are byte-stable across runs (re-asserted every frame while on). Lighter than a full clock freeze.
- **`settle --frames N`** parks the response until N more frames render, then returns the post-input snapshot - collapses the send→wait→snapshot trio into one call.
- **`scenario <file.json>`** runs a committed JSON array of step objects (each a driver command, e.g. `open-view`/`set-filter`/`assert`/`wait`) and returns a `{ok,total,failures,steps[]}` transcript; exits non-zero if any step (or `assert` payload) fails. With `--toon`, scenario input may be TOON; `.toon` files are decoded as TOON automatically with JSON fallback. **`fixture <file.json>`** seeds the isolated `FOXY_CONFIG_DIR` from `{ "files": { "settings.json": …, "repositories.json": … } }` - allowlisted to the small JSON config only, **never** `database.db`.
- **`exec --stdin`** opens **one** TCP connection and streams newline-delimited command JSON from stdin, printing one response per command - amortizes the per-call process spawn + connect + token handshake across a whole interactive session. (Each stdin line is a driver-command object, e.g. `{"command":"snapshot","fields":["view"]}`.) Input remains NDJSON even when `--toon` is active; only the streamed responses switch to TOON.

## Semantic / pipelined / determinism commands (drive by intent)

These close the gaps that otherwise force many brittle round-trips. Ordered by leverage.

- **`invoke <action> [--repo-index N] [--profile NAME] [--params '<json-or-toon>'] [--allow-destructive]`** drives Foxy by **named intent** instead of pixels - the command palette. `invoke list-actions` (or `--list-actions`) enumerates the registry: each entry has `name`, `destructive`, `params`, `summary`. Non-destructive intents (`open-settings`, `open-add-repository-modal`, `close-modals`, `select-repository`, `apply-profile`, `toggle-activity-log`, …) run unconditionally; any intent that mutates core/disk state (`start-sync`, `recheck-repo`, `recheck-integrity`, `force-redownload`, `launch-game`) requires `--allow-destructive` and keeps Foxy's normal confirmation/preflight (e.g. `launch-game` still routes through the launch preflight). The response echoes `{action, ran, destructive}` plus a post-action `snapshot`. `open-settings` accepts optional params such as `{"tab":"customization"}`; `--repo-index`/`--profile` are merged into `params`; TOON params are accepted when `--toon` is active.
- **`batch --stdin` (or `--steps '<json-or-toon-array>'`)** runs an **array of commands server-side in one round-trip**, in order on the UI thread, resuming across frames for any step that parks (`settle`/`wait`/`screenshot`/`drag`) - distinct from `exec` (interactive, one-at-a-time) and `scenario` (client-side, reconnect per step). Response: `{ ok, total, results:[ <AgentGuiResponse>, … ] }`; `--stop-on-error` (default true) halts at the first failing step. TOON input is accepted when `--toon` is active. Collapses an entire act→observe loop (`set-filter` → `settle` → `addons` → `assert`) into a single call.
- **`diff --baseline last|frame:<n>`** is a field-level delta vs a stored observation (every `snapshot`/`diff` stores one): `{added_nodes[], removed_nodes[], changed_fields:{field:{from,to}}, text_added[], text_removed[], baseline_frame, current_frame}`. High-noise fields (`fps`/`frame`/`cumulative_pass_nr`/`pointer`/`content_rect`) are excluded. `frame:<n>` prefers an exact stored frame, else the most recent at or before it. Pairs naturally with `batch` (act, then `diff` in one round-trip).
- **`drag --from-id <id>|--from-x/--from-y --to-id <id>|--to-x/--to-y [--steps N] [--button b]`** is a **one-call drag**: it schedules a pointer-down, `N` interpolated moves, then a pointer-up across **real frames** (a single in-frame down→move→up is not classified as a drag by egui), then returns the post-drag snapshot. Makes drag-reorder flows (profile ordering, list reordering) reachable.
- **`query "<jmespath>"`** evaluates one **JMESPath** expression over the union of the structured fetches (`snapshot` + `settings` + `progress` + `repositories` + `spaces` + `profiles` + `addons` + `missions` + `inventory` + `toasts` + `filters` + `pending_updates` + `download_summary` + `app_update` + `memory` + `backups` + `arma_profiles`). Only the sub-documents the expression mentions are built. Read-only. Array sections are the inner arrays (e.g. `repositories[?state=='pending-update'].name`), scalars stay nested (`progress.download_percent`). Response: `{expr, result}`.
- **`checkpoint <name>` / `restore <name>` / `checkpoint --list`** save & roll back the serializable **UI-state subset** (`current_view`, selection indices, `repository_settings_tab`, space id, all list filters, the simple modal flag, full `SettingsViewState`) for safe exploration without a cold restart. **UI state only** - it does **not** roll back core/DB/disk (a synced repo stays synced); the response carries `ui_state_only: true`. For true data isolation use `FOXY_CONFIG_DIR` + `fixture`.
- **`element --id <id>` / `element --x --y`** is deep single-node introspection: `role`, `text`, `enabled`/`focused`/`hovered`, `rect`, `sibling_index`, `has_click`/`has_drag`/`has_scroll`, `tooltip`, and `maps_to_action` (the `invoke` name when known). With `--x/--y` it returns the **hit-test winner** (smallest rect containing the point) - exactly what is under a misfiring coordinate click. (Foxy's node model is flat, so `parent_id`/`child_ids` are currently null/empty.)
- **`events [--kind a,b] [--since <gen>] [--limit N]`** is the causal counterpart to `logs`: a ring buffer of semantic UI events - `click`, `key`, `scroll`, `type`, `view-change`, `modal-open`/`modal-close`, `toast-shown`, `focus-change`, `download-state`, `invoke`, `drag`. It returns a monotonic `generation`; pass it back as `--since` for incremental tailing. Confirms a click actually registered or a transition actually happened.
- **`clock advance --ms N` / `clock freeze` / `clock resume` / `clock status`** drive the **virtual UI clock** so time-based behaviors fire on demand and deterministically (no wall-clock waiting). Currently scoped to UI-side timers (toast expiry); `advance` jumps logical time forward, `freeze`/`resume` pause/continue it without a jump. Complements `stable-render` (which freezes animation time for *stable pixels*); this *drives* time forward. Response: `{action, offset_ms, frozen}`.
- **`dialog expect --path <p>` / `dialog expect --cancel` / `dialog pending` / `dialog clear`** automate native OS file/folder pickers that otherwise hard-block the headless harness. `expect` pre-registers the response the **next** picker returns (one-shot; honored only under agent-gui mode); `pending` reports `{dialog_open, queued, intercepted_count}`. Echoed paths are redacted to basenames. The app's picker call sites consult the queued slot before spawning the real native dialog.

## Envelope (parsing)

Every client response is wrapped twice, whether serialized as JSON or TOON: `CommandSuccess.data` holds the `AgentGuiResponse`, whose `.data` holds the payload. So a snapshot field is at **`.data.data.<field>`**:

```powershell
$snap = (& $exe agent-gui snapshot --json | ConvertFrom-Json).data.data
$snap.view; $snap.repository_settings_tab; $snap.repositories_count; $snap.cumulative_pass_nr
```

To skip the double envelope, add **`--flat`** (prints just the inner payload, so the field is at `.data.<field>`) or **`--field <dotted.path>`** (prints only that value, e.g. `--field view`, `--field nodes.0.rect`). Both are client-side and opt-in; the default shape is unchanged so existing recipes keep working.

Snapshot includes: `view`, `settings_tab`, `repository_settings_tab`, `repositories_count`, `selected_repository`, `fps`, `frame` (= cumulative frames), `cumulative_pass_nr`, `busy`, `busy_reasons` (stable kebab-case names of the background-work flags currently set - *why* `busy` is true; empty when idle), `active_modal_count`, `active_modals` (stable kebab-case names of the open dialogs - assert *which* modal appeared, not just how many), `pointer` (latest cursor position in logical points, or `null` when no real cursor is over the window - minimized/headless runs read `null`, so confirm `hover` via its own echoed coords), `focused` (the keyboard-focused widget: a friendly name for a registered target, else the raw egui id, else `null`), `content_rect`, `texts`, `nodes`, `startup_frame_rendered`, `pixels_per_point`, `zoom_factor` (UI-scale multiplier; drive it with `scale`). Add **`--fields a,b,c`** to project only those keys server-side, or **`--since-frame N`** to get `{changed:false, frame}` when nothing has rendered since frame `N`.

## Recipe: detect an egui multi-pass / scroll-FPS regression

The "changed id between passes" warning and the scroll FPS drop come from per-frame **multi-pass** (egui `request_discard`). Read it directly instead of grepping logs: `cumulative_pass_nr - frame` should grow 1:1 (no extra passes). Diff the counters across a scroll burst:

```powershell
$snap = (& $exe agent-gui snapshot --json | ConvertFrom-Json).data.data
$cr = $snap.content_rect
$bx = [math]::Round($cr.x + $cr.w - 30)   # right-edge icon-button column
$by = [math]::Round($cr.y + $cr.h * 0.5)
$f0 = (& $exe agent-gui fps --json | ConvertFrom-Json).data.data
for ($i=0; $i -lt 40; $i++) { & $exe agent-gui scroll --x $bx --y $by --dy -400 --json | Out-Null }
$f1 = (& $exe agent-gui fps --json | ConvertFrom-Json).data.data
$extra = ($f1.cumulative_pass_nr - $f0.cumulative_pass_nr) - ($f1.cumulative_frame_nr - $f0.cumulative_frame_nr)
"extra_passes=$extra"   # 0 = healthy single-pass; >0 = multi-pass cost (the bug)
```

**Caveat (learned the hard way):** the driver injects discrete events and cannot reliably reproduce *interactive* multi-pass triggers (real hover-over-tooltip + scroll momentum, large window/high-DPI). `extra_passes` may read 0 in the harness even when a real user sees the warning. Treat a non-zero reading as a positive signal; a zero reading is not proof the interactive path is clean - confirm widget-id fixes by reasoning + an interactive check too.

## Recipe: confirm a virtualized list actually scrolled

Snapshots don't expose row contents, so verify motion by screenshot diff:

```powershell
& $exe agent-gui screenshot --output "$dir\a.png" --json | Out-Null
for ($i=0;$i -lt 20;$i++){ & $exe agent-gui scroll --x $bx --y $by --dy -600 --json | Out-Null }
& $exe agent-gui screenshot --output "$dir\b.png" --json | Out-Null
(Get-FileHash "$dir\a.png").Hash -ne (Get-FileHash "$dir\b.png").Hash   # True = it scrolled
```

## Recipe: catch warnings/errors a feature emits while you drive it

`logs` reads the live in-memory buffer, so you can bracket an interaction and see exactly what it logged - no file tailing, no `RUST_LOG` parsing. Use `--since-generation` to slice out only the entries the interaction produced:

```powershell
$g = ((& $exe agent-gui logs --limit 1 --json | ConvertFrom-Json).data.data).generation
& $exe agent-gui open-view repository-settings --repo-index 0 --tab external-addons --json | Out-Null
for ($i=0;$i -lt 30;$i++){ & $exe agent-gui scroll --x $bx --y $by --dy -400 --json | Out-Null }
$new = (& $exe agent-gui logs --since-generation $g --json | ConvertFrom-Json).data.data
$new.entries | Where-Object { $_.level -in 'WARN','ERROR' } | Select-Object level,source,message
```

A non-empty WARN/ERROR slice (e.g. egui's "changed id between passes") is a concrete regression signal that pairs well with the `extra_passes` counter above.

## Cleanup

```powershell
& $exe agent-gui close --json | Out-Null
Get-Process Foxy -ErrorAction SilentlyContinue | Where-Object { $_.Path -like '*\target\*\debug\Foxy.exe' } | Stop-Process -Force
Remove-Item -Recurse -Force $dir
```

## Gotchas (quick reference)

- **Process-spawn overhead:** each `agent-gui` call is a new process (~100-300 ms). Loop in one PowerShell block; don't shell out per event from the agent turn.
- **Negative numbers:** fixed via `allow_hyphen_values`; `--dy -600` works. (Older builds needed `--dy=-600`.)
- **stdout vs stderr:** the app logs (incl. egui warnings) go to the redirected **stdout** file in debug; release builds use `windows_subsystem=windows` (no console).
- **Both ends need `FOXY_CONFIG_DIR`:** the client reads the session file from `get_config_directory()`.
- **Don't copy `database.db`** (~1.3 GB). JSON-only is enough for addon-list rendering tests.
- **`--agent-gui` skips Start Menu hijacking, but a plain `cargo run` (no `--agent-gui`) still repoints `%APPDATA%\...\Start Menu\Programs\Foxy.lnk`.** When launching the dev build outside agent-gui mode, guard with an empty sibling `unins000.exe` next to the exe so the app treats it as an "installed" copy and leaves the shortcut alone. The installed app lives at `C:\Program Files (x86)\Foxy\Foxy.exe`.
- **Live-smoke new commands; fmt/clippy/unit tests miss GUI bugs.** Real-GUI-only failure modes (egui/AccessKit focus panics, clap arg-id collisions, per-frame style resets) are documented in [references/harness-design.md](references/harness-design.md) "Extending the harness - implementation gotchas". Run a new command against an isolated `FOXY_CONFIG_DIR` before calling it done.

## Validation

Docs-only change to this skill:

```powershell
py -3 "${env:USERPROFILE}\.codex\skills\.system\skill-creator\scripts\quick_validate.py" skills\foxy-gui-driver
```

Harness/CLI code change - follow `AGENTS.md`: targeted tests first, then `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, then `cargo test`.

## Example agent tasks

- "Open Repository Settings → External Addons (`--tab external-addons`) and confirm the tab is active via `snapshot.repository_settings_tab`."
- "Scroll the external addons list and report `extra_passes` and FPS before/after."
- "Screenshot-diff to prove the optional-addons list scrolls after a galley-cache change."
