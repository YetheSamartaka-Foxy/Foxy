# Tools agent notes

- Tools are standalone helper projects, not the main app workspace.
- `tools/agent-check/` is a read-only checker for shared skills, discovery loaders, agent role pairs, instruction imports, catalog coverage, and local Markdown file links across all canonical skills/references. Discover skills dynamically instead of maintaining per-skill lists. Run it from the repository root with `cargo agent-check`; it must never launch Foxy or read runtime data.
- Keep `agent-check` standalone with its own lockfile and small metadata-parser dependencies. Validate changes with `cargo fmt --manifest-path tools/agent-check/Cargo.toml -- --check`, `cargo clippy --locked --manifest-path tools/agent-check/Cargo.toml --all-targets -- -D warnings`, `cargo test --locked --manifest-path tools/agent-check/Cargo.toml`, and `cargo agent-check`. Root workspace checks do not cover standalone tools.
- Keep tool changes scoped to the specific helper unless the user asks for a shared workflow change.
- For the i18n checker, load `../conventions/i18n_CONVENTIONS.md` before changing locale validation rules.
- The i18n checker treats duplicate top-level JSON keys as errors. For targeted translation batches, use `--require-translated-key-file <path>` with a UTF-8 file containing exact `en.json` keys, one per line, using `\n` for newline characters inside a key.
- The i18n checker package ships two binaries over a shared `lib.rs`: `i18n-checker` (validation only, the default binary) and `locale-apply` (the only one that writes locale files). Keep the checker read-only; put file mutation in `locale-apply`.
- It is a standalone workspace on purpose. Do not add it to the root workspace: `serde_json`'s `preserve_order` feature is not additive, so joining the workspace risks changing `serde_json::Map` behavior for `Foxy` and `foxy-server-backend-cli`. Nothing here needs it, because key order comes from the raw scanner in `lib.rs`.
- Validate it with `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` using `--manifest-path tools/i18n-checker/Cargo.toml`.
