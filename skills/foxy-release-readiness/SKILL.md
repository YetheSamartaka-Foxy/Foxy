---
name: foxy-release-readiness
description: Review Foxy release readiness across version derivation, build provenance, update manifests, redistributables, installer packaging, and data preservation.
---

# Release readiness

Use this skill to prepare or review a Foxy release candidate and its distributable artifacts.

## Workflow

1. Read [testing conventions](../../conventions/TESTING_CONVENTIONS.md), [server CLI notes](../../foxy-server-backend-cli/AGENTS.md) when its artifact is included, and the release-related sections of [root instructions](../../AGENTS.md).
2. Confirm the app version source: `build.rs` derives it from the first `# MAJOR.MINOR.PATCH` heading in `CHANGELOG.md`. Inspect the current heading and source commit metadata rather than inferring a version from a binary filename.
3. Trace update behavior through [app update tasks](../../src/core/tasks/app_update/mod.rs), [manifest parser](../../src/core/tasks/app_update/manifest.rs), [installer](../../src/core/tasks/app_update/installer.rs), and [update manifest generation](../../foxy-server-backend-cli/src/update_manifest.rs). Check digest, version, and platform fields against the consumed format.
4. Inspect [Windows installer](../../installer/windows/foxy-setup.iss), [Linux installer builder](../../installer/linux/build-installer.sh), [packaging workflow](../../.github/workflows/build-artifacts.yml), and [build.rs](../../build.rs) for staged files. Keep `steam_api64.dll` next to Windows `Foxy.exe` and `libsteam_api.so` next to Linux `Foxy`. Windows delay loading still requires the library for the Steam helper; Linux resolves its library before `main`, so a missing copy prevents startup.
5. Check installer and self-update paths for install location and user data behavior. Runtime data belongs under the Foxy config root and per-game-space directories; do not include or overwrite live user config, databases, logs, or caches as release artifacts.
6. Compare distributable contents with the packaging workflow and the expected release manifest. Record missing assets, stale hashes, version disagreement, or unverified platform coverage as concrete blockers.
7. Run the repository's prescribed checks for changed code and inspect their results. A local build is evidence for that target only; do not claim Windows packaging or runtime checks when they were not run.
   Smoke the packaged binary's `version` command on each available target, including the Windows layout with its staged Steam library. Compare the reported version/commit with the intended source revision. Use isolated configuration for any broader startup smoke.
8. Keep release notes and user documentation within requested scope. Do not add a changelog entry unless asked.

## Deliverables and boundary

- For local updater metadata, follow the app-update section of [server publish validation](../foxy-server-publish-validation/SKILL.md). Check installer placement as well as digests; manifest generation does not copy installer payloads.
- Provide a release checklist with artifact names, version and commit evidence, manifest digest status, redistributable presence, installer settings, and executed checks.
- Separate observed evidence from inferred readiness, and identify unavailable target builds or signing steps.
- Stop before creating a release, uploading assets, signing, publishing an update manifest, or modifying installed user data unless the task explicitly authorizes that operation.
