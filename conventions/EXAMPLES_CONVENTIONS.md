# Examples conventions

- Use `examples/` for reference data, test fixtures, and reproducible sample configs.
- Current JSON examples live in:
  - `examples/json/appdata/` (`settings.json`, `repositories.json`, `repository_spaces.json`, `window_state.json`)
  - `examples/json/remote_repositories/` (`repository_space.json`, `repo.json`)
- Runtime currently reads/writes `settings.json`, `repositories.json`, `repository_spaces.json`, and `window_state.json` in the Foxy config dir (default `%APPDATA%\\Foxy` on Windows); keep those examples schema-accurate.
- Keep `examples/` aligned with real runtime formats used by `%APPDATA%\\Foxy` files and remote `repository_space.json` / `repo.json` manifests.
- When adding or changing config schemas, manifest schemas, or generated repository JSON, update relevant files in `examples/` in the same change.
- Treat examples as non-production data: do not store secrets, tokens, or personal local paths.
