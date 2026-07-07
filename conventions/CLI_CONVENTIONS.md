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
- CLI must operate on the same config/data root as UI (app-global `app_settings.json`/`games.json`/`window_state.json`, plus per-game-space `game_settings.json`/`repositories.json`/`repository_spaces.json`/`database.db` under `games/<space_id>/`) and honor `--config-dir`.
- Per-game-space files resolve from the active game space in `games.json`. `foxy game list|use|create|remove` manages game spaces; `game use` takes effect immediately for subsequent CLI commands and on the next UI start. `game remove` is destructive and requires `--yes`.
- `foxy game launch` is the repository-free launcher for active generic game spaces. For `twwh3`, it reads enabled managed Workshop items, writes `used_mods.txt`/`my_mods.txt`, then launches with the generated manifest argument. Without `--execute`, or with global `--dry-run`, it must only preview the command and manifest.
- Steam Workshop management is per active game space through `foxy workshop list|add|import|remove|set|freeze|unfreeze|export|resolve`. Network or Steam-touching paths support explicit backend selection (`steam-helper`, `steamcmd`, or `none`) and destructive removal keeps `--yes`.

## Server Backend CLI

- For `foxy-server-backend-cli create`, preserve `appUpdateUrl` passthrough from config (`config.json`) to generated `repo.json`; if both config and `--app-update-url` are set, CLI flag wins.
