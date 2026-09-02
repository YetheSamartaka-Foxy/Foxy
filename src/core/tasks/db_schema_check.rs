//! Live schema compatibility probe.
//!
//! [`db_schema_version`](super::db_schema_version) tracks the *intended* schema
//! generation in a JSON sidecar, but nothing ever checked the database actually
//! on disk. That gap is load-bearing: the bootstrap schema is written with
//! `CREATE TABLE IF NOT EXISTS`, so applying it to a database built by an older
//! Foxy is a silent no-op that leaves the old tables in place. A user who
//! dismisses the wipe prompt therefore keeps running against a schema the
//! current SQL cannot execute - every repository upsert fails with a parse
//! error, every sync then finds zero mods and reports "up to date".
//!
//! This module closes the gap by asking the engine to *prepare* the statements
//! the sync path depends on. Preparing parses and binds against the live schema
//! without executing anything, so a missing column or a missing `ON CONFLICT`
//! target surfaces here instead of silently mid-sync. Column coverage is checked
//! generically against the bootstrap schema so future drift is caught too.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use log::{error, info, warn};
use turso::{Connection, Database};

use crate::core::models::pending_update::PENDING_UPDATE_UPSERT_SQL;
use crate::core::models::repository::REPOSITORY_UPSERT_SQL;
use crate::core::tasks::db_turso::TURSO_BOOTSTRAP_SCHEMA;
use crate::core::utils::format::sanitize_log_path;

/// Statements whose *parse* depends on schema shapes the bootstrap column list
/// cannot express - primarily the `ON CONFLICT` targets, which need the
/// composite UNIQUE constraints to exist. Kept pointing at the production SQL
/// so the probe can never drift from what the sync actually runs.
const PROBE_STATEMENTS: &[(&str, &str)] = &[
    ("repositories upsert", REPOSITORY_UPSERT_SQL),
    ("pending_updates upsert", PENDING_UPDATE_UPSERT_SQL),
];

/// Result of probing one database file. Empty `problems` means compatible.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchemaCompatibility {
    pub problems: Vec<String>,
}

