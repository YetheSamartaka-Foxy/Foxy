---
name: foxy-diagnostic-fixture-sanitizer
description: "Prepare privacy-safe diagnostic or log fixtures for Foxy tests while preserving failure-relevant structure and checking what existing redaction actually removes."
---

# Foxy diagnostic fixture sanitization

Use this workflow when diagnostic material must become a test fixture, issue attachment, or parser sample. The existing log helpers redact the local account path, but they preserve repository URLs and do not sanitize arbitrary archives or structured settings.

## Workflow

1. Identify the exact fixture format and all fields that can contain credentials, hosts, server addresses, account names, absolute paths, repository names, or mod names.
2. Preserve the fields needed to reproduce the behavior, then replace identifiers consistently with established placeholders. Keep counts, byte sizes, ordering, and timings when they matter to the regression.
3. Do not treat `sanitize_log_path`, `sanitize_log_url`, `redact_log_text`, proxy password redaction, or the testkit log parser as a general fixture sanitizer. Read each helper's implementation and scope.
4. For a small text fixture, use UTF-8-safe edits on a copy. For structured JSON, TOML, or archives, parse and rewrite with the Rust tool that owns the format. Extend a relevant Rust helper with focused tests when the requested sanitization needs a missing capability; do not substitute an unverified regex pass.
5. Review the final diff and inspect the serialized fixture for URLs, drive paths, credentials, real names, and accidental encoding damage.

## Current tooling limit

Foxy has path and log-text redaction in `src/core/utils/format.rs`, proxy-password redaction in `src/core/utils/proxy.rs`, and testkit parsers for selected log formats. It has no general-purpose bug-bundle or diagnostic-fixture sanitizer. Do not invent a CLI command or claim regex replacement covers nested or encoded data.

## Evidence and boundary

Record which fields were replaced and which were retained for reproduction. Stop if a value's sensitivity or semantic role is unclear; ask for a synthetic example rather than publishing the original. Never mutate a user's live logs, settings, or bug bundle in place.

Validate the output with the owning parser or replay tool, compare preserved counts and timing fields, and search every extracted member for residual identifiers. Keep the replacement map private under `.local/`; deliver only the sanitized fixture and a coverage report.

## References

- [Path and log redaction](../../src/core/utils/format.rs)
- [Proxy credential redaction](../../src/core/utils/proxy.rs)
- [Testkit log corpus parser](../../testkit/runner/src/collect/logs.rs)
- [Example data rules](../../conventions/EXAMPLES_CONVENTIONS.md)
