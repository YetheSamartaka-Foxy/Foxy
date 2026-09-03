# Game spaces and game modules

Load this when touching `src/core/game/`, the app/game settings split, runtime
space switching, the Steam Workshop backend, managed extra files, or `.foxypack`
config packs.

Foxy is a multi-game mod updater. Arma 3 is the reference module; Total War:
WARHAMMER III and Arma Reforger ship alongside it. The design rationale lives in
`plan-progress/plan.md` (external), with per-phase checkpoints beside it; this
file is the working contract.

---

## Vocabulary

- **Game**: a supported title, keyed by a stable `game_id` (`arma3`, `twwh3`,
  `reforger`, `generic`). `generic` is the user-configured module: the player
  supplies the executable, optional Steam app id, argument template, and mods
  manifest name through its game settings, so one module covers any Steam
  Workshop game Foxy has no dedicated module for.
- **Game module**: the Rust implementation of one game's behavior (detection,
  launch, settings schema, capabilities). Implements `GameModule`, registered in
  the static `GameRegistry`.
- **Game space**: the persisted per-game workspace. Its own settings half,
  repositories, repository spaces, visual folders, stores, and `database.db`.
  Exactly one is active per process; the active id is remembered in `games.json`.
- **Repository space** (pre-existing): a group of repositories sharing a folder,
  now scoped inside a game space. Distinct concept, keep the naming apart in UI
  copy and i18n keys.

---

## On-disk layout

```
%APPDATA%\Foxy\
  app_settings.json      games.json      window_state.json
  logs\  backups\
  games\<space_id>\
    game_settings.json   repositories.json   repository_spaces.json
    repository_visual_folders.json           extra_files.json
    workshop.json        reforger_addons.json
    database.db (+ sidecars, db_meta.json)   images\  extra_files\  workshop\
```

Rules:

- App-global files resolve from `app_paths::foxy_data_dir()`. Everything
  space-scoped resolves from `spaces::active_game_space_dir()`. New per-game data
  goes in the space directory; new app-global data needs a reason.
- Game-space ids are a single path component. `is_valid_game_space_id` enforces a
  lowercase ASCII slug that is not a Windows reserved device name. Validate before
  any filesystem call - a hand-edited `games.json` must never steer
  `remove_dir_all`.
- The one-shot legacy migration (`spaces/migration.rs`) runs when `games.json` is
  absent, is idempotent, never overwrites an existing destination, and leaves
  `*.pre-gamespaces.bak` copies. It must never delete user data.

---

## Settings split

`SettingsViewState` stays a single in-memory type. `GAME_SPACE_SETTINGS_KEYS`
(`spaces/settings_split.rs`) partitions it into `app_settings.json` and
`game_settings.json` by serde field name.

- A setting is game-scoped when it names a game install, a game-specific folder,
  or anything keyed by that space's repositories (scheduled jobs and cleanup
  folders both qualify: they reference `(remote_url, local_path)` instances and
  addon directories that only exist in one space).
- Moving a key between halves needs no migration. Reads merge both files; the next
  save re-partitions every key and drops it from the old half.
- The key list is matched by string, so a serde rename silently reclassifies a
  setting. `every_game_space_key_exists_on_settings_view_state` guards this. Keep
  it passing rather than working around it.

---

## Capabilities

`GameCapabilities` is the only correct way to vary behavior per game. Do **not**
write `module.id() == "arma3"`.

- `repository_sync` gates the repository sidebar and repository management.
- `repository_launch` gates launching a game from a repository's addon selection.
  It is separate from `repository_sync`: a game can sync repository file trees
  without a launch plan being meaningful for it (Total War: WARHAMMER III syncs
  but launches from its Workshop store). A module that sets it implements
  `GameModule::build_repository_launch_plan`; both the GUI Launch button and
  `foxy launch` go through that method, so no caller builds an Arma-shaped plan
  for another game. Reforger sets it and turns each enabled repository folder
  into an `-addons` mod id plus an `-addonsDir` root; its `reforger_addons.json`
  GUID store stays a separate launch path behind `foxy game launch`.
- `client_side_addons` gates the client-side addon marking (the row button, the
  Client-side only filter, the repository `clientSide` manifest flag, and the
  join-preflight exemption for addons the server did not report). Arma 3 servers
  report their addon list and tolerate extra client-only mods; a Reforger server
  activates exactly its own mod set on join, so the marking has nothing to mean
  there and no surface offers it.
- `steam_workshop`, `direct_download`, `extra_files`, `profiles`,
  `foxy_config_export`, `teamspeak3_plugins` gate their own surfaces.

A capability that is `true` must be backed by something the user can actually
reach; a flag that describes an unbuilt feature is worse than a missing one.
A game that lacks a feature renders no control for it, rather than a disabled or
dead one (`conventions/ACCESSIBILITY_CONVENTIONS.md`).

A module whose Steam app id is not known at compile time (the `generic` module)
overrides `steam_app_id_from_settings` instead of `steam_app_id`. Anything that
resolves a Workshop store must call the settings-aware method, or a
user-configured game silently gets no Workshop support.

`GameSettingsSchema` carries `directories`, `texts`, and `toggles`. A `TextSetting`
is free text the module cannot describe statically; every entry needs a binding in
`src/ui/views/settings/game_space.rs` (`text_field_mut`) and a key in
`GAME_SPACE_SETTINGS_KEYS`, or the field is skipped with a warning.

