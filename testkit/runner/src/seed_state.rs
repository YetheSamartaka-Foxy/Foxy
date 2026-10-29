use anyhow::{Context, Result, bail, ensure};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Pending-update folder key. Matches `content_hash::normalize_path`: slashes,
/// no trailing slash, and lowercase on Windows.
pub fn normalize_local_path(path: &str) -> String {
    let mut normalized = path.replace('\\', "/");
    while normalized.ends_with('/') {
        normalized.pop();
    }
    if cfg!(windows) {
        normalized.make_ascii_lowercase();
    }
    normalized
}

pub fn canonical_repository_url(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.ends_with('/') {
        trimmed.to_string()
    } else {
        format!("{trimmed}/")
    }
}

pub fn pending_diff(addon_name: &str) -> String {
    serde_json::json!([{
        "name": addon_name,
        "needs_update": true,
        "total_bytes": 0,
        "files": []
    }])
    .to_string()
}

pub fn addon_needing_update(payload: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    value.as_array().and_then(|mods| {
        mods.iter().find_map(|mod_summary| {
            if mod_summary["needs_update"].as_bool() == Some(true) {
                mod_summary["name"].as_str().map(str::to_string)
            } else {
                None
            }
        })
    })
}

fn active_space_database(config: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(config.join("games.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let id = value["active_game_space_id"].as_str()?;
    let path = config.join("games").join(id).join("database.db");
    path.is_file().then_some(path)
}

fn child_path(parent: &str, name: &str) -> String {
    let slash = if parent.contains('\\') && !parent.contains('/') {
        '\\'
    } else {
        '/'
    };
    format!("{}{slash}{name}", parent.trim_end_matches(['\\', '/']))
}

fn database_files(config: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    if let Some(active) = active_space_database(config) {
        found.push(active);
    }
    let games = config.join("games");
    if games.is_dir() {
        for entry in std::fs::read_dir(&games)? {
            let path = entry?.path().join("database.db");
            if path.is_file() && !found.contains(&path) {
                found.push(path);
            }
        }
    }
    let legacy = config.join("database.db");
    if legacy.is_file() {
        found.push(legacy);
    }
    ensure!(
        !found.is_empty(),
        "The case config has no game-space database yet"
    );
    Ok(found)
}

pub fn apply(
    config: &Path,
    remote_url: &str,
    local_path: &str,
    stale_link: bool,
    incomplete_parts: bool,
) -> Result<()> {
    let runtime = tokio::runtime::Runtime::new().context("Starting the database runtime")?;
    runtime.block_on(apply_async(
        config,
        remote_url,
        local_path,
        stale_link,
        incomplete_parts,
    ))
}

async fn apply_async(
    config: &Path,
    remote_url: &str,
    local_path: &str,
    stale_link: bool,
    incomplete_parts: bool,
) -> Result<()> {
    let wanted_url = canonical_repository_url(remote_url);
    let wanted_path = normalize_local_path(local_path);
    let mut last_error = None;
    for _ in 0..20 {
        match seed_once(
            config,
            &wanted_url,
            &wanted_path,
            stale_link,
            incomplete_parts,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(error) if database_locked(&error) => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.context("Seeding the startup database")?)
}

fn database_locked(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}").to_ascii_lowercase();
    text.contains("lock") || text.contains("busy")
}

async fn seed_once(
    config: &Path,
    wanted_url: &str,
    wanted_path: &str,
    stale_link: bool,
    incomplete_parts: bool,
) -> Result<()> {
    for database_path in database_files(config)? {
        let database = turso::Builder::new_local(&database_path.to_string_lossy())
            .build()
            .await
            .context("Opening a game-space database")?;
        let connection = database
            .connect()
            .context("Connecting to the game-space database")?;
        if let Some(repository_id) =
            matching_repository(&connection, wanted_url, wanted_path).await?
        {
            seed_repository(
                &connection,
                repository_id,
                wanted_path,
                stale_link,
                incomplete_parts,
            )
            .await?;
            eprintln!(
                "seed-startup-state: stale_link={stale_link} incomplete_parts={incomplete_parts}"
            );
            return Ok(());
        }
    }
    bail!("The case repository is not in the game-space database")
}

async fn matching_repository(
    connection: &turso::Connection,
    wanted_url: &str,
    wanted_path: &str,
) -> Result<Option<i64>> {
    let mut rows = connection
        .query("SELECT id, remote_url, local_path FROM repositories", ())
        .await?;
    while let Some(row) = rows.next().await? {
        let id = row.get::<i64>(0)?;
        let url = canonical_repository_url(&row.get::<String>(1)?);
        let path = normalize_local_path(&row.get::<String>(2)?);
        if url == wanted_url && path == wanted_path {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

async fn seed_repository(
    connection: &turso::Connection,
    repository_id: i64,
    local_path: &str,
    stale_link: bool,
    incomplete_parts: bool,
) -> Result<()> {
    let addons = repository_addons(connection, repository_id).await?;
    ensure!(
        !addons.is_empty(),
        "The case repository has no addon to attach startup state to"
    );
    let stored_url = stored_repository_url(connection, repository_id).await?;
    let existing = pending_payload(connection, &stored_url, local_path).await?;
    let addon = if let Some(payload) = existing.as_deref() {
        let name = addon_needing_update(payload)
            .context("The saved pending update does not name an addon")?;
        addons
            .iter()
            .find(|addon| addon.name.eq_ignore_ascii_case(&name))
            .context("The pending addon is not in the repository")?
            .clone()
    } else {
        addons[0].clone()
    };
    if existing.is_none() {
        let updated_at = chrono::Utc::now().timestamp();
        connection
            .execute(
                "INSERT INTO pending_updates (repository_url, local_path, diff_json, updated_at) \
                 VALUES (?, ?, ?, ?)",
                (
                    turso::Value::Text(stored_url),
                    turso::Value::Text(local_path.to_string()),
                    turso::Value::Text(pending_diff(&addon.name)),
                    turso::Value::Integer(updated_at),
                ),
            )
            .await?;
    }
    if stale_link {
        insert_stale_link(connection, &addon).await?;
    }
    if incomplete_parts {
        connection
            .execute(
                "DELETE FROM subfiles WHERE file_id IN (SELECT file_id FROM addon_files WHERE addon_id = ?)",
                [turso::Value::Integer(addon.id)],
            )
            .await?;
    }
    Ok(())
}

#[derive(Clone)]
struct SeedAddon {
    id: i64,
    name: String,
    remote_path: String,
    local_path: String,
}

async fn repository_addons(
    connection: &turso::Connection,
    repository_id: i64,
) -> Result<Vec<SeedAddon>> {
    let mut rows = connection
        .query(
            "SELECT a.id, a.name, a.remote_path, a.local_path \
             FROM addons a \
             JOIN repository_addons ra ON ra.addon_id = a.id \
             WHERE ra.repository_id = ? \
             ORDER BY a.id",
            [turso::Value::Integer(repository_id)],
        )
        .await?;
    let mut addons = Vec::new();
    while let Some(row) = rows.next().await? {
        addons.push(SeedAddon {
            id: row.get::<i64>(0)?,
            name: row.get::<String>(1)?,
            remote_path: row.get::<String>(2)?,
            local_path: row.get::<String>(3)?,
        });
    }
    Ok(addons)
}

async fn stored_repository_url(
    connection: &turso::Connection,
    repository_id: i64,
) -> Result<String> {
    let mut rows = connection
        .query(
            "SELECT remote_url FROM repositories WHERE id = ?",
            [turso::Value::Integer(repository_id)],
        )
        .await?;
    let row = rows.next().await?.context("Missing repository row")?;
    Ok(row.get::<String>(0)?)
}

async fn pending_payload(
    connection: &turso::Connection,
    remote_url: &str,
    local_path: &str,
) -> Result<Option<String>> {
    let mut rows = connection
        .query(
            "SELECT diff_json FROM pending_updates WHERE repository_url = ? AND local_path = ?",
            (
                turso::Value::Text(remote_url.to_string()),
                turso::Value::Text(local_path.to_string()),
            ),
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(row.get::<String>(0)?),
        None => None,
    })
}

async fn insert_stale_link(connection: &turso::Connection, addon: &SeedAddon) -> Result<()> {
    let mut existing = connection
        .query(
            "SELECT files.id FROM files \
             JOIN addon_files ON addon_files.file_id = files.id \
             WHERE addon_files.addon_id = ? AND files.name = 'stale-link.bin'",
            [turso::Value::Integer(addon.id)],
        )
        .await?;
    if existing.next().await?.is_some() {
        return Ok(());
    }
    let file_id = next_id(connection, "files").await?;
    let subfile_id = next_id(connection, "subfiles").await?;
    let remote_path = child_path(&addon.remote_path, "stale-link.bin");
    let local_path = child_path(&addon.local_path, "stale-link.bin");
    connection.execute("BEGIN", ()).await?;
    let inserted = async {
        connection
            .execute(
                "INSERT INTO files \
                 (id, name, remote_path, local_path, local_checksum, remote_checksum, local_content_hash, length, data_order) \
                 VALUES (?, 'stale-link.bin', ?, ?, 'STALE', 'STALE', '', 4, 0)",
                (
                    turso::Value::Integer(file_id),
                    turso::Value::Text(remote_path),
                    turso::Value::Text(local_path),
                ),
            )
            .await?;
        connection
            .execute(
                "INSERT INTO addon_files (addon_id, file_id) VALUES (?, ?)",
                (
                    turso::Value::Integer(addon.id),
                    turso::Value::Integer(file_id),
                ),
            )
            .await?;
        connection
            .execute(
                "INSERT INTO subfiles \
                 (id, file_id, path, local_length, local_start, remote_length, remote_start, local_checksum, remote_checksum, data_order) \
                 VALUES (?, ?, 'stale-link.bin', 4, 0, 4, 0, 'STALE', 'STALE', 0)",
                (
                    turso::Value::Integer(subfile_id),
                    turso::Value::Integer(file_id),
                ),
            )
            .await?;
        connection.execute("COMMIT", ()).await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error) = inserted {
        let _ = connection.execute("ROLLBACK", ()).await;
        return Err(error);
    }
    Ok(())
}

async fn next_id(connection: &turso::Connection, table: &str) -> Result<i64> {
    ensure!(matches!(table, "files" | "subfiles"), "Unexpected id table");
    let sql = if table == "files" {
        "SELECT COALESCE(MAX(id), 0) + 1 FROM files"
    } else {
        "SELECT COALESCE(MAX(id), 0) + 1 FROM subfiles"
    };
    let mut rows = connection.query(sql, ()).await?;
    let row = rows.next().await?.context("Missing next id")?;
    Ok(row.get::<i64>(0)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_path_key_matches_the_pending_update_normalizer() {
        let key = normalize_local_path(r"C:\Mods\Main\");
        assert!(!key.ends_with('/'));
        assert!(!key.contains('\\'));
        if cfg!(windows) {
            assert_eq!(key, "c:/mods/main");
        } else {
            assert_eq!(key, "C:/Mods/Main");
        }
    }

    #[test]
    fn pending_diff_names_the_addon_that_needs_an_update() {
        let payload = pending_diff("@mod_01");
        assert_eq!(addon_needing_update(&payload).as_deref(), Some("@mod_01"));
        assert_eq!(addon_needing_update("[]"), None);
    }

    #[test]
    fn repository_url_keeps_one_trailing_slash() {
        assert_eq!(
            canonical_repository_url("http://127.0.0.1:9"),
            "http://127.0.0.1:9/"
        );
        assert_eq!(
            canonical_repository_url("http://127.0.0.1:9/"),
            "http://127.0.0.1:9/"
        );
    }
}
