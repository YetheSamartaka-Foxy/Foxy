---
name: foxy-manifest-state-validation
description: "Validate Foxy repository and addon manifest state across legacy and FoxyMode formats, stored metadata, file parts, and generated examples."
---

# Foxy manifest state validation

Use this workflow when a repository looks up to date despite changed metadata, files vanish from sync, part rows disagree with a manifest, or a manifest change affects updates.

## Workflow

1. Identify the format and producer: repository `repo.json`, legacy `mod.srf`, FoxyMode `foxy_addons.json` and `foxy_addon.json`, or app-update manifest. Do not assume the legacy mod list is populated in FoxyMode repositories.
2. Read the owning parser and the exact serializer or server producer. Compare accepted field names, casing, aliases, defaults, checksum meaning, path joins, and version gates.
3. Trace parsed metadata into database rows. Verify remote paths and local paths separately, preserve `data_order`, and check that stored file and part state is refreshed or carried according to the sync contract.
4. Compare against the relevant example JSON. If the schema or generated output changes, update the matching example and preserve sanitized placeholder identifiers.
5. Add a parser or state regression test for the changed contract. For an end-to-end claim, use the testkit's manifests-only mirror or a reviewed isolated origin; do not contact an arbitrary live repository.

## Evidence

Report the source format, parser/producer, fields that differ, affected DB identity, expected versus observed state, and test level. A successful JSON parse does not establish a complete remote graph or correct sync queue.

## Boundaries

Do not edit runtime DB files, caches, or user manifests. Stop if the producing server version or authoritative manifest is unknown; request that exact artifact or use a safe example.

## References

- [Manifest and example rules](../../conventions/EXAMPLES_CONVENTIONS.md)
- [Sync metadata and checksum contract](../../conventions/SYNC_ALGO_CONVENTION.md)
- [Remote repository refresh](../../src/core/tasks/remote_repository.rs)
- [Addon file manifest parser](../../src/core/tasks/remote_files.rs)
- [Remote addon upsert](../../src/core/tasks/remote_mods/upsert.rs)
- [Remote repository example](../../examples/json/remote_repositories/repo.json)
