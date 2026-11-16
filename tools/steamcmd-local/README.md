# Local SteamCMD login

Standalone Linux/WSL2 helper for reusable developer-machine SteamCMD authentication. It is independent of the application and delivery milestones. It reads `.local/steamcmd.env` by default:

```dotenv
FOXY_STEAMCMD_USERNAME=
FOXY_STEAMCMD_PASSWORD=
```

Keep that file private and git-ignored. On Windows, retain a current-user-only NTFS ACL. Values are literal text, optionally surrounded by matching single or double quotes; there is no shell expansion or escape processing. Duplicate/unknown keys and empty values are refused. Put Steam account names in the username field, not display names. Steam Guard codes are entered interactively with echo disabled and are never saved to this file.

Run as an ordinary Linux user with an owned0700 Steam home and a previously installed vendor SteamCMD directory:

```text
cargo run --locked --manifest-path tools/steamcmd-local/Cargo.toml -- --steamcmd-dir /opt/local-steamcmd/bin --steam-home /opt/local-steamcmd/home
```

Use `--credentials` for another private env file. Keep the same Linux user, SteamCMD directory and Steam home across runs. The helper requests login without supplying a password first, allowing SteamCMD to reuse its cached credentials and trusted-device state. Password is supplied only if the vendor prompts for it. It quits without logging out or clearing the cache. Steam can still expire or revoke that authentication.

Add `--cached-only` to verify reuse: this disables vendor password prompting and refuses password, code or mobile-approval requests. `CACHED_LOGIN_VERIFIED` means the vendor login request succeeded and exited cleanly without supplying a password or Guard code. A stable cache lock prevents concurrent helper sessions from modifying the same cache.

For a public app install, pass `--install-app <id> --install-dir <path>`. The install directory must be an existing empty ordinary directory, separate from both the SteamCMD home and executable directory. Installs default to the Linux platform; use `--platform windows` to request Windows depots. `--platform` accepts only `linux` or `windows` and only affects install requests. SteamCMD validates the public package after download. Install sessions have a two-hour operation budget, including time waiting for authentication; ordinary login sessions retain their five-minute budget.

This general reusable authentication cache and optional public-package install helper is separate from the D04 delivery work. Keep using the same Linux user, SteamCMD directory and Steam home so authentication state is reused across general helper runs.

Credentials are sent through a fresh terminal's prompts, never command arguments. Raw vendor output is consumed privately; only fixed status messages and generic errors are displayed. An email/authenticator code prompt emits `STEAM_GUARD_REQUIRED` and waits for hidden input; app approval emits `STEAM_GUARD_APPROVAL_REQUIRED`. A temporary0600 vendor request contains the validated username and login/quit commands, with stop-on-failure enabled; it contains no password or code and is removed afterward. Authentication success uses the vendor request's exit status, independently of progress wording. The cache remains in the supplied runtime. This utility does not launch game engines or grant native platform/layout acceptance.

Validation:

```text
cargo fmt --manifest-path tools/steamcmd-local/Cargo.toml -- --check
cargo clippy --locked --manifest-path tools/steamcmd-local/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path tools/steamcmd-local/Cargo.toml
```

Unit tests use synthetic credentials/prompts and never initiate an account login. The ignored native failure test requires `FOXY_STEAMCMD_DIR` and `FOXY_STEAMCMD_HOME`; run it as an ordinary user with exclusive use of that runtime. It verifies that the vendor stops with a nonzero exit for a missing-argument login command and never logs into an account. Native login remains an explicit manual run. Status and prompt text are plain, keyboard-driven and independent of color; sensitive input is intentionally hidden.