`GameRegistry::active_module()` returns `None` for a space naming an unregistered
game. Anything that *acts* (auto-detection, launch, writes) must use it and refuse
on `None`. `GameRegistry::active()` falls back to the default module with a
warning and is for read-only use (labels, capability queries) only.

---

## Runtime space switching

Switching happens in-process (`src/ui/app/runtime/game_space_switch.rs`):

1. Refuse while mutating background work runs or debug mode shadows state;
   `game_space_switch_block_reason` returns the message shown in both the
   disabled-button hover text and the toast.
2. Drain the persistence queue so queued writes land in the space they belong to.
3. Release the outgoing database handle (`close_active_database_sync`), activate
   the target, honor a pending wipe marker, stop the filesystem watcher.
4. Reset every space-scoped `Foxy` field, then reload the space the way startup
   would.

Consequences for new code:

- Nothing space-derived may live in a process-wide `OnceLock`. Key it by database
  path (`DB_SLOT`, per-database maintenance) or by space id, and re-resolve after a
  switch.
- Every new `Foxy` field must be either reset in `reset_space_scoped_state` or
  listed in `APP_GLOBAL_FOXY_FIELDS` with a reason.
  `every_foxy_field_is_either_space_scoped_and_reset_or_listed_app_global` fails
  otherwise. A space-scoped field left out of the reset leaks the previous space's
  data into the next one.

---

## Sharing a Workshop selection

`workshop.json` entries carry an optional `load_order`; `workshop::launch_order_key`
is the single sort used by every launch plan, share code, checksum, and bundle, so
those four never disagree about order.

- **Share code** (`workshop/share.rs`): the pipe-separated list players paste to
  each other, `<id>[@version][;load_order]|...`, with `local:<percent-encoded name>`
  entries for mods that are not on the Workshop. The plain form is the WH3 Mod
  Manager format; `@version` is a Foxy extension other tools cannot read, so it is
  opt-in on export. Importing a version pin records the request but never writes it
  onto the local entry, because Steam only serves the latest build and a fake pin
  would make two different states report one checksum.
- **Bundle** (`workshop/bundle.rs`): a `.foxyshare` zip holding `share.json` plus
  the frozen snapshot of every pinned item under `frozen/<item_id>/`. This is what
  actually carries pinned versions. Bundles are shared between users, so treat one
  as untrusted input: paths are validated with `is_safe_child_path`, payloads must
  belong to an item the manifest lists, and the total is capped by
  `MAX_BUNDLE_UNCOMPRESSED_BYTES`.
- **Pins** (`workshop/pin.rs`): freezing copies a mod's live Steam folder into the
  space's snapshot store and records the folder's metadata hash. Drift is detected
  by re-hashing the live folder with the same function and comparing, so a Workshop
  update is visible without re-reading mod files. `PinState` is what every surface
  reports; `freeze_all` is the bulk path.
- **State checksum** (`workshop/checksum.rs`): a Paradox-style short code over the
  game's Steam build id, the ordered enabled mod set with each mod's version, and
  the launch arguments. Two players compare codes to see whether they are running
  the same thing; `diff_state_checksums` says what differs when they are not.

---

## Managed extra files and `.foxypack`

Packs are shared between users, so treat an imported pack as untrusted input.

- Entry ids and payload names are path components: `validate_entry` rejects
  anything that is not a safe child path.
- Destinations must be absolute (or `{game_dir}`-relative), must contain no `..`
  component, and must not resolve into the game space directory or the Foxy data
  root. Containment is a text comparison, which is why `..` is refused outright
  rather than resolved.
- Add-time validation is not enough: imported entries never saw it, so activation
  re-validates.
- `inspect_pack` reports every path an import would write to or sync into, plus
  the uncompressed payload size. The CLI names those paths when refusing without
  `--yes`; any future import UI must show them too. Imports are capped by
  `MAX_PACK_UNCOMPRESSED_BYTES`.

---

## Adding a game module

1. Implement `GameModule` in `src/core/game/`, reusing `GenericRunScriptModule`
   when the game only needs a templated launch plus a mods manifest.
2. Register it in `GameRegistry::new`.
3. Set capability flags to what the module actually delivers today.
4. Add its install-dir setting to `SettingsViewState`, to
   `GAME_SPACE_SETTINGS_KEYS`, to the directory binding in
   `src/ui/views/settings/game_space.rs`, and to the auto-detection binding in
   `src/ui/app/persistence/settings.rs` (the unmatched arm warns rather than
   silently discarding detection). Do the same for any `TextSetting` the module
   declares.
5. Add an example game space under `examples/json/appdata/games/<id>/`.
6. Add English strings to `src/ui/locales/en.json` only; translation fan-out is a
   separate task (`conventions/i18n_CONVENTIONS.md`).

---

## Packaging note

`steamworks` makes `steam_api64.dll` a load-time dependency of the binary. It is
delay-loaded on MSVC so a missing copy does not prevent startup, staged beside the
executable by `build.rs` and by the CI packaging step, and shipped by the Inno
Setup installer. Keep all three in sync when changing how the app is packaged.

Delay-loading defers the fault rather than removing it: the first Steamworks call
against a missing library raises `0xC06D007E` inside the loader, which no Rust
error handling can catch. Two rules follow:

- Only the `foxy steam-helper` subprocess may call Steamworks. Never call it from
  the UI process or a shared code path.
- `workshop::run_steam_helper_command` checks the library is beside the executable
  before spawning the helper, so a packaging mistake produces a clear message
  instead of a subprocess that dies with no output.
