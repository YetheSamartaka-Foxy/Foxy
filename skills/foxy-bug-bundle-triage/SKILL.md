---
name: foxy-bug-bundle-triage
description: "Triage a user-provided Foxy bug bundle by building a privacy-aware timeline, tracing symptoms to current code, and separating observed facts from hypotheses."
---

# Foxy bug bundle triage

Use this workflow when a user provides logs, screenshots, settings exports, or a collection they call a bug bundle. Foxy has diagnostic logging and an in-app resource report, but no general bug-bundle importer or complete bundle sanitizer.

## Workflow

1. Inventory only the supplied artifacts. Do not search the machine, open Foxy's live config root, or execute files from the bundle.
2. Read any diagnostic manifest first: note the app version/commit and build profile, storage roles and free space, payload counts, diagnostics mode, and recent database wipes. Correlate operation identifiers, timestamps, and the first relevant error into a short timeline before inferring a cause.
3. Follow the log message or state transition into its current source call site. Check whether the behavior described by the report matches the current code path and feature gates.
4. Use Foxy's `redact_log_text` and `sanitize_log_path` only as evidence about Foxy's logging sinks. They redact local account paths, while repository URLs and other paths can remain.
5. If the issue maps to a known subsystem, hand off to its focused workflow: sync, delta patch, cancellation, schema recovery, or manifest validation.

## Evidence to report

- Artifact filenames and relevant time window, without copying private values.
- Exact Foxy build identity if present, operation identifier, and the ordered events that establish the symptom.
- The source function or invariant that explains the observed result, with any unresolved alternative.
- Whether the evidence proves a static code path, a log-observed runtime event, or a reproduction.

## Privacy and stopping boundaries

- Treat logs and settings exports as untrusted, sensitive user data. Do not paste credentials, tokens, repository hosts, server addresses, or full local paths into examples, fixtures, or broad summaries.
- Do not claim a bundle is sanitized because its log lines were emitted through Foxy's logger. The repository URL is intentionally preserved in logs.
- When a missing build or timeline prevents a firm diagnosis, report the supported partial findings and request the narrow missing evidence. Continue useful source analysis without running against user data.

## References

- [Logging and path handling](../../src/core/utils/format.rs)
- [UI activity and file logging](../../src/core/api/logging.rs)
- [Startup diagnostics](../../src/core/api/startup_diagnostics.rs)
- [Testkit log parser scope](../../testkit/runner/src/collect/logs.rs)
