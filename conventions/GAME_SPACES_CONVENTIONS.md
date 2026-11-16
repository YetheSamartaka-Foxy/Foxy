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
  Workshop game Foxy has no dedicated module for. It also syncs repositories.
  A custom game repository names its build in `repo.json` `gameBuild`
  (`{ modName, executable }`, `foxy-server-backend-cli new --game generic`);
  the client stores it on `Repository::game_build`, strips that folder from
  every addon list and profile (`apply_repo_game_build`, also on load), and
  never renders it as an addon. Because sync skips disabled addons, keeping it
  out of the lists is also what guarantees it is always downloaded. Its launch
  plan carries a `GameBuildTarget`: the downloaded executable starts directly,
  never through Steam, ignoring the Game Directory and Executable settings.
  Other enabled addon folders are offered only as `{mods}`.
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
    verified_hashes.json   manifest_cache\
    server\servers.json
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

Application mode is app-global: `core::app_mode::AppMode` defaults to Client and serializes as `client` or `server` in `app_settings.json`. Keep `app_mode` out of `GAME_SPACE_SETTINGS_KEYS`. `Foxy::launch_mode_override` is session-only and survives space switching; effective mode must never be copied into persisted settings. The bootstrap reads only the saved mode before claiming the database. Client mode owns its DB locally. Server bootstrap closes/suspends the client seam before starting or authenticating the separate foreground metadata owner; Server UI never claims/opens Turso. Mode transitions drain persistence and all retained client work, retain old ownership on failure and explicitly claim/resume before client reload. `GameModule::dedicated_server_support` reports WIP targets separately from existing client capabilities and grants no executable server operations.

`GameModule::server_capabilities` separately advertises typed definitions, installation adoption, deployment preflight and configuration preview/snapshot/rollback operations for Arma 3 and Reforger. Broad acquisition/runtime-deployment/lifecycle flags remain false. Use the module's typed `definition_kind`, never an ID branch in shared orchestration. Unsupported modules hide controls. `core::server::ServerStore` pins a captured context and writes the ordered v2 catalog atomically in `server/servers.json`; strict v1 migration preserves an exact backup only on a successful owned catalog save. All read/plan operations are write-free. Mutations pin the captured space's process claim. Installation roots are canonical external paths; adopted roots never become cleanup/update authority. Preflight uses shared runtime/profile/config/content/log proposals plus Arma mission/key/BattlEye or Reforger addon-temp/storage proposals, all still requiring native engine proof. Configuration snapshots and selection resolve under `server/instances/<id>/configuration/`, separate from proposed runtime destinations and the disposable DB. Do not cache stores or results across switches without captured identity.

Foreground ownership primitives live in `foxy-supervisor/`. Installation coordination uses physical directory identity in a stable same-user OS local-data namespace, not the active space or configurable root. This limited coordination exception stores persistent locks and strict consumer registrations; definitions and runtime artifacts remain space-scoped. Shared preflight/capability reports describe tested Windows/Linux primitives, the persistent-registration recovery barrier and unimplemented orphan recovery/deployment gates without claiming locks or enabling lifecycle. Session event identity includes owner generation, space, instance, operation and run generation. D03 worker admission/retention, watcher shutdown, engine close and foreground metadata-owner handoff are separate from these process events; none enables accepted dedicated-engine lifecycle.

Client server-query dispatch captures space ID and a checked monotonic query generation. A space reset advances the generation before accepting more results, clears current pending/cache state and retains unfinished handles for nonblocking joining. Reject stale results before changing even an identical current endpoint's pending flag. Returning to the same space does not revive old results. Exhausted generation disables further dispatch. This query-specific protection does not establish complete worker shutdown or DB/IPC handoff.

Desktop draft state lives in `ui/app/server/` and resets as one space-scoped field. Workers retain a JoinHandle and captured root/space/game/request identity, use explicit ServerContext selection and never resolve mutable active paths after spawning. The root comes from the Client GUI's held absolute claim or the Server GUI's authenticated captured owner context. Server workers retain a serialized metadata permit through result cleanup and bind result delivery to owner identity/generation as well as space/game/request. Keep work busy through result consumption, reject dispatch once a space switch/close is pending, and wait for completion before handoff. Dirty drafts block space and mode switching and remain available to save or discard; close requires explicit discard. Client/Server navigation has independent current/last pairs; reset both on space changes so repository indices from another space cannot be restored. Mode handoff preserves saved Server view state within the same space.

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
  without a launch plan being meaningful for it. A module that sets it implements
  `GameModule::build_repository_launch_plan`; both the GUI Launch button and
  `foxy launch` go through that method, so no caller builds an Arma-shaped plan
  for another game. Reforger sets it and turns each enabled repository folder
  into an `-addons` mod id plus an `-addonsDir` root, and a selected server into
  `-client <address>:<port>` (the game has no password parameter); its
  `reforger_addons.json` GUID store stays a separate launch path behind
  `foxy game launch`. Total War: WARHAMMER III sets it too: the enabled `.pack`
  files of a repository's addons, optional addons, and external addons become
  the `used_mods.txt` manifest, the same manifest the Workshop store feeds.
