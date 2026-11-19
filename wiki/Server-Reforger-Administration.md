# Reforger Server Administration

This page covers typed administration for a running native Reforger instance and its scenario-end policy. These operations require a compatible native runtime and an owner-managed foreground run.

In the desktop app, select the Reforger instance from the Server sidebar and open **Operations** for typed administration and RCon. Reforger save-session selection is available from the related **Setup** workflow. The workspace map and navigation are in the [Server Admin Guide](Server-Admin-Guide.md#finding-your-way-around-server-mode).

The Server **Addons** page shows Reforger's native addon review and lock status. Use its **Setup** link to edit desired addon intent. This review does not imply that addon content has been acquired or activated.

## Typed RCon administration

The main Foxy binary exposes typed Reforger operations through the authenticated server owner. Read-only `players` and `bans` need a running owner. Ban pages are one-based. Mutations require `--yes` and the exact `--expected-run-generation` from current instance status. `--dry-run` validates command arguments without contacting an owner or writing records.

```text
foxy server reforger-admin players <instance-id> --game-space reforger --json
foxy server reforger-admin bans <instance-id> --page 1 --game-space reforger --json
foxy server reforger-admin kick <instance-id> --player-id <id> --expected-identity <identity> --expected-run-generation <run> --game-space reforger --yes --json
foxy server reforger-admin ban <instance-id> --identity <identity> --duration-seconds 3600 --reason "Operator review" --expected-run-generation <run> --game-space reforger --yes --json
foxy server reforger-admin remove-ban <instance-id> --identity <identity> --expected-run-generation <run> --game-space reforger --yes --json
foxy server reforger-admin reload-scenario <instance-id> --expected-run-generation <run> --game-space reforger --yes --json
foxy server reforger-admin logout <instance-id> --expected-run-generation <run> --game-space reforger --yes --json
```

Kick checks a fresh player listing in the serialized RCon session and sends only if that player id still has the expected persistent identity. Identity inputs currently accept only a conservative 44-character Base64 form. This is provisional: native identity format and successful ban behavior still need confirmation, so do not treat it as a general UID specification.

Replies retain the bounded command record and the exact captured transcript for the command. Native empty acknowledgements are not proof that a mutation completed. The JSON `parsed.complete` value applies only to that captured list response. Foxy does not claim the player response is the complete roster or that every ban page has been enumerated. An empty ban list is considered known only when the server explicitly says `Server has no bans to list.` A timeout or unknown outcome means the command may have run; inspect history and the server before deciding whether to try again. Foxy does not resend unknown commands.

The typed command timeout defaults to 10 seconds and accepts `--timeout-ms` from 1000 through 60000. For a separate raw `server rcon probe`, `--listen-ms` defaults to 300 and accepts 0 through 30000 to collect delayed asynchronous messages after the command ACK. Allow time for semantic messages such as permission denials, and treat the ACK as transport evidence only. `ban --duration-seconds 0` requests a permanent ban. Reforger permissions are configured for all RCon clients in the committed `server.json`; admin-only commands require the configured permission to allow administration.

Exit 0 means a complete observed list or verified scenario reload. Exit 5 means an unknown outcome, incomplete list, or acknowledged mutation whose effect remains unverified. Exit 4 means the command was not sent, the native command was rejected, or the owner was unavailable; exit 2 means invalid input. These commands do not open Turso. `reload-scenario` uses the lifecycle operation and succeeds only after fresh committed scenario/world-load evidence and a later query on the same process run. It does not restart the process. Process `restart` remains a stop followed by a new run.

Some Reforger builds buffer scenario and world log output until process shutdown. In that case, a reload can return exit 5 with an unconfirmed outcome, and a maintenance run can remain `unknown`, even if the engine performed the reload. Foxy does not retry either command automatically. A healthy query on the same run confirms the process remains responsive, but does not prove a reload occurred. Treat an unconfirmed result as unknown until separate native evidence establishes the effect. Native RCon permission denials can also arrive after the command ACK; allow the probe's listen interval to capture the semantic message.

## Scheduled Reforger scenario reload

Reforger scheduled maintenance can use `reload-scenario` to reload the currently committed scenario in the same process, or `backup` to create a self-verifying server backup. Backup jobs use stopped consistency by default; `--live-backup` explicitly permits best-effort capture while running. The `update` action remains recorded as blocked because unattended installation updates are unavailable. Scenario reload is separate from Arma's `rotate-mission`, which selects another deployed mission. No broadcast, save, or ad hoc mission-selection action is implied.

The [maintenance jobs example](../examples/json/server_backend/server_reforger_maintenance_jobs.json) uses a disabled recurring reload job and omits Arma-only announcement and rotation options. Jobs run only while the game-space server owner is running. A verified archive protects the included local save/profile files, but does not establish that the engine can resume or preserve a playthrough.

## Reforger session intent

`server session list|plan|apply <instance-id>` inspects recognized local save metadata and reviews the next launch's session selection. Plan choices are `configured`, `latest`, `selected` (requires `--save-id`) or `new`; apply requires the reviewed `--expected-generation`, `--expected-plan-fingerprint` and `--yes`, and refuses while writers are active. `--dry-run` validates the plan without changing intent or contacting an owner. This changes only saved next-launch intent: it does not start or stop the server, request a save, convert data, or guarantee that the engine restores the selected playthrough. External persistence providers are outside local backup coverage unless the backup report explicitly lists them as covered.

## Reforger scenario end behavior

The Reforger draft `settings.scenario_end` is a tagged enum. If omitted, it defaults to shutdown, preserving old definitions and runtime manifests. `shutdown` accepts `delay_seconds` from 0 through 3600 and maps to `-autoreload <N> -autoshutdown`. `reload` accepts the same delay range and maps to `-autoreload <N>`, allowing the engine to reload its current scenario in-process. `disabled` maps to `-autoreload disabled -autoshutdown`.

```json
{"mode":"shutdown","delay_seconds":0}
{"mode":"reload","delay_seconds":30}
{"mode":"disabled"}
```

Committed native runtime manifests freeze this policy in optional `reforger_scenario_end`; older manifests without it behave as shutdown. Natural engine exit does not cause Foxy to restart the process or adopt child processes. Existing foreground containment retains and cleans its descendants. Local backup support does not guarantee engine save durability or successful playthrough restoration.

The [scenario-end examples](../examples/json/server_backend/reforger_scenario_end.json) illustrate only serialized policy shapes and grant no runtime authority.
