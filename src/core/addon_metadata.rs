use log::{debug, warn};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::core::db::{DbValue, FoxyDb, params};
use crate::core::tasks::init_database::{bulk_write_chunk_rows, read_chunk_ids};

pub(crate) type AddonDisplayNameSnapshot = HashMap<(String, String), HashMap<String, String>>;

pub(crate) fn addon_display_name_key(repo_url: &str, local_path: &str) -> (String, String) {
    (
        crate::core::models::repository::normalize_repository_url(repo_url),
        crate::core::utils::content_hash::normalize_path(local_path),
    )
}

pub(crate) fn extract_addon_display_name(addon_path: &str) -> Option<String> {
    let addon_path = addon_path.trim();
    if addon_path.is_empty() {
        return None;
    }
    let mod_cpp = Path::new(addon_path).join("mod.cpp");
    let contents = fs::read_to_string(mod_cpp).ok()?;
    parse_mod_cpp_display_name(&contents)
}

pub(crate) fn parse_mod_cpp_display_name(contents: &str) -> Option<String> {
    let bytes = contents.as_bytes();
    let mut idx = 0usize;
    while idx < bytes.len() {
        if bytes.get(idx..idx.saturating_add(2)) == Some(b"//") {
            idx += 2;
            while bytes.get(idx).is_some_and(|byte| *byte != b'\n') {
                idx += 1;
            }
            continue;
        }
        if bytes.get(idx..idx.saturating_add(2)) == Some(b"/*") {
            idx += 2;
            while idx + 1 < bytes.len() && bytes.get(idx..idx.saturating_add(2)) != Some(b"*/") {
                idx += 1;
            }
            idx = idx.saturating_add(2).min(bytes.len());
            continue;
        }
        if bytes[idx].eq_ignore_ascii_case(&b'n')
            && bytes
                .get(idx..idx.saturating_add(4))
                .is_some_and(|slice| slice.eq_ignore_ascii_case(b"name"))
            && is_identifier_boundary_before(bytes, idx)
            && is_identifier_boundary_after(bytes, idx + 4)
        {
            let mut cursor = idx + 4;
            skip_cpp_ws_and_comments(bytes, &mut cursor);
            if bytes.get(cursor) == Some(&b'=') {
                cursor += 1;
                skip_cpp_ws_and_comments(bytes, &mut cursor);
                if let Some(value) = read_cpp_quoted_string(bytes, &mut cursor) {
                    let value = value.trim();
                    if !value.is_empty() {
                        return Some(value.to_string());
                    }
                }
            }
        }
        idx += 1;
    }
    None
}

pub(crate) async fn backfill_missing_addon_display_names(db: &FoxyDb) {
    let rows = match db
        .query_all(
            "SELECT id, local_path FROM addons WHERE display_name = ''",
            params![],
        )
        .await
    {
        Ok(rows) => rows,
        Err(err) => {
            warn!("Failed to load addons for display-name backfill: {}", err);
            return;
        }
    };
    if rows.is_empty() {
        return;
    }

    let updates = rows
        .iter()
        .filter_map(|row| {
            let id = row.get_i64("id").ok()?;
            let local_path = row.get_string("local_path").ok()?;
            extract_addon_display_name(&local_path).map(|name| (id, name))
        })
        .collect::<Vec<_>>();
    if updates.is_empty() {
        debug!("Addon display-name backfill found no local mod.cpp names to persist");
        return;
    }

    persist_addon_display_names(db, &updates).await;
    log::info!(
        "Backfilled addon display names from local mod.cpp files (updated={})",
        updates.len()
    );
}

pub(crate) async fn regenerate_addon_display_names_for_repo_id(db: &FoxyDb, repo_id: i64) {
    let ids: Vec<i64> = match db
        .query_all(
            "SELECT addon_id FROM repository_addons WHERE repository_id = ?",
            params![repo_id],
        )
        .await
    {
        Ok(rows) => rows
            .iter()
            .filter_map(|row| row.get_i64("addon_id").ok())
            .collect(),
        Err(err) => {
            warn!(
                "Failed to load repository addon links for display-name regeneration (repo_id={}): {}",
                repo_id, err
            );
            return;
        }
    };
    regenerate_addon_display_names_for_ids(db, &ids).await;
}

