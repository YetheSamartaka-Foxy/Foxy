use crate::core::api::ProgressEvent;
use crate::core::utils::format::sanitize_log_path;
use anyhow::{Context, anyhow};
use log::{info, warn};
use rand::{RngExt, distr::Alphanumeric, rng};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::sync::Mutex;
use tokio::sync::broadcast::Sender;

pub(crate) type SharedRollbackSession = Arc<Mutex<UpdateRollbackSession>>;

const ROLLBACK_DIR_NAME: &str = "update-rollback";
const MANIFEST_FILE_NAME: &str = "manifest.json";
const BACKUPS_DIR_NAME: &str = "backups";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RollbackManifest {
    repository_url: String,
    committed: bool,
    entries: Vec<RollbackEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RollbackEntry {
    file_id: u64,
    target_path: PathBuf,
    backup_path: Option<PathBuf>,
    original_existed: bool,
    original_size: Option<u64>,
    promoted: bool,
    restored: bool,
}

pub(crate) struct UpdateRollbackSession {
    session_dir: PathBuf,
    manifest_path: PathBuf,
    backups_dir: PathBuf,
    manifest: RollbackManifest,
}

impl UpdateRollbackSession {
    pub(crate) async fn new(
        rollback_root: impl AsRef<Path>,
        repository_url: &str,
    ) -> anyhow::Result<Self> {
        let base_dir = rollback_root.as_ref().join(ROLLBACK_DIR_NAME);
        fs::create_dir_all(&base_dir).await.with_context(|| {
            format!(
                "failed to create rollback root {}",
                sanitize_log_path(&base_dir)
            )
        })?;

        let session_id = unique_session_id();
        let session_dir = base_dir.join(session_id);
        let backups_dir = session_dir.join(BACKUPS_DIR_NAME);
        fs::create_dir_all(&backups_dir).await.with_context(|| {
            format!(
                "failed to create rollback backup directory {}",
                sanitize_log_path(&backups_dir)
            )
        })?;

        let manifest_path = session_dir.join(MANIFEST_FILE_NAME);
        let manifest = RollbackManifest {
            repository_url: repository_url.to_string(),
            committed: false,
            entries: Vec::new(),
        };
        let session = Self {
            session_dir,
            manifest_path,
            backups_dir,
            manifest,
        };
        session.persist_manifest().await?;
        Ok(session)
    }

    pub(crate) async fn cleanup_stale_sessions(
        rollback_root: impl AsRef<Path>,
        _progress_tx: Option<&Sender<ProgressEvent>>,
    ) -> anyhow::Result<()> {
        let base_dir = rollback_root.as_ref().join(ROLLBACK_DIR_NAME);
        let Ok(mut sessions) = fs::read_dir(&base_dir).await else {
            return Ok(());
        };

        while let Some(entry) = sessions.next_entry().await? {
            let session_dir = entry.path();
            let manifest_path = session_dir.join(MANIFEST_FILE_NAME);
            let Ok(manifest_bytes) = fs::read(&manifest_path).await else {
                continue;
            };
            let Ok(manifest) = serde_json::from_slice::<RollbackManifest>(&manifest_bytes) else {
                warn!(
                    "Leaving unreadable rollback manifest in place: {}",
                    sanitize_log_path(&manifest_path)
                );
                continue;
            };

            if manifest.committed || manifest.entries.iter().all(|entry| entry.restored) {
                remove_dir_if_exists(&session_dir).await?;
                continue;
            }

            let backups_dir = session_dir.join(BACKUPS_DIR_NAME);
            let session = Self {
                session_dir,
                manifest_path,
                backups_dir,
                manifest,
            };
            warn!(
                "Discarding stale rollback session for repository {} and preserving promoted files for resume",
                session.manifest.repository_url
            );
            if let Err(err) = remove_dir_if_exists(&session.session_dir).await {
                warn!(
                    "Failed to discard stale rollback session for repository {}: {}",
                    session.manifest.repository_url, err
                );
            }
        }

        Ok(())
    }

    pub(crate) async fn promote_file(
        &mut self,
        file_id: u64,
        staged_path: impl AsRef<Path>,
        target_path: impl AsRef<Path>,
    ) -> anyhow::Result<()> {
        let target_path = normalize_path(target_path.as_ref());
        self.prepare_replace(file_id, &target_path).await?;

        #[cfg(target_os = "windows")]
        if entry_for_target(&self.manifest.entries, &target_path)
            .map(|entry| entry.original_existed)
            .unwrap_or(false)
        {
            remove_file_if_exists(&target_path).await.with_context(|| {
                format!(
                    "failed to remove existing target before promote {}",
                    sanitize_log_path(&target_path)
                )
            })?;
        }

        let staged_path = staged_path.as_ref();
        if let Err(err) = fs::rename(staged_path, &target_path).await {
            if let Err(restore_err) = self.restore_entry(file_id, &target_path).await {
                warn!(
                    "Failed to restore target after promote failure for {}: {}",
                    sanitize_log_path(&target_path),
                    restore_err
                );
            }
            return Err(anyhow!(err)).with_context(|| {
                format!(
                    "failed to promote staged file {} -> {}",
                    sanitize_log_path(staged_path),
                    sanitize_log_path(&target_path)
                )
            });
        }

        if let Some(entry) = entry_for_target_mut(&mut self.manifest.entries, &target_path) {
            entry.promoted = true;
            entry.restored = false;
        }
        self.persist_manifest().await?;
        Ok(())
    }

    pub(crate) async fn restore_entry(
        &mut self,
        file_id: u64,
        target_path: impl AsRef<Path>,
    ) -> anyhow::Result<()> {
        let target_path = normalize_path(target_path.as_ref());
        let Some(idx) = self
            .manifest
            .entries
            .iter()
            .position(|entry| entry.file_id == file_id && entry.target_path == target_path)
        else {
            return Ok(());
        };

        let entry = self.manifest.entries[idx].clone();
        restore_entry_filesystem(&entry).await?;
        self.manifest.entries[idx].restored = true;
        self.persist_manifest().await?;
        Ok(())
    }

    pub(crate) async fn restore_all(
        &mut self,
        progress_tx: Option<Sender<ProgressEvent>>,
    ) -> anyhow::Result<()> {
        let mut restore_errors = Vec::new();
        let total = self
            .manifest
            .entries
            .iter()
            .filter(|entry| entry.promoted && !entry.restored)
            .count();
        let entries = self.manifest.entries.clone();
        for (restored_count, entry) in entries
            .into_iter()
            .filter(|entry| entry.promoted && !entry.restored)
            .enumerate()
        {
            if let Some(tx) = progress_tx.as_ref() {
                let percent = if total == 0 {
                    0.0
                } else {
                    restored_count as f32 / total as f32
                };
                let _ = tx.send(ProgressEvent::Stage {
                    label: "Reverting changes".to_string(),
                    percent,
                });
            }

            match restore_entry_filesystem(&entry).await {
                Ok(()) => {
                    if let Some(current) =
                        entry_for_target_mut(&mut self.manifest.entries, &entry.target_path)
                    {
                        current.restored = true;
                    }
                    if let Err(err) = self.persist_manifest().await {
                        restore_errors.push(err);
                    }
                }
                Err(err) => restore_errors.push(err),
            }
        }

        if let Some(tx) = progress_tx.as_ref() {
            let _ = tx.send(ProgressEvent::Stage {
                label: "Reverting changes".to_string(),
                percent: 1.0,
            });
        }

        if restore_errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(
                "rollback failed for {} file(s); rollback backups were kept in {}",
                restore_errors.len(),
                sanitize_log_path(&self.session_dir)
            ))
        }
    }

    pub(crate) async fn commit(&mut self) -> anyhow::Result<()> {
        self.manifest.committed = true;
        self.persist_manifest().await?;
        remove_dir_if_exists(&self.session_dir)
            .await
            .with_context(|| {
                format!(
                    "failed to remove rollback session {}",
                    sanitize_log_path(&self.session_dir)
                )
            })
    }

    pub(crate) fn touched_file_ids(&self) -> HashSet<u64> {
        self.manifest
            .entries
            .iter()
            .map(|entry| entry.file_id)
            .collect()
    }

    async fn prepare_replace(&mut self, file_id: u64, target_path: &Path) -> anyhow::Result<()> {
        if entry_for_target(&self.manifest.entries, target_path).is_some() {
            return Ok(());
        }

        let metadata = fs::metadata(target_path).await;
        let (original_existed, original_size, backup_path) = match metadata {
            Ok(meta) => {
                let backup_path = self.backup_path(file_id, target_path);
                fs::copy(target_path, &backup_path).await.with_context(|| {
                    format!(
                        "failed to create rollback backup {} -> {}",
                        sanitize_log_path(target_path),
                        sanitize_log_path(&backup_path)
                    )
                })?;
                let backup_meta = fs::metadata(&backup_path).await.with_context(|| {
                    format!(
                        "failed to verify rollback backup {}",
                        sanitize_log_path(&backup_path)
                    )
                })?;
                if backup_meta.len() != meta.len() {
                    return Err(anyhow!(
                        "rollback backup size mismatch for {}: expected {}, got {}",
                        sanitize_log_path(target_path),
                        meta.len(),
                        backup_meta.len()
                    ));
                }
                (true, Some(meta.len()), Some(backup_path))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (false, None, None),
            Err(err) => {
                return Err(anyhow!(err)).with_context(|| {
                    format!(
                        "failed to inspect target before rollback registration {}",
                        sanitize_log_path(target_path)
                    )
                });
            }
        };

        self.manifest.entries.push(RollbackEntry {
            file_id,
            target_path: target_path.to_path_buf(),
            backup_path,
            original_existed,
            original_size,
            promoted: false,
            restored: false,
        });
        self.persist_manifest().await
    }

    fn backup_path(&self, file_id: u64, target_path: &Path) -> PathBuf {
        let mut sanitized = target_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file")
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
                    ch
                } else {
                    '_'
                }
            })
            .collect::<String>();
        if sanitized.is_empty() {
            sanitized = "file".to_string();
        }
        self.backups_dir
            .join(format!("{}_{}.original", file_id, sanitized))
    }

    async fn persist_manifest(&self) -> anyhow::Result<()> {
        let json =
            serde_json::to_vec_pretty(&self.manifest).context("failed to serialize manifest")?;
        fs::write(&self.manifest_path, json).await.with_context(|| {
            format!(
                "failed to write rollback manifest {}",
                sanitize_log_path(&self.manifest_path)
            )
        })
    }
}

