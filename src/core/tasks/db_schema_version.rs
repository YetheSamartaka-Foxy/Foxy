//! Database schema-version tracking and the auto-wipe gate.
//!
//! Foxy occasionally has to change the database schema in ways that are not
//! cleanly forward-migratable (the Turso migration is the first big one). For
//! those cases the database simply needs to be wiped and rebuilt from the
//! current migrations. Rather than silently corrupting or guessing, we track a
//! single monotonically increasing [`DB_SCHEMA_VERSION`] and persist the
//! version the local database was last built/wiped at in a small JSON sidecar
//! next to `database.db`.
//!
//! On startup the GUI compares the stored version against the compiled one. If
//! the running binary ships a *newer* schema than the local database was built
//! for, the user is prompted (a blocking modal) to wipe-and-continue or to
//! dismiss and keep using the old data at their own risk.
//!
//! The sidecar file - not the mere presence of `database.db` - is the primary
//! source of truth, because `database.db` is created very early in startup (an
//! empty file is touched before the UI exists), so its mere existence cannot
//! distinguish a fresh install from an upgrade. The *one* exception is a
//! sidecar that is missing entirely: a fresh install has no sidecar **and** no
//! populated database, whereas a build that predates schema versioning has a
//! populated `database.db` but never wrote a sidecar. We treat the latter
//! (missing sidecar + non-empty database) as schema version 0 and prompt for a
//! wipe, so those users are notified rather than silently left on stale data.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::core::utils::format::sanitize_log_path;

/// Current database schema generation understood by this binary.
///
/// Bump this by one **only** when a schema change requires an existing database
/// to be wiped (not when a change is cleanly handled by a forward migration in
/// `migrations/`). Bumping it makes older databases prompt the user to wipe on
/// next launch. Keep it aligned with the breaking-change history below.
///
/// `22`: The persistence engine moved from SeaORM/SQLite to Turso with a folded
/// bootstrap schema; existing databases (sidecar `21`) are prompted to
/// wipe-and-rebuild on first launch.
///
/// `23`: subfiles `(file_id, path)` uniqueness moved from an inline
/// `CONSTRAINT … UNIQUE` to a standalone `idx_subfiles_file_id_path` unique index
/// so the bulk-load path can DROP/CREATE it around a whole-wipe force-redownload
/// (after_turso_regression_analysis5.md P0-d). The autoindex name differs, so
/// existing `22` databases must rebuild for the named index to exist.
///
/// `24`: dropped the `idx_subfiles_path_remote_checksum (path, remote_checksum)`
/// index - every subfiles query filters by `file_id` (covered by the remaining two
/// indexes), so it had no primary user and only added a 4th B-tree to every part
/// write. Removing it cuts the 66k-row force-redownload insert ~25%
/// (after_turso_regression_analysis6.md). Existing `23` databases rebuild so the
/// dropped index does not linger.
pub const DB_SCHEMA_VERSION: u32 = 24;

/// Content-hash format generation understood by this binary.
pub const CONTENT_HASH_FORMAT: u32 = 2;

fn legacy_content_hash_format() -> u32 {
    1
}

/// Persisted database metadata sidecar (`db_meta.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbMeta {
    /// Schema generation the local database was last built or wiped at.
    pub schema_version: u32,
    /// If the user chose to keep an out-of-date database "at their own risk",
    /// this records the target version they dismissed, so we don't nag every
    /// launch - but a *further* schema bump will prompt again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_for_version: Option<u32>,
    /// Content-hash format generation the stored baselines were built with.
    #[serde(default = "legacy_content_hash_format")]
    pub content_hash_format: u32,
}

/// Information handed to the UI so it can render the wipe prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DbSchemaWipePrompt {
    pub stored_version: u32,
    pub target_version: u32,
    /// The live database was probed and cannot run this build's statements, so
    /// "continue without wiping" is not a real option: every sync would persist
    /// nothing and still report success. The UI drops the dismiss action for a
    /// blocking prompt.
    pub blocking: bool,
}

