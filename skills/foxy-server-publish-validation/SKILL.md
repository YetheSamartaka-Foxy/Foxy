---
name: foxy-server-publish-validation
description: Build, validate, and troubleshoot Foxy server repository output using the real CLI layouts, manifests, dry-run, verify, and atomic publication contracts.
---

# Server publish validation

Use this skill for `foxy-server-backend-cli` config validation, repository generation, generated manifests, and output verification.

## Workflow

1. Read [server CLI agent notes](../../foxy-server-backend-cli/AGENTS.md), [CLI conventions](../../conventions/CLI_CONVENTIONS.md), [examples conventions](../../conventions/EXAMPLES_CONVENTIONS.md), and [testing conventions](../../conventions/TESTING_CONVENTIONS.md).
2. Confirm the intended input mode and schema in [server config](../../foxy-server-backend-cli/src/config.rs), then compare with [create-space example](../../examples/json/server_backend/space.json), [Reforger config](../../examples/json/server_backend/reforger_config.json), or [generic config](../../examples/json/server_backend/generic_config.json).
3. Trace the relevant command parser and source modules before suggesting flags. Use the CLI's actual `validate`, `create`, `create-space`, `verify`, `diff`, and `--dry-run` support; do not invent switches.
4. Match layout semantics: `copy` writes repository content, `pool` shares copied mods through relative symlinks, and `link` points at source folders and requires `--yes` because manifests are written there. Preserve the rule that a real directory is never replaced by a symlink.
5. For create-space, preserve identical hashes for same-named shared mods, the full space manifest when using `--only`, and the distinction between shared and per-repository keys. Keep `--per-repo-keys` collision checks intact.
6. Treat `--dry-run` as a read-only preview. `--atomic --yes` stages and replaces the complete output directory and cannot target external pool or keys destinations. Explain the replacement scope clearly before using it on a real publication target.
7. Preserve `--json` as one result object on stdout with human progress on stderr. Keep `create`'s server launch line as its final standalone output where that mode emits one.
8. Run `verify` against generated output to rehash published content and check manifests. Use `diff` or `--report` when the task is specifically comparing against prior output.
9. Update [Server Admin Guide](../../wiki/Server-Admin-Guide.md) and examples when user-visible flags, schemas, or generated JSON change.

## Local app-update metadata

Use the [actual command definitions](../../foxy-server-backend-cli/src/cli.rs) and [updater manifest generator](../../foxy-server-backend-cli/src/update_manifest.rs). For a fresh local staging root, substitute the intended changelog version and staged installer filenames:

```text
foxy-server-backend-cli setup-app-updater --version <version> --windows-installer .local/release-stage/installers/Foxy-setup.exe --linux-installer .local/release-stage/installers/Foxy-installer.sh --changelog CHANGELOG.md --output .local/release-stage
```

For an existing staged manifest, use `new-app-update` with the same installer/changelog/output flags to preserve older versions. `setup-app-updater` requires a Windows installer. Include `--linux-aarch64-installer` only when that artifact was built and is in scope.

The generators compute BLAKE3 digests and sizes and write relative `installers/<filename>` entries; they do not copy installers into the output. Stage the files at those paths, then compare each entry's version, platform key, algorithm, size, and digest with the artifact and the [client manifest parser](../../src/core/tasks/app_update/manifest.rs). Repository `verify` is not an updater-manifest verifier. Keep staging local until publication is authorized.

## Deliverables and stopping boundary

- Report input config, selected layout and options, output validation result, and exact checksums or manifest contracts affected.
- Use temporary or explicitly designated output for local generation. A successful dry-run or verify does not publish files remotely.
- Stop before copying to a live hosting destination, deleting prior publication, pushing to a server, or invoking network upload tools unless explicitly authorized.