- `GameModule::content_formats` names the container formats a game's
  repositories ship, as `foxy_formats` ids (Arma 3 PBO, Reforger PAC1). Hashing
  trusts a single declared format without opening the archive head; an empty
  list falls back to the manifest-marker and magic probe, so a new module must
  declare its formats or pay that probe on every file.
- `client_side_addons` gates the client-side addon marking (the row button, the
  Client-side only filter, the repository `clientSide` manifest flag, and the
  join-preflight exemption for addons the server did not report). Arma 3 servers
  report their addon list and tolerate extra client-only mods; a Reforger server
  activates exactly its own mod set on join, so the marking has nothing to mean
  there and no surface offers it.
- `creator_dlc` gates the Creator DLC checkboxes in a repository's launch
  settings (Arma 3 only) and the join-preflight Creator DLC sections, which
  offer to enable the DLCs the server runs and disable enabled ones it does
  not. Servers report Creator DLCs as flagged entries in the rules payload's
  mod list (`ARMA3_CREATOR_DLCS` maps their Steam app IDs to `-mod=` codes);
  when that payload does not decode, the server's DLCs are unknown and no DLC
  change is offered. The other launch checkboxes come from
  `GameModule::repository_launch_flags`: Arma 3's map to dedicated `Repository`
  fields, every other game's toggle a token in `additional_params`, which is
  also where a `repo.json` `clientParameters` string lands.
- `join_addon_preflight` gates the pre-join Steam rules query for a server's
  addon list. Only Arma 3 servers publish one; a game without it goes straight
  from the online check to the launch.
- `GameModule::server_query_port` maps a listed game port to its A2S query port
  for the server cards and the join online check. The default is the Source
  convention (game port + 1, Arma 3); Reforger's query port is independent of
  the game port, so it answers with the default `a2sPort` 17777.
- Per-repository override combos in Repository Settings appear only when the
  active module's `settings_schema()` has the matching global toggle id, so a
  game never shows overrides for another game's settings.
- `steam_workshop`, `direct_download`, `extra_files`, `profiles`,
  `foxy_config_export`, `teamspeak3_plugins` gate their own surfaces.

A capability that is `true` must be backed by something the user can actually
reach; a flag that describes an unbuilt feature is worse than a missing one.
A game that lacks a feature renders no control for it, rather than a disabled or
dead one (`conventions/ACCESSIBILITY_CONVENTIONS.md`).

Addon inventory discovery calls `GameModule::is_addon_directory`: the default
recognizes an `addons` subfolder, while Reforger recognizes a root `.gproj`,
including packed addons with a root-level `.pak`. Inventory workers capture the
module before spawning and skip Steam Workshop scanning when the game lacks that
capability. Arma 3 editor mission visibility and profile resolution require the
`profiles` capability.

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

## Addon Workshop links

Addon browser links dispatch through `GameModule::addon_workshop`: Steam Workshop
capability plus the settings-aware App ID selects Steam; Reforger overrides the
provider. `game/addon_workshop.rs` resolves bounded local metadata, then bounded
repository metadata requests on a worker. Browser URLs use validated nonzero
Steam IDs or exactly 16 hexadecimal Reforger GUID digits, never metadata URLs or
launch-only fallback IDs. Pending UI lookups are cleared on a game-space switch.

## Runtime space switching

The [accepted behavior contracts](BEHAVIOR_CONTRACTS.md) define backup provenance,
private-payload preservation, Workshop removal/bundle selection and launch
preparation ownership. These choices are settled for future gap reviews.

Switching happens in-process (`src/ui/app/runtime/game_space_switch.rs`):

1. Refuse while mutating background work runs or debug mode shadows state;
   `game_space_switch_block_reason` returns the message shown in both the
   disabled-button hover text and the toast. A worker that resolves its space
   folder once (a Workshop task) counts as mutating work. Results that arrive
   after the switch are dropped when their pending entry was cleared by it.
   Per-space artifacts outside the space folder (delta-patch plans) live in a
   folder keyed by the space directory.
2. Drain the persistence queue so queued writes land in the space they belong to.
3. Signal the filesystem watcher and wait across frames until its worker has
   finished; do not start or respawn it while a switch is pending. Close client
   worker admission and join retained workers while retaining the outgoing claim.
