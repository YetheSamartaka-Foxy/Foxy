use super::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct EmptyPublication {
    repository_empty: bool,
    addons: HashSet<String>,
}

fn empty_publication_key(url: &str, path: &str) -> String {
    format!(
        "{}\n{}",
        crate::core::models::repository::normalize_repository_url(url),
        crate::core::utils::content_hash::normalize_path(path)
    )
}

fn empty_publication_path(context: &FoxyContext) -> Option<PathBuf> {
    context
        .manifest_cache
        .as_ref()
        .map(|cache| cache.space_dir().join("empty_publications.json"))
}

fn load_empty_publications(path: &Path) -> Result<BTreeMap<String, EmptyPublication>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| "Could not read accepted empty-publication state".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(_) => Err("Could not read accepted empty-publication state".into()),
    }
}

pub(crate) fn accepted_empty_scope(
    context: &FoxyContext,
    url: &str,
    path: &str,
) -> Result<(bool, HashSet<String>), String> {
    let Some(record_path) = empty_publication_path(context) else {
        return Ok((false, context.empty_remote_addon_names()));
    };
    let stored = load_empty_publications(&record_path)?;
    Ok(stored
        .get(&empty_publication_key(url, path))
        .map(|entry| (entry.repository_empty, entry.addons.clone()))
        .unwrap_or_default())
}

pub(crate) fn record_empty_scope(
    context: &FoxyContext,
    url: &str,
    path: &str,
    repository_empty: bool,
    addon: Option<(&str, bool)>,
) -> Result<(), String> {
    static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = WRITE
        .lock()
        .map_err(|_| "Empty-publication state is unavailable".to_owned())?;
    let Some(record_path) = empty_publication_path(context) else {
        return Ok(());
    };
    let mut stored = load_empty_publications(&record_path)?;
    let entry = stored.entry(empty_publication_key(url, path)).or_default();
    entry.repository_empty = repository_empty;
    if let Some((name, empty)) = addon {
        if empty {
            entry.addons.insert(name.to_lowercase());
        } else {
            entry.addons.remove(&name.to_lowercase());
        }
    }
    crate::core::utils::fs_safety::atomic_write(
        &record_path,
        &serde_json::to_vec_pretty(&stored)
            .map_err(|_| "Could not encode empty-publication state".to_owned())?,
    )
    .map_err(|_| "Could not save accepted empty-publication state".into())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedFilePreview {
    pub local_path: String,
    pub resolved_path: String,
    pub addon_name: String,
    pub shared: bool,
}

async fn retained_empty_files(
    context: Arc<FoxyContext>,
    url: &str,
    path: &str,
) -> Result<Vec<RetainedFilePreview>, String> {
    let (repository_empty, empty_addons) = accepted_empty_scope(&context, url, path)?;
    if !repository_empty && empty_addons.is_empty() {
        return Ok(Vec::new());
    }
    let repository = crate::core::models::repository::load_repository_by_remote_url_and_local_path(
        context.clone(),
        url,
        path,
    )
    .await
    .map_err(|_| "Could not locate retained publication files".to_owned())?;
    let rows = context.db().query_all(
        "SELECT DISTINCT f.local_path, a.name, EXISTS(SELECT 1 FROM addon_files other_af JOIN repository_addons other_ra ON other_ra.addon_id = other_af.addon_id WHERE other_af.file_id = f.id AND other_ra.repository_id != ?) AS shared FROM repository_addons ra JOIN addons a ON a.id = ra.addon_id JOIN addon_files af ON af.addon_id = a.id JOIN files f ON f.id = af.file_id WHERE ra.repository_id = ?",
        crate::core::db::params![repository.id as i64, repository.id as i64],
    ).await.map_err(|_| "Could not inspect retained file ownership".to_owned())?;
    let paths: Vec<_> = rows
        .iter()
        .filter_map(|row| {
            let addon = row.get_string("name").ok()?;
            (repository_empty || empty_addons.contains(&addon.to_lowercase()))
                .then(|| {
                    Some((
                        row.get_string("local_path").ok()?,
                        addon,
                        row.get_bool("shared").ok()?,
                    ))
                })
                .flatten()
        })
        .collect();
    let sibling_roots = context.db().query_all(
        "SELECT DISTINCT a.local_path FROM repository_addons ra JOIN addons a ON a.id = ra.addon_id WHERE ra.repository_id != ?",
        crate::core::db::params![repository.id as i64],
    ).await.map_err(|_| "Could not inspect retained file ownership".to_owned())?
        .iter().map(|row| row.get_string("local_path").map(PathBuf::from))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Could not inspect retained file ownership".to_owned())?;
    tokio::task::spawn_blocking(move || {
        let sibling_targets = resolved_owned_targets(&sibling_roots)?;
        let mut files = Vec::new();
        for (local_path, addon_name, shared) in paths {
            match Path::new(&local_path).canonicalize() {
                Ok(resolved) if resolved.is_file() => {
                    let shared = shared
                        || sibling_targets
                            .iter()
                            .any(|root| resolved.starts_with(root));
                    files.push(RetainedFilePreview {
                        local_path,
                        resolved_path: resolved.to_string_lossy().into_owned(),
                        addon_name,
                        shared,
                    });
                }
                Ok(_) => return Err("A retained path is no longer a regular file".to_owned()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    return Err(
                        "Could not read retained files. Restore access before removing them."
                            .to_owned(),
                    );
                }
            }
        }
        files.sort_by(|a, b| a.local_path.cmp(&b.local_path));
        files.dedup_by(|a, b| a.local_path == b.local_path);
        Ok(files)
    })
    .await
    .map_err(|_| "Retained-file inspection stopped".to_owned())?
}

fn resolved_owned_targets(roots: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut targets = Vec::new();
    for root in roots {
        match root.canonicalize() {
            Ok(resolved) => targets.push(resolved),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                return Err(
                    "Could not inspect another installation's retained file ownership".into(),
                );
            }
        }
        targets.extend(
            linked_targets(root)
                .map_err(|_| {
                    "Could not inspect another installation's linked file ownership".to_owned()
                })?
                .into_iter()
                .map(|(_, target)| target),
        );
    }
    Ok(targets)
}