/// Outcome of comparing the stored sidecar against the compiled schema version.
/// Kept as a pure value so the decision is unit-testable without filesystem IO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SchemaDecision {
    /// No sidecar yet and no populated database (a genuine fresh install). Write
    /// the current version and trust the normal migration/rebuild path. No
    /// prompt - there is nothing to wipe.
    Bootstrap(u32),
    /// Stored version is current (or newer, e.g. a downgrade). Nothing to do.
    UpToDate,
    /// Stored version is behind, but the user already dismissed this exact
    /// target. Suppress the prompt for this launch.
    Dismissed,
    /// Stored version is behind a breaking bump and not yet dismissed: prompt.
    PromptWipe { stored: u32, target: u32 },
}

/// Pure decision: given the loaded sidecar (if any), whether a populated
/// database already exists on disk, and the compiled version, decide what to
/// do. No side effects so it can be exhaustively unit-tested.
pub(crate) fn decide(
    meta: Option<&DbMeta>,
    legacy_db_present: bool,
    compiled: u32,
) -> SchemaDecision {
    match meta {
        // Fresh install: no sidecar and no data. Bootstrap silently.
        None if !legacy_db_present => SchemaDecision::Bootstrap(compiled),
        // Upgrade from a build that predates schema versioning: a real database
        // exists but no sidecar was ever written. Treat the absent version as 0
        // and prompt so the user is notified the cache must be rebuilt.
        None => SchemaDecision::PromptWipe {
            stored: 0,
            target: compiled,
        },
        Some(meta) if meta.schema_version >= compiled => SchemaDecision::UpToDate,
        Some(meta) if meta.dismissed_for_version == Some(compiled) => SchemaDecision::Dismissed,
        Some(meta) => SchemaDecision::PromptWipe {
            stored: meta.schema_version,
            target: compiled,
        },
    }
}

/// Whether a populated database already exists on disk. Used only to
/// distinguish a genuine fresh install (no sidecar, no data) from an upgrade
/// from a build that predates schema versioning (no sidecar, real data). A
/// missing or freshly `touch`ed 0-byte file counts as absent.
fn legacy_database_present() -> bool {
    let path = crate::core::tasks::db_turso::database_file_path();
    fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false)
}

/// Path to the schema-version sidecar (`db_meta.json`) beside the active game
/// space's database.
pub fn db_meta_path() -> PathBuf {
    crate::core::game::spaces::active_game_space_dir().join("db_meta.json")
}

fn read_meta() -> Option<DbMeta> {
    let path = db_meta_path();
    let raw = fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<DbMeta>(&raw) {
        Ok(meta) => Some(meta),
        Err(err) => {
            log::warn!(
                "Failed to parse database schema meta {}: {}; treating as missing",
                sanitize_log_path(&path),
                err
            );
            None
        }
    }
}

fn write_meta(meta: &DbMeta) {
    let path = db_meta_path();
    if let Some(parent) = path.parent()
        && let Err(err) = fs::create_dir_all(parent)
    {
        log::error!(
            "Failed to create directory for database schema meta {}: {}",
            sanitize_log_path(&path),
            err
        );
        return;
    }
    match serde_json::to_string_pretty(meta) {
        Ok(serialized) => {
            if let Err(err) = fs::write(&path, serialized) {
                log::error!(
                    "Failed to write database schema meta {}: {}",
                    sanitize_log_path(&path),
                    err
                );
            }
        }
        Err(err) => log::error!("Failed to serialize database schema meta: {}", err),
    }
}

/// Evaluate the stored schema version against the compiled one, performing the
/// bootstrap write for fresh/legacy databases, and return a prompt descriptor
/// when the user should be asked to wipe. Call once during GUI startup.
pub fn evaluate_and_bootstrap() -> Option<DbSchemaWipePrompt> {
    let meta = read_meta();
    match decide(meta.as_ref(), legacy_database_present(), DB_SCHEMA_VERSION) {
        SchemaDecision::Bootstrap(version) => {
            log::info!(
                "Database schema meta missing; bootstrapping to schema version {}",
                version
            );
            write_meta(&DbMeta {
                schema_version: version,
                dismissed_for_version: None,
                content_hash_format: CONTENT_HASH_FORMAT,
            });
            None
        }
        SchemaDecision::UpToDate => {
            log::debug!("Database schema up to date (version {})", DB_SCHEMA_VERSION);
            None
        }
        SchemaDecision::Dismissed => {
            log::warn!(
                "Database schema is behind (stored < {}) but the wipe prompt was dismissed for this version; keeping existing data",
                DB_SCHEMA_VERSION
            );
            None
        }
        SchemaDecision::PromptWipe { stored, target } => {
            log::warn!(
                "Database schema is out of date: stored={} target={}; prompting for wipe",
                stored,
                target
            );
            Some(DbSchemaWipePrompt {
                stored_version: stored,
                target_version: target,
                blocking: false,
            })
        }
    }
}

