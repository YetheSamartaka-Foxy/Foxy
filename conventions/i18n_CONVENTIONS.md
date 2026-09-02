### i18n Conventions

- **`src/ui/locales/en.json` is the single source of truth.** All other locale files are translations derived from it. When adding or changing UI text, update `en.json` only unless the user explicitly asks for non-English translations.
- Do not update non-English locale files for new or changed English UI text unless explicitly requested. Translation coverage, fallback cleanup, and new-language work are separate localization tasks.
- Never translate from one non-English locale to another. Use `en.json` for every explicitly requested target locale.
- Use the i18n helpers for all user-facing text: `self.t(...)`, `self.t_fmt(...)` in `Foxy` impls, or `tr(...)`, `tr_fmt(...)` where needed.
- Keep translation keys stable and readable. English phrase keys are the current repo standard.
- Preserve placeholder names exactly in formatted strings, including braces: `{count}`, `{size}`, `{name}`, `{path}`.
- Do not hardcode new user-visible strings in views unless there is a deliberate temporary reason.
- Locale files live in `src/ui/locales/`. Bundled translations are: `ar`, `bg`, `bn`, `cs`, `da`, `de`, `el`, `es`, `et`, `fa`, `fi`, `fr`, `he`, `hi`, `hr`, `hu`, `id`, `it`, `ja`, `ko`, `lt`, `lv`, `nb`, `nl`, `pl`, `pt`, `pt-BR`, `ro`, `ru`, `sk`, `sl`, `sr`, `sv`, `th`, `tl`, `tr`, `uk`, `ur`, `vi`, `zh`.

### Translation Workflow

Use parser-based JSON handling for audits and merge decisions. Avoid broad formatter rewrites of locale files; final diffs should be value-only where possible, without key reordering, indentation churn, or CRLF/LF churn.

For any non-English locale change:

1. Load root `AGENTS.md`, this file, and nested `AGENTS.md` files for the edited subtree.
2. Confirm the user explicitly requested translation/localization work, then identify whether the task is new key coverage, new language creation, or exact-English fallback cleanup.
3. Translate values from `en.json`, preserving placeholders and technical terms.
4. Validate JSON parsing, placeholder preservation, and translation coverage.
5. Run the i18n checker before handoff.

Do not trust PowerShell console rendering for non-ASCII locale content. It may show `?` or mojibake while the file is valid UTF-8. Prefer parser/checker results, or print escaped output when inspecting exact values.

Do not paste translated non-ASCII strings through PowerShell here-strings, `Set-Content`, or ad hoc shell write paths unless the full UTF-8 path is proven. These paths can silently replace unsupported characters with literal `?` in the file. Prefer `apply_patch` for targeted edits, or a checked UTF-8 script/source file, then parse the JSON and explicitly scan changed values for literal `?` characters.

### Exact-English Fallback Cleanup

When finding untranslated strings in existing locale files, do not treat every exact match as a bug. Product names, acronyms, short technical labels, placeholders, units, and some borrowed UI terms may intentionally match English.

Recommended process:

1. Compare each non-English locale to `en.json` with a JSON parser.
2. Exclude values that should remain unchanged, such as `Foxy`, `Arma 3`, `Steam`, `GitHub`, `BLAKE3`, `MD5`, `WGPU`, `Glow`, `TS3`, `{name} ({scope})`, and units like `Mb/s`.
3. Prioritize clear user-facing prose and action labels: warnings, confirmations, destructive actions, help text, and complete sentences.
4. Keep a changed-key file for placeholder auditing, but do not rely on `--require-translated-key-file` when only some locales for a key were edited. That checker requires every locale listed for the key to differ from English and can over-report legitimate unchanged locales.
5. For fallback cleanup, validate only the locale/key pairs changed in the working tree:

   ```powershell
   cargo run --manifest-path tools/i18n-checker/Cargo.toml -- --audit-changed-since HEAD
   ```

6. Run the global checker:

   ```powershell
   cargo run --manifest-path tools/i18n-checker/Cargo.toml -- --strict
   ```

### New Or Changed Keys

By default, add or change new UI text only in `src/ui/locales/en.json`. Do not fan those keys out to non-English locale files unless the request explicitly includes translation coverage for those locales.

When translating a specific batch of newly added or changed `en.json` keys, write those exact keys to a temporary UTF-8 text file, one key per line. Use `\n` in the file when a JSON key contains a newline escape.

For multi-locale batches, prefer the `locale-apply` helper binary instead of hand-editing every locale file:

```powershell
cargo run --manifest-path tools/i18n-checker/Cargo.toml --bin locale-apply -- --repo . --translations translations.json --keys-out changed-keys.txt
```

The `translations.json` file must be UTF-8 and shaped as `{ "locale": { "English key from en.json": "translated value" } }`. The helper preserves existing formatting, inserts missing keys near their `en.json` order, checks placeholders, and rejects literal `?` in changed values unless `--allow-question-mark` is passed after manual review.

Then run:

```powershell
cargo run --manifest-path tools/i18n-checker/Cargo.toml -- --strict --require-translated-key-file changed-keys.txt
```

That single run covers both the placeholder audit and the coverage check: placeholder parity is scanned for every shared key, and it fails the run for the keys named by `--require-translated-key-file`. Placeholder mismatches on other keys are reported as `[?] PLACEHOLDER` warnings; use `--strict-placeholders` to fail on those too.

Use `--require-translated-key-file` only when every non-English locale for each listed key is expected to be translated away from the exact English value.

### Batch Translation

Locale JSON files are large enough that whole-file translation in one pass is slow and error-prone. For full language creation or large coverage work, use a batch+merge pattern.

