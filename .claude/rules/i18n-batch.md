---
paths:
  - "src/ui/locales/**"
  - "src/ui/i18n.rs"
---

# Locale file translation - batch+merge pattern

When asked to translate `en.json` to a new language (or re-translate an existing one), **always use the batch+merge pattern** described in `conventions/i18n_CONVENTIONS.md` under "Batch Translation", and follow `skills/foxy-locale-translator/SKILL.md`.

For a handful of new or changed keys across all locales, skip the batch pattern: write one `translations.json` and apply it with the `locale-apply` binary in `tools/i18n-checker/` (see "Batch Translation" and the targeted-batch steps in `conventions/i18n_CONVENTIONS.md`).

## Quick reference

1. **Read** `en.json` line count (~1850 lines, ~1840 keys).
2. **Spawn parallel agents** (about 25 keys each, so roughly 74 batches for a full locale) using the `Agent` tool in a **single message** so they run concurrently. Use `run_in_background: true`. Only do this when the user asked for subagents or a full new language.
   - Each agent reads its assigned line range from `en.json` (using `Read` with `offset`/`limit`).
   - Each agent writes its batch as `{ "<code>": { "English key": "translated value" } }` to `src/ui/locales/{code}_batch_{NN}.json`.
3. **Wait** for all batch agents to complete (you will be notified automatically).
4. **Merge** the batches with `locale-apply`, run the i18n checker and the placeholder audit, scan changed values for literal `?`, and delete the temporary batch files.

## Translation rules (include in every batch agent prompt)

- Translate from **English only** - never between non-English locales.
- Preserve all `{placeholder}` tokens exactly (e.g. `{count}`, `{size}`, `{name}`, `{path}`).
- Keep technical terms untranslated: Arma 3, Arma Reforger, Total War: WARHAMMER III, Steam, Steam Workshop, GitHub, BLAKE3, MD5, Foxy, Swifty, TeamSpeak 3, TS3, WGPU, Glow.
- Keep command-line switches, file names, and units exact (`-profiles`, `mission.sqm`, `used_mods.txt`, `Mb/s`).
- Use characters native to the target language (diacritics, non-Latin scripts, etc.).
- Maintain the same key order as `en.json`.

## Do NOT translate as a single agent

A single-agent translation of ~1840 keys is slow and error-prone. Always prefer the batch approach when translating a full locale file.
