# Repository skills

`skills/<name>/SKILL.md` is the canonical workflow. Prefer it over user-global copies when working in Foxy. Codex loaders in `.agents/skills/` and Claude Code loaders in `.claude/skills/` link back here; keep their names and descriptions aligned.

## Load when needed

- The categorized catalog in `skills/README.md` routes all specialized workflows. Use the narrow skill for diagnosis, sync/database integrity, performance attribution, desktop verification, or platform/release work; use the foundation skills below for shared mechanics. Do not load all skill bodies for one task.
- Rust implementation and crate-aware validation: `skills/foxy-rust-change/SKILL.md`.
- Instructions, skills, discovery, roles, or checker maintenance: `skills/foxy-agent-maintenance/SKILL.md` and `conventions/AGENT_WORKFLOWS.md`.
- Locale translation, fallback cleanup, placeholder audits, or i18n checking: `skills/foxy-locale-translator/SKILL.md` and `conventions/i18n_CONVENTIONS.md`.
- Agentic GUI automation and desktop inspection: `skills/foxy-gui-driver/SKILL.md`.
- Developer-machine UX/performance cases, baselines, calibration, and WAL/MVCC comparison: `skills/foxy-testkit/SKILL.md`, then `testkit/README.md`.

## Maintenance

- Keep descriptions precise, bodies focused, and conditional detail in linked references. Update the matching convention and root router when a workflow changes.
- Add both discovery loaders for every canonical skill. Keep canonical `agents/openai.yaml` and its `.agents/skills/<name>/agents/openai.yaml` copy identical. Default prompts name `$<skill-name>`.
- Link every canonical skill from `skills/README.md`. The Rust checker discovers skills dynamically, validates local links throughout canonical skills and references, and checks text rules in canonical and discovery metadata.
- Use repo-relative commands and compiled Rust helpers under `tools/`; do not add scripting-language helpers. For translation batches use the Rust `locale-apply` binary and audit non-ASCII strings for literal `?` corruption.
- Store local batches, caches, and working notes in ignored locations, not skill directories.
- Run `cargo agent-check` and `git diff --check` after changing skills or their entrypoints. For checker code changes follow `tools/AGENTS.md`.
