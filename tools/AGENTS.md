# Tools agent notes

- Tools are standalone helper projects, not the main app workspace.
- Keep tool changes scoped to the specific helper unless the user asks for a shared workflow change.
- For the i18n checker, load `../conventions/i18n_CONVENTIONS.md` before changing locale validation rules.
- The i18n checker treats duplicate top-level JSON keys as errors. For targeted translation batches, use `--require-translated-key-file <path>` with a UTF-8 file containing exact `en.json` keys, one per line, using `\n` for newline characters inside a key.
- For applying translated values before checker validation, use `skills/foxy-locale-translator/scripts/apply_translation_batch.py`; keep the checker focused on validation rather than file mutation.
