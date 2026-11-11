# Testing conventions

## Validation strategy

- Start with targeted checks for the code you changed, then broaden to full checks before final handoff when feasible.
- Required final checks for code changes:
  - Format changed code without rewriting unrelated files.
  - `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`
  - `cargo test --locked --workspace`
- Run `cargo run` when the change affects UI startup, CLI behavior, app bootstrapping, or user-facing runtime flows. Skip it for docs-only changes and note that it was not needed.
- The tree has existing formatting differences. Do not run a blanket writing `cargo fmt` over it. Use editor formatting for the changed region, or `rustfmt --edition 2024 <changed-leaf.rs>` and inspect the diff. Formatting a file with module declarations may visit child modules, so check its scope first. `cargo fmt -- <files>` does not reliably restrict Cargo's file selection. A read-only `cargo fmt --all -- --check` is safe for reporting the workspace baseline; distinguish unrelated differences from changes introduced by the task.
- If clippy reports warnings, including pre-existing warnings in untouched files, resolve them when the fix is safe and straightforward (for example `collapsible_if`, `map_or` -> `is_none_or`, or `matches!`). Skip only when fixing would change semantics or require risky refactoring, and note that explicitly.

## Selecting checks

- During iteration, select the affected crate: `cargo check --locked -p Foxy --all-targets`, `cargo test --locked -p Foxy <filter>`, or the corresponding workspace package. A non-virtual workspace defaults to its root package, so an unqualified `cargo test` does not cover all members. Use `--workspace` at integration.
- Check that filtered tests actually ran. A test filter reduces execution, not package compilation; `cargo check` also does not verify linking or runtime behavior.
- For feature-dependent changes, test the affected feature explicitly or use `--all-features` when the combined set is valid. All-features checks do not exercise every feature combination or other platforms.
- Standalone helpers under `tools/` have separate workspaces and lockfiles. Use their `--manifest-path` commands in `tools/AGENTS.md`; root workspace checks do not cover them. For a standalone-only change, run those checks rather than rebuilding the application.
- For agent instructions, skills, loaders, and role changes, run `cargo agent-check` and `git diff --check`. The focused `Agent setup` CI workflow checks the helper and metadata without building Foxy. Documentation-only changes need no application build.
- Keep Cargo builds, Clippy, and tests sequential in the main agent so shared target directories are not contended. Do not run developer-machine testkit cases from unit tests, CI, or hooks.

See [Cargo package selection](https://doc.rust-lang.org/cargo/commands/cargo-check.html), [test selection](https://doc.rust-lang.org/cargo/commands/cargo-test.html), and [feature behavior](https://doc.rust-lang.org/cargo/reference/features.html).

## Unit tests

When a change adds or modifies a pure function, parser, helper, or self-contained algorithm in the core or CLI, add or update unit tests in the same file's `#[cfg(test)]` module. Be conservative and cover only the code you are actually changing:

- New public/crate-visible function: add at least one happy-path and one edge-case test.
- Bug fix: add a regression test that fails without the fix.
- Changed function signature or behavior: update existing tests so they assert the new contract.

Do not add tests for trivial wrappers, logging-only code, UI draw functions, or code that only delegates to external I/O without local logic. Those belong in integration tests, not inline unit tests.

Existing test modules live inline next to the code they exercise. Follow the same pattern; do not create separate test files unless integration/E2E scope requires it.