/// Re-raise the wipe prompt as non-dismissible when the *live* database was
/// probed and found unable to run this build's statements.
///
/// The sidecar decision alone is not enough: the bootstrap schema is applied
/// with `CREATE ... IF NOT EXISTS`, so a user who dismissed the prompt keeps a
/// database whose repository upsert and pending-update writes fail to parse.
/// Every sync then finds zero mods and still reports success, which is how field
/// installs sat for weeks silently skipping updates. Once the probe has spoken,
/// dismissal is no longer offered.
pub fn blocking_prompt_if_live_schema_incompatible() -> Option<DbSchemaWipePrompt> {
    if !crate::core::tasks::db_schema_check::live_schema_incompatible() {
        return None;
    }
    Some(DbSchemaWipePrompt {
        stored_version: read_meta().map(|meta| meta.schema_version).unwrap_or(0),
        target_version: DB_SCHEMA_VERSION,
        blocking: true,
    })
}

/// CLI-side, read-only schema gate. Performs the same fresh/legacy sidecar
/// bootstrap as [`evaluate_and_bootstrap`] (so first runs are not nagged) and,
/// when the local database is behind a breaking bump that has not been
/// dismissed, logs a warning and returns a short user-facing hint string. Never
/// wipes - the CLI must not destroy data without explicit consent (`--wipe-db
/// --yes`). Returns `None` when nothing is out of date.
pub fn cli_wipe_hint() -> Option<String> {
    evaluate_and_bootstrap().map(|prompt| {
        format!(
            "local database schema is out of date (stored v{}, this build expects v{}); \
             run `foxy --wipe-db --yes` to wipe and rebuild, or continue at your own risk",
            prompt.stored_version, prompt.target_version
        )
    })
}

/// Record that the database was wiped/rebuilt at the current schema version.
/// Clears any prior "dismissed" marker. Call after a successful wipe.
pub fn mark_wiped() {
    log::info!(
        "Recording database schema version {} after wipe",
        DB_SCHEMA_VERSION
    );
    write_meta(&DbMeta {
        schema_version: DB_SCHEMA_VERSION,
        dismissed_for_version: None,
        content_hash_format: CONTENT_HASH_FORMAT,
    });
}

/// Whether stored content-hash baselines must be blanked and lazily rebuilt.
pub(crate) fn content_hash_baselines_need_retire() -> bool {
    read_meta().is_some_and(|meta| meta.content_hash_format < CONTENT_HASH_FORMAT)
}

/// Record that stored baselines are now on the current content-hash format.
pub(crate) fn mark_content_hash_format_current() {
    let Some(mut meta) = read_meta() else {
        return;
    };
    log::info!(
        "Recording content-hash format {} after baseline retire",
        CONTENT_HASH_FORMAT
    );
    meta.content_hash_format = CONTENT_HASH_FORMAT;
    write_meta(&meta);
}

/// Whether the persisted schema metadata is already current for this binary.
pub fn is_current() -> bool {
    read_meta().is_some_and(|meta| meta.schema_version >= DB_SCHEMA_VERSION)
}

