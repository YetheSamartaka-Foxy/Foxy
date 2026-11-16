# Local SteamCMD helper

Read `../AGENTS.md` and [README.md](README.md). This standalone Linux/WSL2 helper is independent of the application and delivery milestones.

- Treat `.local/steamcmd.env`, vendor auth caches and temporary request files as private. Never print their contents, place passwords/codes on argv, or forward raw vendor output. Use synthetic credentials in tests. Steam Guard is interactive and is never persisted by the helper.
- Reuse the same ordinary user, private Steam home and SteamCMD directory. Preserve cached credentials and trusted-device state; never log out, reset or move the cache as part of normal login. Retain the cache lock through process cleanup. Cached-only verification must refuse password/code/approval fallback.
- Installs require an explicit app id and separate empty ordinary directory. Platform selection is typed. Keep stop-on-failure and validate in the vendor request, and require its clean exit for success. Login or installation success does not establish game-engine or layout acceptance.
- Keep native vendor/account tests explicitly opt-in. Ordinary tests run only synthetic parser, request and terminal fixtures. Preserve bounded input/output and waited child cleanup.
- Validate with this standalone manifest: `cargo fmt --manifest-path tools/steamcmd-local/Cargo.toml -- --check`, `cargo clippy --locked --manifest-path tools/steamcmd-local/Cargo.toml --all-targets -- -D warnings`, and `cargo test --locked --manifest-path tools/steamcmd-local/Cargo.toml`. Run Linux checks for the supported implementation; root workspace checks exclude this helper.
