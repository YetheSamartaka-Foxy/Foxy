---
name: foxy-workshop-integration
description: Change Foxy Steam Workshop or addon integration, including capability gating, per-space state, backend selection, pinning, sharing, bundles, and launch order.
---

# Workshop integration

Use this skill for Steam Workshop management, addon Workshop links, pinned versions, share codes, and `.foxyshare` bundles.

## Workflow

1. Read [game space conventions](../../conventions/GAME_SPACES_CONVENTIONS.md), [core conventions](../../conventions/CORE_CONVENTIONS.md), and [core agent notes](../../src/core/AGENTS.md). Review [CLI conventions](../../conventions/CLI_CONVENTIONS.md) if commands or automation change.
2. Trace the game capability and active module in [GameModule](../../src/core/game/mod.rs), the [Workshop implementation](../../src/core/game/workshop/mod.rs), and the [addon Workshop resolver](../../src/core/game/addon_workshop.rs). Gate behavior through capabilities and settings-aware app ID resolution.
3. Keep Workshop stores scoped to the active game space. Resolve paths after space changes; do not cache space-derived paths globally. Clear pending UI lookups when switching spaces.
4. Preserve `launch_order_key` as the single ordering rule for launches, checksums, exports, and bundles. Inspect [pin state](../../src/core/game/workshop/pin.rs) and [state checksum](../../src/core/game/workshop/checksum.rs) before changing pin or identity semantics.
5. Treat imported bundle contents as untrusted. Preserve safe-child path validation, manifest membership checks, and the uncompressed size cap in [bundle handling](../../src/core/game/workshop/bundle.rs). Keep share-code parsing and export behavior in [share handling](../../src/core/game/workshop/share.rs).
6. Steamworks may run only in `foxy steam-helper`; ordinary UI and shared code must not call it. Preserve the explicit `steam-helper`, `steamcmd`, and `none` backend choices where applicable, and inspect [Workshop CLI](../../src/cli/commands/workshop.rs) before changing their contracts.
7. If adding or changing user-visible commands, flags, output, or UI, update the relevant tables in [README](../../README.md) and [User Guide](../../wiki/User-Guide.md). Keep `--json` stable and destructive removal behind `--yes`.
8. If persisted formats change, update the relevant sample such as [Workshop settings](../../examples/json/appdata/games/arma3/workshop.json) and inspect other game examples for shared schema impact.

## Deliverables and verification

- Describe capability gates, state scope, backend behavior, and any import safety preserved or changed.
- Add focused tests for changed parsing or pure state logic, following [testing conventions](../../conventions/TESTING_CONVENTIONS.md).
- Run the affected crate checks when implementation changes. Do not claim Steam, Workshop network, or another platform was exercised unless it was actually run.
- Stop before subscribing, downloading, uploading, or otherwise changing a live Steam account or Workshop item unless the task explicitly authorizes it.
