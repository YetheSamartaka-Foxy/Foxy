---
paths:
  - "src/ui/locales/**"
  - "src/ui/i18n.rs"
---

# Locale file translation - batch+merge pattern

When asked to translate `en.json` to a new language (or re-translate an existing one), **always use the batch+merge pattern** described in `AGENTS.md` under "i18n conventions > Batch translation".

## Quick reference

1. **Read** `en.json` line count (~858 lines, ~852 keys).
2. **Spawn ~34 parallel agents** (25 keys each) using the `Agent` tool in a **single message** so they run concurrently. Use `run_in_background: true`.
   - Each agent reads its assigned line range from `en.json` (using `Read` with `offset`/`limit`).
   - Each agent writes translated key-value pairs (no outer braces) to `src/ui/locales/{code}_batch_{NN}.json`.
3. **Wait** for all batch agents to complete (you will be notified automatically).
4. **Spawn one combiner agent** (foreground) that reads all batch files in order, concatenates with correct commas, wraps in `{ }`, writes the final `{code}.json`, validates JSON, and deletes temp files.

## Translation rules (include in every batch agent prompt)

- Translate from **English only** - never between non-English locales.
- Preserve all `{placeholder}` tokens exactly (e.g. `{count}`, `{size}`, `{name}`, `{path}`).
- Keep technical terms untranslated: Arma 3, Steam, GitHub, BLAKE3, MD5, Foxy, Swifty, TeamSpeak 3, TS3.
- Use characters native to the target language (diacritics, non-Latin scripts, etc.).
- Maintain the same key order as `en.json`.

## Do NOT translate as a single agent

A single-agent translation of ~850 keys takes ~15 minutes. The batch pattern reduces this to ~1-2 minutes. Always prefer the batch approach when translating a full locale file.
