---
name: foxy-regression-reproducer
description: "Turn a reported Foxy regression into the smallest failing Rust test or isolated testkit case, with a clear before-fix failure and post-fix proof."
---

# Foxy regression reproducer

Use this workflow when the reported behavior is understood well enough to encode. Prefer the narrowest test at the layer where the contract lives.

## Workflow

1. Read root and nearest subtree guidance, then locate the caller and source of truth for the behavior.
2. Search for existing tests around that contract before adding a new one. A pure helper belongs beside its implementation; cross-module behavior may need an integration test.
3. Reduce the input to the smallest safe fixture that still fails. Use placeholder hosts, paths, and repository names. Never derive committed fixtures from user logs without replacing real-world identifiers.
4. Add a regression test that fails before the fix for the reported reason. Avoid asserting incidental log wording or copying the implementation into the test.
5. Run the focused test, then the required checks from the testing conventions when feasible. For behavior that only appears in a real GUI or populated profile, use the established GUI or testkit workflow instead of claiming a unit test proves it.

## Evidence to hand off

- The triggering input and expected behavior.
- The exact test name and package, and whether it failed before the fix.
- What the test covers and what still requires a runtime or testkit check.

## Boundaries

- Create new isolated cases under `.local/` or ignored `testkit/cases/` when needed for the requested reproduction. Preserve existing cases, ledgers, baselines, and user data; creating a case does not require running it against a live profile.
- Keep one regression cause per test where possible. Stop if the report lacks a deterministic trigger and describe the missing observation.

## References

- [Testing conventions](../../conventions/TESTING_CONVENTIONS.md)
- [Sync regression guardrails](../../conventions/SYNC_ALGO_CONVENTION.md)
- [Testkit workflow](../foxy-testkit/SKILL.md)