async fn restore_entry_filesystem(entry: &RollbackEntry) -> anyhow::Result<()> {
    if !entry.promoted && entry.target_path.exists() == entry.original_existed {
        return Ok(());
    }

    if entry.original_existed {
        let Some(backup_path) = entry.backup_path.as_ref() else {
            return Err(anyhow!(
                "rollback manifest is missing backup path for {}",
                sanitize_log_path(&entry.target_path)
            ));
        };
        remove_file_if_exists(&entry.target_path).await?;
        if let Some(parent) = entry.target_path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!(
                    "failed to create target parent during rollback {}",
                    sanitize_log_path(parent)
                )
            })?;
        }
        fs::copy(backup_path, &entry.target_path)
            .await
            .with_context(|| {
                format!(
                    "failed to restore rollback backup {} -> {}",
                    sanitize_log_path(backup_path),
                    sanitize_log_path(&entry.target_path)
                )
            })?;
    } else {
        remove_file_if_exists(&entry.target_path).await?;
    }

    info!(
        "Restored rollback entry file_id={} target={}",
        entry.file_id,
        sanitize_log_path(&entry.target_path)
    );
    Ok(())
}

async fn remove_file_if_exists(path: &Path) -> anyhow::Result<()> {
    crate::core::utils::file_io::retry_remove_file(path)
        .await
        .map_err(|err| {
            anyhow!(err).context(format!("failed to remove file {}", sanitize_log_path(path)))
        })
}

