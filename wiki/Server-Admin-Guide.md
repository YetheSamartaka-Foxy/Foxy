# Server Admin Guide

This guide covers everything a server administrator needs to set up and maintain Foxy-compatible repositories for Arma 3 and Arma Reforger communities, and for custom games, with `foxy-server-backend-cli`.

The main Foxy binary also has a Server mode preview. `foxy server context --game-space <id> --json` inspects an existing game space's resolved paths and support status without changing files or opening its database. `foxy server content` records addon folders and repository profiles in the game space's `server/content-sources.json`. It does not open the sync database and it does not download anything. `already-present` means the server already has those addon files, including when this machine is the repository host. `download` means a later acquisition may fetch that repository into a different folder. Using `download` for a folder that is the repository's own local path is refused.

Server mode now authenticates a foreground metadata owner through Windows named pipes or Linux Unix sockets. The owner holds the game-space database claim without opening Turso; attached desktop and CLI processes never open that database. Entering Server mode first drains client workers and saves, stops the watcher and closes the client engine. Client repository schedules and database-backed tools remain paused. Returning to Client stops and joins a desktop-owned owner before reacquiring the client claim; a separate terminal-owned owner must be stopped in its terminal first. A failed transition keeps the prior scope and exposes retry without sharing Turso.

Run `foxy server supervise --game-space <id>` in a terminal to host an owner until Ctrl+C. `foxy server owner --game-space <id> --json` authenticates and reports its exact context, space and generation. Owner inspection is read-only; `supervise` claims only its captured game space and does not change remembered selection. Competing owners return busy code6. Both reject `--wipe-db`; `supervise --dry-run` refuses without starting an owner. Closing a desktop does not stop an owner started in another terminal. Desktop-owned child lifetime is tied to the parent connection and orderly operation drain; detached ownership remains unavailable.

In this guide, the exclusive ownership requirement for an actual offline metadata save means either a retained, serialized metadata permit from the authenticated owner or a temporary exclusive game-space claim when no owner exists. The same core revision, fingerprint, cancellation and atomic-publication checks run in both cases. A protected OS operation lock remains held until the client's retained worker finishes, including after a lost connection; owner shutdown and replacement wait for it. These permits never grant Turso, engine launch, activation or recovery authority. The space-management protocol uses the reserved `space_catalog` identity; individual instance plans still validate their exact saved identities and revisions. The authenticated wire and replay/event primitives remain bounded; no public network listener is created.

The owner capability is an ephemeral connection secret in a private same-user runtime directory, protected by an explicit Windows owner/DACL or Linux0700/0600 permissions and kernel peer credentials. It is absent from argv, normal JSON reports, logs and exported configuration. Do not copy the runtime discovery file into a repository or support bundle. Under the actual operation and database locks, a replacement may retire only an exact valid crashed metadata-owner record and its verified owned Linux socket; malformed, foreign or unknown data remains blocked and preserved. This proves metadata-owner replacement only, not game-process or save recovery.

```text
foxy server content add-folder --game-space arma3 --id hosted --path <addon-folder>
foxy server content add-profile --game-space arma3 --name main --folder hosted --url https://repo.example.invalid/main --local-path <same-or-other-path> --supply already-present --addon @mod_01
foxy server content list --game-space arma3 --json
```

