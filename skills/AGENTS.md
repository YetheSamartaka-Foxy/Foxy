# AGENTS.md
## Repo Skills

Skills in this directory are shareable agent workflows maintained with the repo. Prefer these repo-local copies over user-global skill copies when working in this repository, because they track Foxy's current conventions and helper scripts.

## Load When Needed

- Foxy locale JSON translation, exact-English fallback cleanup, placeholder audits, or i18n checker workflows: `skills/foxy-locale-translator/SKILL.md`.
- Agentic GUI automation, Playwright-like desktop UI debugging, screenshots, FPS probes, semantic UI snapshots, or driver IPC for Codex/Claude: `skills/foxy-gui-driver/SKILL.md`.
- Claude Code invocation of the GUI driver: `.claude/skills/foxy-gui-driver/SKILL.md`, which is a thin loader for `skills/foxy-gui-driver/SKILL.md`.

## Maintenance

- Keep skill scripts runnable from the repository root.
- Keep command examples repo-relative, not user-specific absolute paths.
- For repeated locale insertion/update work, prefer the `locale-apply` binary in `tools/i18n-checker/` over one-off generated patch blocks. Skills in this repo call compiled Rust helpers, not scripting-language helpers.
- For locale skills or examples, avoid PowerShell text-write patterns that can corrupt non-ASCII text into literal `?`; require UTF-8-safe edits plus a post-edit scan for `?` in changed translated values.
- When localization conventions change, update both `conventions/i18n_CONVENTIONS.md` and `skills/foxy-locale-translator/SKILL.md` in the same change.
- When the GUI driver workflow changes, update `skills/foxy-gui-driver/SKILL.md` first, then keep `.claude/skills/foxy-gui-driver/SKILL.md` as a small pointer to that canonical file.
- Validate skill metadata with:

  ```powershell
  py -3 "${env:USERPROFILE}\.codex\skills\.system\skill-creator\scripts\quick_validate.py" skills\foxy-locale-translator
  py -3 "${env:USERPROFILE}\.codex\skills\.system\skill-creator\scripts\quick_validate.py" skills\foxy-gui-driver
  ```
