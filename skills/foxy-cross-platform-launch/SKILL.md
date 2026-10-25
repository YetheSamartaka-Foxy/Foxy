---
name: foxy-cross-platform-launch
description: Trace and improve Foxy game launch behavior across Windows and other targets, including argument quoting, permissions, launcher selection, and Steam helper packaging.
---

# Cross-platform launch

Use this skill for game, editor, Steam, or custom-game process startup and packaging changes.

## Workflow

1. Read [core conventions](../../conventions/CORE_CONVENTIONS.md), [game space conventions](../../conventions/GAME_SPACES_CONVENTIONS.md), and [core agent notes](../../src/core/AGENTS.md). For UI launch behavior, also read [UI conventions](../../conventions/UI_CONVENTIONS.md).
2. Trace the caller and selected `GameModule` through [game launch types](../../src/core/game/launch.rs), [game module contract](../../src/core/game/mod.rs), and [CLI launch](../../src/cli/commands/launch.rs). Confirm which launch plan and executable are selected before editing.
3. Keep argument boundaries explicit as `OsString` or argument vectors. Do not construct shell command strings for ordinary process launches. For Windows custom command-line construction, preserve `CommandLineToArgvW` quoting rules, including quotes and trailing backslashes.
4. Route game, editor, and Steam process starts through `spawn_unelevated`. Foxy never requests elevation and must not give an elevated token to a game. If dropping elevation fails, preserve the existing visible warning and spawn behavior.
5. Keep game-specific choices behind `GameCapabilities` and `GameModule`; a missing capability should remove the feature surface, not leave a dead control.
6. Keep Steamworks calls isolated to the `foxy steam-helper` subprocess. The main executable has a delay-loaded Steam dependency, so verify helper startup checks for `steam_api64.dll` before invoking Steamworks.
7. For Windows packaging changes, inspect [build.rs](../../build.rs), [Windows installer](../../installer/windows/foxy-setup.iss), and the applicable CI packaging workflow. The redistributable must sit beside `Foxy.exe` in each shipped layout.
8. For app update flow changes, trace [app update tasks](../../src/core/tasks/app_update/mod.rs), [manifest parsing](../../src/core/tasks/app_update/manifest.rs), and [installer flow](../../src/core/tasks/app_update/installer.rs). Preserve user-selected install location and existing data unless the requested behavior explicitly changes it.
9. Update user documentation when launch behavior or options visible to users change. Check [User Guide](../../wiki/User-Guide.md) and README launch or CLI sections as applicable.

## Platform-specific checks

- Native Linux: inspect [Steam launch resolution](../../src/core/steam.rs) for the selected binary or desktop launcher, executable permission handling, argument boundaries, and the working directory. Verify the supported fallback path before suggesting permission changes to user files.
- Windows under Wine: inspect [the native-host bridge](../../src/core/steam/wine.rs). Preserve host Steam discovery/readiness, Wine path conversion, the supported native/Flatpak/Snap launcher choice, and translation into the target game's Proton prefix. Native Unix spawns have no Windows waitable process handle; do not assume PID or wait-status behavior matches a Windows child.
- Exercise paths with spaces and non-ASCII names in synthetic fixtures. Report native Windows, native Linux, and Wine evidence separately; cross-compilation alone does not prove launch behavior.

## Deliverables and verification

- State the launch path, platform-specific behavior, argument handling, and files changed.
- Run only checks appropriate to the changed Rust crate or packaging flow, following [testing conventions](../../conventions/TESTING_CONVENTIONS.md). Do not imply another platform was tested without a platform build or runtime check.
- For Windows-sensitive behavior, inspect the exact generated arguments and packaging artifacts; report when Windows execution was unavailable.
- Stop before publishing releases, running installers on user data, or modifying runtime configuration. Those actions require task-specific authorization.
