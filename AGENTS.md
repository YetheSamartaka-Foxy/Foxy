# AGENTS.md
## Foxy repo guide for Codex, Antigravity, Gemini 3, Claude

This repo is a Rust edition 2024, version 1.96 desktop app named `Foxy` (Arma 3 mod updater). It uses egui/eframe for UI and **Turso** (pure-Rust, async-native, SQLite-compatible engine) for core storage, accessed through the `src/core/db/` seam.

Keep this file as the compact root router. Put detailed conventions in `conventions/` or nested `AGENTS.md` files so agents load them only when relevant. When workflows, commands, schemas, or conventions change, update the matching agent docs in the same change.

---

## Always follow
- Make small, reviewable changes that compile and pass the relevant checks.
- Match existing style and local patterns; avoid large refactors unless explicitly requested.
- Prefer existing helpers and clear code over new abstractions. Add an abstraction only when it removes real duplication or matches an established local pattern.
- Keep files focused; split growing modules into well-named directory modules before they become hard to review.
- Do not reformat unrelated code, rename unrelated items, or change dependencies unless the task requires it.
- Do not log secrets, tokens, or user file paths.
- Never use em dashes (—) or en dashes (–) in any text (code, comments, docs, UI strings, commit messages, changelog). Use a plain hyphen `-` instead.
- Default to writing no comment. Add one only to explain a non-obvious *why* (a safety invariant, an ordering constraint, a subtle edge case) that the code cannot state itself, and keep it to one or two lines. Never add narrative bug-history, dated regression stories, benchmark anecdotes, decorative separators, restated-code, or "what this obvious line does" comments; put operational context in logs, tests, commit messages, or conventions docs instead. When editing existing code, do not leave behind comments that narrate the change you just made.
- Do not edit runtime `database.db`, logs, caches, backups, temp patch artifacts, or user config files unless explicitly requested.
- Use `rg`/`rg --files` for discovery, inspect the nearest `mod.rs`, and confirm behavior in code before changing it.
- Check for nested `AGENTS.md` files before editing a subtree; more-specific instructions apply to files under that directory.
- When editing locale or other non-ASCII text, do not paste Unicode through PowerShell here-strings or other paths that may replace characters with literal `?`. Use UTF-8-safe edits and audit changed strings for `?` corruption before handoff.

---

## Quick map
- App starts in `src/main.rs`; GUI flow is `src/ui/window.rs` -> `src/ui/launcher.rs` -> `src/ui/app/mod.rs` (`Foxy`).
- CLI flow is `src/cli/args.rs` -> `src/cli/mod.rs` -> `src/cli/commands/`; output contracts live in `src/cli/output.rs` and `src/cli/exit_codes.rs`.
- UI code is split between state/behavior in `src/ui/app/`, screens in `src/ui/views/`, shared types in `src/ui/types/`, and shared presentation helpers in `src/ui/palette.rs`, `src/ui/fonts.rs`, `src/ui/i18n.rs`, and `src/ui/app/ui_helpers/`.
- Core code lives in `src/core/`: API/sync orchestration in `src/core/api/`, the Turso DB seam in `src/core/db/` (engine init/bootstrap in `src/core/tasks/db_turso.rs`), query/domain models in `src/core/models/`, task pipelines in `src/core/tasks/`, and shared utilities in `src/core/utils/`.
- The authoritative schema is the folded bootstrap file `sql/turso_schema.sql`; `migrations/` holds the historical SQLite migrations (no longer applied). Data/manifest references live in `sql/` and `examples/`.
- Server-side repository generator lives in `foxy-server-backend-cli/`; standalone helper tools live in `tools/`.
- Shareable repo-maintained agent skills live in `skills/`; Claude Code project-skill entrypoints live in `.claude/skills/` and should point back to the repo-maintained source.
- Runtime data defaults to `%APPDATA%\Foxy` on Windows and can be overridden with `FOXY_CONFIG_DIR` or CLI `--config-dir`.

---

## Load when needed
- UI layout, app state, widgets, palette, fonts, or egui interaction: `conventions/UI_CONVENTIONS.md` and `src/ui/AGENTS.md`.
- Agentic GUI automation, semantic UI snapshots, screenshots, FPS probes, simulated scroll/click/key input, or Codex/Claude desktop driver work: `skills/foxy-gui-driver/SKILL.md`, then `conventions/UI_CONVENTIONS.md` and `src/ui/AGENTS.md` if UI code changes are needed.
- User-facing text, locales, pluralization, RTL, i18n checker changes, or exact-English fallback cleanup: `skills/foxy-locale-translator/SKILL.md` and `conventions/i18n_CONVENTIONS.md`.
- Accessibility, keyboard flow, focus, status/error clarity, contrast, or CLI readability: `conventions/ACCESSIBILITY_CONVENTIONS.md`.
- CLI commands, flags, output contracts, `--json`, `--dry-run`, or destructive actions: `conventions/CLI_CONVENTIONS.md`.
- Core, the Turso data layer (the `src/core/db/` seam, `tasks/db_turso.rs`), schema, transactions, filesystem/network safety, or sync tasks: `conventions/CORE_CONVENTIONS.md` and `src/core/AGENTS.md`.
- Sync algorithm, quick scan, remote refresh, tree hashing, download queue, delta patch, pending updates, or sync performance: `conventions/SYNC_ALGO_CONVENTION.md`.
- Performance budgets, speed-of-light ratios, `SOL` log lines, perf baselines, or regression analysis: `conventions/SPEED_OF_LIGHT.md`.
- Tests, validation commands, pure helper coverage, or regression tests: `conventions/TESTING_CONVENTIONS.md`.
- Config examples, manifest examples, generated repository JSON, or sample fixtures: `conventions/EXAMPLES_CONVENTIONS.md`.
- Changelog entries: `conventions/CHANGELOG_CONVENTIONS.md`.
- `foxy-server-backend-cli/` changes: `foxy-server-backend-cli/AGENTS.md`.
- `tools/` changes: `tools/AGENTS.md`.
- `skills/` changes: `skills/AGENTS.md`.