4. Prepare the destination on a worker while the outgoing workspace stays active:
   reserve its database claim, recover configuration/launch journals, and validate
   its settings, repositories, spaces and folders. Failure offers Retry or Cancel
   and resumes outgoing worker admission.
5. Release outgoing hash-recovery evidence and close its database with
   `close_active_database_sync`; failure aborts before
   activation. The close invalidates the shared context, drains that DB's idle
   pool and refuses while handles/connections or seam operations remain.
   Promote the target reservation and activate the target. On activation failure,
   release the unused target claim and reclaim/resume the outgoing
   space before normal work continues.
6. Reset every space-scoped `Foxy` field, then reload the space the way startup
   would: explicitly claim it and resume DB admission before loading settings
   or honoring a pending wipe marker.

Consequences for new code:

- `core::server::ServerContext::capture` is a read-only inspection seam: capture
  an absolute root and validated existing space/game identity once, then derive
  paths from that value. It never uses the creating/fallback active-space helpers,
  repairs a registry, or changes remembered selection. Capturing alone grants no
  database or server-operation authority.

- Nothing space-derived may live in a process-wide `OnceLock`. Key it by database
  path (`DB_SLOT`, per-database maintenance) or by space id, and re-resolve after a
  switch.
- Every new `Foxy` field must be either reset in `reset_space_scoped_state` or
  listed in `APP_GLOBAL_FOXY_FIELDS` with a reason.
  `every_foxy_field_is_either_space_scoped_and_reset_or_listed_app_global` fails
  otherwise. A space-scoped field left out of the reset leaks the previous space's
  data into the next one.
- A switch closes client-worker admission and joins retained server-query,
  join-preflight, mission-scan, image, startup pending-restore, startup
  quick-scan eligibility, cached-update, quick-scan, and repository-sync workers
  before releasing the database.
  Those handles stay app-global so the reset cannot detach a running thread.
  Image fetches write only into the directory captured at dispatch. Database
  reads skip their section after admission closes; a read that already started
  is joined before release. Worker results carry the captured space and
  generation, and a stale preflight or cached-update result does not take a
  newer request. A quick scan can still be inside its database section after its
  activity keys are cleared, so the switch joins that handle instead of treating
  an empty key set as proof the thread has finished. Addon delete, repository wipe,
  force-redownload probes, addon hash recalculation, metadata refresh, space
  freshness, addon-size reads, direct download, Workshop actions, and database
  wipes are joined the same way, and a stale result is not applied. Addon backup,
  pending-update clears, overview stats, TeamSpeak scans, repository-space
  imports, and benchmark index writes are joined too. A game or editor launch
  does not start while admission is closed. Two instance ids keep disjoint
  proposed role directories, which is not engine isolation.

### Accepted game-space and queued-work contracts

R01-R24 in [reviewed contracts](REVIEWED_BEHAVIOR_CONTRACTS.md) also apply.
Configuration ownership is root-wide. Ordinary partial settings saves preserve
the successfully saved half; opaque valid values stay in their original file.
Divergent readable legacy/current copies remain preserved with a persistent
review notice. A remotely removed space member requires an explicit Keep in
group or Keep standalone choice without moving or deleting payloads.

- A space switch drains saves into the outgoing space before activation. While it waits, freeze new edits and show a Switching state with a cancel action. On save failure, stay in the outgoing space and offer Retry, explicit Discard changes and switch, or Cancel switching. Discard waits for in-flight writes to settle; no queued save may be retargeted to the destination.
- A manual bulk request freezes its target set and each target's normalized `(remote_url, local_path)` identity at enqueue time. If a queued row is removed or its identity changes, skip that target and explain why. Do not add repositories that join the group later. Resolve that repository's current profile when its unchanged identity actually starts.
- If a requested update through a repository space would put incompatible versions in that space's shared root, block it and report the conflicting members. For ordinary repositories sharing a folder outside that space contract, warn about affected instances and let the requested repository proceed; keep their current states independent.
- An edit, disable, or delete of a scheduled job that has an active run must ask whether it affects future runs only or also stops the current run and its post-action. Keep the active run snapshot unless the user explicitly revokes it.

---

## Player profile backups

Arma 3 player profile backups use the `profiles` capability and the space's
`profile_backups` settings object (`enabled`, `interval_days`; zero means each
application start). The default is enabled daily with three retained snapshots
per profile path and name. Store snapshots and restore recovery journals under
the captured game-space directory's `profile_backups/`, independently of the
repository database and addon backups. Run file operations off the UI thread,
validate restore roots independently of backup manifests, and finish or recover
profile writes before switching spaces or launching the game.

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
