# Server CLI agent notes

- Load `../conventions/CLI_CONVENTIONS.md` before changing command behavior, flags, generated output, or automation contracts.
- Load `../conventions/EXAMPLES_CONVENTIONS.md` when generated `repo.json` or `repository_space.json` shape changes.
- Preserve `appUpdateUrl` passthrough from config to generated `repo.json`; CLI `--app-update-url` wins over config.
- `dlcContent` in config.json accepts the object form or a list of codes and is written verbatim into `repo.json`; unknown codes are a parse error.
- `create` prints a `-mod=` line last (Creator DLC codes, then enabled non-client-side mods, prefixed by `--mod-line-prefix`); keep it a single grep-able line for wrapper scripts.
- Key collection (`src/keys.rs`) is opt-in via `--collect-keys`/`--keys-output`/`--additional-keys`, flattens by file name, and must stay non-destructive toward the destination folder.
