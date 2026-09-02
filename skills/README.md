# Foxy Agent Skills

This directory contains shareable Agent Skills for Foxy repository work. The repo copy is the maintained source for project-specific workflows; user-global installs can be refreshed from here when needed. Claude Code project-skill entrypoints live under `.claude/skills/` and should stay as thin loaders that point back here.

## Available Skills

- `foxy-locale-translator`: translation and validation workflow for `src/ui/locales/*.json`, including placeholder audits and exact-English fallback cleanup.
- `foxy-gui-driver`: design and workflow for adding or using a local agentic GUI driver for Foxy's egui desktop UI, including semantic snapshots, screenshots, input simulation, and FPS probes.

## Using A Skill

Agents working in this repo should read the relevant `SKILL.md` directly. For localization work, start with:

```powershell
Get-Content -Raw skills\foxy-locale-translator\SKILL.md
```

Then follow `conventions/i18n_CONVENTIONS.md` for repo-wide localization rules.

For agentic GUI driver work, start with:

```powershell
Get-Content -Raw skills\foxy-gui-driver\SKILL.md
```

Claude Code can also invoke the GUI driver directly as `/foxy-gui-driver` because this repo includes `.claude/skills/foxy-gui-driver/SKILL.md`.

## Maintaining Skills

- Keep commands repo-relative so the skill works for every contributor.
- Keep helper tooling as compiled Rust under `tools/`, invoked from the skill with `cargo run --manifest-path`, rather than as scripts checked into the skill directory. Contributors then need only the Rust toolchain.
- Do not store local caches or temporary translation batches here.
- Keep `.claude/skills/foxy-gui-driver/SKILL.md` short and pointed at `skills/foxy-gui-driver/SKILL.md`; avoid duplicating the full workflow in both places.
- When a workflow changes, update the matching convention document and root `AGENTS.md` router in the same change.

To refresh a user-global Codex install from this repo on Windows:

```powershell
Copy-Item -Recurse -Force skills\foxy-locale-translator "${env:USERPROFILE}\.codex\skills\"
Copy-Item -Recurse -Force skills\foxy-gui-driver "${env:USERPROFILE}\.codex\skills\"
```