pub(crate) async fn regenerate_addon_display_names_for_ids(db: &FoxyDb, ids: &[i64]) {
    if ids.is_empty() {
        return;
    }
    let mut addons: Vec<(i64, String, String)> = Vec::new();
    let mut sorted = ids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    for chunk in sorted.chunks(read_chunk_ids()) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql =
            format!("SELECT id, local_path, display_name FROM addons WHERE id IN ({placeholders})");
        let values: Vec<DbValue> = chunk.iter().copied().map(DbValue::from).collect();
        match db.query_all(&sql, values).await {
            Ok(batch) => {
                for row in batch {
                    let (Ok(id), Ok(local_path), Ok(display_name)) = (
                        row.get_i64("id"),
                        row.get_string("local_path"),
                        row.get_string("display_name"),
                    ) else {
                        continue;
                    };
                    addons.push((id, local_path, display_name));
                }
            }
            Err(err) => {
                warn!(
                    "Failed to load addons for display-name regeneration: {}",
                    err
                );
                return;
            }
        }
    }

    // Keep synchronous mod.cpp parsing off the async executor.
    let updates = match tokio::task::spawn_blocking(move || {
        addons
            .into_iter()
            .filter_map(|(id, local_path, current_name)| {
                let parsed = extract_addon_display_name(&local_path)?;
                (parsed != current_name).then_some((id, parsed))
            })
            .collect::<Vec<_>>()
    })
    .await
    {
        Ok(updates) => updates,
        Err(err) => {
            warn!(
                "Failed to parse mod.cpp files for display-name regeneration: {}",
                err
            );
            return;
        }
    };
    persist_addon_display_names(db, &updates).await;
}

pub(crate) async fn load_addon_display_name_snapshot(
    db: &FoxyDb,
    repo_urls: &[String],
) -> AddonDisplayNameSnapshot {
    let mut snapshot = AddonDisplayNameSnapshot::new();
    if repo_urls.is_empty() {
        return snapshot;
    }

    let rows = match db
        .query_all(
            "SELECT r.remote_url, r.local_path, a.name, a.display_name
             FROM repositories r
             JOIN repository_addons ra ON ra.repository_id = r.id
             JOIN addons a ON a.id = ra.addon_id
             WHERE r.remote_url IN (SELECT value FROM json_each(?))",
            params![serde_json::to_string(repo_urls).unwrap_or_default()],
        )
        .await
    {
        Ok(rows) => rows,
        Err(err) => {
            warn!("Failed to load addon display-name snapshot: {}", err);
            return snapshot;
        }
    };

    for row in rows {
        let Ok(repo_url) = row.get_string("remote_url") else {
            continue;
        };
        let Ok(local_path) = row.get_string("local_path") else {
            continue;
        };
        let Ok(addon_name) = row.get_string("name") else {
            continue;
        };
        let Ok(display_name) = row.get_string("display_name") else {
            continue;
        };
        if display_name.trim().is_empty() {
            continue;
        }
        snapshot
            .entry(addon_display_name_key(&repo_url, &local_path))
            .or_default()
            .insert(addon_name, display_name);
    }
    snapshot
}

async fn persist_addon_display_names(db: &FoxyDb, updates: &[(i64, String)]) {
    if updates.is_empty() {
        return;
    }
    for chunk in updates.chunks(bulk_write_chunk_rows()) {
        let placeholders = vec!["(?, ?)"; chunk.len()].join(", ");
        let sql = format!(
            "WITH data(id, display_name) AS (VALUES {})
             UPDATE addons
                SET display_name = (SELECT data.display_name FROM data WHERE data.id = addons.id)
              WHERE id IN (SELECT id FROM data)",
            placeholders
        );
        let mut values: Vec<DbValue> = Vec::with_capacity(chunk.len() * 2);
        for (id, display_name) in chunk {
            values.push((*id).into());
            values.push(display_name.clone().into());
        }
        if let Err(err) = db
            .execute_retry("persist addon display names", &sql, values)
            .await
        {
            warn!("Failed to persist addon display names: {}", err);
            return;
        }
    }
}

fn is_identifier_boundary_before(bytes: &[u8], idx: usize) -> bool {
    idx == 0 || !is_cpp_identifier_byte(bytes[idx - 1])
}

fn is_identifier_boundary_after(bytes: &[u8], idx: usize) -> bool {
    bytes
        .get(idx)
        .is_none_or(|byte| !is_cpp_identifier_byte(*byte))
}

fn is_cpp_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn skip_cpp_ws_and_comments(bytes: &[u8], cursor: &mut usize) {
    loop {
        while bytes.get(*cursor).is_some_and(u8::is_ascii_whitespace) {
            *cursor += 1;
        }
        if bytes.get(*cursor..cursor.saturating_add(2)) == Some(b"//") {
            while bytes.get(*cursor).is_some_and(|byte| *byte != b'\n') {
                *cursor += 1;
            }
            continue;
        }
        if bytes.get(*cursor..cursor.saturating_add(2)) == Some(b"/*") {
            *cursor += 2;
            while *cursor + 1 < bytes.len()
                && bytes.get(*cursor..cursor.saturating_add(2)) != Some(b"*/")
            {
                *cursor += 1;
            }
            *cursor = (*cursor).saturating_add(2).min(bytes.len());
            continue;
        }
        break;
    }
}

