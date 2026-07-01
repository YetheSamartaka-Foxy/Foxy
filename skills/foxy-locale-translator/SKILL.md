---
name: foxy-locale-translator
description: Translate and validate Foxy `src/ui/locales/*.json` files from `en.json`. Use when Codex is asked to add, update, review, or fix Foxy localization files, translate newly added English fallback strings, validate locale coverage, handle locale JSON keys containing escaped characters such as `\n`, or prevent duplicate keys and exact-English fallback mistakes.
---

# Foxy Locale Translator

Use this skill for Foxy locale JSON work. Keep the main workflow deterministic, then use
target-language judgment for natural UI wording.

## Workflow

1. Load the repo instructions first: root `AGENTS.md`, `conventions/i18n_CONVENTIONS.md`, and `tools/AGENTS.md` if the checker changes.
2. Treat `src/ui/locales/en.json` as the only source of truth. For ordinary new or changed UI text, update English only; translate non-English locales only when the user explicitly requests localization work.
3. Translate from English to each explicitly requested target locale, never from another translation.
4. Preserve placeholders exactly, including braces and names such as `{name}`, `{path}`, `{count}`, and `{size}`.
5. Keep product and technical names unchanged where appropriate: Arma 3, Steam, GitHub, BLAKE3, MD5, Foxy, Swifty, TeamSpeak 3, TS3.
6. Decide whether the requested localization job is full key coverage or exact-English fallback cleanup:
   - For new/changed keys that must be translated in every locale, keep a changed-key file with the exact `en.json` keys, one key per line. Write `\n` in that file when the JSON key contains a newline escape.
   - For fallback cleanup where only some locales still equal English, validate changed locale/key pairs against the git baseline instead of requiring every locale for the key to differ from English.
7. For multi-locale batches, prepare a UTF-8 JSON translation map and apply it with `scripts/apply_translation_batch.py` instead of hand-building a large patch. This avoids brittle context matching when existing locale files contain mojibake or other old encoding damage.
8. Avoid regex-only edits for serialized JSON keys containing escapes. Prefer parser-based edits, then write back only changed values or do exact line replacement against the serialized key text.
9. Preserve existing formatting and line endings where possible. Full JSON reserialization can create large noisy diffs; if a parser rewrite is useful for computing values, reconstruct edits onto the original file text before finalizing.
10. Do not trust PowerShell console rendering for non-ASCII text; it may show `?` or mojibake while the file is valid UTF-8. Validate by parsing JSON and running the checker. Use escaped output (`unicode_escape`) when inspecting non-ASCII values in PowerShell.
11. Do not paste translated non-ASCII strings through PowerShell here-strings, `Set-Content`, or ad hoc shell write paths unless the full UTF-8 path is proven. These paths can silently replace unsupported characters with literal `?` in the file. Prefer `apply_patch` for targeted edits, or a checked UTF-8 script/source file.

## Translation Quality Rules

- Inspect call sites for short, ambiguous, destructive, or placeholder-heavy strings before translating. Short labels need UI context more often than full sentences.
- Translate values, not JSON keys. Keys are lookup IDs and may intentionally remain English.
- Preserve every placeholder token exactly, but move placeholders when target-language grammar requires it.
- When using machine translation, protect placeholders and product/technical terms before translation, then audit the result. Machine translation may translate placeholder names (`{name}` -> `{nombre}`) or leak protection tokens into output.
- Check every changed non-English value for literal `?` replacements after writing. A passing JSON parse or i18n key-coverage check does not prove the translated characters survived encoding.
- Keep one complete thought per translated value. Avoid literal word-by-word translations that read like concatenated UI fragments.
- Use target-language punctuation and spacing naturally. Do not assume English full stops, colon spacing, percent spacing, or sentence order fits every locale.
- Keep destructive action text direct and unambiguous. Match the severity of English without softening warnings such as delete, remove, reset, force, overwrite, or cleanup.
- Do not translate trademarks or product names. Decline or inflect them only when unavoidable for grammar and UI clarity.
- Keep command-line switches, file names, units, protocol names, and code identifiers exact unless the source intentionally localizes them. Examples: `-profiles`, `mission.sqm`, `Mb/s`, `egui`, `WGPU`, `Glow`.
- For plural or count-related strings, preserve Foxy's plural object shape and verify every required category still exists.

For detailed rationale and source links from W3C, Microsoft, and Mozilla guidance, read
`references/localization-quality.md` only when needed.

## Validation

For a batch of new or changed keys, write translations to a temporary UTF-8 JSON file shaped like:

```json
{
  "de": {
    "English key from en.json": "German value"
  },
  "pt-BR": {
    "English key from en.json": "Brazilian Portuguese value"
  }
}
```

Then apply it from the repository root:

```powershell
py -3 skills\foxy-locale-translator\scripts\apply_translation_batch.py --repo . --translations translations.json --keys-out changed-keys.txt
```

Use `--after-key "Existing en.json key"` when adding missing keys and the nearest previous `en.json` key is not already present in the target locale. Use `--allow-question-mark` only after manually confirming every literal `?` in changed values is intentional.

Before running the full checker, audit the changed-key file:

```powershell
py -3 skills\foxy-locale-translator\scripts\audit_changed_keys.py --repo . --keys changed-keys.txt
```

Always run:

```powershell
cargo run --manifest-path tools/i18n-checker/Cargo.toml -- --strict
```

For a specific translated batch, also run:

```powershell
py -3 skills\foxy-locale-translator\scripts\validate_changed_keys.py --repo . --keys changed-keys.txt
```

The helper script calls Foxy's checker with `--require-translated-key-file`, which fails when any listed key still has the exact English value in a non-English locale. This targeted check avoids false positives from legitimate unchanged values such as product names and placeholders.

For translated batches containing non-ASCII text, also scan the listed keys for literal `?` values before handoff; investigate every hit unless the question mark is intentionally part of the translation.

For exact-English fallback cleanup, where a key may already be correctly translated in some locales and legitimately unchanged in others, validate only pairs changed in the working tree:

```powershell
py -3 skills\foxy-locale-translator\scripts\audit_changed_locale_pairs.py --repo . --baseline HEAD
```

This catches placeholder mismatches and changed values that still equal `en.json` without requiring every locale for the same key to change.

## Editing Notes

- Preserve existing key order unless there is a deliberate reason to reorder.
- Keep diffs value-only when possible; avoid formatter churn from indentation, key order, or CRLF/LF changes.
- Remove duplicate top-level JSON keys by keeping the last occurrence, because `serde_json` uses the last value at runtime.
- After translation edits, run `git diff --check`. CRLF-to-LF warnings for locale JSON files are informational unless the repo explicitly requires CRLF.