async fn remove_dir_if_exists(path: &Path) -> anyhow::Result<()> {
    match fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(anyhow!(err))
            .with_context(|| format!("failed to remove directory {}", sanitize_log_path(path))),
    }
}

fn entry_for_target<'a>(
    entries: &'a [RollbackEntry],
    target_path: &Path,
) -> Option<&'a RollbackEntry> {
    entries
        .iter()
        .find(|entry| entry.target_path == target_path)
}

fn entry_for_target_mut<'a>(
    entries: &'a mut [RollbackEntry],
    target_path: &Path,
) -> Option<&'a mut RollbackEntry> {
    entries
        .iter_mut()
        .find(|entry| entry.target_path == target_path)
}

fn normalize_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn unique_session_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let suffix: String = rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect();
    format!("{millis}-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::fs;

    #[tokio::test]
    async fn rollback_restores_replaced_file() {
        let dir = tempdir().unwrap();
        let rollback_root = dir.path().join("tmp");
        let target = dir.path().join("addon.pbo");
        let staged = dir.path().join("addon.pbo.foxy.part");
        fs::write(&target, b"original").await.unwrap();
        fs::write(&staged, b"updated").await.unwrap();

        let mut session = UpdateRollbackSession::new(&rollback_root, "https://example.test/")
            .await
            .unwrap();
        session.promote_file(7, &staged, &target).await.unwrap();
        assert_eq!(fs::read(&target).await.unwrap(), b"updated");

        session.restore_all(None).await.unwrap();
        assert_eq!(fs::read(&target).await.unwrap(), b"original");
        assert!(session.touched_file_ids().contains(&7));
    }

    #[tokio::test]
    async fn rollback_removes_created_file() {
        let dir = tempdir().unwrap();
        let rollback_root = dir.path().join("tmp");
        let target = dir.path().join("new-addon.pbo");
        let staged = dir.path().join("new-addon.pbo.foxy.part");
        fs::write(&staged, b"new").await.unwrap();

        let mut session = UpdateRollbackSession::new(&rollback_root, "https://example.test/")
            .await
            .unwrap();
        session.promote_file(11, &staged, &target).await.unwrap();
        assert_eq!(fs::read(&target).await.unwrap(), b"new");

        session.restore_all(None).await.unwrap();
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn commit_deletes_session_directory() {
        let dir = tempdir().unwrap();
        let rollback_root = dir.path().join("tmp");
        let session_dir = {
            let mut session = UpdateRollbackSession::new(&rollback_root, "https://example.test/")
                .await
                .unwrap();
            let session_dir = session.session_dir.clone();
            session.commit().await.unwrap();
            session_dir
        };

        assert!(!session_dir.exists());
    }

    #[tokio::test]
    async fn cleanup_stale_sessions_preserves_promoted_files() {
        let dir = tempdir().unwrap();
        let rollback_root = dir.path().join("tmp");
        let target = dir.path().join("addon.pbo");
        let staged = dir.path().join("addon.pbo.foxy.part");
        fs::write(&target, b"original").await.unwrap();
        fs::write(&staged, b"updated").await.unwrap();

        let mut session = UpdateRollbackSession::new(&rollback_root, "https://example.test/")
            .await
            .unwrap();
        session.promote_file(7, &staged, &target).await.unwrap();
        drop(session);

        UpdateRollbackSession::cleanup_stale_sessions(&rollback_root, None)
            .await
            .unwrap();
        assert_eq!(fs::read(&target).await.unwrap(), b"updated");
    }
}