pub async fn preview_retained_empty_files(
    url: &str,
    path: &str,
) -> Result<Vec<RetainedFilePreview>, String> {
    let space = crate::core::game::spaces::active_game_space_dir();
    let context = Arc::new(
        create_context_with_recheck_level(RecheckLevel::DEFAULT)
            .await
            .as_ref()
            .clone()
            .with_target_local_path(path)
            .with_manifest_cache(crate::core::utils::manifest_cache::ManifestCache::in_space(
                &space,
            )),
    );
    retained_empty_files(context, url, path).await
}

pub async fn remove_retained_empty_files(
    url: String,
    path: String,
    approved: Vec<RetainedFilePreview>,
) -> Result<usize, String> {
    crate::core::tasks::db_schema_version::ensure_database_ready_for_updates().await?;
    let space = crate::core::game::spaces::active_game_space_dir();
    let context = Arc::new(
        create_context_with_recheck_level(RecheckLevel::DEFAULT)
            .await
            .as_ref()
            .clone()
            .with_target_local_path(&path)
            .with_manifest_cache(crate::core::utils::manifest_cache::ManifestCache::in_space(
                &space,
            )),
    );
    let repository = load_repository_by_remote_url(context.clone(), &url)
        .await
        .map_err(|_| "Could not locate retained file ownership".to_owned())?;
    let addon_rows = context.db().query_all(
        "SELECT a.name FROM repository_addons ra JOIN addons a ON a.id = ra.addon_id WHERE ra.repository_id = ?",
        crate::core::db::params![repository.id as i64],
    ).await.map_err(|_| "Could not inspect retained file ownership".to_owned())?;
    let selected: HashSet<_> = approved
        .iter()
        .map(|file| file.addon_name.to_lowercase())
        .collect();
    let overrides = addon_rows
        .iter()
        .map(|row| {
            let name = row
                .get_string("name")
                .map_err(|_| "Could not inspect retained file ownership".to_owned())?
                .to_lowercase();
            let enabled = selected.contains(&name);
            Ok((name, enabled))
        })
        .collect::<Result<HashMap<_, _>, String>>()?;
    let _write_claims = claim_addon_writes(context.clone(), &url, &overrides, None).await?;
    let current = retained_empty_files(context.clone(), &url, &path).await?;
    if approved
        .iter()
        .any(|file| file.shared || !current.contains(file))
    {
        return Err(
            "Retained file ownership or linked target changed. Review removal again.".into(),
        );
    }
    let mut rollback = crate::core::tasks::download_files::UpdateRollbackSession::new_for_instance(
        &space, &url, &path,
    )
    .await
    .map_err(|_| "Could not prepare retained-file recovery".to_owned())?;
    for file in &approved {
        if let Err(error) = rollback
            .remove_file(0, Path::new(&file.resolved_path))
            .await
        {
            let restored = rollback.restore_all(None).await;
            return Err(format!(
                "Could not remove retained content: {error}; recovery: {restored:?}"
            ));
        }
    }
    rollback
        .commit()
        .await
        .map_err(|_| "Retained-file recovery cleanup could not finish".to_owned())?;
    Ok(approved.len())
}