---

## Project invariants
- Foxy is one binary with GUI and CLI modes. Debug no-arg launches favor the UI; release terminal no-arg launches print CLI help.
- UI is immediate-mode egui driven by `Foxy::update`; long-running work should run through background tasks and state/progress events.
- CLI and UI operate on the same config/data root.
- Repository URLs are normalized with a trailing slash; preserve that invariant in new code.
- Sync correctness depends on two checksum systems: remote tree hashes use ordered rollups, while local quick checks use BLAKE3 content rollups before tree-hash verification. See `conventions/SYNC_ALGO_CONVENTION.md`.
- Download execution is patch-first when a persisted patch plan exists, with automatic fallback to full-file download if patch validation or apply fails.
- Schema changes edit the bootstrap `sql/turso_schema.sql` and, when an existing local DB cannot keep using it, bump `DB_SCHEMA_VERSION` (`src/core/tasks/db_schema_version.rs`) to trigger the startup wipe-and-rebuild prompt; config or manifest schema changes require updated examples in the same change.
- App update metadata can come from repository-space or repository manifests; empty settings may be auto-filled, but non-empty settings are treated as user override.
- Arma 3, Steam Workshop, TeamSpeak 3, editor missions, repo-provided launch parameters, and DLC metadata are user-facing integration areas; inspect the relevant UI/core code before changing them.
- A repository *instance* is identified by `(remote_url, local_path)`, never by URL alone. The same URL installed to a different folder (a standalone install, or the same repo joined to a repository space) is an independent instance with its own addons/files/pending-update state. URL is only a within-folder tiebreaker (repos in one space share `space.shared_path` as their folder, so URL distinguishes distinct repos there). DB key is the composite UNIQUE on `repositories (remote_url, local_path)` in `sql/turso_schema.sql`. Any code that purges, wipes, dedups, or keys repo status by URL alone is a bug - scope it by `(url, local_path)`. See `conventions/CORE_CONVENTIONS.md` (core/DB) and `conventions/UI_CONVENTIONS.md` (status maps).
- Windows builds use `build.rs` plus `windres` icon resources, and `src/main.rs` handles console attachment for CLI/debug output. The embedded resource object `app_icon.o` is committed to git; `build.rs` only re-runs `windres` when `BUILD_ICON=1` but always links `app_icon.o`. CI does not set `BUILD_ICON`, so editing `app_icon.rc` alone has no effect on release builds - regenerate and commit `app_icon.o`, or prefer the Inno Setup installer (`installer/windows/foxy-setup.iss`) for things like elevation/AppCompat flags.
- Dev rebuild time is dominated by rustc re-processing the single ~103k-LOC monolithic binary crate, not codegen or linking. `[profile.dev] debug = "line-tables-only"` is the adopted mitigation; `rust-lld` and `sccache` were measured to give no net benefit as defaults. The real future lever is splitting the binary into library crates - do not re-litigate toolchain/linker tweaks.

---

## Workflow
- Triage scope first: UI, core, CLI, DB/schema, examples, tooling, or build.
- Locate the source of truth before adding behavior or abstractions.
- For non-trivial work, keep a terse scratch plan with goal, files to touch, risks/invariants, and validation steps.
- Ship a thin end-to-end slice when possible, then iterate with focused follow-ups.
- Prefer targeted checks while developing; broaden to full checks before final handoff when feasible.
- For docs-only changes, no build/test run is required; say that in the final response.

---

## Communication
- Answer in the fewest words that fully address the request. Lead with the answer; skip preamble, restating the question, and closing summaries.
- Default to a few sentences or a short bullet list. Reserve headings and multi-section write-ups for genuinely large or multi-part tasks the user asked to see laid out.
- Keep progress updates sparse and high-signal. Prefer one short sentence only when starting a new phase, making edits, waiting on long commands, or hitting a blocker.
- Avoid verbose running summaries, repeated status prints, and blow-by-blow narration of routine discovery or validation.
- Do not paste large command outputs unless the user asks. Summarize only the result and any actionable failures.
- Keep final handoffs compact by default: changed files, validation, and notable caveats only. Do not re-explain code you just wrote unless asked.

---

## Validation
- Required for code changes when feasible: `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test`.
- Start with targeted tests/checks close to the changed code, then run broader checks once the fix is stable.
- Run `cargo run` only when the change affects UI startup, CLI behavior, app bootstrapping, or user-facing runtime flows.
- If clippy reports warnings, fix safe straightforward ones. Skip only when fixing would change semantics or require risky refactoring, and note it explicitly.
- New or changed pure functions, parsers, helpers, and self-contained algorithms need focused unit tests. See `conventions/TESTING_CONVENTIONS.md`.

---

## Conditional gates
- UI or CLI user-facing changes need an accessibility impact note, or an explicit defer rationale.
- UI keyboard flow changes should validate `Arrow` navigation, `Tab`/`Shift+Tab` traversal, focus visibility, and `Enter` activation where applicable.
- DB changes must be migration-based; never manually edit runtime databases.
- Config/manifest schema changes must update `examples/`.
- CLI destructive actions must preserve `--yes`; machine-readable paths must preserve stable `--json`.
- Changelog changes happen only when explicitly requested.

---

## Final response
- State what changed and where in 1-3 bullets or a short paragraph.
- List validation performed, or clearly say why it was not run.
- Mention only relevant skipped checks, pre-existing failures, or deferred accessibility/CLI parity notes.
