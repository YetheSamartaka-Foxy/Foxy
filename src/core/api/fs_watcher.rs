use super::logging::request_background_repaint;
use super::*;
use crate::core::db::params;
use crate::core::utils::format::sanitize_log_path_str;
use std::time::{SystemTime, UNIX_EPOCH};

fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

pub(super) fn normalize_path_for_match(path: &str) -> String {
    crate::core::utils::content_hash::normalize_path(path)
}

fn path_matches_mod(root: &str, candidate: &str) -> bool {
    if candidate == root {
        return true;
    }
    let prefix = format!("{}/", root);
    candidate.starts_with(&prefix)
}

#[derive(Clone, Debug)]
struct LinkedRepoPath {
    remote_url: String,
    local_path: String,
}

#[derive(Clone, Debug)]
struct ModPathEntry {
    local_path: String,
    linked_repos: Vec<LinkedRepoPath>,
}

async fn build_mod_repo_index(context: Arc<FoxyContext>) -> Vec<ModPathEntry> {
    let db = context.db();

    // Fetch only needed columns, run all three queries concurrently
    let (repos_result, repo_mods_result, mods_result) = tokio::join!(
        db.query_all(
            "SELECT id, remote_url, local_path FROM repositories",
            params![],
        ),
        db.query_all(
            "SELECT repository_id, addon_id FROM repository_addons",
            params![]
        ),
        db.query_all("SELECT id, local_path FROM addons", params![]),
    );

    let repos = match repos_result {
        Ok(rows) => rows,
        Err(err) => {
            warn!("Failed to load repositories for watcher index: {}", err);
            return Vec::new();
        }
    };
    let repo_by_id: HashMap<i64, LinkedRepoPath> = repos
        .iter()
        .filter_map(|row| {
            Some((
                row.get_i64("id").ok()?,
                LinkedRepoPath {
                    remote_url: row.get_string("remote_url").ok()?,
                    local_path: normalize_path_for_match(&row.get_string("local_path").ok()?),
                },
            ))
        })
        .collect();

    let repo_mods = match repo_mods_result {
        Ok(rows) => rows,
        Err(err) => {
            warn!("Failed to load repo/mod links for watcher index: {}", err);
            return Vec::new();
        }
    };

    let mods = match mods_result {
        Ok(rows) => rows,
        Err(err) => {
            warn!("Failed to load mods for watcher index: {}", err);
            return Vec::new();
        }
    };

    let mut linked_repos_by_mod: HashMap<i64, Vec<LinkedRepoPath>> = HashMap::new();
    for link in &repo_mods {
        let (Ok(repository_id), Ok(addon_id)) =
            (link.get_i64("repository_id"), link.get_i64("addon_id"))
        else {
            continue;
        };
        if let Some(repo) = repo_by_id.get(&repository_id) {
            linked_repos_by_mod
                .entry(addon_id)
                .or_default()
                .push(repo.clone());
        }
    }

    let mut entries = Vec::new();
    for row in &mods {
        let (Ok(id), Ok(local_path)) = (row.get_i64("id"), row.get_string("local_path")) else {
            continue;
        };
        if local_path.trim().is_empty() {
            continue;
        }
        let Some(linked_repos) = linked_repos_by_mod.get(&id) else {
            continue;
        };
        if linked_repos.is_empty() {
            continue;
        }
        entries.push(ModPathEntry {
            local_path: normalize_path_for_match(&local_path),
            linked_repos: linked_repos.clone(),
        });
    }

    entries
}

fn repo_urls_for_changed_paths(
    mod_index: &[ModPathEntry],
    changed_paths: &HashSet<String>,
) -> HashSet<String> {
    let mut repo_urls = HashSet::new();
    for changed_path in changed_paths {
        for entry in mod_index {
            if !path_matches_mod(&entry.local_path, changed_path) {
                continue;
            }

            let mut matched_repo_root = false;
            for repo in &entry.linked_repos {
                if path_matches_mod(&repo.local_path, changed_path) {
                    repo_urls.insert(repo.remote_url.clone());
                    matched_repo_root = true;
                }
            }

            if !matched_repo_root {
                for repo in &entry.linked_repos {
                    repo_urls.insert(repo.remote_url.clone());
                }
            }
        }
    }
    repo_urls
}

