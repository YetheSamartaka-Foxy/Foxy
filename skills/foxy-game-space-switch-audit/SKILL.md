---
name: foxy-game-space-switch-audit
description: "Audit Foxy game-space switching for state reset, storage routing, task isolation, capability gating, and repository-instance identity."
---

# Foxy game-space switch audit

Use this workflow for runtime space switching, per-space state, storage paths, or work that can finish after a switch. Read [game-space conventions](../../conventions/GAME_SPACES_CONVENTIONS.md), [core conventions](../../conventions/CORE_CONVENTIONS.md), [UI conventions](../../conventions/UI_CONVENTIONS.md), and `src/core/AGENTS.md` / `src/ui/AGENTS.md` as relevant.

Trace `src/core/game/spaces/`, `src/ui/app/runtime/game_space_switch.rs`, `reset_space_scoped_state`, and the operation's DB/task entrypoint. The active game-space directory is the source for all per-space data. A process-wide cache must not retain derived state across a switch.

## Audit isolation

- Classify each touched value as app-global or space-scoped. Every new `Foxy` field must be reset or listed in the app-global field registry with a reason.
- Follow path resolution through `active_game_space_dir()` for settings, repositories, DB, cache, workshop, and extra-file state. Do not route new per-game data to the global root.
- Trace the busy-work gate first: active downloads/scans block switching rather than being forcibly aborted. Drain outgoing persistence, close the outgoing DB, activate the target, stop watchers, reset space-scoped state, and reload. Preserve destination DB claiming before it opens, and drop or reject stale receivers/results.
- Check in-flight tasks and late results. Each result must retain the originating space/context and must not mutate the new active space.
- Verify no DB is opened before the one-process claim. Lock-conflict flow must avoid background tasks and persistence under a conflicting owner.
- Key repository instance data by normalized `(remote_url, local_path)`. Same URL in distinct folders must preserve separate addons, pending state, and status.
- Gate game-varying controls and behavior on `GameCapabilities`, never module ID checks. Unsupported controls should not render.
- Inspect persistence queues, window state, launch checks, downloads, and selected repositories for stale values that survive the transition.
- Use the existing reset guard test as the primary structural check when source edits are made; inspect its field list when adding state.

For each operation crossing a switch, record:

- The captured space ID and database path at dispatch.
- Its cancellation or completion channel and the receiver's lifetime.
- Whether it writes files, DB rows, settings, or UI state.
- The check that rejects a result after owner space changes.
- Whether a subsequent switch or restart can recover a partial operation.
- Whether the switch waits for persistence that must land in the outgoing space.
- Whether any process-wide cache key includes the active space or DB path.
- Whether watchers are stopped and rebound to the new space's paths.
- Whether the old database handle is closed before the destination can be removed.

## Evidence and stopping

Build a before/switch/after table for each scoped value and in-flight operation: owner space, storage path, receiver/task identity, and visible/persisted destination. Distinguish static trace from a runtime switch run. Never inspect or mutate a user's runtime DB or settings to reproduce isolation.

Stop when every affected value and result has one explicit owner across the switch, the DB claim order is preserved, and same-URL separate-folder behavior remains independent. If cancellation is unavailable, prove stale-result rejection at delivery and report any runtime path not exercised.

For a new space-scoped `Foxy` field, inspect the reset guard's accounting test and its field registry rather than relying on a manual list alone.

Do not simplify instance scoping to URL-only even when two repositories currently share a folder. URL is only the within-folder tiebreaker for repository spaces.

Keep any reproduction isolated and synthetic; audit notes belong under ignored `.local/`, not in tracked fixtures.