#[derive(Clone, Debug)]
pub struct LinkedTargetPreview {
    pub link_path: String,
    pub target_path: String,
    pub affected_repositories: Vec<String>,
}

pub(crate) fn linked_targets(root: &Path) -> std::io::Result<Vec<(PathBuf, PathBuf)>> {
    let mut pending = vec![root.to_path_buf()];
    let mut links = Vec::new();
    while let Some(path) = pending.pop() {
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if metadata.file_type().is_symlink() {
            links.push((path.clone(), path.canonicalize()?));
        } else if metadata.is_dir() {
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_dir() || kind.is_symlink() {
                    pending.push(entry.path());
                }
            }
        }
    }
    links.sort();
    Ok(links)
}

pub async fn preview_linked_targets(root: &str) -> Result<Vec<LinkedTargetPreview>, String> {
    let root = PathBuf::from(root);
    let links = tokio::task::spawn_blocking(move || linked_targets(&root))
        .await
        .map_err(|_| "Linked-folder inspection stopped".to_owned())?
        .map_err(|_| {
            "Could not inspect linked folders. Restore access before updating.".to_owned()
        })?;
    if links.is_empty() {
        return Ok(Vec::new());
    }
    let context = create_context_with_recheck_level(RecheckLevel::DEFAULT).await;
    let rows = context
        .db()
        .query_all(
            "SELECT name, local_path FROM repositories",
            crate::core::db::params![],
        )
        .await
        .map_err(|_| "Could not identify installations sharing linked files".to_owned())?;
    let repositories: Vec<(String, PathBuf)> = rows
        .iter()
        .filter_map(|row| {
            Some((
                row.get_string("name").ok()?,
                PathBuf::from(row.get_string("local_path").ok()?),
            ))
        })
        .collect();
    tokio::task::spawn_blocking(move || {
        links
            .into_iter()
            .map(|(link, target)| {
                let affected_repositories = repositories
                    .iter()
                    .filter_map(|(name, path)| {
                        let resolved = path.canonicalize().ok()?;
                        (target.starts_with(&resolved)
                            || resolved.starts_with(&target)
                            || linked_targets(path)
                                .ok()?
                                .iter()
                                .any(|(_, other)| other == &target))
                        .then(|| name.clone())
                    })
                    .collect();
                LinkedTargetPreview {
                    link_path: link.to_string_lossy().into_owned(),
                    target_path: target.to_string_lossy().into_owned(),
                    affected_repositories,
                }
            })
            .collect()
    })
    .await
    .map_err(|_| "Linked-folder inspection stopped".to_owned())
}

pub(crate) async fn bind_write_targets(
    root: &str,
    enrolled: &[LinkedTargetPreview],
) -> Result<Arc<crate::core::tasks::download_files::PinnedDownloadTargets>, String> {
    let root_path = PathBuf::from(root);
    let links = tokio::task::spawn_blocking(move || linked_targets(&root_path))
        .await
        .map_err(|_| "Linked-folder inspection stopped".to_owned())?
        .map_err(|_| {
            "Could not inspect linked folders. Restore access before updating.".to_owned()
        })?;
    if links.len() != enrolled.len()
        || links.iter().any(|(link, target)| {
            !enrolled.iter().any(|approved| {
                crate::core::utils::content_hash::normalize_path(&approved.link_path)
                    == crate::core::utils::content_hash::normalize_path(&link.to_string_lossy())
                    && crate::core::utils::content_hash::normalize_path(&approved.target_path)
                        == crate::core::utils::content_hash::normalize_path(
                            &target.to_string_lossy(),
                        )
            })
        })
    {
        return Err("Linked folders require explicit target approval before updating. Review the linked destinations and retry.".into());
    }
    Ok(Arc::new(
        crate::core::tasks::download_files::PinnedDownloadTargets::from_links(
            PathBuf::from(root),
            links,
        )?,
    ))
}

