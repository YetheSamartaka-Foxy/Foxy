---
name: foxy-turso-schema-recovery
description: "Diagnose Foxy database schema incompatibility and guide safe recovery through the live probe, per-space repair state, and successful wipe contract."
---

# Foxy Turso schema recovery

Use this workflow when startup reports an outdated or incompatible database, writes fail to prepare, or repository checks are blocked. The sidecar records intended schema generation; the live database probe determines compatibility.

## Workflow

1. Identify the active game space and build version. Do not open or edit its runtime database manually.
2. Compare the bootstrap in `sql/turso_schema.sql`, `DB_SCHEMA_VERSION`, the sidecar decision, and `db_schema_check` findings. The bootstrap uses `CREATE IF NOT EXISTS`, so it does not upgrade old table layouts by itself.
3. Check columns, production upsert parse probes, and composite unique indexes. For repository identity, both the desired composite index and absence of an obsolete URL-only index matter.
4. Follow the UI and CLI repair gates. Launch and Join remain usable, while checks and updates stay blocked until a wipe succeeds. Dismissing the startup warning or continuing one launch does not clear repair-required state.
5. If a code change is needed, update the bootstrap and bump the schema version only when existing local databases cannot keep using the schema. Update models and examples in the same change.

## Recovery boundary

Never run a wipe as a diagnostic probe. A wipe replaces per-space database state. Explain the exact prompt or CLI path and wait for the user's explicit repair request before taking an action that would erase it. On failure, retain the repair requirement and report the live probe error.

## Evidence

Report the space-scoped build/schema decision, probe evidence, blocked operations, and whether recovery is static, test-backed, or a completed user-authorized repair. Label a result taken from supplied logs as reported evidence rather than an independently executed probe. Do not open the runtime DB merely to fill this field, or treat a dismissed modal as proof of compatibility.

## References

- [Core schema and Turso rules](../../conventions/CORE_CONVENTIONS.md)
- [Bootstrap schema](../../sql/turso_schema.sql)
- [Schema version decisions](../../src/core/tasks/db_schema_version.rs)
- [Live schema probe](../../src/core/tasks/db_schema_check.rs)
- [Repair prompt behavior](../../conventions/UI_CONVENTIONS.md)