impl SchemaCompatibility {
    pub fn is_compatible(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Probe outcome for the database file currently open. Keyed by path so a
/// runtime game-space switch re-probes the space it switched to instead of
/// reusing the previous space's verdict.
static LAST_PROBE: Mutex<Option<(PathBuf, SchemaCompatibility)>> = Mutex::new(None);

/// Latch set the first time any probe reports an incompatible database. The UI
/// asks [`live_schema_incompatible`] every frame, and resolving the active
/// space directory touches the filesystem, so the healthy path must stop at a
/// relaxed load. Never cleared: it only gates the authoritative per-path check
/// below, which a post-wipe re-probe brings back to compatible on its own.
static ANY_INCOMPATIBLE: AtomicBool = AtomicBool::new(false);

/// Whether the database for the active game space was found structurally
/// unusable. `false` while no probe has run yet for that path, so callers never
/// escalate on missing information. Resolves the active space's path on each
/// call, so it stays correct across a runtime space switch.
pub fn live_schema_incompatible() -> bool {
    ANY_INCOMPATIBLE.load(Ordering::Relaxed)
        && !live_probe_result().is_none_or(|result| result.is_compatible())
}

/// Problems found by the last probe of the active game space's database.
pub fn live_schema_problems() -> Vec<String> {
    live_probe_result()
        .map(|result| result.problems)
        .unwrap_or_default()
}

fn live_probe_result() -> Option<SchemaCompatibility> {
    let path = crate::core::game::spaces::active_game_space_dir().join("database.db");
    LAST_PROBE.lock().ok().and_then(|probe| {
        probe
            .as_ref()
            .filter(|(probed, _)| *probed == path)
            .map(|(_, result)| result.clone())
    })
}

/// Probe the freshly opened database and record the verdict for `path`. Runs
/// once per database open; the statement prepares are parse-only, so the cost is
/// a few milliseconds even on a slow disk.
async fn probe_and_record(path: &Path, conn: &Connection) {
    let result = probe(conn).await;
    if result.is_compatible() {
        info!("STARTUP: live database schema matches this build");
    } else {
        error!(
            "STARTUP: live database schema at {} is incompatible with this build and cannot be used; \
             the database must be wiped and rebuilt ({} problem(s))",
            sanitize_log_path(path),
            result.problems.len()
        );
        for problem in &result.problems {
            error!("STARTUP: schema problem: {}", problem);
        }
        ANY_INCOMPATIBLE.store(true, Ordering::Relaxed);
    }
    if let Ok(mut slot) = LAST_PROBE.lock() {
        *slot = Some((path.to_path_buf(), result));
    }
}

/// Convenience wrapper that opens its own tuned connection.
pub(crate) async fn probe_database(path: &Path, db: &Database) {
    match crate::core::tasks::db_turso::connect_tuned(db).await {
        Ok(conn) => probe_and_record(path, &conn).await,
        Err(err) => warn!("STARTUP: could not probe live database schema: {}", err),
    }
}

async fn probe(conn: &Connection) -> SchemaCompatibility {
    let mut problems = Vec::new();

    for (table, expected) in expected_columns(TURSO_BOOTSTRAP_SCHEMA) {
        let Some(actual) = live_columns(conn, &table).await else {
            // A table the engine cannot describe is either absent or unreadable;
            // the bootstrap would have created it, so treat it as missing.
            problems.push(format!("table `{table}` is missing"));
            continue;
        };
        let mut missing: Vec<String> = expected.difference(&actual).cloned().collect();
        if !missing.is_empty() {
            missing.sort();
            problems.push(format!(
                "table `{table}` is missing column(s): {}",
                missing.join(", ")
            ));
        }
    }

    for (label, sql) in PROBE_STATEMENTS {
        if let Err(err) = conn.prepare(*sql).await {
            problems.push(format!("{label} cannot be prepared: {err}"));
        }
    }

    SchemaCompatibility { problems }
}

/// Column names the engine reports for `table`, or `None` when the table does
/// not exist.
async fn live_columns(conn: &Connection, table: &str) -> Option<BTreeSet<String>> {
    let mut rows = conn
        .query(&format!("PRAGMA table_info({table})"), ())
        .await
        .ok()?;
    let mut columns = BTreeSet::new();
    while let Ok(Some(row)) = rows.next().await {
        // `table_info` yields (cid, name, type, notnull, dflt_value, pk).
        if let Ok(turso::Value::Text(name)) = row.get_value(1) {
            columns.insert(name);
        }
    }
    if columns.is_empty() {
        None
    } else {
        Some(columns)
    }
}

/// Parse `CREATE TABLE` blocks out of the bootstrap schema into
/// `table -> column names`, so the probe stays correct as the schema evolves
/// without a second hand-maintained list.
fn expected_columns(schema: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut tables = BTreeMap::new();
    let mut current: Option<(String, BTreeSet<String>)> = None;

    for raw in schema.lines() {
        let line = match raw.find("--") {
            Some(idx) => &raw[..idx],
            None => raw,
        }
        .trim();
        if line.is_empty() {
            continue;
        }

        if current.is_none() {
            if let Some(name) = parse_create_table(line) {
                current = Some((name, BTreeSet::new()));
            }
            continue;
        }

        if line.starts_with(')') {
            if let Some((name, columns)) = current.take() {
                tables.insert(name, columns);
            }
            continue;
        }

        if let Some((_, columns)) = current.as_mut()
            && let Some(column) = parse_column_name(line)
        {
            columns.insert(column);
        }
    }

    tables
}

/// Table name from a `CREATE TABLE [IF NOT EXISTS] <name> (` line.
fn parse_create_table(line: &str) -> Option<String> {
    let upper = line.to_ascii_uppercase();
    let rest = upper
        .strip_prefix("CREATE TABLE IF NOT EXISTS ")
        .map(|_| &line["CREATE TABLE IF NOT EXISTS ".len()..])
        .or_else(|| {
            upper
                .strip_prefix("CREATE TABLE ")
                .map(|_| &line["CREATE TABLE ".len()..])
        })?;
    let name = rest.trim().trim_end_matches('(').trim();
    (!name.is_empty()).then(|| name.to_ascii_lowercase())
}

/// Column name from a table-body line, skipping table-level constraint clauses.
fn parse_column_name(line: &str) -> Option<String> {
    let token = line.split_whitespace().next()?.trim_end_matches(',');
    if token.is_empty() {
        return None;
    }
    const CONSTRAINT_KEYWORDS: &[&str] = &[
        "constraint",
        "primary",
        "unique",
        "foreign",
        "check",
        "on",
        "references",
    ];
    let lowered = token.to_ascii_lowercase();
    if CONSTRAINT_KEYWORDS.contains(&lowered.as_str()) {
        return None;
    }
    Some(lowered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_schema_parses_into_tables_and_columns() {
        let tables = expected_columns(TURSO_BOOTSTRAP_SCHEMA);

        for table in [
            "repositories",
            "pending_updates",
            "addons",
            "files",
            "subfiles",
            "repository_addons",
            "addon_files",
            "download_target_file",
            "download_target_file_part",
            "download_patch_file",
            "download_patch_op",
        ] {
            assert!(tables.contains_key(table), "missing table {table}");
        }
        assert!(!tables.contains_key("file_subfiles"));
    }

    #[test]
    fn repository_identity_columns_are_expected() {
        let tables = expected_columns(TURSO_BOOTSTRAP_SCHEMA);
        let repositories = &tables["repositories"];
        for column in [
            "id",
            "name",
            "remote_url",
            "local_path",
            "image",
            "local_checksum",
            "remote_checksum",
            "local_content_hash",
            "foxy_mode",
        ] {
            assert!(
                repositories.contains(column),
                "repositories should declare {column}"
            );
        }
        // The table-level UNIQUE must not be mistaken for a column.
        assert!(!repositories.contains("constraint"));
        assert!(!repositories.contains("repositories_unique_remote_local"));

        assert!(tables["pending_updates"].contains("local_path"));
        assert!(!tables["pending_updates"].contains("primary"));
    }

    #[test]
    fn create_table_header_variants_parse() {
        assert_eq!(
            parse_create_table("CREATE TABLE IF NOT EXISTS repositories ("),
            Some("repositories".to_string())
        );
        assert_eq!(
            parse_create_table("CREATE TABLE addons ("),
            Some("addons".to_string())
        );
        assert_eq!(parse_create_table("CREATE INDEX idx_foo ON addons ("), None);
    }

    #[test]
    fn constraint_lines_are_not_columns() {
        assert_eq!(
            parse_column_name("id INTEGER PRIMARY KEY,"),
            Some("id".into())
        );
        assert_eq!(
            parse_column_name(
                "CONSTRAINT repositories_unique_remote_local UNIQUE (remote_url, local_path)"
            ),
            None
        );
        assert_eq!(
            parse_column_name("PRIMARY KEY (repository_url, local_path)"),
            None
        );
        assert_eq!(
            parse_column_name("FOREIGN KEY (file_id) REFERENCES files(id)"),
            None
        );
    }

    #[tokio::test]
    async fn fresh_bootstrap_database_probes_clean() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("database.db");
        let db = crate::core::tasks::db_turso::build_and_bootstrap(path.to_str().unwrap())
            .await
            .unwrap();
        let conn = crate::core::tasks::db_turso::connect_tuned(&db)
            .await
            .unwrap();

        let result = probe(&conn).await;

        assert!(
            result.is_compatible(),
            "bootstrap schema must probe clean, got {:?}",
            result.problems
        );
    }

    /// The exact shape reported by the 1.1.0 field logs: a pre-identity-split
    /// `repositories` table with no `local_path` and a `remote_url`-only UNIQUE.
    /// Both the column check and the `ON CONFLICT` prepare must flag it.
    #[tokio::test]
    async fn legacy_repositories_table_is_flagged_incompatible() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("database.db");
        let db = crate::core::tasks::db_turso::build_and_bootstrap(path.to_str().unwrap())
            .await
            .unwrap();
        let conn = crate::core::tasks::db_turso::connect_tuned(&db)
            .await
            .unwrap();
        conn.execute("DROP TABLE repositories", ()).await.unwrap();
        conn.execute(
            "CREATE TABLE repositories (\
             id INTEGER PRIMARY KEY, name TEXT NOT NULL, remote_url TEXT NOT NULL UNIQUE, \
             image TEXT NOT NULL DEFAULT '', local_checksum TEXT NOT NULL DEFAULT '', \
             remote_checksum TEXT NOT NULL DEFAULT '', local_content_hash TEXT NOT NULL DEFAULT '', \
             foxy_mode TEXT NOT NULL DEFAULT '')",
            (),
        )
        .await
        .unwrap();

        let result = probe(&conn).await;

        assert!(!result.is_compatible());
        assert!(
            result
                .problems
                .iter()
                .any(|p| p.contains("repositories") && p.contains("local_path")),
            "expected the missing identity column to be reported, got {:?}",
            result.problems
        );
    }

    /// Every column present but the UNIQUE still keyed on `remote_url` alone.
    /// Column coverage cannot see this; only preparing the real upsert can, and
    /// it is the shape behind the "ON CONFLICT clause does not match any PRIMARY
    /// KEY or UNIQUE constraint" errors in the 1.1.0 field logs.
    #[tokio::test]
    async fn missing_composite_unique_is_flagged_by_statement_probe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("database.db");
        let db = crate::core::tasks::db_turso::build_and_bootstrap(path.to_str().unwrap())
            .await
            .unwrap();
        let conn = crate::core::tasks::db_turso::connect_tuned(&db)
            .await
            .unwrap();
        conn.execute("DROP TABLE repositories", ()).await.unwrap();
        conn.execute(
            "CREATE TABLE repositories (             id INTEGER PRIMARY KEY, name TEXT NOT NULL, remote_url TEXT NOT NULL UNIQUE,              local_path TEXT NOT NULL DEFAULT '', image TEXT NOT NULL DEFAULT '',              local_checksum TEXT NOT NULL DEFAULT '', remote_checksum TEXT NOT NULL DEFAULT '',              local_content_hash TEXT NOT NULL DEFAULT '', foxy_mode TEXT NOT NULL DEFAULT '')",
            (),
        )
        .await
        .unwrap();

        let result = probe(&conn).await;

        assert!(!result.is_compatible());
        assert!(
            result
                .problems
                .iter()
                .any(|p| p.contains("repositories upsert")),
            "expected the upsert prepare to fail, got {:?}",
            result.problems
        );
    }
}
