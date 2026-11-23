use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::utils::format::sanitize_log_path;

/// Bump only when an existing database must be wiped; forward-migrated changes
/// do not require a bump. Keep aligned with the bootstrap schema.
pub const DB_SCHEMA_VERSION: u32 = 25;

pub const CONTENT_HASH_FORMAT: u32 = 2;

fn legacy_content_hash_format() -> u32 {
    1
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbMeta {
    pub schema_version: u32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_for_version: Option<u32>,

    #[serde(default = "legacy_content_hash_format")]
    pub content_hash_format: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DbSchemaWipePrompt {
    pub stored_version: u32,
    pub target_version: u32,

    pub blocking: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SchemaDecision {
    Bootstrap(u32),

    UpToDate,

    Dismissed,

    PromptWipe { stored: u32, target: u32 },
}

pub(crate) fn decide(
    meta: Option<&DbMeta>,
    legacy_db_present: bool,
    compiled: u32,
) -> SchemaDecision {
    match meta {
        None if !legacy_db_present => SchemaDecision::Bootstrap(compiled),

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

fn wipe_requirement(
    meta: Option<&DbMeta>,
    legacy_db_present: bool,
    compiled: u32,
    live_incompatible: bool,
) -> Option<DbSchemaWipePrompt> {
    let stored = meta.map(|meta| meta.schema_version).unwrap_or(0);
    let outdated = match meta {
        Some(meta) => meta.schema_version < compiled,
        None => legacy_db_present,
    };
    (outdated || live_incompatible).then_some(DbSchemaWipePrompt {
        stored_version: stored,
        target_version: compiled,
        blocking: live_incompatible,
    })
}

pub(crate) fn current_wipe_requirement() -> Option<DbSchemaWipePrompt> {
    wipe_requirement(
        read_meta().as_ref(),
        legacy_database_present(),
        DB_SCHEMA_VERSION,
        crate::core::tasks::db_schema_check::live_schema_incompatible(),
    )
}

pub(crate) async fn ensure_database_ready_for_updates() -> Result<(), String> {
    evaluate_and_bootstrap();
    crate::core::tasks::db_turso::init_turso_database().await;
    if let Some(prompt) = current_wipe_requirement() {
        return Err(format!(
            "The local database must be rebuilt before mod checks or updates can run \
             (stored v{}, this build expects v{}). Run `foxy --wipe-db --yes` to rebuild it, \
             or use the database repair button in Foxy.",
            prompt.stored_version, prompt.target_version
        ));
    }
    Ok(())
}

fn legacy_database_present() -> bool {
    database_populated(&crate::core::tasks::db_turso::database_file_path())
}

fn database_populated(db_path: &Path) -> bool {
    fs::metadata(db_path).is_ok_and(|meta| meta.len() > 0)
}

const DB_META_FILE: &str = "db_meta.json";

pub fn db_meta_path() -> PathBuf {
    crate::core::game::spaces::active_game_space_dir().join(DB_META_FILE)
}

fn read_meta() -> Option<DbMeta> {
    read_meta_at(&db_meta_path())
}

fn read_meta_at(path: &Path) -> Option<DbMeta> {
    let raw = fs::read_to_string(path).ok()?;
    match serde_json::from_str::<DbMeta>(&raw) {
        Ok(meta) => Some(meta),
        Err(err) => {
            log::warn!(
                "Failed to parse database schema meta {}: {}; treating as missing",
                sanitize_log_path(path),
                err
            );
            None
        }
    }
}

fn write_meta(meta: &DbMeta) {
    write_meta_at(&db_meta_path(), meta);
}

fn write_meta_at(path: &Path, meta: &DbMeta) {
    if let Some(parent) = path.parent()
        && let Err(err) = fs::create_dir_all(parent)
    {
        log::error!(
            "Failed to create directory for database schema meta {}: {}",
            sanitize_log_path(path),
            err
        );
        return;
    }
    match serde_json::to_string_pretty(meta) {
        Ok(serialized) => {
            if let Err(err) = fs::write(path, serialized) {
                log::error!(
                    "Failed to write database schema meta {}: {}",
                    sanitize_log_path(path),
                    err
                );
            }
        }
        Err(err) => log::error!("Failed to serialize database schema meta: {}", err),
    }
}

fn bootstrap_meta(path: &Path, version: u32) {
    log::info!(
        "Database schema meta missing; bootstrapping to schema version {}",
        version
    );
    write_meta_at(
        path,
        &DbMeta {
            schema_version: version,
            dismissed_for_version: None,
            content_hash_format: CONTENT_HASH_FORMAT,
        },
    );
}

/// Every opener may create the initial sidecar; otherwise the next open misclassifies a fresh database as legacy.
pub(crate) fn bootstrap_if_fresh(db_path: &Path) {
    let meta_path = db_path.with_file_name(DB_META_FILE);
    if let SchemaDecision::Bootstrap(version) = decide(
        read_meta_at(&meta_path).as_ref(),
        database_populated(db_path),
        DB_SCHEMA_VERSION,
    ) {
        bootstrap_meta(&meta_path, version);
    }
}

pub fn evaluate_and_bootstrap() -> Option<DbSchemaWipePrompt> {
    let meta = read_meta();
    match decide(meta.as_ref(), legacy_database_present(), DB_SCHEMA_VERSION) {
        SchemaDecision::Bootstrap(version) => {
            bootstrap_meta(&db_meta_path(), version);
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

/// A live schema probe overrides dismissal because CREATE IF NOT EXISTS cannot repair incompatible tables.
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

pub fn cli_wipe_hint() -> Option<String> {
    evaluate_and_bootstrap();
    current_wipe_requirement().map(|prompt| {
        format!(
            "local database rebuild required (stored v{}, this build expects v{}); \
             mod checks and updates are unavailable until `foxy --wipe-db --yes` rebuilds it; \
             launches remain available",
            prompt.stored_version, prompt.target_version
        )
    })
}

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
    // The wipe removed the check results, so this version owns the empty cache.
    crate::core::tasks::check_cache_version::record_running_app_as_check_cache_owner();
}

pub(crate) fn content_hash_baselines_need_retire() -> bool {
    read_meta().is_some_and(|meta| meta.content_hash_format < CONTENT_HASH_FORMAT)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_meta_without_database_bootstraps_without_prompt() {
        assert_eq!(decide(None, false, 21), SchemaDecision::Bootstrap(21));
    }

    #[test]
    fn missing_meta_with_legacy_database_prompts() {
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
    fn rebuilt_1_2_0_database_does_not_prompt_for_1_2_1() {
        let meta = DbMeta {
            schema_version: 25,
            dismissed_for_version: None,
            content_hash_format: CONTENT_HASH_FORMAT,
        };
        assert_eq!(
            decide(Some(&meta), true, DB_SCHEMA_VERSION),
            SchemaDecision::UpToDate
        );
        assert_eq!(
            wipe_requirement(Some(&meta), true, DB_SCHEMA_VERSION, false),
            None
        );
    }

    #[test]
    fn dismissed_old_schema_still_requires_rebuild_for_updates() {
        let meta = DbMeta {
            schema_version: 24,
            dismissed_for_version: Some(DB_SCHEMA_VERSION),
            content_hash_format: CONTENT_HASH_FORMAT,
        };
        assert_eq!(
            wipe_requirement(Some(&meta), true, DB_SCHEMA_VERSION, false),
            Some(DbSchemaWipePrompt {
                stored_version: 24,
                target_version: DB_SCHEMA_VERSION,
                blocking: false,
            })
        );
    }

    #[test]
    fn live_incompatibility_requires_rebuild_despite_current_metadata() {
        let meta = DbMeta {
            schema_version: DB_SCHEMA_VERSION,
            content_hash_format: CONTENT_HASH_FORMAT,
            ..Default::default()
        };
        assert_eq!(
            wipe_requirement(Some(&meta), true, DB_SCHEMA_VERSION, true),
            Some(DbSchemaWipePrompt {
                stored_version: DB_SCHEMA_VERSION,
                target_version: DB_SCHEMA_VERSION,
                blocking: true,
            })
        );
    }

    #[test]
    fn missing_metadata_requires_rebuild_only_for_a_populated_database() {
        assert_eq!(
            wipe_requirement(None, false, DB_SCHEMA_VERSION, false),
            None
        );
        assert_eq!(
            wipe_requirement(None, true, DB_SCHEMA_VERSION, false),
            Some(DbSchemaWipePrompt {
                stored_version: 0,
                target_version: DB_SCHEMA_VERSION,
                blocking: false,
            })
        );
    }

    #[test]
    fn newer_stored_version_does_not_prompt() {
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

    #[test]
    fn the_first_open_of_a_fresh_database_records_the_current_version() {
        let dir = tempfile::tempdir().unwrap();
        bootstrap_if_fresh(&dir.path().join("database.db"));
        let meta = read_meta_at(&dir.path().join(DB_META_FILE)).unwrap();
        assert_eq!(meta.schema_version, DB_SCHEMA_VERSION);
        assert_eq!(
            wipe_requirement(Some(&meta), true, DB_SCHEMA_VERSION, false),
            None
        );
    }

    #[test]
    fn opening_a_populated_or_versioned_database_leaves_its_sidecar_alone() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("database.db");
        let meta_path = dir.path().join(DB_META_FILE);
        fs::write(&db_path, b"populated").unwrap();
        bootstrap_if_fresh(&db_path);
        assert!(!meta_path.exists());

        fs::remove_file(&db_path).unwrap();
        let older = DbMeta {
            schema_version: 24,
            dismissed_for_version: None,
            content_hash_format: CONTENT_HASH_FORMAT,
        };
        write_meta_at(&meta_path, &older);
        bootstrap_if_fresh(&db_path);
        assert_eq!(read_meta_at(&meta_path), Some(older));
    }
}
