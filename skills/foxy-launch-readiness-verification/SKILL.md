---
name: foxy-launch-readiness-verification
description: "Verify Foxy Launch and Join readiness ordering, advisory versus blocking checks, database repair prompts, and dispatch-time rechecks."
---

# Foxy launch readiness verification

Use this workflow for game Launch, editor launch, server Join, or readiness prompts. Read [UI conventions](../../conventions/UI_CONVENTIONS.md), [accessibility conventions](../../conventions/ACCESSIBILITY_CONVENTIONS.md), [core conventions](../../conventions/CORE_CONVENTIONS.md), and [game-space conventions](../../conventions/GAME_SPACES_CONVENTIONS.md). For UI automation, read [GUI driver skill](../foxy-gui-driver/SKILL.md).

Trace `src/ui/app/repository/join_preflight.rs`, `prelaunch_checks.rs`, launch and Join views under `src/ui/views/repository/`, `src/core/game/launch.rs`, and `src/ui/app/runtime/database_repair.rs`. Confirm the actual path for the target action and game capability.

## Verify the readiness contract

- Classify each check as blocking or advisory and confirm the UI explains its consequence. Enabled Steam checks block applicable launches; TeamSpeak, external addons, addon/DLC selections, and other checks follow their documented advisory behavior.
- Confirm standalone addon downloads do not inherit a Steam requirement intended for game launch.
- Verify Join continuation resumes required server/addon queries before dispatch; it must not launch from stale or incomplete readiness state.
- Confirm process probes run in a background worker, results are applied only to the current modal/action, and closing/replacing the modal or switching space drops stale receivers.
- Preserve the final Steam check at dispatch for games with a Steam App ID. Generic games without one do not acquire a false Steam gate.
- Verify repository-instance identity for displayed addon snapshots and checks: normalized URL plus local path, not URL alone.
- Check readiness ordering and one-action continuation behavior for database repair. The DB reminder follows Steam, TeamSpeak, addon, and DLC readiness and must not interrupt an earlier Join continuation that still queries the server.
- Repair-required DB state remains separate from prompt visibility. Dismissing a warning must not silently enable blocked mod checks or updates; Launch/Join remain available.
- Confirm process polling never blocks rendering, modal actions remain reachable when content scrolls, and failure/cancel paths retain a safe choice.
- For game-dependent checks, follow capabilities rather than game IDs and verify unsupported readiness features are absent.

Exercise or trace these distinct outcomes:

- A blocking prerequisite is absent at initial prompt time.
- A volatile prerequisite changes between prompt and dispatch.
- An advisory warning is ignored and the operation proceeds.
- A process probe completes after the modal is replaced.
- Join continuation performs its deferred server query before final confirmation.
- A database reminder is dismissed, wiped successfully, or canceled.
- Steam becomes unavailable after its readiness probe but before dispatch.
- A database-lock conflict is discovered while opening the target space.
- A readiness result arrives for a different repository instance.

## Evidence and stopping

Create an ordered decision table for normal, blocked, advisory, unavailable, canceled, and stale-result cases. Record game capability, process state, DB repair state, modal action, and final dispatched operation. Separate static call-flow review from live UI evidence; screenshots alone do not prove dispatch-time checks.

Stop when every readiness result leads to the intended blocking/advisory action, stale results cannot affect another action or space, and the final dispatch repeats required volatile checks. If runtime paths are not exercised, say which branches remain statically reviewed only. Include keyboard/focus impact for changes to the modal.

Do not treat a prompt rendering correctly as proof that the final process command is safe; follow the continuation through `launch_value` and process dispatch.

Verify that any process launch continues through the unelevated spawn helper. A successful readiness flow does not authorize inheriting an elevated token.

Do not copy real game paths, server addresses, or repository names into a reproducible case or review artifact.
