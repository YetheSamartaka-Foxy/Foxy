# Tools agent notes

- Tools are standalone helper projects, not the main app workspace.
- Keep tool changes scoped to the specific helper unless the user asks for a shared workflow change.
- For the i18n checker, load `../conventions/i18n_CONVENTIONS.md` before changing locale validation rules.
- The i18n checker treats duplicate top-level JSON keys as errors. For targeted translation batches, use `--require-translated-key-file <path>` with a UTF-8 file containing exact `en.json` keys, one per line, using `\n` for newline characters inside a key.
- The i18n checker package ships two binaries over a shared `lib.rs`: `i18n-checker` (validation only, the default binary) and `locale-apply` (the only one that writes locale files). Keep the checker read-only; put file mutation in `locale-apply`.
- It is a standalone workspace on purpose. Do not add it to the root workspace: `serde_json`'s `preserve_order` feature is not additive, so joining the workspace risks changing `serde_json::Map` behavior for `Foxy` and `foxy-server-backend-cli`. Nothing here needs it, because key order comes from the raw scanner in `lib.rs`.
- Validate it with `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` using `--manifest-path tools/i18n-checker/Cargo.toml`.
