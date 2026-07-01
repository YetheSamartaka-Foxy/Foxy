# CLI conventions

- Core idea: if a user can perform an operational action in UI, they should usually be able to perform it via CLI too.
- Be conservative when extending CLI: add commands for deterministic, scriptable, automation-friendly operations first; avoid purely visual/UI-only controls or highly interactive flows.
- When adding a new app capability, explicitly consider CLI parity in the same change.
- If CLI parity is in scope, add or extend CLI commands and output for it.
- If CLI parity is not in scope, include a short rationale in change notes explaining why it is deferred.
- Keep UI and CLI behavior aligned by reusing shared core/domain logic instead of duplicating business rules.
- Preserve automation guarantees in CLI output and behavior.
- `--json` output should remain machine-readable and stable.
- Destructive actions should require `--yes`.
- Support `--dry-run` where feasible.
- CLI must operate on the same config/data root as UI (`settings.json`, `repositories.json`, `repository_spaces.json`, `window_state.json`, `database.db`) and honor `--config-dir`.

## Server Backend CLI

- For `foxy-server-backend-cli create`, preserve `appUpdateUrl` passthrough from config (`config.json`) to generated `repo.json`; if both config and `--app-update-url` are set, CLI flag wins.
