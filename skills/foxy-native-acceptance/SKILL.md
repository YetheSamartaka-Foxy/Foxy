---
name: foxy-native-acceptance
description: "Prepare and assess real Foxy external-process or platform acceptance with cheap preflight, isolated native runs and fresh evidence. Use for native engines, containment or helper integration, not publish generation or testkit performance cases."
---

# Foxy native acceptance

Read applicable subtree instructions and [testing conventions](../../conventions/TESTING_CONVENTIONS.md). Use the existing domain fixture and its documented command/environment contract; keep compiled Rust helpers in their intended parent containment. Native Arma activation/layout commands and their limits already live in the testing conventions. Use [GUI driver](../foxy-gui-driver/SKILL.md) for desktop interaction and [cross-platform launch](../foxy-cross-platform-launch/SKILL.md) when changing launch code.

## Preflight before an expensive run

Establish the required platform, ordinary-user privilege/namespace policy, accepted executable/build, complete source, native dependencies and available private scratch. Keep adopted/source data read-only. On Windows use short scratch paths and verify the engine's actual path syntax, working directory and config arguments; test generated arguments locally before a multi-generation run. On Linux require the necessary POSIX modes and execute bits on suitable storage. Preview the real CLI/owner route before engine execution where it is available.

Check fixture logic before paying for the whole matrix: exact generation expectations, fresh nonce/hostname/mission markers, per-run output discovery and separately scoped mutable profiles. Compile/check the affected paths on each required platform early enough to catch platform-only setup mistakes. Existing authentication/session caches stay protected and reusable; missing credentials or engine packages are explicit external blockers, never substitutes or silent skips.

## Run with retained ownership and bounded diagnostics

Use actual native binaries through the existing retained containment/lease/consumer mechanisms. Keep teardown and cancellation joined; PID-only cleanup and deleting unknown evidence are forbidden. Record command, build/input scope, host/privilege assumptions, result and artifact location in the private task note. Keep real paths and identifiers out of tracked fixtures/docs.

Where a suitable bounded probe already exists, use it to resolve uncertain setup before full acceptance. Otherwise inspect the existing read-only plan and fixture diagnostics; do not invent a second launcher or substitute an unrelated probe. After a timeout or failure, inspect the fresh report and exact configured identity, executable/config/path selection and retained cleanup outcome before retrying. Preserve failed evidence. Increase a deadline only for an explained measured need; do not repeatedly rerun the same failing setup or treat a timeout as rejection/success.

## Accept only the demonstrated boundary

Required Windows/Linux, provider or GUI gates remain required. A compile, synthetic fixture, prior layout result or diagnostic subset cannot replace the milestone's complete native scenario. Distinguish process creation, engine identity, mission readiness, content/key separation, namespace persistence, graceful save and lifecycle ownership. The fixture must observe the claimed effect from the current generation; neither a requested save nor an earlier RPT is durable-restart proof.

Reuse a prior native result only when its input/build/environment dependencies and claimed boundary remain valid, documenting why; rerun affected scenarios after changes to launch, payload, ownership, recovery or their evidence parser. Stop when the required matrix and cleanup evidence pass. Use [test oracle review](../foxy-test-oracle-review/SKILL.md) when changing expectations or the acceptance claim itself.
