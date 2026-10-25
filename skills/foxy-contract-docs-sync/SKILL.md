---
name: foxy-contract-docs-sync
description: Keep Foxy user guides, in-app help, CLI output contracts, examples, and runtime behavior aligned when commands or configuration change.
---

# Contract and docs synchronization

Use this skill when a Foxy change affects user-visible behavior, command syntax, JSON output, configuration, manifests, or examples.

## Workflow

1. Read [CLI conventions](../../conventions/CLI_CONVENTIONS.md), [examples conventions](../../conventions/EXAMPLES_CONVENTIONS.md), [i18n conventions](../../conventions/i18n_CONVENTIONS.md), and [testing conventions](../../conventions/TESTING_CONVENTIONS.md). Inspect nested instructions for the source subtree.
2. Find the behavior's source of truth before editing docs. CLI parsing and output live under [src/cli](../../src/cli); in-app Help is [help.rs](../../src/ui/views/help.rs); locale strings live in [locales](../../src/ui/locales); examples are under [examples](../../examples).
3. For a command or flag change, verify accepted names, defaults, prerequisites, destructive confirmation, dry-run behavior, exit codes, and `--json` shape against the implementation. Preserve stable automation output and keep human progress off JSON stdout.
4. For config or manifest changes, update the matching example in the same change and confirm it reflects the runtime schema and per-game-space versus app-global location.
5. Update [User Guide](../../wiki/User-Guide.md) for desktop app and client CLI behavior. Update [Server Admin Guide](../../wiki/Server-Admin-Guide.md) for server CLI behavior. Update README feature, CLI, or data-layout sections when affected.
6. For in-app Help changes, edit English source strings and follow locale conventions. Keep placeholders, escaped newlines, and keys intact; do not silently change the meaning of translated text.
7. Keep docs examples non-production: no secrets, real hosts, user paths, or live repository and mod names. Use established placeholders.
8. Review every changed statement against source and examples. Remove claims that depend on unrun network, Steam, platform, or release operations.

## Deliverables and verification

- Identify the runtime contract changed and the docs, Help text, locale keys, or examples updated to match it.
- Include an accessibility impact note for UI or CLI changes, and state whether an affected desktop action has CLI parity or why it is deferred.
- For docs-only work, no application build is needed. For code or locale checker changes, follow the focused checks in [testing conventions](../../conventions/TESTING_CONVENTIONS.md) and [locale conventions](../../conventions/i18n_CONVENTIONS.md).
- Stop when the source contract and relevant published guidance agree; do not create a changelog entry unless requested.
