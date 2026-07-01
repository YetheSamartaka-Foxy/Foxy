# Testing conventions

## Validation strategy

- Start with targeted checks for the code you changed, then broaden to full checks before final handoff when feasible.
- Required final checks for code changes:
  - `cargo fmt`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo test`
- Run `cargo run` when the change affects UI startup, CLI behavior, app bootstrapping, or user-facing runtime flows. Skip it for docs-only changes and note that it was not needed.
- The tree is **not** rustfmt-clean: many untouched files report `rustfmt --check` diffs. Never run a blanket `cargo fmt` over the repo - it will rewrite unrelated files and bury your change. Format only the code you touched (e.g. `cargo fmt -- <changed files>` or rely on editor format-on-save for the edited region).
- If clippy reports warnings, including pre-existing warnings in untouched files, resolve them when the fix is safe and straightforward (for example `collapsible_if`, `map_or` -> `is_none_or`, or `matches!`). Skip only when fixing would change semantics or require risky refactoring, and note that explicitly.

## Unit tests

When a change adds or modifies a pure function, parser, helper, or self-contained algorithm in the core or CLI, add or update unit tests in the same file's `#[cfg(test)]` module. Be conservative and cover only the code you are actually changing:

- New public/crate-visible function: add at least one happy-path and one edge-case test.
- Bug fix: add a regression test that fails without the fix.
- Changed function signature or behavior: update existing tests so they assert the new contract.

Do not add tests for trivial wrappers, logging-only code, UI draw functions, or code that only delegates to external I/O without local logic. Those belong in integration tests, not inline unit tests.

Existing test modules live inline next to the code they exercise. Follow the same pattern; do not create separate test files unless integration/E2E scope requires it.