/// Record that the user dismissed the wipe prompt for `target` and chose to keep
/// the existing database. Preserves the stored (older) version so the schema is
/// still considered out of date, but suppresses the prompt until the next bump.
pub fn mark_dismissed(stored: u32, target: u32) {
    log::warn!(
        "User dismissed database wipe prompt (stored={} target={}); keeping old data at their own risk",
        stored,
        target
    );
    write_meta(&DbMeta {
        schema_version: stored,
        dismissed_for_version: Some(target),
        content_hash_format: read_meta()
            .map(|meta| meta.content_hash_format)
            .unwrap_or_else(legacy_content_hash_format),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_meta_without_database_bootstraps_without_prompt() {
        // Genuine fresh install: no sidecar and no data.
        assert_eq!(decide(None, false, 21), SchemaDecision::Bootstrap(21));
    }

    #[test]
    fn missing_meta_with_legacy_database_prompts() {
        // Upgrade from a build that predates schema versioning: a real database
        // exists but no sidecar was ever written. Prompt to wipe (stored = 0).
        assert_eq!(
            decide(None, true, 24),
            SchemaDecision::PromptWipe {
                stored: 0,
                target: 24
            }
        );
    }

    #[test]
    fn equal_version_is_up_to_date() {
        let meta = DbMeta {
            schema_version: 21,
            dismissed_for_version: None,
            ..Default::default()
        };
        assert_eq!(decide(Some(&meta), true, 21), SchemaDecision::UpToDate);
    }

    #[test]
    fn newer_stored_version_does_not_prompt() {
        // Running an older binary against a newer DB (downgrade): never wipe.
        let meta = DbMeta {
            schema_version: 30,
            dismissed_for_version: None,
            ..Default::default()
        };
        assert_eq!(decide(Some(&meta), true, 21), SchemaDecision::UpToDate);
    }

    #[test]
    fn older_version_prompts() {
        let meta = DbMeta {
            schema_version: 21,
            dismissed_for_version: None,
            ..Default::default()
        };
        assert_eq!(
            decide(Some(&meta), true, 22),
            SchemaDecision::PromptWipe {
                stored: 21,
                target: 22
            }
        );
    }

    #[test]
    fn dismissed_for_exact_target_is_suppressed() {
        let meta = DbMeta {
            schema_version: 21,
            dismissed_for_version: Some(22),
            ..Default::default()
        };
        assert_eq!(decide(Some(&meta), true, 22), SchemaDecision::Dismissed);
    }

    #[test]
    fn dismissed_legacy_zero_is_suppressed_until_next_bump() {
        // A legacy user dismissed the prompt: mark_dismissed wrote schema 0 with
        // dismissed_for_version = target. Suppress for that target, prompt again
        // on the next bump.
        let meta = DbMeta {
            schema_version: 0,
            dismissed_for_version: Some(24),
            ..Default::default()
        };
        assert_eq!(decide(Some(&meta), true, 24), SchemaDecision::Dismissed);
        assert_eq!(
            decide(Some(&meta), true, 25),
            SchemaDecision::PromptWipe {
                stored: 0,
                target: 25
            }
        );
    }

    #[test]
    fn dismissed_for_older_target_still_prompts_on_next_bump() {
        // Dismissed v22, but binary now ships v23: must prompt again.
        let meta = DbMeta {
            schema_version: 21,
            dismissed_for_version: Some(22),
            ..Default::default()
        };
        assert_eq!(
            decide(Some(&meta), true, 23),
            SchemaDecision::PromptWipe {
                stored: 21,
                target: 23
            }
        );
    }

    #[test]
    fn meta_roundtrips_through_json() {
        let meta = DbMeta {
            schema_version: 21,
            dismissed_for_version: Some(22),
            ..Default::default()
        };
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: DbMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, parsed);
    }

    #[test]
    fn meta_without_dismissed_field_parses() {
        let parsed: DbMeta = serde_json::from_str(r#"{"schema_version": 21}"#).unwrap();
        assert_eq!(parsed.schema_version, 21);
        assert_eq!(parsed.dismissed_for_version, None);
    }

    #[test]
    fn meta_without_content_hash_format_is_legacy_format_one() {
        let parsed: DbMeta = serde_json::from_str(r#"{"schema_version": 24}"#).unwrap();
        assert_eq!(parsed.content_hash_format, 1);
        assert!(parsed.content_hash_format < CONTENT_HASH_FORMAT);
    }

    #[test]
    fn meta_content_hash_format_roundtrips() {
        let meta = DbMeta {
            schema_version: 24,
            dismissed_for_version: None,
            content_hash_format: CONTENT_HASH_FORMAT,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: DbMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.content_hash_format, CONTENT_HASH_FORMAT);
        assert_eq!(meta, parsed);
    }
}