pub(super) async fn claim_addon_writes(
    context: Arc<FoxyContext>,
    url: &str,
    overrides: &HashMap<String, bool>,
    pinned_targets: Option<Arc<crate::core::tasks::download_files::PinnedDownloadTargets>>,
) -> Result<Vec<std::fs::File>, String> {
    let repository = load_repository_by_remote_url(context.clone(), url)
        .await
        .map_err(|_| "Could not locate update ownership".to_owned())?;
    let rows = context.db().query_all("SELECT a.name, a.local_path, a.enabled FROM repository_addons ra JOIN addons a ON a.id = ra.addon_id WHERE ra.repository_id = ?", crate::core::db::params![repository.id as i64])
        .await.map_err(|_| "Could not inspect update ownership".to_owned())?;
    let roots: Vec<String> = rows
        .iter()
        .filter_map(|row| {
            let name = row.get_string("name").ok()?;
            overrides
                .get(&name.to_lowercase())
                .copied()
                .unwrap_or(row.get_bool("enabled").ok()?)
                .then(|| row.get_string("local_path").ok())
                .flatten()
        })
        .collect();
    tokio::task::spawn_blocking(move || {
        let mut claimed = Vec::new();
        let mut seen = HashSet::new();
        for root in roots {
            let root = PathBuf::from(root);
            let root = pinned_targets
                .as_ref()
                .map(|targets| targets.resolve(&root))
                .transpose()?
                .unwrap_or(root);
            std::fs::create_dir_all(&root).map_err(|_| "Could not prepare an addon write target. Restore access and retry.".to_owned())?;
            let canonical = root.canonicalize().map_err(|_| "Could not resolve an addon write target".to_owned())?;
            if !seen.insert(canonical.clone()) { continue; }
            let file = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
                .open(canonical.join(crate::core::utils::content_hash::ADDON_WRITE_LOCK_FILE)).map_err(|_| "Could not claim an addon write target. Restore access and retry.".to_owned())?;
            file.try_lock().map_err(|_| "Another Foxy operation owns shared addon files. Wait for it to finish and retry.".to_owned())?;
            claimed.push(file);
        }
        Ok(claimed)
    }).await.map_err(|_| "Addon ownership inspection stopped".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_install_has_no_link_enrollment() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("@mod_01")).unwrap();
        assert!(linked_targets(root.path()).unwrap().is_empty());
    }

    #[test]
    fn retained_ownership_uses_resolved_existing_roots() {
        let root = tempfile::tempdir().unwrap();
        let addon = root.path().join("@mod_01");
        std::fs::create_dir(&addon).unwrap();
        let roots = resolved_owned_targets(&[addon.clone(), root.path().join("absent")]).unwrap();
        assert_eq!(roots, vec![addon.canonicalize().unwrap()]);
        assert!(
            addon
                .canonicalize()
                .unwrap()
                .join("file.bin")
                .starts_with(&roots[0])
        );
    }

    #[cfg(unix)]
    #[test]
    fn retained_ownership_includes_files_linked_from_a_sibling_root() {
        let root = tempfile::tempdir().unwrap();
        let shared = tempfile::tempdir().unwrap();
        let file = shared.path().join("file.bin");
        std::fs::write(&file, b"shared").unwrap();
        std::os::unix::fs::symlink(&file, root.path().join("linked.bin")).unwrap();
        let roots = resolved_owned_targets(&[root.path().to_path_buf()]).unwrap();
        assert!(roots.contains(&file.canonicalize().unwrap()));
    }

    #[cfg(unix)]
    #[test]
    fn linked_targets_are_resolved_without_traversing_them() {
        let root = tempfile::tempdir().unwrap();
        let shared = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(shared.path(), root.path().join("@mod_01")).unwrap();
        let links = linked_targets(root.path()).unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].1, shared.path().canonicalize().unwrap());
    }
}
