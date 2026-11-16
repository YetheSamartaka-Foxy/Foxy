# Arma server missions

The Server page has a **Mission library** for the active Arma 3 game space. Import a mission PBO or an unpacked `Name.World` folder containing `mission.sqm`. The actual filename determines the engine template, such as `Example.VR`; a display title does not change it. Import leaves the source untouched. A folder becomes a deterministic PBO with the original member bytes, while an existing PBO is retained byte-for-byte.

Choose **Review mission import** to inspect the template, world, full PBO size/hash, recognized title/author/addons/parameter choices and diagnostics. **Apply reviewed mission to library** saves an immutable revision. Cancel leaves the library unchanged. Files, member names, metadata and total bytes are bounded; links, traversal, case collisions, malformed archives and sources changed after review are refused. The payload limit is 512 MiB.

Metadata is advisory. Foxy reads a bounded literal subset of text `mission.sqm` and `description.ext`, with source offsets and lines. It does not execute SQF, expand includes/macros, decode binary configuration or decompress mission metadata. Compressed PBO members are retained without interpretation; unsupported metadata is shown as unknown. Required addons are declarations, not proof that dependencies are installed.

## Ordered cycles

After refreshing the library, **Add revision to mission cycle** adds that exact revision to the current draft. Save the draft before reviewing content bindings and staging. Configure each entry's Enabled state, difficulty and typed numeric Params; move entries up/down or remove their binding. Disabled entries do not render or require deployment content. Removing a binding never deletes an imported revision or its external source.

The cycle can contain file templates from existing staged content as well as library revisions. **Addon class mission** explicitly identifies an addon class rather than a filename. Class availability is currently unverified and blocks activation; a similarly named PBO is not evidence for an addon class. Keep an operator-owned server configuration authoritative: its cycle changes through the configuration-import review, and structured cycle edits are disabled while that file owns the server role.

The existing server overrides provide random mission order, mission whitelist and automatic next-mission selection. Persistence and `-autoInit` are separate controls. Combining auto-init with custom per-entry Params shows a warning because behavior can depend on the engine and mission; the accepted native runtime has focused parameter evidence. Verify the actual mission values and defaults before relying on them. Custom difficulty requires the managed profile or a separately reviewed operator profile.

**Review current library revision** refreshes a source under its stable source identity. Applying a changed source creates another immutable revision. Existing cycle entries stay pinned to the old revision until **Rebind to current revision** is explicitly chosen and the draft is saved. Existing configuration archives, content stages and running/committed runtimes remain unchanged. Stage the new choice and prepare/activate a new deployment to use it.

## HTTP acquisition and transfer

**Review mission download** acquires a PBO from an explicit HTTP(S) URL and checks the expected lowercase BLAKE3 digest before review. Supply its `Name.World` template and expected hash. Credentials, query strings, fragments and redirects are refused. Acquisition is bounded and cancellable; it never runs downloaded scripts during inspection. Applying the review saves a library revision. This direct download is separate from Steam or Workshop acquisition.

The engine's `missionHTTPDownloadBaseURL` setting controls player mission transfer, rather than Foxy's library acquisition. Review that configured host before use: the engine may send player/server identifiers there. Ordinary game transfer supports missions up to 300 MiB; the optional HTTP path supports up to 512 MiB and may fall back to game transfer. Foxy does not host HTTP missions. A larger mission requires an appropriate usable transfer path and operator setup. See [Bohemia's HTTP transfer contract](https://community.bistudio.com/wiki/Arma_3%3A_Mission_HTTP_Download).

## CLI

```text
Foxy server mission list --json
Foxy server mission import --source <Example.World.pbo> --json
Foxy server mission import --source <Example.World-folder> --apply --yes --json
Foxy server mission refresh <mission_id> --apply --yes --json
Foxy server mission acquire --url <http-or-https-url> --template Example.World --expected-blake3 <64-lowercase-hex> --apply --yes --json
Foxy server mission cycle <instance_id> --expected-revision 1 --json
Foxy server mission cycle <instance_id> --expected-revision 1 --file <cycle.json> --apply --yes --json
```

`cycle.json` is a JSON array of typed entries; see [the example](../examples/json/server_backend/arma3_mission_cycle.json). Read the import result for actual mission/revision IDs rather than copying example placeholders. The cycle command checks the instance revision before saving. Apply requires `--yes`; review omits `--apply`. All commands accept `--game-space`. Acquisition review downloads into temporary storage; `--dry-run` validates acquisition inputs without network requests, temporary files, ownership claims or persistent writes.

Library data lives under `games/<space_id>/server/mission-library/`. Immutable PBO snapshots join the existing content stage and its archived manifest, then the deployment record, preparation and verified instance runtime. Nothing is installed into an adopted server root or a shared mod source. Missing, conflicting or changed mission evidence blocks preparation/activation. Portable instance packs retain mission references, not mission bytes; import/acquire locally and explicitly rebind them before staging.