- Batch size: about 25 keys.
- Source: always read from `src/ui/locales/en.json`.
- Output: temporary files like `src/ui/locales/{code}_batch_{NN}.json`.
- Merge: combine batches in key order, parse the result as JSON, then delete temp files.
- Validation: run placeholder audits and the i18n checker after merge.

Each batch prompt must include:

- Target language code and name.
- Exact key or line range from `en.json`.
- Output as JSON shaped for the `locale-apply` binary: `{ "locale": { "English key": "translated value" } }`.
- Instruction to preserve all `{placeholder}` tokens exactly.
- Instruction to keep technical terms unchanged: Arma 3, Steam, GitHub, BLAKE3, MD5, Foxy, Swifty, TeamSpeak 3, TS3, WGPU, Glow.
- Instruction to keep command-line switches, filenames, units, protocol names, and code identifiers exact unless intentionally localized: `-profiles`, `mission.sqm`, `Mb/s`, `egui`.
- Instruction to translate values only, not keys.

### Machine Translation Guardrails

Machine translation can be useful for a first pass, but it must be audited.

- Protect placeholders before translation and verify they are restored exactly. Common failures include `{name}` becoming `{nombre}` or `{नाम}`.
- Protect product and technical terms, then check that no protection token leaked into output.
- Check for literal `?` replacements in every changed non-English value. A passing JSON parse or i18n key-coverage check does not prove the translated characters survived encoding.
- Manually review short labels, destructive actions, warnings, and domain-specific terms.
- Avoid translating command switches such as `-profiles`; a translated switch will break runtime behavior or user guidance.
- Do not translate file names such as `mission.sqm` or units such as `Mb/s` unless the source value intentionally changes.
- If a parser rewrite was used to compute translations, reconstruct final edits onto the original file text to keep diffs value-only.

### New Language Checklist

When adding a new language:

1. Create `src/ui/locales/{code}.json` translated from `en.json`.
2. In `src/ui/i18n.rs`, add the bundle static, `LocaleFormat`, `define_collator!`, and match arms in `resolve_bundle`, `resolve_locale_format`, `resolve_collator`, `plural_category`, `SUPPORTED_LANGUAGES`, and `detect_system_language`.
3. If the script requires a new font, add it to `src/ui/fonts/` and register it in `src/ui/app/init/startup.rs`.
4. Add the language name key to every existing locale JSON file.
5. Add the language to the language selector in `src/ui/views/settings/app_general.rs`.
6. Update the bundled locale list in `README.md`, root `AGENTS.md` if present there, and this file.
7. Run:

   ```powershell
   cargo run --manifest-path tools/i18n-checker/Cargo.toml -- --strict
   ```

### Validation

- After any locale-file change, run:

  ```powershell
  cargo run --manifest-path tools/i18n-checker/Cargo.toml -- --strict
  ```

- For changed-key batches, pass `--require-translated-key-file changed-keys.txt`.
- For exact-English fallback cleanup, pass `--audit-changed-since HEAD`.
- Run `git diff --check` before final handoff.
- For docs-only convention changes, no Rust build or test run is required.

### L10n Conventions

i18n provides the infrastructure: keys, translation files, formatting helpers. L10n is the per-locale adaptation that makes the app feel native to each audience.

#### Date, Time, And Number Formatting

- Use locale-aware formatting for dates, times, numbers, and file sizes. Never hardcode a single format such as `MM/DD/YYYY`.
- File sizes should respect locale convention where applicable. Keep binary `MiB`/`GiB` vs decimal `MB`/`GB` choices consistent within a locale.
- Decimal and thousands separators vary by locale. Rely on formatting libraries rather than manual string building.

#### Sorting And Collation

- Text sorting must use locale-appropriate collation rules, for example Czech `č` sorts after `c`, not after `z`.
- When ordering user-visible lists such as mod names, addon names, and profiles, apply locale collation rather than byte-order sorting.

#### Text Expansion And Layout

- Expect translated strings to be significantly longer or shorter than English. German can be about 30% longer; CJK may be shorter but taller.
- UI layouts must not clip, overlap, or break when strings expand. Test with the longest bundled locale when a UI surface changes.
- Avoid fixed-width containers for translatable text. Prefer flexible or wrapping layouts.
- When adding new UI labels or messages, verify in `en.json` and translated JSON that the result fits common layouts.

#### RTL Support

- RTL locales are Arabic (`ar.json`), Persian (`fa.json`), Urdu (`ur.json`), and Hebrew (`he.json`). Their `LocaleFormat` sets `TextDirection::Rtl`.
- RTL-aware layout helpers (`content_layout`, `trailing_layout`, `leading_align2`, `is_rtl`) in `src/ui/i18n.rs` drive widget placement. Use them instead of hardcoded `Layout::left_to_right` or `Layout::right_to_left`.
- Do not assume left-to-right text direction in new layout code. Avoid hardcoded directional padding or margins where a mirrored layout would break.
- egui currently does not provide full bidirectional text rendering. Mixed RTL/LTR text within a single label may not reorder correctly, and `TextEdit` does not support RTL cursor movement. Layout-level mirroring is the current support scope.
- Bundled Arabic font: `src/ui/fonts/NotoSansArabic-Regular.ttf` is loaded as a proportional fallback in `Foxy::new` so Arabic, Persian, and Urdu glyphs render correctly.
- Bundled Hebrew font: `src/ui/fonts/NotoSansHebrew-Regular.ttf` is loaded as a proportional fallback for Hebrew glyphs.

#### Locale-Aware Content Rules

- Pluralization rules differ across locales. Use i18n formatting helpers with proper plural categories rather than naive `if count == 1` checks.
- Currency, measurement units, or future domain-specific formatting must be locale-driven.
- When displaying user-generated content alongside localized UI text, keep locale context consistent.
