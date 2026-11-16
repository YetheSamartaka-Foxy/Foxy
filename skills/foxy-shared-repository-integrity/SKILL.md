---
name: foxy-shared-repository-integrity
description: "Trace same-URL repositories, shared folders, addon propagation, and pending-update state without collapsing independent Foxy repository instances."
---

# Foxy shared repository integrity

Use this workflow when repositories share a URL, addon folder, repository space, or downloaded content, or when a purge or update appears to affect a sibling.

## Identity contract

A repository instance is keyed by `(remote_url, local_path)`. The same URL at different local paths is independent. A repository-space shared folder can contain distinct repository instances, where URL remains a tiebreaker. Addon and file rows also have path-scoped identity.

## Workflow

1. Trace the caller and query from UI or CLI into the model and DB operation. Record both normalized URL and local path at each identity boundary.
2. Inspect SQL conflict keys and delete predicates. URL-only lookup may be a deliberate within-folder selection, but URL-only purge, status collapse, or pending-update deletion is suspect.
3. For shared addon content, follow propagation and the quick scan it queues for each sibling. Propagation may mark verified shared bytes clean; it must not clear that sibling's own pending work.
4. Check `local_content_hash` alongside copied tree state. A sibling marked clean without a matching content baseline can be excluded from the update queue and then fail the sync with no files to download.
5. Validate with the focused same-URL/different-path and shared-folder tests named by the touched module. Use a testkit lane only when the question requires an end-to-end shared-folder reproduction.

## Evidence

Report the full composite identity with placeholders, the exact query or conflict key, and the observed state for each sibling. Distinguish static key analysis from a database test or live profile run.

## Boundaries

Do not purge, rekey, or edit a runtime database as an investigation step. Keep game-space selection in view: only the active space's database and paths participate in that process.

## References

- [Repository identity rules](../../conventions/CORE_CONVENTIONS.md)
- [Repository model and normalization](../../src/core/models/repository.rs)
- [Addon display-name instance tests](../../src/core/addon_metadata.rs)
- [Schema uniqueness constraints](../../sql/turso_schema.sql)
