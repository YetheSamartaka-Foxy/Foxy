# Server CLI agent notes

- Load `../conventions/CLI_CONVENTIONS.md` before changing command behavior, flags, generated output, or automation contracts.
- Load `../conventions/EXAMPLES_CONVENTIONS.md` when generated `repo.json` or `repository_space.json` shape changes.
- Preserve `appUpdateUrl` passthrough from config to generated `repo.json`; CLI `--app-update-url` wins over config.
- `dlcContent` in config.json accepts the object form or a list of codes and is written verbatim into `repo.json`; unknown codes are a parse error.