fn read_cpp_quoted_string(bytes: &[u8], cursor: &mut usize) -> Option<String> {
    if bytes.get(*cursor) != Some(&b'"') {
        return None;
    }
    *cursor += 1;
    let mut value = String::new();
    while let Some(byte) = bytes.get(*cursor).copied() {
        *cursor += 1;
        match byte {
            b'"' => return Some(value),
            b'\\' => {
                if let Some(next) = bytes.get(*cursor).copied() {
                    *cursor += 1;
                    value.push(next as char);
                }
            }
            _ => value.push(byte as char),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tasks::db_turso::build_test_database;

    #[test]
    fn display_name_keys_normalize_repository_urls_and_local_paths() {
        assert_eq!(
            addon_display_name_key(
                " https://repo.example.invalid:8080/repo ",
                "R:\\Mods\\MainRepo\\"
            ),
            addon_display_name_key(
                "https://repo.example.invalid:8080/repo/",
                "R:/Mods/MainRepo"
            )
        );
    }

    #[tokio::test]
    async fn display_name_snapshot_keeps_same_url_instances_separate() {
        let db = FoxyDb::from_turso(build_test_database().await);
        db.execute(
            "INSERT INTO repositories (id, name, remote_url, local_path) VALUES
             (1, 'MainRepo', 'https://repo.example.invalid:8080/repo/', 'R:/Mods/MainRepo'),
             (2, 'RepoAlpha', 'https://repo.example.invalid:8080/repo/', 'R:/Mods/RepoAlpha')",
            params![],
        )
        .await
        .expect("seed repository instances");
        db.execute(
            "INSERT INTO addons (id, name, remote_path, local_path, required, display_name) VALUES
             (11, '@mod_01', 'repo/@mod_01', 'R:/Mods/MainRepo/@mod_01', 1, 'Addon Alpha'),
             (12, '@mod_01', 'repo/@mod_01', 'R:/Mods/RepoAlpha/@mod_01', 1, 'Addon Beta')",
            params![],
        )
        .await
        .expect("seed addon display names");
        db.execute(
            "INSERT INTO repository_addons (repository_id, addon_id) VALUES (1, 11), (2, 12)",
            params![],
        )
        .await
        .expect("link instance addons");

        let snapshot = load_addon_display_name_snapshot(
            &db,
            &["https://repo.example.invalid:8080/repo/".to_string()],
        )
        .await;

        assert_eq!(snapshot.len(), 2);
        for (path, display_name) in [
            ("R:/Mods/MainRepo", "Addon Alpha"),
            ("R:/Mods/RepoAlpha/", "Addon Beta"),
        ] {
            assert_eq!(
                snapshot
                    .get(&addon_display_name_key(
                        "https://repo.example.invalid:8080/repo",
                        path
                    ))
                    .and_then(|names| names.get("@mod_01"))
                    .map(String::as_str),
                Some(display_name)
            );
        }
        assert!(load_addon_display_name_snapshot(&db, &[]).await.is_empty());
    }

    #[test]
    fn parses_mod_cpp_name_assignment() {
        let input = r#"
            class Mod {
                picture = "logo.paa";
            };
            name = "Community Base Addons v3.18.6";
        "#;

        assert_eq!(
            parse_mod_cpp_display_name(input).as_deref(),
            Some("Community Base Addons v3.18.6")
        );
    }

    #[test]
    fn parser_ignores_identifier_substrings_and_comments() {
        let input = r#"
            displayName = "Wrong";
            // name = "Wrong";
            class X { name = "Right"; };
        "#;

        assert_eq!(parse_mod_cpp_display_name(input).as_deref(), Some("Right"));
    }

    #[test]
    fn parser_skips_comments_around_name_assignment() {
        let input = r#"
            name /* assignment marker */ =
                // display value follows on the next line
                "Task Force \"Arrowhead\" Radio";
        "#;

        assert_eq!(
            parse_mod_cpp_display_name(input).as_deref(),
            Some("Task Force \"Arrowhead\" Radio")
        );
    }

    #[test]
    fn parser_ignores_empty_name_before_later_valid_assignment() {
        let input = r#"
            name = "   ";
            class ModInfo {
                name = "ACE";
            };
        "#;

        assert_eq!(parse_mod_cpp_display_name(input).as_deref(), Some("ACE"));
    }
}