Use `--supply download` only when `<addon-folder>` is not already the repository copy. `--dry-run` prints the decision and does not write the file. `foxy server instance start <id>` refuses execution unless engine layout, deployment admission and owner lifecycle acceptance gates pass. Native Arma activation/recovery passed Windows and WSL acceptance for the exact tested layout, but activation never starts a process. Use `launch-plan` or `start --dry-run` to inspect intent without spawning. Native Steam session preparation, Arma and Reforger dedicated depot installation, and Arma Workshop acquisition are available through the commands below. Omit `--game-space` to inspect the remembered selection. See [Client and Server modes](User-Guide.md#client-and-server-modes).

### Native Steam sessions

Server mode can prepare a native Steam session for a supported game space. Every operation opens a real Steam CM connection and closes it before returning. Login obtains a refresh token and then requires a successful native CM logon before saving it. This prepares credentials for future provider work; it does not install a game, request depot files, download Workshop content or retain a live connection.

Use a local alias to name the protected session record. The default alias is `default`; it is not the Steam account name.

```text
foxy server steam status --game-space arma3 --account default --json
foxy server steam login --game-space arma3 --account default --credentials-stdin
foxy server steam resume --game-space arma3 --account default --json
foxy server steam anonymous --game-space arma3 --json
foxy server steam forget --game-space arma3 --account default --yes
```

`status` inspects local storage without contacting Steam. Its `prepared` result reports whether a protected record is present; it does not prove that Steam will still accept the token. `resume` tries the stored token without asking for a password or falling back to attended login. If Steam rejects it, Foxy preserves the record and reports that attended authentication is required. `login` requires credentials from either the private stdin prompt or a protected `--credentials-file`. The stdin form accepts one bounded JSON line with `account_name` and `password` fields. A credentials file may contain that JSON object or literal `FOXY_STEAMCMD_USERNAME` and `FOXY_STEAMCMD_PASSWORD` entries. Keep the file private to the current user. Passwords, refresh tokens and Guard codes are never accepted as command-line arguments, printed in JSON results or stored as plain logs. Passwords and Guard codes are not saved.

Guard email or authenticator codes are requested through private input and may be retried after a rejected or expired code. Mobile confirmation must be approved in the Steam mobile app. `--json` keeps result data on stdout and directs phase prompts to stderr. Global `--dry-run` only inspects the selected alias: it performs no network request, does not read a credentials file or stdin, and does not write. `forget` requires `--yes` except during dry-run. These commands reject `--wipe-db` and do not open the game-space database.

Session records live at `games/<space_id>/server/steam-auth/<alias>.json` beneath the config root, with a per-store `.lock`. The record is scoped to its config root, game and space. Different spellings of the same config folder (slashes, letter case, `\\?\` prefixes) share the same session. Windows protects it with current-user DPAPI and a private ACL. Linux stores the token as plaintext protected by an owner-only directory and file permissions; this is permission protection, not encryption against another process running as the same user. `anonymous` does not modify a saved account token. These records are separate from the generic SteamCMD credential cache; using or forgetting a native session does not log out, clear or reset that cache.

The desktop Server mode has a **Native Steam sessions** panel with masked credential and Guard-code fields, alias inspection, attended login, token verification, anonymous connection and confirmed Forget. It uses a retained cancellable worker and reports connection phases. A completed login or verification only confirms that the native CM accepted the session at that time; it does not establish depot or Workshop access.


### Managed installation from Steam

On Windows and Linux x64, Arma 3 and Reforger instances can receive managed dedicated servers downloaded directly from Steam. Use a prepared native session when the depots require an account, or request `--anonymous`. Foxy never starts SteamCMD for this.

```text
foxy server installation --game-space arma3 install plan <instance-id> --directory <new-folder> --account default
foxy --yes server installation --game-space arma3 install apply <instance-id> --directory <new-folder> --account default --expected-plan-fingerprint <hash>
```

`plan` connects to Steam read-only. It reports the branch build, the depots selected for this platform with their manifest IDs and sizes, depots skipped for other platforms, languages or shared redistributables, whether the session is entitled to every depot, required and free disk space, and any interrupted Foxy staging folders for the same destination. It downloads and writes nothing. `--branch` selects another public branch; password branches are refused. `--anonymous` can review the build, but Arma 3 server depots require an account that owns the app, so the plan is blocked.

`apply` needs `--yes` and the exact plan fingerprint. It reviews Steam again and stops without writing if a different build was published. It downloads the depots into a locked sibling staging folder, verifies every file's size and SHA-1 against the depot manifests and requires the dedicated executable. It writes `foxy-installation.json` (app, branch, build, depot manifests and inventory hash), makes the Linux dedicated executable runnable, then publishes the folder with one rename. The new folder must be outside Foxy's data directory and separate from every existing dedicated installation; existing folders are never overwritten.

For Reforger, `--channel stable` selects dedicated app `1874900` and `--channel experimental` selects app `1890870`. The default for a fresh installation is Stable. Each app has its own Steam branches, so Experimental usually still uses `--branch public`. Select the channel in the desktop **New installation channel** control or pass it to both CLI review and apply. Keep each complete channel in its own installation folder; the verified receipt records channel and app. Arma 3 has no separate Experimental app selector.

```text
foxy server installation --game-space reforger install plan <instance-id> --directory <new-folder> --channel experimental --anonymous
foxy --yes server installation --game-space reforger install apply <instance-id> --directory <new-folder> --channel experimental --anonymous --expected-plan-fingerprint <hash>
```

A verified installation is separate from runtime compatibility. Activation and addon acquisition still require the exact accepted engine layout; a new or Experimental executable does not become supported merely because its Steam package verifies. Installation binding does not modify an existing committed runtime or restart a server.

Only a complete verified folder is recorded as managed and bound to the instance. A changed catalog/instance, binding failure or cancellation observed before binding leaves a published candidate `published_unbound` for explicit adoption. After the final binding begins, completion can win a late cancellation request. Before publication, failure or cancellation removes staging and leaves the destination absent. A later review lists unlocked interrupted staging for cleanup on apply. Global `--dry-run` reviews Steam and checks the fingerprint without downloads or writes. Replacing a bound installation also requires the old physical root's exclusive maintenance gate and no live/unresolved consumers.

### Managed installation maintenance

```text
foxy server installation --game-space arma3 check <instance-id> --account default
foxy server installation --game-space arma3 validate <instance-id>
foxy server installation --game-space arma3 update plan <instance-id> --directory <new-folder> --account default
foxy --yes server installation --game-space arma3 update apply <instance-id> --directory <new-folder> --account default --expected-plan-fingerprint <hash>
foxy server installation --game-space arma3 repair plan <instance-id> --directory <new-folder> --account default
foxy --yes server installation --game-space arma3 repair apply <instance-id> --directory <new-folder> --account default --expected-plan-fingerprint <hash>
```

`check` compares the recorded branch/build/depot manifests with Steam without downloading or changing files. It does not verify local bytes. `validate` works offline and re-hashes the complete installed file inventory, excluding only Foxy's root receipt. Missing, changed, linked or extra files and an invalid executable fail validation (exit 4); an invalid receipt or an adopted installation is refused. Validation observes current content and does not authorize mutation.

`update` and `repair` publish a new installation using the reviewed contract above. Both require a new folder; existing installations are never updated in place. Repair reacquires the complete build shown in the review, which can be newer than the installed build. Choose the branch explicitly (default `public`). The review shows the old and target build, changed depot selection and peers retained on the old installation. Apply acquires admission and exclusive maintenance for the previous physical root, refusing live or unresolved server/headless consumers and pending recovery across game spaces/configuration roots. Stop consumers through their owners and resolve recovery first; Foxy never stops or restarts them implicitly.

Check uses the installed receipt's app/channel. Update and Repair also keep that channel when `--channel` is omitted; specifying a different channel is refused before contacting Steam. Use a fresh Install into another folder for an explicit channel change. The desktop channel selector applies only to new installations; Check, Update and Repair retain the bound channel.

After full verification, only the selected instance switches to the new managed installation. Existing complete installations, unknown files, hand-maintained configurations and all other bindings remain at their previous roots. A failed or cancelled download leaves the old binding untouched. Cancellation observed before binding or failed/stale binding is reported as `published_unbound` after publication, retaining the complete candidate for explicit adoption. Completion can win a late cancellation after final binding begins. Retrying means reviewing again and applying the new fingerprint; a changed catalog, receipt or physical installation invalidates the previous review. Use Ctrl+C to cancel and wait for joined cleanup. These commands preserve `--json`, `--yes` and `--dry-run`, and do not open the game-space database.

### Workshop items for servers

Arma 3 server instances can use Steam Workshop items downloaded directly by Foxy with a prepared native Steam session. Foxy does not use SteamCMD or the desktop Steam client for this.

```text
foxy server workshop --game-space arma3 plan <item-id-or-link>... --account default
foxy --yes server workshop --game-space arma3 acquire <item-id-or-link>... --account default --expected-plan-fingerprint <hash>
foxy server workshop --game-space arma3 list
foxy server workshop --game-space arma3 verify <item-id>
```

`plan` asks Steam for each item read-only and reports whether it will be downloaded and pinned (`acquire`), moved to a newer revision (`update`), is already `current`, or is `unavailable` with a reason: not found or private, another game's item, banned, a collection, legacy single-file storage, or no content. It also checks whether the session may download Arma 3 Workshop content; anonymous sessions can review items but cannot download them. Nothing is downloaded or written.

`acquire` needs `--yes` and the plan fingerprint. Each item downloads into a new locked staging folder under `games/<space_id>/server/workshop/items/<item-id>/`. Every file must match the Steam manifest exactly before Foxy publishes the revision as `<manifest-id>/@workshop_<item-id>` with an `inventory.json`. Only then does the item's pin move: the content profile `workshop-<item-id>` (already-present, no URL) points at the new revision, so content bindings, staging and key review use it like any other local source. Earlier revisions are kept. An item whose revision changed on Steam after the review fails without changes. Items succeed or fail independently; the overall result is `complete` only when every item that needed a download was pinned, otherwise `partial`, `failed` or `cancelled` with exit 4. Cancelling removes the current staging folder; already finished items stay pinned.

`verify` re-hashes the pinned revision against its inventory without writing. If it reports a mismatch, run `plan --repair` and `acquire --repair` for that item to download the same pinned revision again and replace the damaged copy.

### Preview content for an instance

After creating an instance, connect registered profiles to a read-only content preview. A binding uses the profile's exact `name` as `source_id`, a role (`required`, `optional`, or `server_only`), and an integer `order`. Bindings are independent of client addon checkboxes. The preview itself does not save them; use the separate reviewed binding save below to retain a selection.

```json
[
  { "source_id": "RepoAlpha", "role": "required", "order": 0 },
  { "source_id": "RepoBeta", "role": "server_only", "order": 1 }
]
```

Save this array as `bindings.json`, using names from `server content list`, then run:

```text
foxy server content list --game-space arma3 --json
foxy server content plan <instance-id> --game-space arma3 --bindings bindings.json
foxy server content plan <instance-id> --game-space arma3 --bindings bindings.json --expected-revision 1 --expected-sources-fingerprint <fingerprint> --json
```

Use `--game-space reforger` for a Reforger instance. The optional revision and raw registry fingerprint refuse a preview if reviewed inputs changed. The report captures instance/catalog revisions and the registry fingerprint, preserves each repository's `(URL, local path)` identity, orders bindings, and proposes source and destination paths. Its own fingerprint identifies the captured intent and observations; it is not a checksum proving addon content integrity.

Planning accepts at most 256 selected profiles, 2048 expanded paths and 4 MiB of expanded source-path data. Saved binding documents are limited to 4 MiB; oversized proposals are refused during review before a save is offered.

`already-present` never schedules a download. Missing local directories remain blockers instead of silently switching to acquisition. Omit `--url` when registering an already-present local-only source; download profiles require a valid HTTP/HTTPS repository URL and a distinct target folder. URLs cannot include credentials, query strings or fragments. A directory that exists is reported as **present, not verified**. A download profile remains **acquisition required**, even if its folder already contains bytes. Linked/unsafe paths, source/destination overlaps, adopted-installation overlaps, existing destinations and collisions between selected profiles are explicit blockers.

Planning creates no folders, logs, locks, database, downloads or copies and can run beside the GUI that owns the space. A completed report returns exit 0 even with blockers; automation checks `data.plan.blockers` and `data.plan.deployment_ready`, which remains false. Invalid/stale requests return 2, missing instances return 3 and unreadable/corrupt storage returns 4. Global `--dry-run` has the same read-only behavior. JSON/TOON retain the normal envelope, and all content commands reject `--wipe-db` before bootstrap.

Actual folder/profile registration saves now require the captured space's exclusive claim, reject stale registry bytes and return code 6 beside another owner. They do not open Turso or remember another space. Registry loading validates the entire saved document, including duplicate names/identities and safe addon components, with a 4 MiB limit. Invalid documents are preserved for review, never repaired implicitly. Schema v1 remains unchanged; examples are under `examples/json/appdata/games/{arma3,reforger}/server/content-sources.json` and `examples/json/server_backend/content_plan_bindings.json`.

Desktop **Server content planning** uses the same core preview on a background worker. Save or discard draft edits, refresh registered profiles, select only the profiles to preview, choose their roles and order with the named Move up/down buttons, then **Preview selected content**. Source/destination paths and textual blockers remain visible in the scrolling form. Refreshing sources or changing instances clears the selection and old preview; editing a selection invalidates its preview. No acquisition, deployment apply, engine launch or content cleanup is enabled by this report.

### Save reviewed content bindings

Each instance can retain reviewed source identities, roles and order in `server/instances/<instance-id>/content-bindings.json`. The schema-v1 document embeds the selected repository profiles and source folders, the reviewed instance/catalog revisions and raw fingerprints, and a monotonically increasing binding generation. This records operator intent. It does not verify addon bytes, select an active deployment or enable launch.

```text
foxy server content bindings show <instance-id> --game-space arma3 --json
foxy server content bindings plan <instance-id> --game-space arma3 --bindings bindings.json --json
foxy server content bindings save <instance-id> --game-space arma3 --bindings bindings.json --expected-plan-fingerprint <data.plan.fingerprint> --dry-run --json
foxy server content bindings save <instance-id> --game-space arma3 --bindings bindings.json --expected-plan-fingerprint <data.plan.fingerprint> --json
```

Read the full `data.plan.document` and `data.plan.preview` before saving, then pass the returned binding-plan fingerprint unchanged. It differs from the directory preview fingerprint and from the source-registry fingerprint. `save` reconstructs the reviewed plan and rechecks it under the captured space's exclusive claim before atomically replacing only the binding file. Changed binding bytes, source registrations, catalog bytes, revisions or selected inputs refuse the save with exit 2 and preserve the previous document. Another owner returns 6; corrupt/future binding documents remain untouched and return 4. Plan/show/dry-run can run beside an owner without creating locks, logs, database or instance folders. Real saves can create the ordinary space-claim files, binding parent folders and owned binding document; no sync database is opened. No `--yes` is required for this configuration save.

`show` returns `data.state.document`, the raw binding `fingerprint`, `review_current` and textual `issues`. A missing document returns null document/fingerprint with no writes. Changes to saved sources or the catalog make an existing review stale; the embedded old identities remain inspectable. Reload and review their current replacements explicitly. Saving does not update the catalog, source registry, client selections or any other instance. Cloning a draft starts without saved bindings; removing a draft does not delete its retained binding file. Filesystem-level content changes remain unverified, regardless of review status.

An empty input array explicitly clears the selection by saving a new generation, without deleting content. Runtime preview blockers can remain present while saving this intent. Future schema versions and invalid documents are refused without implicit migration or repair. Empty example documents for both games are under `examples/json/appdata/games/<game>/server/instances/<instance-id>/content-bindings.json`; their synthetic zero catalog fingerprint deliberately requires a fresh review.

In the desktop, choose **Reload saved content bindings** to load both the saved selection and current sources. Missing profiles and stale reviews stay visible. Adjust selection, roles and order, then **Preview selected content** and inspect the report before **Save reviewed content bindings**. A preview made after only **Refresh server content profiles** cannot save until the saved binding baseline is reloaded. Selection edits remain transient until Save; reload/instance changes discard those edits. An empty selection is labelled before review/save. Workers preserve close/switch ownership, and stale input errors require reload/review instead of overwriting the previous file.

### Verified local content stages

After saving and reviewing content bindings, plan a local stage from those exact saved bindings and already-present bytes. `stage plan` hashes files and returns a manifest fingerprint, stage ID and destination without writes or a game-space claim. `stage apply` rebuilds the plan and requires the returned plan fingerprint, so changed files or bindings make the request stale. Apply `--dry-run` performs the same byte and fingerprint checks without files or ownership claims. A real apply claims only the captured game space, pins that claim through publication, and never opens the sync database.

```text
foxy server content bindings show <instance-id> --game-space arma3 --json
foxy server content stage plan <instance-id> --game-space arma3 --expected-bindings-fingerprint <binding-fingerprint> --json
foxy server content stage apply <instance-id> --game-space arma3 --expected-bindings-fingerprint <binding-fingerprint> --expected-plan-fingerprint <data.plan.fingerprint> --json
foxy --dry-run server content stage apply <instance-id> --game-space arma3 --expected-bindings-fingerprint <binding-fingerprint> --expected-plan-fingerprint <data.plan.fingerprint> --json
foxy server content stage list <instance-id> --game-space arma3 --json
foxy server content stage verify <instance-id> <stage-id> --game-space arma3 --json
```

Stages live at `server/instances/<id>/content-stages/stage_<manifest-blake3>/`. Each contains `manifest.json` with the schema-v1 saved bindings, ordered source roots, exact directory/file inventory and BLAKE3 hashes for files. Empty directories and empty files are retained. A repeat apply reuses a matching stage only after checking its manifest and exact file inventory and hashes. Verification reads the retained manifest and files; it does not depend on current source files. Listing reports stage IDs and unknown entries but never verifies integrity. Corrupt stages and unrelated existing destination data are preserved and cause reuse to fail; Foxy never overwrites or cleans unowned stage data.

Ctrl+C requests cooperative cancellation during plan/apply/verify and waits for the worker and temporary-stage cleanup before returning operation-failed (exit 4). Cancellation leaves previously published stages intact. Plan, list, and verify do not claim the game space or open the sync database; actual apply alone needs the captured space claim. Staging never downloads content, activates a stage, changes an active deployment, or reports engine readiness. Native Windows and Linux builds advertise this capability for Arma 3 and Reforger. D05 acceptance exercises Arma use of local retained stages, but does not establish provider acquisition, general engine compatibility or server readiness.

### Acquire repository content

Download profiles can be acquired into a separate retained content stage after saving and reviewing content bindings. The review and confirmed apply are:

```text
foxy server content acquire plan <instance-id> --game-space arma3 --seed-stage <stage-id> --json
foxy server content acquire apply <instance-id> --game-space arma3 --seed-stage <stage-id> --expected-plan-fingerprint <data.plan.fingerprint> --yes --json
foxy --dry-run server content acquire apply <instance-id> --game-space arma3 --seed-stage <stage-id> --expected-plan-fingerprint <data.plan.fingerprint> --json
```

`--seed-stage` is optional. Plan validates local saved intent and any selected seed offline; it performs no HTTP requests, writes, downloads or database opens. Apply resolves the current repository publication when it starts, so the preview fingerprint does not pin remote content. Each profile uses a contained sync child with private scratch state and its own scratch database. A seed is copied into that scratch only when the saved stage records the same original repository URL and local path. The normal sync path handles refresh, patch-first downloads with full-file fallback, and final verification. Foxy publishes one stage only after every selected profile has completed and passed verification. Original source folders and repository identities are preserved. The resulting stage is retained and inactive: this operation does not activate a deployment, change client addon enablement, or write back to source folders. It does not use native game providers, Steam depots or Workshop acquisition.

Inspect `data.outcome.status` and its per-profile `items` in JSON. A completed CLI envelope can have `ok: true` while acquisition outcome is failed or cancelled; those outcomes exit 4 and must not be treated as successful. Downloads/database witness fields may be `null` when a helper failure prevented Foxy from observing what happened. Temporary cleanup errors are explicit, and retained or unknown scratch data is not pruned automatically. The live client database is never opened.

### Read-only deployment composition review

Review the persisted active configuration alongside one exact retained content stage:

```text
foxy server deploy plan <id> --stage <stage-id> --expected-revision <n> --expected-configuration-generation <n> --game-space arma3 --json
foxy --dry-run server deploy plan <id> --stage <stage-id> --game-space arma3 --json
```

`--stage` is optional. If omitted, Foxy reports a missing-stage blocker and does not choose the newest stage. Configuration evidence comes from the persisted active selection, not a revision highlighted while browsing history. Expected revision and generation flags reject stale review inputs. The deterministic review fingerprint identifies the report only; there is no apply or activation authority. The command and its `--dry-run` form perform the same read-only plan, reject `--wipe-db`, and can run while another Foxy owns the game-space database. They do not claim or open it, initialize logs, write files, access the network, or hash the adopted executable.

Completed composition reports exit `0` even when blockers remain. Stale expected values exit `2`, a missing instance exits `3`, and corrupt catalog, selection or binding-store state and other storage failures exit `4`. A corrupt selected archive is returned as a completed integrity blocker, preserving the rest of the report. The report keeps integrity separate from current intent: an intact configuration can differ from the current saved draft, and an intact content archive can carry bindings that differ from the current saved selection.

For Arma 3, `.bikey` files are inventory candidates only. Their presence or matching bytes do not establish publisher trust or engine placement; same-name byte conflicts and truncated candidate details remain explicit. Configured mission templates are matched to staged `.pbo` candidates under `mpmissions`; missing or conflicting candidates remain blockers. Reforger addon versions, dependencies and scenario-provider provenance are not resolved by local file hashes. Installation consumer inventory distinguishes complete/unknown inspection and registered-consumer count, but registration does not prove a process is live, and an empty inventory does not prove it stopped; liveness and exclusive maintenance authority remain false. Exact native-tested Arma executable layout compatibility is reported separately by installation inspection on Windows and Linux. Composition still grants no installation verification, publisher trust, runtime materialization, activation, recovery or detached ownership. Native engine layout evidence does not complete those deployment gates. The report never marks deployment ready or starts a process.

The desktop review uses the same core report. It is available only when the selected game capability allows it and the saved draft is clean. It uses the currently selected stage ID explicitly. Worker progress and Cancel remain visible while the editor is busy; cancellation joins the worker and leaves retained archives untouched. Reports are invalidated by relevant draft, stage, configuration, binding, source, catalog, or instance changes. Unloaded optional UI panels do not invalidate the report; persisted active selection and core-captured source/binding evidence remain authoritative.

## Server instance drafts

The main `foxy` binary manages versioned server drafts for Arma 3 and Arma Reforger. The same list/create/inspect/edit/clone/remove flow applies to both games, with separate typed game settings. `server capabilities [--game-space <id>]` reports draft, installation, deployment and lifecycle support independently. Lifecycle is available only for the accepted native Arma runtime described below. Other game modules reject draft operations before writes.

In the desktop app, select **Settings > Application > Server mode (WIP)**. **Server instances** provides Create, typed configuration fields, Save/Discard, Rename, Clone, Remove and Reload. These use the same validation, revisions and store as the CLI, with serialized metadata permits from the foreground owner. Saves run on captured-space workers; space and mode switching wait for completion, dirty edits block switching, and closing dirty forms requires an explicit discard decision. Failures preserve entered edits. See [Client and Server modes](User-Guide.md#client-and-server-modes) for navigation and keyboard behavior. CLI editing can attach to the same Server owner; read-only commands also remain available beside a client GUI.

Select an existing game space with `--game-space <id>` on each server command, or omit it to use the remembered space. The selector never updates `games.json`. Create spaces first through the normal `foxy game create` command. Server draft commands require a valid existing registry and never migrate or repair it. They operate independently of the saved application mode.

```text
foxy server capabilities --game-space arma3 --json
foxy server instance list --game-space arma3 --json
foxy --dry-run server instance add --game-space arma3 --name MainServer
foxy server instance add --game-space arma3 --name MainServer --json
foxy server instance show server_00000000000000000000000000000001 --game-space arma3 --json
foxy server instance rename server_00000000000000000000000000000001 --game-space arma3 --name TrainingServer --expected-revision 1
foxy server instance clone server_00000000000000000000000000000001 --game-space arma3 --name SecondServer --expected-revision 2
foxy server instance validate --game-space arma3 --file draft.json
foxy server instance replace --game-space arma3 --file draft.json --expected-revision 2
foxy --yes server instance remove server_00000000000000000000000000000001 --game-space arma3 --expected-revision 3
```

Use the actual generated `server_...` ID from Add or List; the ID above is a placeholder. Commands accept IDs, so duplicate display names are safe. Rename changes the display name only; it preserves the engine hostname/name. Clone copies typed settings under a new ID and revision 1, preserving the original network settings; choose distinct ports before any future deployment.

For a configuration edit, save **only `data.instance`** from the JSON Show result as a UTF-8 file, edit its typed `definition.settings`, then Validate and Replace it. Human Show output also includes the complete typed document. Preserve its ID, space, schema version and current revision. Replace requires the matching `--expected-revision` and increments the stored instance revision; Add, Clone, Rename, Replace and Remove also increment the catalog revision. Stale edits fail with code `2`, and missing IDs fail with code `3`. Reload Show before retrying. The same workflow works with `--game-space reforger`; an Arma 3 payload cannot be saved in a Reforger space.

Schema v2 contains an ordered catalog in `games/<space_id>/server/servers.json`, written with a synchronized temporary file and atomic replacement under the existing game-space process claim. It survives a sync database rebuild. Unknown fields, unsupported versions, duplicate IDs, dangling installation bindings and corrupt records block saves. Strict v1 projection and backup migration are described below. Storage directories and catalog/claim files must be ordinary files/directories without symlinks or Windows reparse points.

Inspection (List, Show, Capabilities and Validate) and global `--dry-run` do not create files, logs, locks or directories. Actual saves require exclusive ownership through the local claim or authenticated metadata permit described above. They return code `6` when a client GUI owns the space or another metadata operation is running, and never open `database.db`. Removal requires `--yes` except during a dry run and removes only the draft record. It never deletes game files. All draft commands reject `--wipe-db`. JSON/TOON retain Foxy's normal single-result envelope.

The typed subset includes Arma 3 hostname, port, capacity, persistence/auto-init, BattlEye, signature verification and mission templates/difficulties; Reforger name, bind/query ports, capacity, visibility, optional scenario resource, cross-platform and mods-required intent, view/grass/network distances, third person, fast validation, BattlEye, and addon GUID/version/required intent. New Arma 3 drafts enable BattlEye and v2 signature verification. New Reforger drafts are unlisted and may omit a scenario. Reforger `required`, cross-platform, mods-required and game-property booleans preserve unset, false and true as distinct intent. Unknown server.json objects are kept and rendered again. These are Foxy draft documents. Validation checks structure and supported policy; it does not prove acquired content, mission/scenario availability, ports or launch readiness. Do not put credential values into drafts. Reforger import removes password, secret and token fields instead of storing them.

### Existing installation adoption and deployment preflight

Schema v2 adds adopted installation records and optional instance bindings in the same catalog. For either supported game, create a draft first, then inspect and adopt an existing native installation:

```text
foxy server installation inspect --game-space arma3 --directory <existing-server-directory>
foxy --dry-run server instance adopt <instance-id> --game-space arma3 --directory <existing-server-directory> --expected-revision 1
foxy server instance adopt <instance-id> --game-space arma3 --directory <existing-server-directory> --expected-revision 1
foxy server installation list --game-space arma3
foxy server installation consumers <installation-id> --game-space arma3 --json
foxy server instance preflight <instance-id> --game-space arma3
foxy server instance bind <instance-id> --game-space arma3 --installation <installation-id> --expected-revision 2
foxy server instance bind <instance-id> --game-space arma3 --clear --expected-revision 3
```

Use `--game-space reforger` for a Reforger space. Windows x64 inspection expects `arma3server_x64.exe` or `ArmaReforgerServer.exe`; Linux x64 expects `arma3server_x64` or `ArmaReforgerServer`, with execute permission. Foxy reads PE/ELF architecture headers and hashes the executable, without invoking it or Steam. These checks provide structural identity, not vendor authenticity, semantic engine version/channel or package completeness. Roots must be ordinary directories separate from Foxy's data directory; links/reparse points and wrong native formats are rejected. Files larger than 1 GiB are refused by this initial executable inspector.

Installation inspection also shows local build observations. On Windows, Foxy reads the executable's product-version resource without executing it. For Arma installations directly under a Steam library's `steamapps/common`, it reads a bounded matching app manifest only when its canonical installation directory agrees exactly. The desktop and CLI `data.build_observation` report the observed version, Steam app/build and branch. An explicit empty beta key means `public`; missing or conflicting branch metadata stays unknown. Missing resources or manifests do not invent a version or channel. These observations remain separate from saved executable fingerprints and do not verify package completeness, vendor authenticity, native directory isolation or permission to launch. Example: [build observation](../examples/json/server_backend/server_installation_build_observation.json).

`data.build_observation.tested_engine_layout` separately identifies an exact match to Foxy's compiled native acceptance matrix. The Windows and Linux x64 Arma dedicated executables tested at runtime version `2.22.154089` match by game, platform, byte length and BLAKE3. A product-version string or Steam manifest alone never selects a layout. Unknown or changed binaries, including older versions, remain unsupported until tested. Linux can recognize the exact tested binary while its executable-version metadata remains unknown.

The tested layout patterns use instance-specific root-relative `-mpmissions` and additive `-keysFolder` directories, or physically separate runtime copies with default mission/key directories. Earlier native two-instance layout checks covered separate configuration, missions with the same filename, server-only content, profiles, durable profile and mission namespaces across restart, and engine-advertised own/peer key paths with the base `a3` key retained. These layout findings do not authorize copying additional keys or establish regular-client signature admission, publisher trust or complete saved-game recovery. The isolated-copy fallback was tested on the same current build; it does not imply older-build support. Native Windows and WSL D05 acceptance passed for the exact tested Arma layout and covered generation switching, rollback, persistence and owner interruption/recovery. This does not claim a broad platform release or general engine readiness. Activation materializes a complete private runtime and per-instance mutable state without writing inside the adopted root. The separate owner-routed lifecycle controls use an exact reviewed runtime; their native acceptance scope is described below.

Adoption and binding recheck the saved draft revision, exact catalog fingerprint and executable evidence before saving. The catalog reuses an existing installation record for the same canonical root and matching evidence. Changed executable evidence requires a future reviewed installation update; Foxy does not overwrite its old observation. Replace cannot change a binding implicitly; use Bind/clear instead. Clone retains the binding. Remove deletes only the instance record and retains installation metadata and all external files. Adoption grants no acquisition, update, cleanup or uninstall authority. Desktop Server mode exposes these same operations through **Existing installation and deployment review**.

Preflight also reads the selected configuration archive and reports its generation, integrity and whether it matches the saved draft. An older intact configuration remains intact when a newer draft exists; this comparison does not deploy either version. Missing, unreadable or damaged configuration history is reported as a blocker. A completed preflight retains exit code 0 with `deployment_ready: false`; check the structured blockers rather than interpreting successful inspection as permission to launch.

`foxy server capabilities --game-space arma3 --json` includes `runtime_prerequisites`, also returned by instance preflight for both games. Windows has a tested foreground process primitive with creation-time job containment and forced cleanup on owner death. It requires an unelevated owner and never falls back to inherited administrator privileges. Linux has native fixture proofs for user/PID namespace containment, including detached descendants and abrupt owner death. It requires an ordinary user with no inherited capabilities, permitted unprivileged namespaces, `clone3`, pidfds and descriptor-closing APIs. A retained namespace init reaps descendants; its reported host PID is diagnostic and differs from the payload PID. Unavailable or denied containment fails closed. These primitives do not establish game, Steam/BattlEye, service or distribution compatibility. Native Arma lifecycle requires the separate reviewed runtime/owner path. Exact tested Arma layout and native runtime materialization passed D05 acceptance on Windows and WSL; automatic process orphan recovery remains a separate gate. Preflight reports installation leases as `not_acquired` and creates no coordination files.

Installation coordination primitives use two persistent lock files in the OS user's local data directory, under `Foxy/server-installation-locks/`, keyed by the installation directory's physical identity. This namespace is independent of `--config-dir` so separate roots and game spaces can coordinate the same installation. It contains no authoritative server definitions and is created only by lease/registration work, never by ordinary inspection. Do not remove or replace coordination files or directories while owners exist. Cross-account services and network filesystems are not supported by this coordination contract.

A foreground session synchronizes a strict consumer record in the physical namespace's `consumers/` before execution. It records owner, space, instance, operation and run identities, without PID, arguments, paths or secrets. Only witnessed cleanup of the entire owned tree retires its exact record before releasing the runtime lease. Uncertain spawn/cleanup and abrupt owner death conservatively leave records. Maintenance admission rejects every unresolved record, including malformed or unreadable metadata, even when the OS content lock has been released. Automatic orphan verification and clearance are still pending; deleting records or trusting a PID/process name cannot substitute for recovery. Forced termination is save-uncertain, and unknown Linux payload exit status remains unknown. These are process-crash barriers, not a power-loss durability guarantee.

`foxy server installation consumers <installation-id> --game-space arma3 --json` provides the same read-only `consumer_inspection` returned in a bound instance's preflight. `recorded_consumers` is an inventory, not proof of running servers. `cleanup_required` is false for a complete empty inventory, true for complete nonempty inventory and null when inspection is unknown. `liveness_verified`, `exclusive_maintenance_authorized` and `writes_performed` are always false. A complete inventory returns exit 0; an unknown completed inspection returns a report with `ok: true` and exit 4. Invalid IDs return 2 and missing adopted IDs return 3. Preflight retains its existing exit-0 blocker convention. Neither command claims a lease, opens the database, creates registry paths or repairs metadata. There is no clear/force/reconcile command.

Unfinished installation maintenance remains blocked after its owner exits. A released OS lock or an empty consumer inventory does not clear it. The reviewed activation recovery operation can resolve only a freshly fingerprinted transaction outcome under the foreground owner; this preview offers no clear/force shortcut.

The same `installation consumers` command also returns `recovery_inspection`, shared with `instance preflight` and desktop **Recovery and admission review**. `maintenance_status` is `clear`, `pending` or `unknown`; `clear` means no pending intent was found. The report includes registered consumers, receipt count, pending evidence and up to 16 historical evidence summaries, with `evidence_truncated` when more exist. Local journal, active-reference and resolution metadata are matched to the exact intent and receipt. Evidence belonging to another config root is labeled `foreign_context` and its local files are not inspected. Unreadable or inconsistent metadata stays visible with an error; incomplete inspection returns exit 4, while a completely inspected pending intent returns exit 0 with `pending` still explicit. Preflight adds recovery blockers and retains its exit-0 review convention.

This is a read-only metadata report. It does not hash prepared archives, acquire installation leases, clear recovery data or verify that a process stopped. Native layout, stopped-owner evidence, content trust and owner handoff remain unavailable or unverified. Activation and resolution availability describe this inspection path only; a separate owner-routed activation/recovery command verifies archives and consumes exact reviewed fingerprints. Neither an empty history nor matched evidence authorizes an engine process operation.

Keep the registry and its namespace directories in place while owners exist; external renaming or cleanup can split active locks. Coordination applies to cooperating owners using this persistent namespace.

Preflight is a read-only report, not an apply plan or readiness guarantee. `data.preflight` includes the saved instance/catalog revisions, installation observation, proposed paths, stable blocker codes, `deployment_ready: false` and `writes_performed: false`. A successfully generated report returns success even when it contains blockers; automation must examine readiness/blockers. Missing instances or unreadable catalogs remain normal command errors. Paths are distinct by space/instance identity, but are not engine launch arguments. Existing/unsafe destinations, executable drift, local saved port collisions and missing missions/scenarios are reported. Host-wide port availability, unknown engine versions, game-specific path semantics, package/content validation, lifetime leases and real two-instance isolation remain unverified. Arma mission paths in particular cannot be redirected into an adopted installation without proven engine support and explicit ownership. No directory or writability probe is created by inspection or preflight.

Reads and dry runs project strict v1 catalogs into v2 in memory without changing files. List reports `migration_pending`; desktop shows the same notice. The first successful owned catalog mutation saves the exact previous document as `server/servers.v1.<BLAKE3>.json` before atomically replacing `servers.json`. Instance identities/order/revisions/settings are preserved by migration, with empty installation records and unbound instances. Only the requested mutation advances revisions. An invalid backup destination or failed replacement leaves the original catalog intact. Do not edit or remove the backup while a save is in progress. Schema 0, future schemas and unknown fields are refused; there is no silent repair or downgrade. Current examples are `examples/json/appdata/games/{arma3,reforger}/server/servers.json`.

Capability flags `installation_adoption` and `deployment_preflight` identify this limited feature. Broad `installation` (acquisition), `deployment` (apply) and `lifecycle` remain false. CLI mutations still require exclusive ownership and may return database-busy code `6` beside a GUI holding that space; read-only commands and dry runs remain claim-free, write-free and DB-free. All of these commands reject `--wipe-db` and preserve `games.json` selection.

### Configuration previews, snapshots and rollback

Existing Arma vendor files can be imported with explicit managed/operator ownership. [Configuration import/export and portable packs](Server-Configuration-Import.md) covers protected password extraction, redacted diffs, unresolved paths, new instance identities and the matching desktop/CLI flows.

The [Arma mission library and ordered cycles](Arma-Server-Missions.md) share the desktop and `server mission` CLI workflow, including bounded metadata, source-preserving imports, pinned revision refresh, hash-verified direct acquisition, and staged/runtime availability checks.

`server instance render` generates configuration from the saved draft without writing. Arma previews contain the selected server/basic configuration files and Foxy's `launch-intent.json` for port, auto-init and file selection. Basic tuning stays empty when omitted. An enabled custom difficulty profile adds `<profile-name>.Arma3Profile`; mission difficulty and numeric parameters are preserved. See [managed Arma configuration](Arma-Server-Configuration.md) for all supported groups, omission behavior and examples. Reforger previews contain `server.json` with native `bindPort`, optional `a2s`, and typed `game` settings; addon `required` remains omitted, false or true. Optional absent scenarios stay absent with a warning. A draft can be rendered and archived before an installation or mission is available; its `launch_ready` remains false.

```text
foxy server instance render <instance-id> --game-space arma3
foxy server configuration plan <instance-id> --game-space arma3 --json
foxy --dry-run server configuration commit <instance-id> --game-space arma3 --expected-revision 1 --expected-generation 0 --expected-fingerprint <renderer-fingerprint>
foxy server configuration commit <instance-id> --game-space arma3 --expected-revision 1 --expected-generation 0 --expected-fingerprint <renderer-fingerprint>
foxy server configuration list <instance-id> --game-space arma3
foxy server configuration verify <instance-id> --game-space arma3 --revision <configuration-revision-id>
foxy server configuration rollback <instance-id> --game-space arma3 --revision <configuration-revision-id> --expected-generation 1 --yes
```

Read the expected revision/generation/fingerprint from Plan and review its files before Commit. A changed draft, selected installation observation or configuration selection rejects the reviewed operation. Commits write only `server/instances/<id>/configuration/`: completed immutable revisions under `revisions/config_<fingerprint>/`, a strict `manifest.json` binding the exact typed input and rendered files, and an atomically replaced `state.json` selecting a revision. Each file is synchronized before selection. An interrupted operation can leave an unselected complete archive; it does not replace the previous selection with partial files. This proves atomic visibility at this boundary, not full power-loss durability of a directory tree. Rendered contents are capped at 1 MiB; state/manifest reads retain the 4 MiB document bound.

Verify re-renders the archived typed input and checks exact file contents, names and manifest identity. Missing, modified, extra or linked files fail verification. A completed integrity report uses `ok: true` with `data.verification.valid`; invalid integrity returns exit code `4` while retaining the report. Missing records or malformed command input use the usual error envelope. Do not treat process success or an archive hash as vendor authenticity or runtime readiness.

Rollback validates the retained target and expected selection generation, then changes only `state.json`. It leaves the current draft, all archives, external installation, runtime and saves intact. It is a saved-configuration selection, not a running-engine or save-data rollback. CLI requires `--yes` except for dry-run; desktop shows a confirmation with the target and expected generation. Unknown/corrupt targets are refused. No cleanup deletes unselected revisions or unowned files automatically.

Desktop **Saved configuration revisions** offers Preview, Prepare commit, Commit reviewed snapshot, Refresh history, Verify and Review rollback. Save or discard draft edits first. All I/O uses the existing captured-space worker and close/switch protection. These capabilities are advertised separately as `configuration_preview`, `configuration_snapshots` and `configuration_rollback`; native activation is separately available for the exact tested Arma layout. Provider acquisition uses separate reviewed workflows, and engine readiness remains unverified; exact native Arma Start/Status/Stop/Restart use the foreground lifecycle workflow below.

### Save and verify offline deployment records

Composition plans are read-only and do not save a deployment. A record is an immutable manifest of verified archive identities plus an offline selected-record state. It does not copy archive files or activate, apply, or launch a deployment.

```text
foxy server deploy prepare <instance-id> --stage <stage-id> --expected-revision <n> --expected-configuration-generation <n> --expected-deployment-generation <n> --game-space arma3 --json
foxy server deploy commit <instance-id> --stage <stage-id> --expected-revision <n> --expected-configuration-generation <n> --expected-deployment-generation <n> --expected-plan-fingerprint <fingerprint> --game-space arma3 --json
foxy --dry-run server deploy commit <instance-id> --stage <stage-id> --expected-revision <n> --expected-configuration-generation <n> --expected-deployment-generation <n> --expected-plan-fingerprint <fingerprint> --game-space arma3 --json
foxy server deploy list <instance-id> --game-space arma3 --json
foxy server deploy verify <instance-id> <record-id> --game-space arma3 --json
```

`prepare` and `commit` accept `--stage`, `--expected-revision`, `--expected-configuration-generation`, and `--expected-deployment-generation`. Supply expected values from the saved instance, configuration, and deployment state to reject inputs that already changed before prepare. Prepare always captures the current inputs and selection; commit rechecks them even when the optional expected values were omitted. The configuration is the persisted active selection, not a history revision highlighted in the UI. The stage must be explicit; omission is a missing-stage blocker, never a request to use the latest archive. `prepare` returns the exact reviewed plan fingerprint and expected selection generation. `commit` recomputes that plan and requires `--expected-plan-fingerprint` to match before saving. The dry run performs this comparison without a claim or write.

The manifest is stored at `server/instances/<id>/deployments/records/<record-id>/manifest.json`. The separate `server/instances/<id>/deployments/state.json` stores schema version, game-space and instance identity, generation, selected record ID, and the record ID inventory. Records reference immutable configuration and content archives by their identifiers and BLAKE3 fingerprints; they do not copy those archives and there is no implicit archive cleanup. `list` reads the typed state inventory and explicitly does not verify integrity. `verify` checks the record manifest and both referenced archives by their retained bytes, independently of current sources or current selection.

Prepare, list, verify, and commit with global `--dry-run` are read-only, do not open Turso or claim the game space, and can run beside its database owner. Actual commit claims only the captured game space and still does not open the database. All deploy record operations refuse `--wipe-db`. Busy is exit `6`; invalid/stale review is `2`, missing instance or record is `3`, and corrupt stored state or other storage failure is `4`. A corrupt archive named by a completed composition review is reported as an integrity blocker; corrupt catalog, binding, or selection state fails the operation. Cancellation is cooperative and waits for cleanup of owned temporary files. If interrupted after publication, a complete unselected archive can remain; unknown files are never removed. Native Windows and WSL Arma activation acceptance exercised these local archives, but is not broad platform release evidence.

### Select an existing offline record

Selection is separate from `deploy commit`, which publishes a newly reviewed record and selects it. To inspect an older indexed record and select it later:

```text
foxy server deploy select-plan <instance-id> <record-id> --expected-deployment-generation <n> --game-space arma3 --json
foxy server deploy select <instance-id> <record-id> --expected-deployment-generation <n> --expected-plan-fingerprint <fingerprint> --game-space arma3 --yes --json
foxy --dry-run server deploy select <instance-id> <record-id> --expected-deployment-generation <n> --expected-plan-fingerprint <fingerprint> --game-space arma3 --json
```

`select-plan` requires an exact record ID present in the saved index and verifies the record plus both referenced archives. It returns the previous and target IDs, expected generation, raw selection-state fingerprint, and a review fingerprint. `select` reruns that verification and requires the same `--expected-deployment-generation` and `--expected-plan-fingerprint`. The raw state and review token guard against changed inventory, selection, target bytes, or plan. `--yes` is required for every non-dry-run `select`, including a no-op; `--dry-run` performs the identical review without `--yes`, a space claim, or writes.

The operation changes only the existing `deployments/state.json` offline selection index. It creates, deletes, copies, or garbage-collects no archives. A no-op selection leaves the generation unchanged. Selection is independent of the current draft, source registry, configuration selection and content bindings, so unsaved draft edits remain intact in the desktop UI. It never activates a runtime deployment, starts a process, or implies recovery/readiness. The plan and dry-run, like list and verify, are database-free, write-free and safe beside a busy owner. Only actual select claims the captured space; none of these commands opens Turso. All commands refuse `--wipe-db`. Invalid/stale/required-yes errors exit `2`, missing instance or unindexed target exits `3`, corrupt/future state or storage errors exit `4`, and database busy exits `6`.

### Inspect consumers, pin records and reconcile storage

`server content consumers` reports registered source profiles and client repository instances for the active configuration root, profiles in its other game spaces, physical folder sharing, and persistent installation consumer registrations. Repository identity is `(remote_url, local_path)`; matching URLs alone do not establish a consumer. Source discovery does not inspect other configuration roots, which are reported as unknown. Installation registrations use the physical installation's shared coordination key, so the report can include consumers registered by another root. These are persistent registrations, not proof that a process is currently live or authorization to retire a consumer. Incomplete inspection and diagnostics remain explicit.

```text
foxy server content consumers --game-space arma3 --json
foxy server deploy pins <instance-id> --game-space arma3 --json
foxy server deploy pin <instance-id> <record-id> --expected-pin-generation <n> --game-space arma3 --yes --json
foxy server deploy unpin <instance-id> <record-id> --expected-pin-generation <n> --game-space arma3 --yes --json
foxy server deploy reconcile plan <instance-id> --game-space arma3 --json
foxy server deploy reconcile apply <instance-id> --expected-plan-fingerprint <fingerprint> --game-space arma3 --yes --json
```

Pin generation starts at zero. Pin and unpin require the current generation; pinning protects that record and its referenced artifacts from reconciliation. Unpinning only removes the pin. Reconciliation plans verify owned stages, preparations and runtimes, list retained reasons, candidate byte counts and content fingerprints, and return a fingerprint over the complete review. Unknown entries are preserved. Interrupted quarantine leftovers are candidates only when their receipt verifies the recorded entries and remaining content; unrecognized or unverifiable leftovers stay untouched. Plans also block unresolved activation recovery and other unsafe conditions. Apply recomputes the plan and requires its exact fingerprint; a real apply requires `--yes`. It claims the captured game space and checks persistent installation consumers before removal. Sources, records, configuration archives, runtime state and other instances are kept. Plan and dry-run do not open Turso or write; actual pin changes and reconciliation do not open Turso either. All commands reject `--wipe-db`.

Share content through ordinary source folders. Private stages, preparations and runtimes are instance-owned copies eligible for reclamation; do not register private copies as content sources. References from other configuration roots cannot be discovered here. Pin the record in its owning root when retaining a deployment for another workflow.

The desktop **Content consumers and storage reconciliation** panel is available on native Windows and Linux for supported server games. Refresh the report to inspect same-root sharing and cross-root installation registrations. Refresh pins, select an indexed record, then pin or unpin it. Review a reconciliation plan and its full candidate fingerprints before opening the existing confirmation dialog. The dialog is required before applying. The panel preserves unknown and unverified data and shows incomplete inspection and blockers.

### Prepare an offline runtime copy

`server deploy runtime plan <instance-id> --expected-deployment-generation <n>` reviews the currently selected indexed record and returns its exact plan fingerprint, immutable preparation ID, destination, manifest inventory and total bytes. The record's pinned configuration and content archives must verify. The operation uses the historical record and saved selection; it does not compare against today's draft, content source registry, configuration selection or bindings. The desktop panel follows the same contract and permits unsaved draft edits while retaining the selected record review.

`server deploy runtime build <instance-id> --expected-deployment-generation <n> --expected-plan-fingerprint <fingerprint>` recomputes and consumes the exact plan. It copies the full configuration archive and content stage into `server/instances/<id>/deployments/prepared/prepared_<hash>/`, with `configuration/`, `content/` and root `manifest.json`. The strict schema-1/layout-1 manifest embeds the pinned record plus a sorted per-file hash/byte inventory. Build uses bounded copies, verifies the completed copy and publishes without replacing an existing path. A matching existing generation is reused only after full verification. Cancellation or failure cleans only its owned temporary directory; it never removes completed or unrecognized entries. `--dry-run` performs the proof without a claim or file writes. A real build claims the captured game space but never opens Turso and does not require `--yes` because it only adds an offline immutable copy.

`server deploy runtime list <instance-id>` reports candidate IDs and unrecognized entry count; the listing is not an integrity check. `server deploy runtime verify <instance-id> <preparation-id>` checks the copied manifest and bytes independently of the original archives and current source/configuration selection. Both are read-only and may run beside a busy DB owner, while verification still requires a valid current context and existing instance. Invalid/stale inputs use code `2`, a missing instance or preparation ID uses code `3`, corrupt/future data and storage errors use code `4`, and another owner remains code `6`. JSON uses the normal stable command envelope. All commands reject `--wipe-db`.

Prepared generations are offline storage, not an engine runtime layout. None of these commands changes the selected record, active deployment, runtime files, process state, installation lease, consumer/provider registration or other generations. They do not imply engine readiness. No power-loss directory durability or protection against a hostile process running as the same user is claimed. Native activation acceptance covers Windows and WSL, not a broad platform release.

### Native activation and recovery

Native activation is accepted for the exact Arma 3 `2.22.154089` executable size/BLAKE3 and tested Windows/Linux x64 layout patterns. Windows and WSL full acceptance covered eight fresh engine generations, activation/rollback, persistence and three owner interruption/recovery scenarios. This is a narrow compatibility match, not a general version fallback or broad platform release. Actual lifecycle uses the separate reviewed foreground owner path described below.

```text
foxy server deploy activation plan <instance-id> <preparation-id> --expected-deployment-generation <n> --expected-activation-generation <n> --game-space arma3 --json
foxy server deploy activation apply <instance-id> <preparation-id> --expected-deployment-generation <n> --expected-activation-generation <n> --expected-plan-fingerprint <fingerprint> --game-space arma3 --yes --json
foxy server deploy activation status <instance-id> --game-space arma3 --json
foxy server deploy activation recovery-plan <instance-id> --game-space arma3 --json
foxy server deploy activation resolve <instance-id> --expected-plan-fingerprint <fingerprint> --game-space arma3 --yes --json
```

Plan is read-only. It verifies the explicit prepared generation and pins its identity, the saved instance/catalog and offline selection, expected deployment-selection and activation generations, the exact adopted installation evidence, and the realized native payload into one review fingerprint. Apply repeats the plan and refuses stale generations or a changed fingerprint. Apply `--dry-run` requires no `--yes` and creates neither owner nor files. A real Apply requires `--yes`, starts or attaches to the authenticated foreground owner, and submits the exact review through protocol 4. The owner pins installation admission and applies the existing transaction/recovery protocol without opening Turso. CLI mutation retains/join behavior for owned foreground owners; attached owners are not stopped.

The runtime is an immutable private complete copy. Selected base files are the platform executable, root native libraries, `steam_appid.txt`, and selected native data directories (`addons`, `dta`, `keys`, DLC/expansion folders, and Linux `linux64`). Configuration files, reviewed mission templates, and required and server-only staged addon files are realized at fixed instance-owned locations: required content is loaded through `-mod`, server-only content through `-serverMod`, and optional content is not loaded. The tested base `keys/a3.bikey` is kept, and only public keys explicitly trusted in the instance's reviewed key policy are added through a private `-keysFolder`; see [Arma server signing keys](Arma-Server-Keys.md). Native Windows evidence confirmed that a regular client carrying an addon signed by a trusted key joins, while a server without that key refuses it with the engine's "not signed by a key accepted by this server" message. The installation and preparation source are read-only. Separate per-instance profile/config/data/cache/runtime state remains mutable across restarts and does not alter the immutable engine payload.

On Windows, a plan rejects engine file paths that reach260 UTF-16 units. Choose a shorter `--config-dir` when that review reports a path limit; an unsupported path is rejected before copying or changing the active deployment.

`status` verifies the active runtime and reports the activation generation and whether transaction recovery is required. `recovery-plan` is read-only and fingerprints the exact retained intent, journal, active pointer, receipt and captured catalog/selection evidence. `resolve` recomputes that review and only applies the same expected outcome when the fingerprint still matches; a real resolve requires `--yes`. Resolve `--dry-run` verifies the exact review without owner startup or writes. If Apply or Resolve returns an uncertain owner/transport result, do not assume it failed and do not issue a new operation. Refresh Status, review Recovery Plan, then Resolve only that displayed outcome. Existing transaction evidence and unknown files are preserved; there is no force-clear command. No activation command starts the game process, acquires external content, or proves publisher trust.

The desktop **Native deployment activation** panel uses the same core plan and owner operation. Refresh status, select an explicit preparation, inspect the review fingerprint and bounded payload inventory, then apply the retained plan. **Deployment recovery** shows and consumes an exact review rather than a clear/force action. Server UI tasks retain cancellation/join protections and never open Turso. GUI/CLI plan and final-status parity, dry-run, stale-fingerprint and `--yes` checks, recovery interaction, keyboard traversal/activation, cancellation and a completed GUI apply passed. Cancellation can arrive after the active reference changes; inspect status and reviewed recovery to learn the retained outcome. Activation acceptance does not establish broad release support, provider acquisition or lifecycle acceptance.

### Arma foreground lifecycle

The exact native Arma runtime has separate owner-routed lifecycle commands. Keep `foxy server supervise --game-space arma3` running in a terminal, or use the owner retained by the Server desktop. Lifecycle commands attach to that existing owner. They never create a temporary owner that disappears when Start returns.

```text
foxy server instance start-plan <instance-id> --game-space arma3 --json
foxy server instance start <instance-id> --expected-activation-generation <n> --expected-plan-fingerprint <fingerprint> --game-space arma3 --yes --json
foxy server instance status <instance-id> --game-space arma3 --json
foxy server instance stop <instance-id> --expected-run-generation <n> --timeout-ms 30000 --game-space arma3 --yes --json
foxy server instance restart <instance-id> --expected-run-generation <n> --expected-activation-generation <n> --expected-plan-fingerprint <fingerprint> --game-space arma3 --yes --json
```

Start Plan verifies the settled committed runtime, current adopted installation and separate runtime-state paths. It creates no files, claims no database and contacts no owner. The review binds the activation generation, exact native launch inputs and foreground exit policy. Start and Restart repeat that review inside the authenticated owner under serialized operation and physical installation admission. Start Plan refuses a committed configuration that enables BattlEye: managed Arma runtimes do not include BattlEye files yet, and such a server would never start its mission. Turn BattlEye off, then commit, prepare and activate again. Saved draft edits do not silently change the committed runtime. Duplicate Start is refused; separate instances keep independent run generations and retained session handles. Status comes from the authenticated owner's observations, with the observation age. A process exists only as observed; engine readiness remains unverified. A replacement owner reports `not_owned`, which is not proof that a former process or consumer was cleaned up.

Stop requests native shutdown and waits within `--timeout-ms`, bounded from1 to120000 milliseconds. Windows requests the retained process's console interrupt or window close; Linux sends SIGINT to its owned namespace payload. An elapsed deadline does not authorize termination: an incomplete Stop returns operation-failed4 and keeps the session, installation lease and consumer protection. Inspect Status before the next action. A separate `stop ... --force --yes` explicitly terminates and waits for the owned tree. Restart with `--force` uses the same explicit policy before launching the reviewed runtime. Forced termination is save-uncertain. A natural process exit is not a whole saved-game durability guarantee.

The default foreground lifetime requests soft shutdown when its owner closes, waits five seconds, then force-cleans remaining owned trees. This exit policy remains part of the reviewed Start fingerprint and `--yes` acceptance. Closing an attached desktop leaves its external owner running. Abrupt owner death leaves persistent consumer evidence. Foxy never clears it automatically; verified old-owner records can be retired only through the explicit review below.

#### Recover witnessed consumers after owner death

After an owner crash, establish a replacement authenticated owner explicitly, then inspect and apply a fingerprinted consumer recovery plan. The detached supervisor keeps the replacement owner available while you review:

```text
foxy server supervise --detach --yes --game-space arma3 --json
foxy server instance recovery-plan <instance-id> --game-space arma3 --json
foxy server instance recover <instance-id> --expected-plan-fingerprint <fingerprint> --game-space arma3 --yes --json
```

`recovery-plan` is read-only and does not start an owner or game process. It reports owner authentication, inspection completeness, eligible registrations, pending/unverified registrations, completed receipt history, blockers and the review fingerprint. Apply requires that exact fingerprint and `--yes`; `--dry-run` checks the exact review without retiring records. A real Apply requires a current authenticated owner and never starts a replacement owner itself. Foxy does not automatically restart a crashed owner. If an owner is still running, attach to it rather than creating another. When an owner died, `server owner` and the commands that attach to it report that the recorded owner for the game space is not running (exit code 4). That fact grants nothing: the dead owner's record is replaced only when a new owner starts, and its consumers wait for this reviewed recovery.

Only a registered schema-v2 consumer with a verified native whole-tree cleanup receipt, from an old owner and matching the selected configuration root, game space and server instance is eligible. The witness retains native owner and containment authority and records verified exit of the Windows Job or Linux PID namespace, including cleanup after owner death. The receipt contains no reusable process handle or PID authority. Foxy does not infer exit from process enumeration, elapsed time or a released lock. Current-owner records, other config roots, spaces or instances, legacy registrations, pending or malformed receipts, unknown data and completed history remain untouched. Apply rechecks the full saved catalog, owner scope, exact record and receipt fingerprints under the metadata permit and physical-installation recovery admission. Cancellation or a per-record failure returns the number retired, number remaining, `inspection_complete`, and an optional `error` so partial effects remain visible. An incomplete post-inspection labels the count as last observed. Activation and maintenance intent use their separate reviewed recovery flows.

Any pending or unavailable receipt in the selected configuration-root/space/instance group blocks retirement of the entire group, including already verified headless clients. Recovery leaves other groups untouched. Completed receipt history is retained for inspection, capped at 128 completed unreferenced receipts; the receipt namespace has a 1024-entry limit. A full or uninspectable namespace fails closed. The existing [schema-v1 registration example](../examples/json/server_backend/server_consumer_registration.json) remains a legacy record and is intentionally not recoverable. The [v1 receipt containing a v2 consumer](../examples/json/server_backend/server_consumer_cleanup_receipt.json) and [review plan](../examples/json/server_backend/server_consumer_recovery_plan.json) use synthetic identities and hashes, with no real recovery authority or usable apply token.

Windows proof waits for the process handles of every verified Job member and checks the complete lifetime process count before recording `windows_job_waited_members`. A member that exits before its handle can be verified, or a history exceeding 4096 processes, leaves cleanup unknown. Older accounting-only `windows_job` receipts are unverified and cannot clear a registration. Linux proof waits for namespace-init teardown. These limits preserve barriers when whole-tree exit cannot be established.

To keep supervision running after closing the desktop, explicitly choose **Detach owner and keep server running**. Foxy confirms the authenticated owner's change of lifetime before releasing the window's parent connection. The same owner, server runs, database claim and physical installation leases remain in place. Reopen Server mode or choose **Reconnect to server owner** to load current authenticated status; changing game space leaves a detached owner supervising its original space. Returning that same space to Client mode remains blocked while its detached owner holds the database claim.

The equivalent CLI flow is:

```text
foxy server supervise --detach --game-space arma3 --yes --json
foxy server instance start <instance-id> --detach --expected-activation-generation <n> --expected-plan-fingerprint <fingerprint> --game-space arma3 --yes --json
foxy server owner --events-after 0 --game-space arma3 --json
foxy server instance stop <instance-id> --expected-run-generation <n> --timeout-ms 30000 --game-space arma3 --yes --json
foxy server supervise --shutdown --game-space arma3 --yes --json
```

`start --detach` can establish the one owner itself; a separate detached supervisor command is optional. Repeated detach attaches to the same owner. A terminal foreground supervisor cannot be detached in place; stop it explicitly before establishing a prepared background owner. Detach and idle shutdown require `--yes`; their dry-runs contact no owner and start no process. Shutdown refuses active sessions or operations, so stop every owned instance first. Its `released: true` result follows verification that the game-space claim was released. This creates no OS service and does not restart an owner after a crash.

Owner inspection includes `lifetime` and a nonsecret prepared `launch_id`. `--events-after N` replays at most eight retained events from that exact authenticated owner generation; `after`, `earliest_available`, `head`, and `gap` make truncation explicit. At most128 events are retained. Use the last returned sequence for the next batch. Lifecycle completion payloads retain their actual outcome, including cancellation or failure. A changed owner generation starts a separate history; reconnect never fabricates readiness or restores an old process from its PID.

Global `--dry-run` validates Start/Restart review or Stop/Status intent without owner attachment, lifecycle submission or writes. Actual Start, Stop and Restart require `--yes`; force is never inferred. JSON/TOON keep the standard envelope. Completed effects return `data.outcome`; Status returns `data.status`, including owner/run/activation identity, phase, process observation, cleanup, timeout and forced outcome. Validation errors use2; failed, timed-out, cancelled or unknown effects use4. After an uncertain delivery, inspect Status instead of blindly creating a new effect. A cancellation arriving after Start may leave the server running under its retained owner; the outcome says so.

`server instance status|monitor <id>` reads an authenticated, cached process/query/resource/log observation. Status preserves `data.status` and adds `data.monitoring`; Monitor returns `data.monitoring` with status, observation, observation age, query age, query freshness and health. Ages are milliseconds. Resource samples report CPU percent (which can exceed100% for a process using multiple logical processors), resident memory in bytes, uptime in milliseconds, and process start time in seconds. A recent query response means only that the owner's configured query recently responded; it does not establish joinability, game FPS, scenario readiness or mission state. These commands do not query an endpoint themselves and accept no endpoint override.

`server instance logs <id> [--contains TEXT] [--after-sequence N] [--limit N]` filters only the bounded cached log snapshot for the current run. The cache retains at most128 lines and32 KiB total, with individual lines capped at4096 bytes; `--limit` accepts1..128 lines and search text is limited to256 bytes. There is no file-path argument. Output preserves truncation and read-error indicators and may be unavailable or stale. Game-generated log text can contain private data; review it before sharing.

The desktop monitoring panel provides search, pause log display, auto-scroll, copy visible lines, and sanitized snapshot export. Pausing logs preserves current process/query/resource updates. Passive refreshes preserve keyboard focus, scroll position and draft edits; a user operation waits behind an in-flight read. Open raw runtime log folder opens the owned instance directory and explicitly exposes unsanitized engine logs. Copy/export use the sanitized bounded snapshot; export requires a new destination file.

`server instance monitor <id> --follow --stream-json [--interval-ms N]` follows cached snapshots until Ctrl+C. Windows also accepts Ctrl+Break. Cancelling the stream leaves the server running. The default poll interval is1000ms; the accepted range is250..30000ms. Each stdout line is one compact JSON frame. A frame includes only newly observed log sequence numbers for the same owner/run; owner or run changes are marked and start a separate history. Do not combine records across that marker. Do not add global `--json` or `--toon`; ordinary `--json` remains one result envelope. Follow uses bounded polling and controlled cancellation. `--dry-run` validates and previews the request without contacting the owner or network.

The desktop **Server lifecycle** panel exposes Refresh runtime status, Plan server start, Start reviewed server, Stop server gracefully and Restart with reviewed plan. Force termination requires its separate explicit control and warning. Controls use the same owner/core path as the CLI and stay hidden for unsupported games. These operations do not add provider acquisition or detached/service ownership. Reforger's exact native scope is described below; Arma key trust remains a separate reviewed policy described in [Arma server signing keys](Arma-Server-Keys.md).

Native lifecycle acceptance passed on Windows and ordinary-user WSL2/Linux for the tested Arma `2.22.154089` layout. The scoped runs cover two instances, duplicate Start, stop timeout and force, reviewed Restart, orderly owner close and abrupt owner death with retained recovery barriers. Desktop checks also cover owned and externally attached window lifetimes. This is not broad platform release or saved-game durability evidence.

### Arma headless clients

An Arma draft can define up to32 headless clients (HCs). Each has a stable `hc_` id, a unique profile name (1-24 letters, digits, underscores or hyphens), a start order, an enabled flag, an addon selection and optional process overrides. HCs run the same committed server runtime in client mode on this machine. Remote HC process control is not provided.

```text
foxy server headless list <instance-id> --game-space arma3 --json
foxy server headless add <instance-id> --name HC1 --expected-revision <n> [--order N] [--disabled] [--no-inherit-required] [--include SOURCE]... [--exclude SOURCE]... [--cpu-count N] [--ex-threads N] [--max-mem MB] [--limit-fps N] [--manual-start] [--start-delay S] [--join-timeout S] [--max-restarts N] --game-space arma3 --json
foxy server headless set <instance-id> <hc-id> --name HC1 --expected-revision <n> [same options] --game-space arma3 --json
foxy server headless remove <instance-id> <hc-id> --expected-revision <n> --game-space arma3 --yes --json
foxy server headless plan <instance-id> --game-space arma3 --json
foxy server headless status <instance-id> --game-space arma3 --json
foxy server headless start <instance-id> --expected-run-generation <n> [--client <hc-id>] --game-space arma3 --yes --json
foxy server headless stop <instance-id> --expected-run-generation <n> [--client <hc-id>] [--force] [--timeout-ms N] --game-space arma3 --yes --json
foxy server headless restart <instance-id> --expected-run-generation <n> [--client <hc-id>] [--timeout-ms N] --game-space arma3 --yes --json
```

`set` replaces the whole definition and keeps its id; options you omit return to their defaults. Add, set and remove save the draft like any other draft change. They require the current draft revision and honor `--dry-run`. Remove requires `--yes`. Draft changes reach a runtime only after a configuration commit, deployment preparation and activation.

By default an HC loads the server's required (shared) addons in server order. `--exclude` skips an inherited required source. `--include` adds a bound required or server-only source; a server-only source is never loaded implicitly. `--no-inherit-required` starts from an empty set. Optional sources are not part of the server runtime and cannot be selected. Every HC `-mod` list follows the server's binding order, then the source id. The order is frozen into the committed runtime, so later binding changes do not alter a running deployment.

An enabled local HC connects from `127.0.0.1`. If `headlessClients[]` is unset, the rendered `server.cfg` admits `127.0.0.1` and the preview says so. An explicit list must contain `127.0.0.1`. An operator-owned server configuration is not rewritten, and the preview warns that it must admit the address itself. `localClient[]` stays an explicit trust choice.

Each HC launch uses `-client -connect=127.0.0.1 -port=<game port>`, its own `-profiles` and `-name`, its `-mod` list and the overrides. It never receives `-config`, `-cfg`, `-serverMod`, `-autoInit` or `-keysFolder`. Its home, profiles, engine report and console log live under the instance's `runtime-state/headless/<hc-id>/`, separate from the server and from other HCs. When the server has a join password, Foxy resolves the protected reference at start. It writes `-password=...` into an owner-only `private/parameters.txt` and passes only `-par=<path>`. The password never appears in arguments, plans, previews or Foxy logs. A password that contains line breaks, control characters or surrounding spaces cannot be passed this way, and the start is refused. On the tested `2.22.154089` build an HC needs no Steam login, license prompt or other attended preparation. In a direct engine test, a server with BattlEye enabled still admitted HCs. Managed Arma runtimes do not include BattlEye files yet, so managed servers run with BattlEye off (see Start Plan above). Foxy never accepts a BattlEye license or edits a profile for you.

`plan` shows the committed HC launch plans from the same review as `server instance start-plan`; it starts nothing. Each HC also carries a start and restart policy, frozen into the committed runtime like its addons:

| Option | Default | Effect |
|---|---|---|
| `--manual-start` | off | The owner does not start this HC on its own; start it explicitly |
| `--start-delay S` | 0 | Seconds to wait after the run first answers its query port, 0-600 |
| `--join-timeout S` | 120 | Seconds after a start before missing join evidence is reported, 15-900; the process keeps running |
| `--max-restarts N` | 3 | Automatic restarts after unexpected exits within 10 minutes, 0-10; 0 turns them off |

The owner manages HCs only while their server run is running. Once the run's own query observation has answered within the last15 seconds, it starts each wanted HC in order, after its delay. It never starts an HC before that, and it pauses automatic starts while the server stops answering. Each HC holds its own installation consumer registration (`<instance-id>-<hc-id>`). An HC that exits unexpectedly is restarted after 5 s, then 10, 20 and 40 s, up to 60 s. When it has used its restarts within 10 minutes, automatic restarts are suspended and the status says so; nothing more is started until you start it again. A start that fails to spawn suspends the HC the same way.

`status` reads the owner's observation of the current run: each HC's state (`waiting_for_server`, `scheduled`, `running`, `stopped` or `restart_suspended`) and reason, its process id, its start time, its join evidence, its starts and automatic restarts in the last 10 minutes, the next start time and its last exit (code and whether Foxy requested it). Join evidence comes only from that HC's own log written after the start. `joined` means its report or console log contains `Starting mission:`, `pending` means not yet, and `no_evidence` means the join timeout passed without it. A later disconnect of a live HC process is not detected. The mission needs `HeadlessClient_F` slots, and Foxy does not verify AI offload.

`start`, `stop` and `restart` go through the authenticated owner and need the current run generation and `--yes`. Without `--client` they act on every HC of the run. `start` requires a query answer within the last15 seconds, starts what is not running, clears a suspension and gives a fresh restart budget; a repeated start creates no duplicates. `stop` waits up to the timeout (default 30 s), or terminates the process trees with `--force`. A stopped HC stays stopped and is never restarted automatically, even after a later unexpected exit. `restart` requires query readiness, stops the selected HCs gracefully, then starts them with a fresh budget. If its stop times out, an HC starts again automatically once it exits; if the restart is cancelled, the HCs stay stopped; if the server stopped answering in the meantime, they start once it answers again. A cancelled or failed explicit start leaves every HC it did not launch as it was, and an HC whose own spawn failed is suspended.

Stopping the server stops its HCs first. A graceful stop waits within the same timeout and leaves the server running if an HC does not exit; automatic starts stay paused for that run (status reason `paused_by_server_stop`) until you start an HC explicitly. Owner close and a server exit stop HCs under the five-second foreground policy. A server restart stops the HCs, then restarts the server. In the new run, HCs that you stopped stay stopped, while the others (including suspended ones, with a fresh budget) start again once the new run answers its query port. A plain server start uses each HC's own start policy. `headless_running` in server status counts HC processes only.

The desktop editor shows a **Headless clients** section for Arma, including each HC's start and restart policy. The lifecycle panel lists HC plans, then for a running server shows each HC's state, join evidence, starts, restarts, next start and last exit, with **Start**, **Stop** and **Restart** buttons per HC and **Start headless clients** and **Stop headless clients** for all of them. Starting and restarting need query readiness. With **Allow force termination**, it also offers **Force stop headless clients**.

Native acceptance passed on Windows and ordinary-user WSL2/Linux for the tested Arma `2.22.154089` layout. A password-protected server refused an HC start before it answered its query port. After that, two HCs connected and took both `HeadlessClient_F` slots. One loaded only the inherited required addon; the other loaded only an explicitly included server-only addon. Each wrote its logs in its own state root, and a repeated start created no duplicates. Each HC held its own consumer registration until a graceful stop cleaned up every process. No argument, plan or log contained the password. On Linux the HCs ran without any Steam client. This is not broad platform release evidence.

Managed lifecycle acceptance passed on Windows and ordinary-user WSL2/Linux with the same layout. Before the server answered its query port, both HCs waited with no process. Each then started automatically, the second after its 5-second delay, and both joined. Stopping one HC left it stopped for 15 seconds, without a restart, while the other stayed joined and only its registration remained. An explicit start and an individual restart each rejoined with a new process. After unexpected engine exits, an HC restarted after about 5 and then 10 seconds. A third exit within the window suspended it, and nothing started for the next 30 seconds. A server restart brought both HCs back once each with fresh budgets. A graceful stop then left no registration and no process.

### Scheduled server maintenance

Arma 3 and Reforger instances can carry up to16 maintenance jobs. A job has a name, an action, a one-time or weekly schedule in local time or UTC, and a policy. Jobs run only inside the server owner for the game space, the same owner that runs lifecycle commands. Foxy never installs an OS service or task. Without a running owner nothing runs, and a scheduled time missed by more than the grace window is recorded as skipped instead of running late.

```text
foxy server maintenance list <instance-id> --game-space arma3 --json
foxy server maintenance add <instance-id> --expected-revision <n> --name "Nightly restart" --action restart --weekly daily --time 04:30 [--utc] [--require-empty] [--disabled] [policy options] --game-space arma3 --json
foxy server maintenance add <instance-id> --expected-revision <n> --name "Patch day" --action restart --at 2026-12-01T05:00 --game-space arma3 --json
foxy server maintenance set <instance-id> <job-id> --expected-revision <n> --name ... --action ... (--at ... | --weekly ... --time ...) --game-space arma3 --json
foxy server maintenance remove <instance-id> <job-id> --expected-revision <n> --game-space arma3 --yes --json
foxy server maintenance run <instance-id> <job-id> --expected-policy-revision <n> --game-space arma3 --yes --json
foxy server maintenance cancel <instance-id> <occurrence-id> --game-space arma3 --yes --json
foxy server maintenance history <instance-id> --game-space arma3 --json
```

`--weekly` takes `daily` or a comma list of `mon` to `sun` and needs `--time HH:MM`. `--at` takes RFC 3339 with an offset, or `YYYY-MM-DDTHH:MM` read in the job's clock. A local time that does not exist on a daylight-saving change runs at the first valid minute after it; a repeated hour runs once. `set` replaces the whole definition and keeps the id; options you omit return to their defaults. Add, set and remove need the current catalog revision and honor `--dry-run`; remove needs `--yes`. Every saved change to a job advances its policy revision, and scheduled times before the change never run. Changing or removing a job cancels its open occurrences, except one that already stopped the server: that one still starts it again. A running owner picks up changes within a few seconds.

| Option | Default | Effect |
|---|---|---|
| `--grace-minutes N` | 10 | A scheduled time missed by more than this is skipped, 0-1440; at least one minute always applies, because the owner checks jobs every few seconds |
| `--require-empty` | off | Restart only with a player count of zero; an unknown count also defers |
| `--max-defer-minutes N` | 30 | How long a busy server or connected players defer the job before it is skipped, 0-720 |
| `--max-attempts N` | 2 | Restart attempts per occurrence, 1-5 |
| `--retry-delay-seconds N` | 60 | Wait between attempts, 10-3600 |
| `--stop-timeout-seconds N` | 60 | Graceful stop timeout of each restart, 5-120 |

`restart` is the only action that runs. It restarts the committed runtime through the same reviewed start plan and the owner's operation admission as a manual restart, under the client id `maintenance`. A scheduled restart never starts a server that is not running: an operator stop before the job records `skipped` with `server_not_running`. If the server stops outside the job during a deferral, or is started outside the job after the job stopped it (even if it was stopped again before a retry), the job records that and does nothing more. When a restart stopped the server but its start failed, the retry only starts it again. `update` and `backup` can be saved, but every occurrence is `blocked` with `adapter_unavailable` until those adapters exist; update an installation explicitly.

Each occurrence is a durable record under the instance's `maintenance/occurrences/`, claimed exactly once from a deterministic id, so a second owner or a repeated tick cannot run the same scheduled time twice. Its state is `claimed`, `deferred`, `running`, `retry_wait`, `succeeded`, `failed`, `skipped`, `blocked`, `cancelled` or `unknown`, with a reason and the run generations before and after. The record is written as `running` before the restart starts. If the owner ends during an occurrence, the next owner records it as `unknown` with `owner_lost` and never repeats it: the server may or may not have restarted. Review the server status and, after a crash, the consumer recovery below. A scheduled time that comes while another occurrence of the same job is still open is recorded as `skipped` with `overlapping_occurrence`, so a manual run just before the schedule does not restart twice. An occurrence that waits behind another one longer than its grace and deferral allowance is skipped as missed. The 64 most recent finished occurrences are kept, plus each job's latest scheduled record, so a pruned record can never let a scheduled time run again. `history` lists records newest first and counts unreadable records without touching them.

`run` records a manual occurrence for the job's current policy revision; a stale revision is refused. While the job already has an open occurrence, `run` returns that occurrence instead of recording a second one (`created: false`). `cancel` cancels a waiting occurrence, or an active restart at its next cancellation point; a restart that already stopped the server can leave it stopped. Both need `--yes` and an existing owner; their dry runs contact no owner. Validation errors use exit code 2; a missing owner or a refused request uses 4.

The desktop **Scheduled maintenance** section lists each job with its schedule, policy, next run, last result and the reason it would not run as scheduled. It offers **Add maintenance job**, **Edit job**, **Remove job**, **Run now** and **Cancel occurrence**, each confirmation focused on its safe choice, and the recent occurrences. The section refreshes every few seconds. Before a queued Foxy update installs, the desktop waits while any game space has supervised server sessions, a detached owner or an owner that does not answer, and says which.

The [job catalog example](../examples/json/server_backend/server_scheduled_jobs.json) and [occurrence example](../examples/json/server_backend/server_scheduled_occurrence.json) use synthetic ids.

Native acceptance passed on Windows and ordinary-user WSL2/Linux for the tested Arma and Reforger layouts. A one-time scheduled restart ran exactly once and replaced the run, a backup job at the same time was blocked, a repeated manual run returned the same open occurrence, and a queued occurrence was cancelled without restarting the server. Killing the owner during a restart left the occurrence `unknown` with `owner_lost`, and the replacement owner did not repeat it. A scheduled restart after an operator stop was skipped and the server stayed stopped. This is not broad platform release evidence.

### Server RCon

#### Arma administration and maintenance

The **Arma administration** section provides player, ban and deployed-mission lists, announcements, GUID-checked moderation, lock/unlock, mission selection and mission restart. **Review command** captures the instance, owner and run; confirmation focuses **Cancel** and supports Tab/Shift+Tab/Enter with visible focus. A reply confirms transport receipt; the effect can take longer or fail independently. Unrecognized or truncated lists remain incomplete, with their raw reply available.

```text
foxy server admin players <instance-id> --game-space arma3 --json
foxy server admin bans <instance-id> --game-space arma3 --json
foxy server admin missions <instance-id> --game-space arma3 --json
foxy server admin announce <instance-id> --message "Maintenance in one minute" --expected-run-generation <run> --game-space arma3 --yes --json
foxy server admin kick <instance-id> --player-id <id> --expected-guid <guid> --reason "Operator review" --expected-run-generation <run> --game-space arma3 --yes --json
foxy server admin add-ban <instance-id> --guid <guid> --minutes 60 --reason "Operator review" --expected-run-generation <run> --game-space arma3 --yes --json
foxy server admin remove-ban <instance-id> --ban-id <id> --expected-guid <guid> --expected-run-generation <run> --game-space arma3 --yes --json
foxy server admin mission <instance-id> --template mission_alpha.vr --expected-run-generation <run> --game-space arma3 --yes --json
```

`lock`, `unlock` and `restart-mission` use the same generation and confirmation options. Get the generation from `server instance status`. Timeout defaults to 10 seconds (`--timeout-ms` accepts 1000-30000). Mutations require the explicit generation and `--yes`. Dry-run validates without contacting an owner or writing records. Exit 0 means a reply arrived, 5 means an unknown outcome or list format, 4 means not sent or owner unavailable, and 2 means invalid input. Kick and ban removal re-read the native list and check the exact GUID before sending. Mission selection rechecks the deployed list and never copies files or restarts the server process.

Arma maintenance jobs can select `--action rotate-mission --mission-template mission_alpha.vr` or keep `--action restart`. Optional `--announcement "text" --announcement-delay-seconds 30` sends one recorded notice before downtime. Cancel during that delay keeps the current server running; a sent notice remains in history. Require-empty policy is checked again after the delay. Rotation requires a new native mission identity and a subsequent fresh query before success. An unconfirmed effect is never repeated automatically.

Arma Stop and Restart drain managed HCs before a recorded RCon `#shutdown`, then await the retained process tree. A missing acknowledgement never causes another shutdown command; a proven pre-send failure may use native graceful stop. Force remains explicit. Restart keeps operator-stopped HC intent. Key/content changes retain the existing reviewed acquisition, immutable stage, preparation, activation and physical-consumer gates. Jobs never rewrite live or adopted files. `--require-backup` blocks while the consistent-backup adapter is unavailable; unattended update and backup actions remain unavailable, and shutdown makes no save guarantee.

#### Shared RCon configuration and raw commands

Arma 3 and Reforger instances can enable RCon, the UDP administration protocol both engines speak (BattlEye RCon protocol version 2). Arma provides it through BattlEye, so the instance also needs BattlEye on. Reforger provides it natively through the `rcon` block of its `server.json` and does not need BattlEye.

```text
foxy server rcon configure <instance-id> --expected-revision <n> --port 2310 [--address 127.0.0.1] --game-space arma3 --json
foxy server rcon configure <instance-id> --expected-revision <n> --port 19999 [--permission monitor|admin] [--max-clients 1-16] --game-space reforger --json
foxy server rcon set-password <instance-id> --expected-revision <n> --game-space arma3 --json
foxy server rcon disable <instance-id> --expected-revision <n> --game-space arma3 --yes --json
foxy server rcon status <instance-id> --game-space arma3 --json
foxy server rcon test <instance-id> --game-space arma3 --json
foxy server rcon command <instance-id> --text "players" [--timeout-ms 10000] --game-space arma3 --yes --json
foxy server rcon history <instance-id> --game-space arma3 --json
foxy server rcon probe [<instance-id>] [--endpoint 127.0.0.1:2310] [--password-stdin] [--command "players"] [--timeout-ms 5000] --game-space arma3 --json
```

`configure`, `set-password` and `disable` change the draft, need its current revision and honor `--dry-run`; `disable` needs `--yes`. RCon settings are part of the committed configuration, so commit, prepare and activate the new revision, then restart the server, to apply a change. The address defaults to `127.0.0.1`, which keeps RCon on this computer. For Arma the port must stay outside the game port range (game port to game port + 4), and Start review refuses RCon while BattlEye is off. For Reforger, `--permission` applies to every RCon connection (it is not a per-operator account) and defaults to `monitor`.

`set-password` reads one line from stdin, hidden on a terminal; a password is never accepted as an argument. It needs 3 to 64 visible ASCII characters without spaces. Foxy stores it in the instance's protected credential store (Windows DPAPI, or a private `0700`/`0600` directory on Linux) under a new reference and points the draft at that reference. Committed revisions keep the reference they were made with, so changing the password never alters a runtime that is already activated. A stored password belongs to one instance: a cloned instance or an imported pack keeps the RCon settings but needs its password set again, and Start review refuses a runtime whose protected password is missing. Previews, exports, start plans, logs and owner messages carry only the reference. At start, Foxy writes the password into private runtime state only: for Arma into the instance's own BattlEye folder (`runtime-state/home/battleye`, passed as `-bepath`), which also receives the runtime's BattlEye files and keeps the files BattlEye writes, such as bans; for Reforger into a private copy of `server.json` that `-config` points at. The Arma BattlEye configuration keeps any vendor lines and replaces only `RConPassword`, `RConPort` and `RConIP`. RCon does not encrypt its password on the wire; keep an endpoint that other computers can reach on a private network or a tunnel. On Linux, BattlEye prints its configuration entries, including the RCon password, to the server console. Foxy replaces the run's protected password, and any value that follows a key such as `RConPassword` or `passwordAdmin`, with `[redacted]` in log views, monitoring snapshots, `server instance logs` and exports. The engine's own output file in the private runtime state keeps what the engine wrote.

While a server with RCon runs, the owner for its game space keeps one RCon session for it. It logs in again with a short backoff after a failed login or a lost session, sends a keepalive when the session has been idle for 25 seconds (servers drop a session after 45 silent seconds), and acknowledges every server message. `status` shows the draft settings, whether a protected password is stored, and the live session: `connecting`, `connected`, `rejected` (wrong password), `no_reply`, `unreachable`, `password_unavailable` or `lost`, with the run generation, a session generation that increases on every new login, and the most recent server messages. Server messages stay in memory only. `test` round-trips an empty keepalive.

`command` sends text exactly as the server expects it, for example `players` on Arma or `#players` on Reforger. Commands that carry a password, such as BattlEye's `RConPassword`, are refused so the value never reaches records or history; change the password with `set-password`. It needs `--yes`, an existing owner and a running server with a live session. Foxy records the command under the instance's `rcon/commands/` before sending it and sends it exactly once: if no complete reply arrives before the deadline, the outcome is `unknown`, because the server may have executed it. Check the server before sending it again; Foxy never resends a command on its own. A command is `not_sent` when nothing was transmitted, for example without a live session. A replied command keeps up to 16 KiB of its reply. If the owner ends while a command is open, the next owner records it as `unknown` (or `not_sent` if it was still queued) with `owner_lost`. The 64 most recent finished commands are kept; replies such as player lists can contain player names and addresses, so the directory is private to your account. Exit codes are 0 for a reply, 5 for an unknown outcome, 4 for not sent or a missing owner, and 2 for validation errors.

`probe` opens a separate short session without the owner. With an instance id it uses the active runtime's endpoint and protected password; `--endpoint` connects elsewhere, such as a tunnel or port forward, and `--password-stdin` reads a different password. It logs in, exchanges a keepalive, and with `--command` and `--yes` sends one command once. The `login` field is `accepted`, `rejected`, `no_reply`, `unreachable` or `local_socket`.

Typed administration commands, such as player lists, messages, kicks, bans and scenario reload, are not part of this release yet; use `command` with text your server version documents.

The desktop **RCon** section shows the draft settings and the live session. It offers **Configure RCon** (or **Edit RCon settings**), **Set RCon password** with a masked field, **Remove RCon**, **Test RCon connection**, and a command field with **Send command...**, whose confirmation focuses **Cancel**. It shows the last command's outcome, its reply, and the recent commands. The [command record example](../examples/json/server_backend/server_rcon_command.json) and [instance example](../examples/json/server_backend/arma3_rcon_instance.json) use synthetic ids.

---

## Reforger native adoption and foreground lifecycle

Reforger dedicated `1.9.0.37` from stable app `1874900` has an exact executable size/BLAKE3 layout entry for Windows and Linux x64. A version or channel label alone grants no compatibility; untested hashes remain blocked. Inspection reports the match without starting the engine, verifying vendor authenticity or granting deployment authority. Native Steam installation supports both Reforger channels through the managed installation workflow above. Steam Workshop acquisition remains specific to Arma 3; Reforger uses its own engine for addon acquisition below.

Use an existing Reforger game space, inspect and adopt its native dedicated installation, and save a draft with an explicit IPv4 bind address, distinct game/A2S UDP ports, a full built-in scenario resource, and no addon intent. For a local run, select `127.0.0.1`; the A2S address follows the bind address. Unspecified endpoint defaults remain valid drafts but block native activation until verified. The [typed loopback example](../examples/json/server_backend/reforger_native_definition.json) uses Game Master Arland. The [server.json example](../examples/json/server_backend/reforger_server.json) shows the imported vendor shape, including an unknown `publicAddress` and `missionHeader`.

`server configuration import-reforger` reviews that file. `--apply --yes` with the current draft revision replaces the saved draft and keeps unknown JSON. `export-reforger` writes a new server.json and does not overwrite an existing file. Password, secret and token values are dropped on import and omitted from export and preview. A typed scenario resource is not proof that the scenario is installed. Addon rows remain desired intent until the exact content has been acquired and retained.

`server configuration scenario parse-log` reads an engine log from `-listScenarios` run without a scenario-loading config. `import-log --yes` stores a complete official listing beside the instance and does not change the selected scenario. A partial or failed log is rejected and does not replace a stored listing. `scenario list` reports whether the selected resource appears in that listing. The desktop Server page reviews the same import, export and listing files. Discovery names the resource the engine printed; it does not check that a file exists on disk.

The existing configuration archive, content stage, deployment record, selection, preparation and activation workflow applies with `--game-space reforger`. Activation copies the reviewed engine payload and `server.json` into an immutable owned runtime. Addon-bearing configurations require the exact reviewed addon lock and staged bytes. Unrelated content and Arma mission/key artifacts are refused. The adopted installation and preparation stay unchanged. Each instance keeps mutable state under `server/instances/<id>/deployments/runtime-state/home/`: `profiles/profile`, `data/addons`, `cache/addons` and `runtime` for native logs. Reforger receives a profile parent and explicit download, temporary and log paths; existing mutable bytes are retained and links/reparse points are refused before launch.

### Reforger addon revisions

Save the desired addon GUIDs, optional exact versions and required settings in the draft. An omitted version means latest acquisition intent. An unset `required` inherits `modsRequiredByDefault`, whose default is true. A missing optional addon remains in the desired set and produces a degraded retained lock. Missing required dependencies or the selected scenario provider block retention. Scenario resource GUIDs and addon GUIDs are separate identities.

Use the desktop **Reforger addon revisions** panel to review and confirm acquisition, or the equivalent CLI:

```text
Foxy server reforger-addons plan <id> --game-space reforger --json
Foxy server reforger-addons acquire <id> --game-space reforger --expected-plan-fingerprint <fingerprint> --yes --json
Foxy server reforger-addons list <id> --game-space reforger --json
```

`plan` and acquisition with `--dry-run` start no engine or download. Actual acquisition copies the accepted engine into a private working directory and starts a retained loopback maintenance session. Its profile, download, temporary, environment and log destinations are separate from the running deployment and save data. Cancellation waits for process cleanup. This operation uses Bohemia Workshop through the vendor engine; it does not use Steam Workshop or SteamCMD. Only the Stable addon channel is supported; a separately installed Experimental app does not enable Experimental addon acquisition.

After the engine stops, Foxy checks project GUIDs, dependency edges, exact versions, package completeness and vendor file hashes, then retains an immutable per-instance revision. Scenario ownership requires the exact resource GUID and path in the verified base or addon resource index. Include its addon in the desired set, or supply `--scenario-provider <addon-guid>` to acquire it as required. A blank provider permits automatic detection among the acquired addons. Ambiguous ownership, unsupported resource-index formats and unknown dependency representations are refused; the accepted engine uses RDBC version 7. The previous selection survives failure.

Successful acquisition removes its verified private engine copy after clean shutdown; logs and downloaded inputs remain in the private acquisition directory. Use empty repository bindings for this workflow, then stage, record, prepare and activate the retained set through the existing deployment controls. The runtime configuration contains exact pins for every realized addon and dependency, all marked required to prevent a partial realized deployment; omitted optional addons are absent, so a later Start does not silently retry latest intent. The saved draft keeps its original intent. [The lock example](../examples/json/server_backend/reforger_addon_lock.json) illustrates this distinction with fictional content.

Use the same `server instance start-plan|start|status|stop|restart` review, generation, fingerprint, `--dry-run` and `--yes` contracts as the Arma workflow above. Start uses only the committed runtime and existing foreground owner. Windows Reforger uses CTRL+BREAK through the retained private-console helper; Linux uses the retained SIGINT path. A stop timeout retains ownership and protection. Force remains explicit, and Restart cannot start another process until the prior tree is cleaned. Process ownership remains separate from scenario readiness; these controls do not promise saved-game durability, detached service ownership or automatic orphan recovery. Scheduled restarts use the same `server maintenance` jobs described in [Scheduled server maintenance](#scheduled-server-maintenance), with `--game-space reforger`.

## Table of Contents

1. [Overview](#overview)
2. [Prerequisites](#prerequisites)
3. [Setting Up a Repository](#setting-up-a-repository)
4. [Server Launch Line and Keys](#server-launch-line-and-keys)
5. [Arma Reforger Repositories](#arma-reforger-repositories)
6. [Custom Game Repositories](#custom-game-repositories)
7. [Repository Structure](#repository-structure)
8. [Repository Spaces](#repository-spaces)
9. [Checks, Previews, and Safe Publishing](#checks-previews-and-safe-publishing)
10. [App Updates Distribution](#app-updates-distribution)
11. [Configuration Reference](#configuration-reference)
12. [Hosting Behind a Reverse Proxy](#hosting-behind-a-reverse-proxy)
13. [Maintaining Repositories](#maintaining-repositories)
14. [Troubleshooting](#troubleshooting)

---

## Overview

As a server administrator, your role is to:

- Organize your mod folders on disk.
- Use `foxy-server-backend-cli` to generate a repository with checksums and manifests.
- Serve the output directory over HTTP/HTTPS so Foxy clients can sync mods.
- Optionally group several repositories into a **repository space** for your community, generated in one pass with `create-space`.
- Optionally keep your server launch scripts and key folder in step with every generated repository.
- Optionally host a **self-hosted app updater** so your community always runs the latest Foxy version.

Foxy clients connect to your repository URL, read the `repo.json` manifest, and download or update only the files (and file parts) that changed.

### Command summary

| Command | Purpose |
|---------|---------|
| `new [config.json] [--game arma3\|reforger\|generic]` | Write a blank repository config |
| `create <config> <output>` | Generate one repository |
| `new-space [space.json]` | Write a blank repository space config |
| `create-space <space.json> <output>` | Generate every repository of a space plus `repository_space.json` |
| `validate <config> [--space] [--output <dir>]` | Check a config without hashing or writing |
| `verify <output>` | Rehash generated output against its manifests |
| `diff <old> <new>` | Compare two generated outputs |
| `audit-keys <config> [--space] [--strict]` | Check Arma 3 keys and PBO signatures |
| `export-reforger-config <config> <output>` | Write an Arma Reforger `game.mods` fragment |
| `setup-app-updater` / `new-app-update` | Create or extend a Foxy app update manifest |

Global flags go before the subcommand: `--json` prints one machine-readable result on stdout (progress and messages go to stderr), and `--no-progress` turns off the animated progress bar. `--version` prints the version and the source commit, so a build from source can be told apart from a published build of the same version.

### Compatibility with Swifty

Foxy is fully backwards compatible with Swifty repositories. The generator supports three modes so you can migrate at your own pace:

| Mode | Flag | Hashing | Output artifacts |
|------|------|---------|------------------|
| **FoxyMode** (default) | `--mode foxy` | BLAKE3 | `foxy_addon.json` per mod, `foxy_addons.json`, `repo.json` |
| **SwiftyMode** | `--mode swifty` | MD5 | `mod.srf` per mod, `repo.json` |
| **HybridMode** | `--mode hybrid` | BLAKE3 + MD5 | All of the above side by side |

HybridMode serves both Foxy and legacy Swifty clients from the same repository. Once your community has migrated, switch to FoxyMode and drop the legacy artifacts.

---

## Prerequisites

1. **A web server** that serves static files over HTTP or HTTPS (nginx, Apache, Caddy, IIS, or any static file host). It must support HTTP range requests, and it must follow symlinks if you use the `pool` or `link` space layouts.

2. **Mod folders** organized in directories, each mod in its own folder (for example `@cba_a3/`, `@ace/`). The structure inside each mod should match what the game expects (for Arma 3 typically `addons/`, `keys/`, optionally `optionals/`).

3. **The `foxy-server-backend-cli` binary.** Build it from the repository root:
   ```
   cargo build --release -p foxy-server-backend-cli
   ```
   The binary lands in the workspace `target/release/` folder as `foxy-server-backend-cli` (Linux) or `foxy-server-backend-cli.exe` (Windows).

   Release archives include `LICENSE` and `THIRD-PARTY-LICENSES.txt`; keep them beside the executable when distributing it. For source builds, use the root `LICENSE` and `foxy-server-backend-cli/THIRD-PARTY-LICENSES.txt`.

---

## Setting Up a Repository

### Step 1: Generate a config template

```
foxy-server-backend-cli new config.json
```

This writes a blank Arma 3 `config.json`. Use `new config.json --game reforger` for an Arma Reforger template, or `--game generic` for a custom game (see [Custom Game Repositories](#custom-game-repositories)). If the file already exists, the command refuses to overwrite it.

The Arma 3 template looks like this:

```json
{
  "repoName": "My Repository",
  "game": "arma3",
  "basePath": ".",
  "appUpdateUrl": "",
  "requiredMods": [
    { "modName": "@example_mod", "enabled": true }
  ],
  "optionalMods": [
    { "modName": "@client_side_sound", "enabled": false, "clientSide": true }
  ],
  "iconImagePath": "icon.png",
  "repoImagePath": "repo.png",
  "clientParameters": "",
  "modLineFiles": [],
  "dlcContent": {
    "csla": false,
    "ef": false,
    "gm": false,
    "rf": false,
    "spe": false,
    "vn": false,
    "ws": false
  },
  "repoBasicAuthentication": {
    "username": "",
    "password": ""
  },
  "version": "3.2.0.0",
  "servers": [
    {
      "name": "Main Server",
      "address": "127.0.0.1",
      "port": "2302",
      "password": "",
      "battleEye": true
    }
  ]
}
```

### Step 2: Edit the config

```json
{
  "repoName": "My Community Mods",
  "basePath": "D:/Arma3/ServerMods",
  "appUpdateUrl": "https://mods.example.com/foxy/",
  "requiredMods": [
    { "modName": "@cba_a3" },
    { "modName": "@ace" },
    { "modName": "@tfar" }
  ],
  "optionalMods": [
    { "modName": "@shacktac_ui", "enabled": false, "clientSide": true }
  ],
  "iconImagePath": "icon.png",
  "repoImagePath": "repo.png",
  "clientParameters": "-skipIntro -noSplash -world=empty",
  "dlcContent": ["gm", "spe"],
  "modLineFiles": ["../server/start-server.cmd"],
  "version": "1.0.0",
  "servers": [
    {
      "name": "Main Server",
      "address": "arma3.example.com",
      "port": "2302",
      "password": "",
      "battleEye": true
    },
    {
      "name": "Training Server",
      "address": "arma3-training.example.com",
      "port": "2402",
      "password": "",
      "battleEye": false
    }
  ]
}
```

Key points:

- **`basePath`** is the root folder containing your mod folders. Mod names in `requiredMods`/`optionalMods` resolve relative to it. On Windows, use forward slashes or escaped backslashes in JSON.
- **`enabled`** defaults to `true`. Set it to `false` for mods that clients see but leave unchecked by default.
- **`clientSide`** marks a mod that only players run (Arma 3). It is never added to the server launch line, and the Foxy client uses it to explain what it enables when joining.
- **Wildcards** are supported in the last path segment: `"@*"` matches every folder starting with `@` inside `basePath`, and `"collections/*"` matches subfolders.
- **Nested mods** such as `@ace/optionals/@ace_noactionmenu` are published as a standalone `@ace_noactionmenu` mod.
- **Mod names are lowercased** in the output. A source folder named `@ACE3` becomes `@ace3`.
- **Images** (`iconImagePath`, `repoImagePath`) resolve relative to `basePath`. If found, they are copied to the output and their SHA-1 checksums are written into `repo.json`.
- If the client still shows a placeholder, check that the generated `repo.json` has a non-empty `repoImagePath` and `repoImageChecksum` and that the published image is reachable. A source image is not published unless its path is configured. Add `"repoImagePath": "repo.png"` to the generator config with `repo.png` under `basePath`, regenerate, and refresh the repository in Foxy.
- **`dlcContent`** lists the Arma 3 Creator DLCs the repository uses (see [DLC content](#dlc-content)).
- **`modLineFiles`** lists server launch scripts to keep in step with the generated launch line (see [Server Launch Line and Keys](#server-launch-line-and-keys)).

### Step 3: Build the repository

```
foxy-server-backend-cli create config.json ./output
```

This reads the config, discovers the files in each mod folder, copies them to the output, computes checksums, and writes the manifest files. For Arma 3 and Reforger it prints the server launch line last; custom games have no server launch line.

Useful options:

| Option | Effect |
|--------|--------|
| `--mode foxy\|swifty\|hybrid` | Generation mode (default `foxy`) |
| `--threads <n>` | Worker threads (default 1 for deterministic ordering; raise it for large repositories) |
| `--app-update-url <url>` | Set or override `appUpdateUrl` in `repo.json`; wins over the config value |
| `--mod-line-prefix <dir>` | Server-side folder prefix for each mod in the launch line (for example `mods`) |
| `--mod-line-include-optional` | Add the optional mods to the launch line |
| `--prune-unused-optionals` | Leave root-level `optionals` folders out of the published copies |
| `--collect-keys`, `--keys-output <dir>`, `--additional-keys <path>` | Collect `.bikey` files into one folder |
| `--dry-run` | List every planned write without writing |
| `--incremental` | Reuse checksums of unchanged mods |
| `--atomic --yes` | Build beside the output and swap it in only after success |
| `--report` | Print added, changed, and removed mods with estimated download bytes |

For scripts or screen-reader-friendly output, turn off the progress bar with the global flag:

```
foxy-server-backend-cli --no-progress create config.json ./output
```

### Step 4: Serve the output

Point your web server at the output directory. Foxy clients access `https://your-server.example.com/repo/repo.json` (or whatever your URL path is). Players add the folder URL, for example `https://your-server.example.com/repo/`.

---

## Server Launch Line and Keys

### The printed launch line

For Arma 3 and Reforger, `create` finishes by printing the server launch line for the generated repository and saves the same single line to `<output>/server_mod_line.txt`, so a wrapper script can read it later. Custom game repositories have no server launch line or `server_mod_line.txt`:

```
foxy-server-backend-cli create config.json ./output --mod-line-prefix mods
# Server mod line:
# -mod=gm;spe;mods/@cba_a3;mods/@ace;mods/@tfar;
```

For Arma 3, the Creator DLC codes from `dlcContent` come first, then the enabled required mods. Disabled and client-side mods are never added; `--mod-line-include-optional` appends the enabled optional mods. Put a nested mod path in `requiredMods` to include it in the default line.

### Keeping launch scripts in step (`modLineFiles`)

List the launch scripts or server configs that should always carry the current mod line:

```json
"modLineFiles": ["../server/start-server.cmd", "/opt/arma3/start.sh"]
```

After a successful generation, each listed file keeps its content except for the value of the launch parameters Foxy produces: `-mod=` for Arma 3, `-addonsDir` and `-addons` for Arma Reforger. Quoting is preserved, so `start.exe "-mod=@old;" -config=server.cfg` becomes `start.exe "-mod=mods/@cba_a3;" -config=server.cfg` and nothing else on the line moves. Every occurrence in a file is updated; `--mod=`, `-modules=`, and `-addons` inside `-addonsDir` are left alone. Relative paths resolve from the config file's own folder.

A listed file that is missing, is not UTF-8 text, or has no such parameter fails the run before any hashing starts, so a typo never leaves a server silently running the old mod set. `validate` performs the same check, `--dry-run` lists the files as `update-mod-line` actions, and the rewrite runs only after the repository output is published. In a space, two repositories may not list the same file.

### Pruning unused optionals

`--prune-unused-optionals` leaves the root `optionals` folder out of published mods (the source is untouched). When the output still holds an `optionals` folder from an earlier run, the command lists it and asks for `--yes` before removing it; a fresh or already-pruned output needs no `--yes`.

### Collecting keys

`--collect-keys` copies every `.bikey` from the generated mods into one flat folder (`<output>/keys` by default) so you can push it to the server in one step:

```
foxy-server-backend-cli create config.json ./output --collect-keys --keys-output ./server/keys --additional-keys ./a3-keys
```

`--keys-output` picks the destination, and `--additional-keys` (repeatable) adds a key file or folder that the generator does not produce, such as `a3.bikey` and the Creator DLC keys. Both imply `--collect-keys`. Keys are flattened by file name: identical duplicates are skipped, and a name clash between two different keys keeps the first one and is reported. Collection never deletes anything already in the destination.

---

## Arma Reforger Repositories

Set `"game": "reforger"` in the config (`new config.json --game reforger` writes a template):

```json
{
  "repoName": "MainRepo",
  "game": "reforger",
  "basePath": "R:/Mods/MainRepo",
  "requiredMods": [{ "modName": "@mod_01", "enabled": true }],
  "optionalMods": [],
  "iconImagePath": "icon.png",
  "repoImagePath": "repo.png",
  "clientParameters": "-noSplash",
  "modLineFiles": [],
  "servers": [{ "name": "Main Server", "address": "127.0.0.1", "port": "2001" }]
}
```

- Addon folders are hashed the same way as Arma 3 mods. Keep each addon's `addon.gproj`, `.pak` files and `resourceDatabase.rdb` together in its own folder. Packed Reforger addons do not need an Arma 3-style `addons/` subfolder. `.pak` archives inside them are parsed so clients can update individual entries.
- The printed launch line is `-addonsDir <prefix or .> -addons <id,...>`. Each id is the mod's `.gproj` GUID, with the project ID, the Workshop `ServerData.json` id, and finally the folder name as fallbacks.
- `dlcContent` has no effect for Reforger and triggers a warning. `clientSide` also triggers a warning because Foxy does not mark Reforger mods as client-side in the repository; a mod marked client-side is omitted from the generated server launch line.
- The generated `repo.json` carries `"game": "reforger"`. Arma 3 output does not include the key.

### Generate and regenerate the repository

The generator's `config.json` is the editable input. The generated `repo.json` is the client manifest; it is neither the generator config nor the Reforger dedicated-server config.

```text
foxy-server-backend-cli new "R:\Mods\MainRepo\config.json" --game reforger
foxy-server-backend-cli create "R:\Mods\MainRepo\config.json" "R:\Mods\Published\MainRepo" --threads 16 --mode foxy --atomic --yes
```

Run `new` only once, edit the template, then use the same `create` command after changing addons or repository metadata. `--atomic --yes` builds beside the published output and replaces it after success, so a regeneration script does not need to delete the current repository first. Serve `R:\Mods\Published\MainRepo` over HTTP/HTTPS. Keep the source and output directories separate.

The example explicitly selects `@mod_01`. To include every addon folder under `basePath`, use `"requiredMods": [{ "modName": "*", "enabled": true }]` and keep unrelated directories outside that folder. Image paths resolve from `basePath`; put `icon.png` and `repo.png` there or adjust their configured paths.

The `servers` list supplies connection details to Foxy clients. It does not configure the dedicated server's bind address, public address, server name, password or game rules.

### Dedicated-server launch modes

Choose the server mode before preparing a launch script:

| Mode | Launch parameters | Configuration |
|------|-------------------|---------------|
| JSON-configured server | `-config <server.json>` | Server settings and Workshop mods in `game.mods` |
| Local-addon server | `-server <world.ent> -addonsDir <root> -addons <GUIDs>` | Loads local addons directly; JSON server config is ignored |

Reforger rejects `-config` together with `-addons` with `-config cannot be used together with addons!`. Removing `-config` also removes its server settings; the local-addon command is not an equivalent way to launch a fully JSON-configured public server. Foxy distribution does not change this restriction. Bohemia documents the local-addon mode for addon testing; a production deployment that needs JSON settings requires a separately verified solution. See [Server Hosting](https://community.bistudio.com/wiki/Arma_Reforger%3AServer_Hosting) and [Startup Parameters](https://community.bistudio.com/wiki/Arma_Reforger%3AStartup_Parameters).

Run the following commands from the folder containing `ArmaReforgerServer.exe`, or use an absolute executable path.

For a JSON-configured server:

```text
ArmaReforgerServer.exe -config ".\configs\MainRepo.json" -profile "R:\Mods\ServerProfile" -maxFPS 60
```

To prepare that server's mod list, export a `game.mods` fragment:

```
foxy-server-backend-cli export-reforger-config config.json reforger_mods.json
foxy-server-backend-cli export-reforger-config config.json reforger_mods.json --include-optional
```

```json
{
  "game": {
    "mods": [
      { "modId": "ABCDEF1234567890", "name": "@mod_01" }
    ]
  }
}
```

Every exported mod needs a resolvable mod id. Merge the fragment's `game.mods` into your full server JSON; the fragment is not a complete server config. Exporting IDs does not publish private local addons to Workshop or make `-config` accept `-addons`.

For a local-addon server running Combat Ops on Arland:

```text
ArmaReforgerServer.exe -server "worlds/MP/Coop_CombatOps_Arland.ent" -addonsDir "R:\Mods\MainRepo" -addons ABCDEF1234567890 -profile "R:\Mods\ServerProfile" -maxFPS 60
```

The Arland world path is identified in the [Scenario Framework guide](https://jacobmeister.github.io/ScenarioFrameworkExplained/); confirm it exists in your installed game version. A JSON `scenarioId` uses a mission `.conf` resource, while `-server` expects the world `.ent`. They are not interchangeable.

| Parameter | Meaning |
|-----------|---------|
| `-server` | World `.ent` to host locally; ignores JSON server config |
| `-addonsDir` | Parent directory containing addon folders, not the individual `.pak` file; multiple roots are comma-separated |
| `-addons` | Comma-separated addon GUIDs from `.gproj`, without braces; replace the synthetic example ID |
| `-profile` | Profile parent directory; Reforger uses a `profile` subfolder for settings |
| `-maxFPS 60` | Caps server frame rate |
| `-addonDownloadDir` | Workshop download parent directory; downloads go into its `addons` subfolder |

Ensure dependencies are available in the game's addon search roots. `-addonsDir` searches local content; `-addonDownloadDir` changes Workshop storage and is not a verified fix for missing Workshop UI entries. When using the generator's `server_mod_line.txt` or `modLineFiles`, use the local-addon mode and a prefix pointing to the addon folders on the server. The generated addon parameters do not supply a world path or JSON server settings.

### Verify loading and repository metadata

- Check both server and client `console.log` files. An entry under `Available addons` only proves discovery; look under `Loaded addons` for the selected GUID and check for package, resource or script errors. Verify the addon's features in an actual scenario.
- Local addons launched with `-addons`/`-addonsDir` can be loaded while absent from the Workshop manager, as described in this [firsthand community report](https://steamcommunity.com/app/1874880/discussions/0/604147119540950733/). The top-right counter alone is not proof of failed loading. Reforger's [addon-manager API](https://community.bistudio.com/wikidata/external-data/arma-reforger/ArmaReforgerScriptAPIPublic/interfaceSCR__AddonManager.html) manages Workshop-item records separately. The exact reason for a missing UI entry or zero counter is not established by a successful engine load log.
- Copied Workshop `meta` files can retain paths from the original machine. Treat those as a diagnostic clue, not a confirmed cause or a reason to rewrite synced files automatically.
- A repository image must be configured, published and reachable. Check the generated `repoImagePath` and `repoImageChecksum` and request the image from the hosted output. Merely placing an image in the source folder does not publish it.
- Metadata-only changes, such as images or server details, can leave the addon checksum unchanged. If startup reports no content changes but still shows old metadata, run a remote repository recheck to fetch its metadata explicitly; regenerating identical addon content does not guarantee an update prompt.

---

## Custom Game Repositories

A repository can carry a game itself, not only mods for it. Players add it in a **Generic game** space; Foxy downloads and updates the game like any repository content, shows it as the game rather than as an addon, and **Launch** starts it from the downloaded folder. Set `"game": "generic"` in the config (`new config.json --game generic` writes a template) and name the folder that holds the build in `gameBuild`:

```json
{
  "repoName": "MainRepo",
  "game": "generic",
  "chunking": "content",
  "basePath": "R:/Mods/MainRepo",
  "gameBuild": { "folder": "@mod_01", "executable": "Game.exe" },
  "requiredMods": [],
  "optionalMods": [],
  "iconImagePath": "icon.png",
  "repoImagePath": "repo.png",
  "clientParameters": "",
  "servers": []
}
```

- `gameBuild.folder` is a folder under `basePath` (no wildcards). It can hold a single executable or a whole game tree: executable, data archives, libraries, and subfolders. It is published as a required, always-enabled entry, and players receive it as `<their repository folder>/@mod_01/`. Do not list it in `requiredMods` or `optionalMods` as well.
- `gameBuild.executable` is the file to start, relative to that folder (`Game.exe`, or `bin/Game.exe`). It must exist when the repository is generated and must stay inside the folder. Foxy starts it directly, without Steam, with the game folder as its working directory.
- `requiredMods` and `optionalMods` can still list mods for the game. Players see those as addons, and the Generic game's launch template receives them as `{mods}`.
- `"chunking": "content"` places part boundaries by content instead of every 5 MB (see [File parts](#file-parts)). When a rebuilt executable or data archive gains or loses bytes, the later parts keep their checksums, so clients download only the parts around the change and copy the rest from the file they already have. Use it for game builds and other large binaries that are rebuilt between releases, such as Godot executables with an embedded data pack, or Unity and Unreal data archives.
- No server launch line is printed and no `server_mod_line.txt` is written. `modLineFiles` is refused for a custom game, and `dlcContent` and `clientSide` are accepted with a warning and ignored.
- `clientParameters` is passed to the game as the repository's launch parameters.
- The generated `repo.json` carries `"game": "generic"`, `"chunking": "content"`, and `"gameBuild": { "modName": "@mod_01", "executable": "Game.exe" }`. `verify` reads `chunking` so it re-hashes the published files the same way.
- In a repository space, a mod shared by several repositories must use the same `chunking` in each of them.

```text
foxy-server-backend-cli new "R:\Mods\MainRepo\config.json" --game generic
foxy-server-backend-cli create "R:\Mods\MainRepo\config.json" "R:\Mods\Published\MainRepo" --atomic --yes
```

After each new build, replace the contents of the game folder with it and run the same `create` command. Players then see an update and download only the changed parts. The player side is described in the User Guide under **Custom game repositories**.

---

## Repository Structure

After `create`, the output directory has the following layout, depending on the generation mode.

### FoxyMode (default)

```
output/
  repo.json                     # Repository manifest (mod lists empty, data in foxy_addons.json)
  foxy_addons.json              # Repo-level mod listing with BLAKE3 checksums
  server_mod_line.txt           # The printed server launch line
  icon.png                      # Repository icon (if configured)
  repo.png                      # Repository banner image (if configured)
  keys/                         # Combined keys (only with --collect-keys)
  @cba_a3/
    foxy_addon.json             # Per-mod manifest (BLAKE3 checksums, file parts)
    addons/
      cba_main.pbo
      ...
    keys/
      cba.bikey
  ...
```

In FoxyMode, `repo.json` holds the metadata (name, servers, client parameters, and so on) but its `requiredMods` and `optionalMods` arrays are **empty**. The mod listings with BLAKE3 checksums live in `foxy_addons.json`, and each mod folder has a `foxy_addon.json` with per-file and per-part checksums.

### SwiftyMode

```
output/
  repo.json                     # Repository manifest with MD5 checksums in mod lists
  icon.png
  repo.png
  @cba_a3/
    mod.srf                     # Per-mod manifest (MD5 checksums, Swifty-compatible)
    addons/
      ...
  ...
```

### HybridMode

```
output/
  repo.json                     # Has foxyMode field + MD5 mod lists for Swifty clients
  foxy_addons.json              # BLAKE3 mod listing for Foxy clients
  @cba_a3/
    mod.srf                     # Swifty manifest (MD5)
    foxy_addon.json             # Foxy manifest (BLAKE3)
    addons/
      ...
  ...
```

HybridMode writes both artifact sets. `repo.json` includes the `foxyMode` marker (so Foxy clients read `foxy_addons.json`) and the MD5 mod lists (so Swifty clients sync normally).

### File parts

Files are split into **parts** for fine-grained checksums and delta updates:

- **PBO files** (Arma 3) are parsed into a `$$HEADER$$` part, one part per archived entry, and a `$$END$$` tail part, so clients patch at the entry level.
- **PAK files** (Arma Reforger, PAC1 format) are parsed into a `$$HEADER$$` part, one part per entry, `$$GAP:n$$` parts for bytes between entries, and a `$$END$$` part.
- **Other files** are split into 5 MB chunks. A 12 MB file becomes three parts: `filename_5000000`, `filename_10000000`, `filename_12000000`.
- **Other files with `"chunking": "content"`** are split where their content says, into parts of 256 KiB to 4 MiB (about 1 MiB on average). Names keep the `filename_<end offset>` form. An inserted or removed byte changes only the parts around it. PBO and PAK archives keep their per-entry parts. All Foxy clients read these parts. Changing `chunking` on an existing repository makes clients download each affected file once in full.
- **`.srf` files** in source mod folders are skipped (they are regenerated), and so is a `foxy_addon.json` at a mod's root, so an earlier output can be reused as a source.

### The `foxyMode` field

FoxyMode and HybridMode write `"foxyMode": "FoxyModeV1"` into `repo.json`. That tells Foxy clients to fetch `foxy_addons.json` and the per-mod `foxy_addon.json` files. Without it (SwiftyMode), clients use the MD5 lists in `repo.json` and the `mod.srf` files.

---

## Repository Spaces

A **repository space** groups several repositories under one URL, so players can add your whole community setup in one step. The Foxy client downloads every repository of a space into one shared folder.

### Generating a space with `create-space`

`create-space` generates every repository of a space plus the `repository_space.json` that links them. The space config only points at the per-repository configs that `create` already uses:

```
foxy-server-backend-cli new-space space.json
foxy-server-backend-cli create-space space.json ./www --layout pool
```

```json
{
  "name": "My Community",
  "baseUrl": "https://mods.example.com/repos/",
  "appUpdateUrl": "https://mods.example.com/foxy/",
  "iconImagePath": "icon.png",
  "repoImagePath": "space.png",
  "repositories": [
    { "config": "modern/config.json", "folder": "modern", "required": true },
    { "config": "vietnam/config.json", "folder": "vietnam", "name": "Vietnam", "required": true },
    { "config": "ww2/config.json", "folder": "ww2", "required": false, "address": "https://cdn.example.com/ww2/" }
  ]
}
```

| Field | Description |
|-------|-------------|
| `name` | Display name of the space |
| `baseUrl` | Public URL the repository folders are served under |
| `appUpdateUrl` | Optional Foxy app update source, written into `repository_space.json` |
| `iconImagePath`, `repoImagePath` | Space icon and banner, relative to the space config |
| `repositories[].config` | Repository config file, relative to the space config |
| `repositories[].folder` | Output subfolder (defaults to the config file name) |
| `repositories[].name` | Display name (defaults to the repository's `repoName`) |
| `repositories[].address` | Public URL (defaults to `<baseUrl>/<folder>/`) |
| `repositories[].required` | Whether players must take this repository (default `true`) |

Each repository lands in `<output>/<folder>/`. Each repository's `basePath` keeps the `create` meaning and resolves from the working directory.

### Layouts

`--layout` decides how mod folders are stored:

| Layout | What happens | When to use |
|--------|--------------|-------------|
| `copy` (default) | Every repository gets a full copy of every mod, exactly like running `create` per repository. | No symlink support on the server or web host. |
| `pool` (recommended) | Each distinct mod is copied once into `<output>/pool` (`--pool-dir` to move it), and every repository holds a relative symlink to it. Shared mods use disk space once and the whole tree can be moved. | Everywhere symlinks work: Linux hosts, or Windows with Developer Mode or an elevated shell. |
| `link` | Every repository symlinks straight to the source mod folder; nothing is copied. Requires `--yes`: manifests are written into the source folders, and any later change there breaks the published checksums until you run `create-space` again. | Only when the sources are already the served copy and never edited in place. |

The web server must follow symlinks for `pool` and `link` (nginx does by default; Apache needs `Options FollowSymLinks`).

A mod name that appears in several repositories must hash identically in all of them, because clients download a space into one shared folder. `create-space` refuses otherwise. Existing real folders are never replaced by symlinks.

### Space options

- `--mode`, `--threads`, `--app-update-url`, the launch line options, `--dry-run`, `--incremental`, `--atomic --yes`, and `--report` work as for `create`, applied to every repository. Arma 3 and Reforger repositories get a launch line and `server_mod_line.txt`; custom game repositories do not. Each applicable repository's `modLineFiles` are rewritten with its own line.
- `--collect-keys` gathers the keys of the whole space into `<output>/keys`. `--per-repo-keys` also writes each repository's keys (plus `--additional-keys`) into `<output>/<folder>/keys`, so a server can symlink one repository's keys folder directly.
- `--only <folder>` (repeatable) regenerates only the selected repositories and keeps the full space manifest. It refuses mods shared with repositories you did not select, and it cannot rebuild the combined keys folder.
- `--prune-unused-optionals` works with `copy` and `pool`, not with `link`.
- For pool output, `--clean --dry-run` lists orphaned generated pool folders and their symlinks, and `--clean --yes` removes them after a successful generation. Cleanup never touches unrelated folders, and a pool outside the output needs an inventory from an earlier `create-space` run.

### The generated `repository_space.json`

```json
{
  "name": "My Community",
  "image": "space.png",
  "imageChecksum": "<sha1>",
  "icon": "icon.png",
  "iconChecksum": "<sha1>",
  "spaceChecksum": "<sha1>",
  "appUpdateUrl": "https://mods.example.com/foxy/",
  "entries": [
    { "Name": "Modern", "Address": "https://mods.example.com/repos/modern/", "Requiered": true },
    { "Name": "Vietnam", "Address": "https://mods.example.com/repos/vietnam/", "Requiered": true },
    { "Name": "WW2", "Address": "https://cdn.example.com/ww2/", "Requiered": false }
  ]
}
```

| Field | Description |
|-------|-------------|
| `name` | Display name for the space in the Foxy client |
| `image`, `imageChecksum` | Banner image path and checksum |
| `icon`, `iconChecksum` | Icon image path and checksum |
| `spaceChecksum` | SHA-1 of what clients show for the space, so they can tell when it changed (see below). Optional; older clients ignore it |
| `appUpdateUrl` | URL of a `foxy-app-updater.json` manifest (fills the client's update source) |
| `entries[].Name` | Display name for the repository |
| `entries[].Address` | Full URL of the repository root (where `repo.json` lives) |
| `entries[].Requiered` | `true` for required repositories, `false` for optional ones (the legacy spelling is intentional) |

`spaceChecksum` is the lowercase hex SHA-1 of the compact JSON array `[name, imageChecksum, iconChecksum, [[Name, Address, Requiered], ...]]`, built from the values exactly as they appear in the file, with the entries in file order. For the example above with real checksums that is the SHA-1 of `["My Community","<sha1>","<sha1>",[["Modern","https://mods.example.com/repos/modern/",true],...]]`. Changing the name, either image, or any entry (including its order or required flag) changes it; `appUpdateUrl` and the image paths are not covered.

You can still write this file by hand if you generate repositories separately; keep the same shape. When you edit it by hand, either regenerate `spaceChecksum` or delete the field. Foxy recomputes the checksum from the content anyway and logs a warning when the published value is stale, so a stale value never hides a change, but deleting it keeps the file honest.

### How spaces work with Foxy

When a player adds the space URL, the client reads `repository_space.json` and adds the required repositories automatically; optional ones can be picked by the player. Clients reread the manifest at launch, every 30 minutes while Foxy runs, and on **Refresh from server** (the globe button on the space page, or the space's context menu). When the checksum matches what the client already has, nothing else is touched; when it changed, the client takes the new name, images, `appUpdateUrl`, and repository list, so a repository you add to the space later shows up for players without them re-adding the space. They still choose whether to install it, and nothing is removed from their disk. The space's `appUpdateUrl` has the **highest priority** for filling the client's app update source, ahead of any `repo.json` value.

Serve the space folder with its images:

```
https://mods.example.com/repos/
  repository_space.json
  space.png
  icon.png
  modern/  vietnam/  pool/
```

Players then add `https://mods.example.com/repos/` as a repository space in Foxy.

---

## Checks, Previews, and Safe Publishing

```
foxy-server-backend-cli validate config.json --output ./www/repo
foxy-server-backend-cli validate space.json --space --output ./www
foxy-server-backend-cli audit-keys config.json --strict
foxy-server-backend-cli create-space space.json ./www --layout pool --dry-run --clean
foxy-server-backend-cli create-space space.json ./www --layout pool --incremental --report
foxy-server-backend-cli verify ./www
foxy-server-backend-cli diff ./old-www ./www --json
```

- **`validate`** checks the config, mod references, published names, and `modLineFiles` without hashing or writing. With `--output`, it also checks output path overlap and layout collisions.
- **`--dry-run`** on `create` and `create-space` lists planned copies, manifest writes, links, pruning, key collection, launch-script updates, and pool cleanup without writing.
- **`--incremental`** reuses a mod's previous checksums when its source files have the same paths, sizes, and modification times and the published files still match. It keeps a `.foxy-hash-cache.json` in the output.
- **`verify`** rehashes generated output against its manifests. Use it when you want a full content check, for example after `--incremental` runs.
- **`--report`** and **`diff`** compare mod checksums before and after and estimate client download bytes from the sizes of added and changed mods.
- **`--atomic --yes`** builds in a sibling folder and replaces the whole output only after success. If publishing fails, it tries to restore the old output. It cannot be combined with `--incremental` or a custom pool or keys destination.
- **`audit-keys`** checks that each PBO has a nearby `.bisign`, that the matching `.bikey` is available (including `--additional-keys`), and that no two different keys share a name. `--strict` fails on findings. It does not verify signatures cryptographically.
- **`--json`** gives one result object on stdout for every command, with checksums or launch lines on success and a message on error.

---

## App Updates Distribution

Foxy supports **self-hosted app updates**, so each community can distribute Foxy releases itself through a `foxy-app-updater.json` manifest.

### Initial setup

```
foxy-server-backend-cli setup-app-updater \
  --version 1.2.1 \
  --windows-installer ./installers/Foxy-1.2.1-setup.exe \
  --linux-installer ./installers/Foxy-1.2.1-linux-x86_64-installer.sh \
  --linux-aarch64-installer ./installers/Foxy-1.2.1-linux-aarch64-installer.sh \
  --changelog ./CHANGELOG.md \
  --output ./update-server
```

This produces:

```
update-server/
  foxy-app-updater.json         # Update manifest (BLAKE3 hashes, schema version 1)
  changelogs/
    1.2.1.json                  # Structured changelog extracted from CHANGELOG.md
```

Requirements:

- A Windows installer is required for every version; the Linux x86_64 and Linux ARM64 installers are optional.
- `--changelog` points to a `CHANGELOG.md` file. Headings like `# 1.1.0` and `# [1.1.0] - 2026-03-28` are supported, and the version must exist in the file.

### Adding new releases

```
foxy-server-backend-cli new-app-update \
  --version 1.2.2 \
  --windows-installer ./installers/Foxy-1.2.2-setup.exe \
  --linux-installer ./installers/Foxy-1.2.2-linux-x86_64-installer.sh \
  --linux-aarch64-installer ./installers/Foxy-1.2.2-linux-aarch64-installer.sh \
  --changelog ./CHANGELOG.md \
  --output ./update-server
```

This keeps every earlier version in the manifest (so players can downgrade from the Version Browser), prepends the new one, and updates `latest`. A version that already exists cannot be added again; remove it from `foxy-app-updater.json` first if you need to republish.

### Server directory layout

```
update-server/
  foxy-app-updater.json
  installers/
    Foxy-1.2.2-setup.exe
    Foxy-1.2.2-linux-x86_64-installer.sh
    Foxy-1.2.2-linux-aarch64-installer.sh
    Foxy-1.2.1-setup.exe
    ...
  changelogs/
    1.2.2.json
    1.2.1.json
```

The manifest references installers by relative path (for example `installers/Foxy-1.2.2-setup.exe`), so place them in an `installers/` folder next to it.

### Connecting updates to repositories

To let Foxy clients find your update source automatically, set `appUpdateUrl` in either:

- **`repository_space.json`** (highest priority), through the space config or by hand.
- **`repo.json`**, through `appUpdateUrl` in `config.json`.
- **The command line** when building: `--app-update-url https://mods.example.com/updates/` (wins over the config value).

The URL points to the folder containing `foxy-app-updater.json`. Clients fill it into their settings only when their update source field is empty; a URL the player typed is treated as an override and never replaced.

### Manifest format

```json
{
  "schemaVersion": 1,
  "latest": "1.2.2",
  "versions": [
    {
      "version": "1.2.2",
      "changelog": "changelogs/1.2.2.json",
      "platforms": {
        "windows-x86_64": {
          "installerPath": "installers/Foxy-1.2.2-setup.exe",
          "installerHash": "<blake3-hex>",
          "installerSize": 12345678
        },
        "linux-x86_64": {
          "installerPath": "installers/Foxy-1.2.2-linux-x86_64-installer.sh",
          "installerHash": "<blake3-hex>",
          "installerSize": 9876543
        },
        "linux-aarch64": {
          "installerPath": "installers/Foxy-1.2.2-linux-aarch64-installer.sh",
          "installerHash": "<blake3-hex>",
          "installerSize": 9765432
        }
      }
    },
    {
      "version": "1.2.1",
      "changelog": "changelogs/1.2.1.json",
      "platforms": { ... }
    }
  ]
}
```

Platform keys are `windows-x86_64`, `linux-x86_64`, and `linux-aarch64`. The client verifies the installer's BLAKE3 hash before running it.

---

## Configuration Reference

### config.json (input to `create`)

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `repoName` | string | yes | - | Display name of the repository |
| `game` | string | no | `"arma3"` | `arma3`, `reforger`, or `generic` (a custom game, no server launch line); selects the server launch line |
| `chunking` | string | no | `"fixed"` | `fixed` (5 MB parts) or `content` (content-defined parts for files that are not PBO or PAK archives) |
| `gameBuild` | object | no | omitted | `{ folder, executable }`: the folder under `basePath` holding a custom game build and the file to start in it; requires `"game": "generic"` |
| `basePath` | string | yes | - | Root folder containing the mod folders |
| `appUpdateUrl` | string | no | `""` | Foxy app update source written into `repo.json` |
| `requiredMods` | array | no | `[]` | Required mod references |
| `optionalMods` | array | no | `[]` | Optional mod references |
| `iconImagePath` | string | no | `""` | Repository icon (relative to `basePath`) |
| `repoImagePath` | string | no | `""` | Repository banner (relative to `basePath`) |
| `clientParameters` | string | no | `""` | Launch parameters suggested to clients |
| `dlcContent` | object or array | no | omitted | Arma 3 Creator DLCs the repository uses |
| `modLineFiles` | array | no | `[]` | Launch scripts whose launch parameters are rewritten (relative to the config file) |
| `repoBasicAuthentication` | object | no | empty | HTTP Basic Auth credentials for protected repositories |
| `version` | string | no | `"3.2.0.0"` | Repository protocol version |
| `servers` | array | no | `[]` | Game server entries |

#### Mod reference format

```json
{ "modName": "@ace", "enabled": true, "clientSide": false }
```

- `modName` (string, required) - folder name or path relative to `basePath`. Supports glob wildcards in the last segment (`@*`, `collections/*`, `mods/@[ac]*`). Absolute paths work for mods stored outside `basePath`.
- `enabled` (boolean, default `true`) - whether the mod is checked by default in the Foxy client.
- `clientSide` (boolean, default `false`, Arma 3) - the mod runs only on players' machines and is left out of the server launch line.

#### Server entry format

```json
{
  "name": "Main Server",
  "address": "arma3.example.com",
  "port": "2302",
  "password": "",
  "battleEye": true
}
```

- `name` (string, required) - display name.
- `address` (string, required) - server IP or hostname.
- `port` (string, required) - game port.
- `password` (string, default `""`) - server password.
- `battleEye` (boolean, default `false`) - whether BattlEye is enabled.

#### Basic authentication

```json
"repoBasicAuthentication": {
  "username": "myuser",
  "password": "mypassword"
}
```

When set, Foxy clients send HTTP Basic Auth headers with every request to this repository. Leave both fields empty to disable it, and make sure the web server enforces the same credentials.

### repo.json (generated output)

| Field | Description |
|-------|-------------|
| `repoName` | Repository display name |
| `game` | Present only for non-Arma 3 repositories, for example `"reforger"` or `"generic"` |
| `chunking` | `"content"` when the config uses content chunking (omitted for `fixed`) |
| `gameBuild` | `{ modName, executable }` for a custom game build (omitted when not configured) |
| `checksum` | Repository-level checksum (BLAKE3 in FoxyMode, SHA-1 in SwiftyMode) |
| `foxyMode` | `"FoxyModeV1"` in FoxyMode and HybridMode |
| `requiredMods` | Array of `{ modName, checkSum, enabled, clientSide? }` (empty in FoxyMode, MD5 in Swifty/Hybrid) |
| `optionalMods` | Same shape as `requiredMods` |
| `iconImagePath`, `iconImageChecksum` | Icon image path and SHA-1 checksum |
| `repoImagePath`, `repoImageChecksum` | Banner image path and SHA-1 checksum |
| `appUpdateUrl` | Foxy app update source (omitted if empty) |
| `clientParameters` | Suggested launch parameters |
| `repoBasicAuthentication` | `{ username, password }` for HTTP Basic Auth |
| `version` | Repository protocol version |
| `servers` | Game server entries |
| `dlcContent` | Full Creator DLC object (omitted when not configured) |

`clientSide` appears on a mod entry only when it is `true`.

### foxy_addons.json (FoxyMode/HybridMode)

| Field | Description |
|-------|-------------|
| `version` | `"FoxyModeV1"` |
| `hashAlgorithm` | `"BLAKE3"` |
| `checksum` | Repository-level BLAKE3 checksum |
| `requiredMods` | Array of `{ modName, checkSum, enabled, clientSide? }` with BLAKE3 checksums |
| `optionalMods` | Same shape as `requiredMods` |

### foxy_addon.json (per mod, FoxyMode/HybridMode)

| Field | Description |
|-------|-------------|
| `name` | Mod folder name (lowercased) |
| `version` | `"FoxyModeV1"` |
| `checksum` | Mod-level BLAKE3 checksum |
| `hashAlgorithm` | `"BLAKE3"` |
| `files` | Array of file entries |

Each file entry:

| Field | Description |
|-------|-------------|
| `path` | Relative file path (forward slashes) |
| `checksum` | File-level BLAKE3 checksum |
| `length` | File size in bytes |
| `fileType` | `"FoxyPboFile"` for `.pbo` files, `"FoxyFile"` for all others |
| `parts` | Array of `{ path, checksum, start, length }` parts |

### DLC content

`dlcContent` tells players which Arma 3 Creator DLCs the repository uses:

```json
"dlcContent": {
  "csla": false,
  "ef": false,
  "gm": true,
  "rf": false,
  "spe": true,
  "vn": false,
  "ws": false
}
```

The config accepts that object form or a shorthand list of the enabled codes:

```json
"dlcContent": ["gm", "spe"]
```

Both produce the full object in `repo.json`. Valid codes are `csla` (CSLA Iron Curtain), `ef` (Expeditionary Forces), `gm` (Global Mobilization), `rf` (Reaction Forces), `spe` (Spearhead 1944), `vn` (S.O.G. Prairie Fire), and `ws` (Western Sahara). An unknown code fails config parsing instead of being dropped. Omit `dlcContent` to leave it out of `repo.json`.

The enabled codes also lead the printed `-mod=` server line. Players apply the selection through **Auto apply repo.json DLC content** in Foxy, and when they join a server Foxy offers to match the Creator DLCs that server actually runs.

---

## Hosting Behind a Reverse Proxy

### nginx

```nginx
server {
    listen 443 ssl;
    server_name mods.example.com;

    ssl_certificate     /etc/ssl/certs/mods.example.com.pem;
    ssl_certificate_key /etc/ssl/private/mods.example.com.key;

    # Repository root
    location /repo/ {
        alias /var/www/foxy-repo/;
        autoindex off;

        # Allow large mod file downloads
        client_max_body_size 0;

        # Cache manifests briefly so updates propagate quickly
        location ~* \.(json)$ {
            expires 5m;
            add_header Cache-Control "public, max-age=300";
        }

        # Cache mod files longer (they are checksum-verified)
        location ~* \.(pbo|pak|bikey|bisign|cpp|bin|png|jpg)$ {
            expires 7d;
            add_header Cache-Control "public, max-age=604800";
        }
    }

    # App update server
    location /foxy/ {
        alias /var/www/foxy-updates/;
        autoindex off;
    }
}
```

nginx follows symlinks by default, so `pool` and `link` space layouts work as is.

### Apache

```apache
<VirtualHost *:443>
    ServerName mods.example.com

    SSLEngine on
    SSLCertificateFile    /etc/ssl/certs/mods.example.com.pem
    SSLCertificateKeyFile /etc/ssl/private/mods.example.com.key

    DocumentRoot /var/www/foxy-repo

    <Directory /var/www/foxy-repo>
        Options -Indexes +FollowSymLinks
        AllowOverride None
        Require all granted
    </Directory>

    # Cache control for JSON manifests
    <FilesMatch "\.(json)$">
        Header set Cache-Control "public, max-age=300"
    </FilesMatch>

    # Cache control for mod files
    <FilesMatch "\.(pbo|pak|bikey|bisign)$">
        Header set Cache-Control "public, max-age=604800"
    </FilesMatch>

    Alias /foxy/ /var/www/foxy-updates/
</VirtualHost>
```

### General tips

- **HTTPS is recommended.** Foxy supports HTTP, but HTTPS protects file integrity in transit.
- **Disable directory listings.** The manifests contain everything clients need.
- **Set cache headers.** JSON manifests (`repo.json`, `foxy_addons.json`, `foxy_addon.json`, `repository_space.json`) should have short TTLs (5-15 minutes) so updates propagate quickly. Mod files can be cached longer because clients verify them by checksum.
- **HTTP range requests** must work, because Foxy downloads large files in parallel ranges and patches individual parts. Foxy rejects a range response that is not `206 Partial Content` or whose `Content-Range` does not match the request, so check any proxy or CDN in front of the server.
- **Busy responses.** Foxy retries `408`, `429`, and `5xx` responses a few times per request and honors `Retry-After` up to 30 seconds, so rate limiting slows a download instead of failing it straight away.
- **Compression.** Enable gzip or brotli for `.json` files. Mod archives are already compressed and do not benefit from transport compression.
- **Basic authentication.** If you set `repoBasicAuthentication`, enforce the same credentials in the web server.
- **Bandwidth.** Large modsets can be tens of gigabytes. Delta patching reduces update sizes significantly, but plan hosting for full first downloads.

---

## Maintaining Repositories

### Updating after mod changes

When mods change, run the same command again:

```
foxy-server-backend-cli create config.json ./output
```

This re-discovers the files, recomputes checksums, regenerates the manifests, and rewrites any `modLineFiles`. Add `--incremental` to skip rehashing mods whose files did not change, `--report` to see what changed, and `--atomic --yes` so players never see a half-written output.

Foxy clients detect changes through the repository checksum in `repo.json` and download only the files and file parts that differ. Before an update starts, clients also reread the manifests of the addons they are about to update, even when the repository checksum is unchanged, so a publish that lands while a player has an update pending is picked up. A client that reads a half-published state retries once with fresh requests and then stops with an error instead of applying it.

### Adding or removing mods

1. Edit `requiredMods` / `optionalMods` in `config.json`.
2. Run `validate` to catch typos.
3. Run `create` (or `create-space`) again. The generated manifests and the launch line reflect the new list.

Publishing an empty repository (explicitly empty `requiredMods` and `optionalMods`) or an empty mod does not remove anything from players' disks. Foxy keeps the installed files and offers **Review removal of retained files** for a separate confirmation. A failed or omitted manifest is never treated as empty.

### Switching generation modes

You can switch modes at any time with `--mode`. Foxy clients detect the `foxyMode` field and use the new manifests; legacy Swifty clients stop working unless you use HybridMode.

A recommended migration path:

1. Start with `--mode hybrid` to serve both clients.
2. Wait for your community to switch to Foxy.
3. Switch to `--mode foxy` to drop the legacy artifacts and benefit from faster BLAKE3 hashing.

### Automation

```bash
#!/bin/bash
# rebuild-repo.sh - run after mod updates
set -e

SPACE_CONFIG="/etc/foxy/space.json"
OUTPUT_DIR="/var/www/foxy-repo"

foxy-server-backend-cli validate "$SPACE_CONFIG" --space --output "$OUTPUT_DIR"
foxy-server-backend-cli --no-progress --json create-space "$SPACE_CONFIG" "$OUTPUT_DIR" \
  --layout pool --incremental --collect-keys --threads 8 > /var/log/foxy-rebuild.json

echo "Repositories updated at $(date)"
```

Use `--threads` matching your CPU cores for large repositories; BLAKE3 (FoxyMode) benefits most. With `modLineFiles` set, your server start scripts already carry the new mod line when the command returns.

---

## Troubleshooting

### "basePath does not exist or is not a directory"

`basePath` must point to an existing folder. On Windows, use forward slashes or escaped backslashes in JSON:

```json
"basePath": "D:/Arma3/ServerMods"
```

### "Mod directory does not exist"

A mod name in `requiredMods` or `optionalMods` resolves to a folder that does not exist under `basePath`. Check the spelling and the folder.

### "Wildcard pattern matched no directories"

A wildcard such as `@*` found no matching folders. This is a warning; the build continues with the mods it found.

### "No mods found after expanding all mod references"

After expanding every reference, no mod folders were found. Check `basePath` and your mod references.

### A `modLineFiles` entry fails the run

The listed file is missing, is not UTF-8 text, or has no `-mod=` (Arma 3) or `-addonsDir`/`-addons` (Reforger) parameter to replace. Fix the path (relative paths resolve from the config file's folder) or add the parameter to the script. `validate` reports the same error without generating anything.

### `create-space` refuses a shared mod

The same mod name produced different checksums in two repositories, usually because the repositories point at different source folders or one source changed mid-build. Point both configs at the same source, or rename one of the mods.

### Symlink errors with `pool` or `link`

Creating symlinks on Windows needs Developer Mode or an elevated shell. Use `--layout copy` where symlinks are not available.

### "--yes" is required

Some actions remove published data or write into your sources: `--layout link`, `--clean`, `--atomic`, and `--prune-unused-optionals` when earlier optionals are published. Run with `--dry-run` first to see what would change, then add `--yes`.

### Clients see no update after rebuild

1. Check that `checksum` in `repo.json` changed. Identical mod files produce the same checksum.
2. Check web server caching. A long cache on JSON files serves a stale `repo.json`.
3. Make sure the client's repository URL points to the folder that contains `repo.json`.

### Clients report inconsistent metadata

Foxy checks that the file parts in a manifest cover each file's declared size. If they do not, it retries once with a fresh request and then stops before any cleanup. Regenerate the repository, preferably with `--atomic --yes`, check the output with `verify`, and clear any cache or CDN in front of it. Files that fail their checksum after download are rolled back on the client the same way.

### Clients stop on a missing file (HTTP 404 or 410)

The client rolls back the attempt and refreshes its file lists on the next try. If the fresh plan still requests a missing file, the published files and manifests disagree: run `verify` on the output and regenerate it. Check that the files were copied to the web root, and that the URL paths match their case on case-sensitive hosts.

### Clients cannot connect

1. Open `https://mods.example.com/repo/repo.json` in a browser; it should return valid JSON.
2. If you use basic authentication, check that the credentials match between `config.json` and the web server.
3. Check that the web server supports HTTP range requests.
4. For `pool` or `link` layouts, check that the web server follows symlinks.

### Archive parse warnings

"Content format parse failed for ..., treating as single file" means a `.pbo` or `.pak` archive is malformed or uses an unsupported layout. The file is hashed as plain chunks, which works but disables per-entry patching for that file.

### Large repository build times

- Increase threads: `--threads 8` or more, matching your CPU cores.
- Use FoxyMode (BLAKE3).
- Use `--incremental` for routine rebuilds and `verify` occasionally for a full check.
- Keep source and output on fast storage (SSD preferred).
- For very large modsets, split into several repositories in one space with `--layout pool`.

### Version already exists in app updater manifest

`new-app-update` refuses a version that is already in the manifest. To republish one:

1. Open `foxy-app-updater.json`.
2. Remove the version from the `versions` array.
3. Update `latest` if needed.
4. Run `new-app-update` again.
