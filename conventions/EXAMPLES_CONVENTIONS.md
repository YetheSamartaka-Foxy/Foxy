# Examples conventions

- Use `examples/` for reference data, test fixtures, and reproducible sample configs.
- Current JSON examples live in:
  - `examples/json/appdata/` (`app_settings.json`, `games.json`, `window_state.json`)
  - `examples/json/appdata/games/arma3/` (`game_settings.json`, `repositories.json`, `repository_spaces.json`, `extra_files.json`, `workshop.json`)
  - `examples/json/appdata/games/twwh3/` (`game_settings.json`, `repositories.json`, `repository_spaces.json`, `extra_files.json`, `workshop.json`)
  - `examples/json/appdata/games/reforger/` (`game_settings.json`, `repositories.json`, `repository_spaces.json`, `extra_files.json`, `workshop.json`, `reforger_addons.json`)
  - `examples/json/remote_repositories/` (`repository_space.json`, `repo.json`)
- Runtime reads/writes app-global files (`app_settings.json`, `games.json`, `window_state.json`) in the Foxy config dir (default `%APPDATA%\\Foxy` on Windows) and per-game-space files (`game_settings.json`, `repositories.json`, `repository_spaces.json`, `repository_visual_folders.json`, `extra_files.json`, `workshop.json`, `reforger_addons.json`, `database.db`) in `games/<space_id>/` under it; keep those examples schema-accurate.
- A legacy flat `settings.json`/`repositories.json` layout is migrated on startup into the split layout above (see `plan-progress/phase-1.md`).
- Keep `examples/` aligned with real runtime formats used by `%APPDATA%\\Foxy` files and remote `repository_space.json` / `repo.json` manifests.
- When adding or changing config schemas, manifest schemas, or generated repository JSON, update relevant files in `examples/` in the same change.
- Treat examples as non-production data: do not store secrets, tokens, or personal local paths.
