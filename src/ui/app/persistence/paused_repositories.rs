use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::Path;

use crate::ui::app::Foxy;
use crate::ui::types::Repository;

fn set_paused_instance(instances: &mut HashSet<String>, key: String, paused: bool) {
    if paused {
        instances.insert(key);
    } else {
        instances.remove(&key);
    }
}

impl Foxy {
    pub(crate) fn clear_persisted_repository_pause_for_cli(
        repository_url: &str,
        local_path: &str,
    ) -> Result<(), String> {
        let path = Self::get_game_space_directory().join("paused_repositories.json");
        clear_paused_instance_file(&path, &Self::repo_instance_key(repository_url, local_path))
    }

    pub(crate) fn load_paused_repository_instances(&mut self) {
        let path = Self::get_game_space_directory().join("paused_repositories.json");
        match std::fs::read_to_string(&path) {
            Ok(json) => match serde_json::from_str::<HashSet<String>>(&json) {
                Ok(instances) => self.paused_repository_instances = instances,
                Err(err) => {
                    log::error!("Failed to parse paused_repositories.json: {}", err);
                    self.report_unreadable_config(&path);
                    self.paused_repository_instances.clear();
                }
            },
            Err(err) if err.kind() == ErrorKind::NotFound => {
                self.paused_repository_instances.clear();
            }
            Err(err) => {
                log::error!("Failed to read paused_repositories.json: {}", err);
                self.report_unreadable_config(&path);
                self.paused_repository_instances.clear();
            }
        }
    }

    pub(crate) fn is_repository_paused(&self, repository: &Repository) -> bool {
        self.paused_repository_instances
            .contains(&Self::repo_instance_key(
                &repository.address,
                &repository.path,
            ))
    }

    pub(crate) fn persist_repository_pause(
        &mut self,
        repository: &Repository,
        paused: bool,
    ) -> Result<(), String> {
        let previous = self.paused_repository_instances.clone();
        let key = Self::repo_instance_key(&repository.address, &repository.path);
        set_paused_instance(&mut self.paused_repository_instances, key, paused);

        let path = Self::get_game_space_directory().join("paused_repositories.json");
        if !self.persistence_path_is_confirmed_for_replacement(&path) {
            self.paused_repository_instances = previous;
            self.show_unreadable_config_replacement = true;
            return Err("Pause state is unreadable; confirm replacement before saving".into());
        }
        let result = (|| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|err| format!("Failed to create pause state directory: {err}"))?;
            }
            let mut instances: Vec<_> = self.paused_repository_instances.iter().cloned().collect();
            instances.sort_unstable();
            let json = serde_json::to_vec_pretty(&instances)
                .map_err(|err| format!("Failed to serialize paused repository state: {err}"))?;
            crate::core::utils::fs_safety::atomic_write(&path, &json)
                .map_err(|err| format!("Failed to write paused_repositories.json: {err}"))
        })();
        if let Err(err) = result {
            self.paused_repository_instances = previous;
            return Err(err);
        }
        Ok(())
    }

    pub(crate) fn resume_paused_repository(&mut self, repo_idx: usize) {
        let Some(repo) = self
            .repository_view_state
            .repositories
            .get(repo_idx)
            .cloned()
        else {
            return;
        };
        if !self.is_repository_paused(&repo) {
            return;
        }

        let key = Self::repo_instance_key(&repo.address, &repo.path);
        let was_idle = self.syncing_repository.is_none() && self.current_sync_mode.is_none();
        self.paused_repository_instances.remove(&key);
        self.start_core_sync(repo_idx, crate::core::api::SyncMode::Download);
        if !was_idle || self.syncing_repository != Some(repo_idx) {
            self.paused_repository_instances.insert(key);
            return;
        }
        if let Err(err) = self.persist_repository_pause(&repo, false) {
            self.paused_repository_instances.insert(key);
            log::error!(
                "Failed to clear paused state for repository {}: {}",
                repo.name,
                err
            );
            self.show_error_toast(
                self.t_fmt("Could not save resume state: {error}", &[("error", err)]),
            );
        }
    }
}

fn clear_paused_instance_file(path: &Path, key: &str) -> Result<(), String> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("Could not read persisted pause state".into()),
    };
    let mut instances = serde_json::from_str::<HashSet<String>>(&contents)
        .map_err(|_| "Persisted pause state is unreadable; refusing to replace it".to_string())?;
    if !instances.remove(key) {
        return Ok(());
    }
    let mut instances: Vec<_> = instances.into_iter().collect();
    instances.sort_unstable();
    let bytes = serde_json::to_vec_pretty(&instances)
        .map_err(|_| "Could not serialize persisted pause state".to_string())?;
    crate::core::utils::fs_safety::atomic_write(path, &bytes)
        .map_err(|_| "Could not write persisted pause state".to_string())
}

#[cfg(test)]
mod tests {
    use super::{clear_paused_instance_file, set_paused_instance};
    use std::collections::HashSet;

    #[test]
    fn pause_state_is_idempotent_and_instance_scoped() {
        let mut instances = HashSet::new();
        set_paused_instance(
            &mut instances,
            "https://repo.example.invalid:8080/\u{1f}R:/Mods/RepoAlpha".into(),
            true,
        );
        set_paused_instance(
            &mut instances,
            "https://repo.example.invalid:8080/\u{1f}R:/Mods/RepoBeta".into(),
            true,
        );

        set_paused_instance(
            &mut instances,
            "https://repo.example.invalid:8080/\u{1f}R:/Mods/RepoAlpha".into(),
            false,
        );

        assert_eq!(instances.len(), 1);
        assert!(instances.contains("https://repo.example.invalid:8080/\u{1f}R:/Mods/RepoBeta"));
    }

    #[test]
    fn resume_of_an_unpaused_instance_is_a_noop() {
        let mut instances = HashSet::new();
        set_paused_instance(&mut instances, "repo-instance".into(), false);
        assert!(instances.is_empty());
    }

    #[test]
    fn clearing_a_missing_pause_file_is_a_noop() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("paused_repositories.json");
        clear_paused_instance_file(&path, "repo-instance").unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn clearing_refuses_to_replace_invalid_pause_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("paused_repositories.json");
        std::fs::write(&path, b"not-json").unwrap();
        let error = clear_paused_instance_file(&path, "repo-instance").unwrap_err();
        assert!(error.contains("unreadable"));
        assert_eq!(std::fs::read(&path).unwrap(), b"not-json");
    }

    #[test]
    fn clearing_one_repository_preserves_sibling_pause_keys() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("paused_repositories.json");
        let mut instances = HashSet::new();
        instances.insert("repo-a".to_string());
        instances.insert("repo-b".to_string());
        instances.insert("repo-c".to_string());
        std::fs::write(&path, serde_json::to_vec(&instances).unwrap()).unwrap();

        clear_paused_instance_file(&path, "repo-b").unwrap();

        let remaining =
            serde_json::from_slice::<HashSet<String>>(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(remaining, HashSet::from(["repo-a".into(), "repo-c".into()]));
    }
}