pub fn spawn_repo_fs_watcher(
    watch_paths: Vec<String>,
    suppress_until_ms: Arc<AtomicU64>,
    result_tx: StdSender<FsChangeEvent>,
    repaint_ctx: Option<egui::Context>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        info!(
            "Filesystem watcher worker spawned for {} configured paths",
            watch_paths.len()
        );
        ensure_logger();
        // DATABASE_URL is set once at startup in main.rs to avoid unsafe env::set_var
        // race conditions in multi-threaded context.

        let rt = match Builder::new_multi_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(err) => {
                error!("Failed to build runtime for fs watcher: {}", err);
                return;
            }
        };

        let context = rt.block_on(create_context());
        let mod_index = rt.block_on(build_mod_repo_index(context.clone()));
        if mod_index.is_empty() {
            warn!("Filesystem watcher disabled: no repository/mod path index available");
            return;
        }

        let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();
        let mut watcher: RecommendedWatcher = match notify::recommended_watcher(move |res| {
            if tx.send(res).is_err() {
                warn!("Filesystem watcher event channel closed");
            }
        }) {
            Ok(w) => w,
            Err(err) => {
                warn!("Failed to initialize filesystem watcher: {}", err);
                return;
            }
        };

        let mut watching_any = false;
        for path in watch_paths {
            if path.trim().is_empty() {
                continue;
            }
            let path_ref = Path::new(&path);
            if !path_ref.exists() {
                warn!(
                    "Skipping watcher path that does not exist: {}",
                    sanitize_log_path_str(&path)
                );
                continue;
            }
            match watcher.watch(path_ref, RecursiveMode::Recursive) {
                Ok(()) => watching_any = true,
                Err(err) => warn!(
                    "Failed to register watcher path {}: {}",
                    sanitize_log_path_str(&path),
                    err
                ),
            }
        }

        if !watching_any {
            warn!("Filesystem watcher disabled: no valid watch paths were registered");
            return;
        }
        info!("Filesystem watcher active");

        let debounce = Duration::from_millis(350);
        let mut pending_paths: HashSet<String> = HashSet::new();
        let mut last_event_at = Instant::now();

        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Ok(event)) => {
                    for path in event.paths {
                        let normalized = normalize_path_for_match(path.to_string_lossy().as_ref());
                        if !normalized.is_empty() {
                            pending_paths.insert(normalized);
                        }
                    }
                    last_event_at = Instant::now();
                }
                Ok(Err(err)) => {
                    warn!("Filesystem watcher error: {}", err);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if pending_paths.is_empty() || last_event_at.elapsed() < debounce {
                        continue;
                    }

                    if unix_time_millis() <= suppress_until_ms.load(Ordering::Relaxed) {
                        pending_paths.clear();
                        continue;
                    }

                    let repo_urls = repo_urls_for_changed_paths(&mod_index, &pending_paths);

                    pending_paths.clear();

                    if !repo_urls.is_empty() {
                        info!(
                            "Filesystem watcher detected local changes for {} repositories",
                            repo_urls.len()
                        );
                        if result_tx
                            .send(FsChangeEvent {
                                repo_urls: repo_urls.into_iter().collect(),
                            })
                            .is_ok()
                        {
                            request_background_repaint(repaint_ctx.as_ref());
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        warn!("Filesystem watcher worker stopped");
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_path_for_match_uses_content_hash_normalize() {
        let result = normalize_path_for_match("C:\\Users\\Test\\Mods");
        assert!(!result.contains('\\'));
        assert!(!result.ends_with('/'));
    }

    #[test]
    fn path_matches_mod_exact_match() {
        assert!(path_matches_mod("mods/@ace", "mods/@ace"));
    }

    #[test]
    fn path_matches_mod_child_path() {
        assert!(path_matches_mod("mods/@ace", "mods/@ace/addons/file.pbo"));
    }

    #[test]
    fn path_matches_mod_no_match() {
        assert!(!path_matches_mod("mods/@ace", "mods/@cba/addons"));
    }

    #[test]
    fn path_matches_mod_prefix_but_not_dir() {
        // "mods/@ace_extra" starts with "mods/@ace" but is NOT a child directory
        assert!(!path_matches_mod("mods/@ace", "mods/@ace_extra/file.pbo"));
    }

    #[test]
    fn path_matches_mod_empty_root() {
        assert!(!path_matches_mod("", "mods/@ace"));
    }

    #[test]
    fn path_matches_mod_empty_candidate() {
        assert!(!path_matches_mod("mods/@ace", ""));
    }

    #[test]
    fn path_matches_mod_deeply_nested_child() {
        assert!(path_matches_mod(
            "mods/@ace",
            "mods/@ace/addons/deep/nested/file.pbo"
        ));
    }

    #[test]
    fn normalize_path_for_match_backslash_normalization() {
        let result = normalize_path_for_match("C:\\Games\\Mods\\@ace");
        assert!(!result.contains('\\'));
    }

    #[test]
    fn normalize_path_for_match_trailing_slash_stripped() {
        let result = normalize_path_for_match("mods/@ace/");
        assert!(!result.ends_with('/'));
    }

    #[test]
    fn normalize_path_for_match_empty_string() {
        let result = normalize_path_for_match("");
        assert!(result.is_empty());
    }

    fn linked_repo(remote_url: &str, local_path: &str) -> LinkedRepoPath {
        LinkedRepoPath {
            remote_url: remote_url.to_string(),
            local_path: normalize_path_for_match(local_path),
        }
    }

    #[test]
    fn repo_urls_for_changed_paths_prefers_matching_repository_root() {
        let mod_index = vec![ModPathEntry {
            local_path: normalize_path_for_match("S:/Swifty/TFR_Repository/@diwako_anomalies"),
            linked_repos: vec![
                linked_repo(
                    "http://a3.tfrod.cz:8080/mody/TFR_40K/",
                    "S:/Swifty/TFR_Repository",
                ),
                linked_repo(
                    "http://example.invalid/other_repo/",
                    "S:/Swifty/Other_Repository",
                ),
            ],
        }];
        let changed_paths = HashSet::from([normalize_path_for_match(
            "S:/Swifty/TFR_Repository/@diwako_anomalies/addons/file.pbo",
        )]);

        let repo_urls = repo_urls_for_changed_paths(&mod_index, &changed_paths);

        assert_eq!(repo_urls.len(), 1);
        assert!(repo_urls.contains("http://a3.tfrod.cz:8080/mody/TFR_40K/"));
        assert!(!repo_urls.contains("http://example.invalid/other_repo/"));
    }

    #[test]
    fn repo_urls_for_changed_paths_falls_back_when_no_repository_root_matches() {
        let mod_index = vec![ModPathEntry {
            local_path: normalize_path_for_match("D:/SharedMods/@ace"),
            linked_repos: vec![
                linked_repo("http://example.invalid/repo_a/", "S:/Swifty/RepoA"),
                linked_repo("http://example.invalid/repo_b/", "S:/Swifty/RepoB"),
            ],
        }];
        let changed_paths = HashSet::from([normalize_path_for_match(
            "D:/SharedMods/@ace/addons/file.pbo",
        )]);

        let repo_urls = repo_urls_for_changed_paths(&mod_index, &changed_paths);

        assert_eq!(repo_urls.len(), 2);
        assert!(repo_urls.contains("http://example.invalid/repo_a/"));
        assert!(repo_urls.contains("http://example.invalid/repo_b/"));
    }
}
