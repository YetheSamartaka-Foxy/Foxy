---
name: foxy-rust-change
description: "Implement or fix Rust behavior in Foxy with focused compiler feedback, crate-aware validation, and existing UI/core/CLI contracts. Use for code changes, not docs-only edits, translation batches, or running performance cases."
---

# Foxy Rust changes

Read root `AGENTS.md`, the nearest subtree instructions, and the matching conventions before editing. Inspect the nearest `mod.rs` and follow the real callers from the reported behavior. For non-trivial work, keep a short plan in `.local/` with the files, invariants, and validation needed.

## Choose the seam

For specialized diagnosis, regression analysis, performance, desktop verification, or release work, select the relevant workflow from the [skill catalog](../README.md). It supplies the domain evidence contract while this skill covers the Rust implementation loop.

- UI: `src/ui/AGENTS.md`, [UI conventions](../../conventions/UI_CONVENTIONS.md), and [accessibility](../../conventions/ACCESSIBILITY_CONVENTIONS.md). Keep work out of immediate-mode rendering; use the existing task/state flow.
- Core or storage: `src/core/AGENTS.md` and [core conventions](../../conventions/CORE_CONVENTIONS.md). Use the Turso seam and claimed game-space database; preserve repository-instance identity.
- Sync: also read [sync guardrails](../../conventions/SYNC_ALGO_CONVENTION.md) before changing checksums, planning, patching, or refresh triggers.
- CLI: [CLI conventions](../../conventions/CLI_CONVENTIONS.md). Preserve exit codes, `--json`, and destructive-action `--yes` contracts.
- Game-dependent behavior: [game-space conventions](../../conventions/GAME_SPACES_CONVENTIONS.md). Gate features through capabilities and keep space-derived state scoped.
- Standalone tools or server CLI: read their own `AGENTS.md`. Tools have separate manifests and are not covered by root workspace checks.

Use existing helpers and inspect versions/features in `Cargo.toml` and `Cargo.lock` before proposing an API. Do not change profiles, dependencies, or the toolchain to speed up ordinary edits. Preserve the established dev profile and the single-binary build constraints.

Keep the observable contract and acceptance inputs stable during structural changes. Deliver one real caller-to-result slice before expanding helpers. Let private types, ownership and existing guards enforce invariants where practical; do not add `Arc<Mutex<_>>`, unsafe code or detached work just to silence the compiler. A retained handle or `Drop` does not by itself prove async cleanup, event ordering or waited shutdown.

Minimize comments: remove what/how narration, commented-out code and stale TODOs. Keep concise non-obvious constraints, safety rationale and public API contracts; preserve legal notices, tooling directives and macro-consumed documentation such as Clap help. Prefer descriptive names and types over prose. For comment cleanup, preserve literal payloads and executable tokens, and do not add unrelated refactors.

## Compiler feedback and tests

Use [testing conventions](../../conventions/TESTING_CONVENTIONS.md) for the complete validation contract. During iteration select the affected package, for example:

```text
cargo check --locked -p Foxy --all-targets
cargo test --locked -p Foxy <test_filter>
cargo test --locked -p foxy-formats <test_filter>
```

`cargo check` skips final code generation and linking, so it cannot replace tests or a required runtime smoke. Selecting a test name still compiles the package; do not expect it to avoid the main binary's compile cost. Check that the filter actually ran the intended tests.

Batch coherent edits and choose one next feedback command, rather than automatically running check, build and test for the same snapshot. If a focused test is the needed next signal, it already compiles the package. Compile once after integrating returned worker changes, group diagnostics by their shared cause, and reserve broad checks for stable integration. Use [milestone execution](../foxy-milestone-execution/SKILL.md) for a larger outcome and [native acceptance](../foxy-native-acceptance/SKILL.md) for real external-process proof.

Fix the first relevant compiler error with its notes and callers before changing downstream errors. Preserve error context; avoid adding `unwrap`, widening visibility, cloning large state, or suppressing a lint simply to make compilation pass. Use `rustc --explain <code>` for an unfamiliar diagnostic. Machine consumers can request `--message-format=json`; handle Cargo event types rather than parsing rendered text.

For changed pure behavior, cover success and a meaningful boundary, or a regression that fails without the fix. Extend existing cases where useful; avoid one-test-per-wrapper quotas. Preserve independent expected values and explain material fixture/expectation changes using [test oracle review](../foxy-test-oracle-review/SKILL.md). Keep testkit cases and live GUI checks in their specialist skills.

When conditional compilation changes, select the affected features explicitly. `--all-features` covers their combined configuration, not every feature combination or operating system. Explain any target-specific validation that was unavailable.

Use `cargo tree -e features -i <dependency>` to find feature activation and `cargo tree -d` for duplicate versions. If a dependency update is in scope, prefer `cargo update <package>` to a workspace-wide update and inspect the lockfile diff. Otherwise use `--locked` to detect unintended resolution changes.

## Integration

For delegated work follow [agent workflows](../../conventions/AGENT_WORKFLOWS.md): workers own disjoint files; the main agent integrates and validates once. Request async review when changes cross cancellation, locks, task dispatch, or shared-state boundaries.

Give reviewers the contract and scoped code without the intended verdict. Verify concrete findings, then recheck fixes and affected paths. Do not restart a whole review/test cycle after notes-only edits or keep reviewing once the outcome and required checks pass.

Update the user guide or server guide when behavior changes, examples when schemas change, and English locale keys when UI text changes. Translate other locales only when requested. Include an accessibility impact note for UI/CLI changes. Leave the changelog alone unless requested.

Inspect the final diff for unrelated formatting, sensitive identifiers from logs, and runtime artifacts. Report changed behavior and evidence, plus checks that failed or were not run.

## Primary references

- [Cargo check](https://doc.rust-lang.org/cargo/commands/cargo-check.html), [test selection](https://doc.rust-lang.org/cargo/commands/cargo-test.html), and [feature coverage](https://doc.rust-lang.org/cargo/reference/features.html).
- [Dependency feature inspection](https://doc.rust-lang.org/cargo/commands/cargo-tree.html), [targeted updates](https://doc.rust-lang.org/cargo/commands/cargo-update.html), [Cargo JSON events](https://doc.rust-lang.org/cargo/reference/external-tools.html#json-messages), and [compiler diagnostics](https://doc.rust-lang.org/rustc/json.html).
