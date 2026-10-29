use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::{
    GameLaunchCtx, GameModule, LaunchCommand, LaunchPlan, addon_workshop::AddonWorkshop,
    extra_files, reforger, workshop,
};
use crate::core::utils::fs_safety::atomic_write;
use crate::ui::types::{Repository, RepositoryServer, SettingsViewState};

static NEXT_PREPARATION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LaunchApproval {
    pub incomplete_extra_files: bool,
    pub live_workshop_fallback: bool,
}

#[derive(Debug)]
pub(crate) enum LaunchFailure {
    ExtraFiles(Vec<String>),
    FrozenCopies(Vec<String>),
    Recovery(String),
    Failed(String),
}

#[derive(Debug)]
pub(crate) struct LaunchHandoff {
    pub pid: u32,
    pub extra_files: extra_files::ActivationSummary,
    pub command: LaunchCommand,
    pub steam_status: Option<crate::core::steam::SteamEnsureResult>,
}

#[derive(Serialize, Deserialize)]
struct SavedTarget {
    path: PathBuf,
    existed: bool,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    targets: Vec<SavedTarget>,
    #[serde(default)]
    created_directories: Vec<PathBuf>,
}

pub(crate) struct Preparation {
    root: PathBuf,
    journal: Journal,
    settled: bool,
}

impl Preparation {
    pub(crate) fn begin(space_dir: &Path, paths: &[PathBuf]) -> Result<Self, String> {
        let root = space_dir.join("launch_preparation").join(format!(
            "{}-{}",
            std::process::id(),
            NEXT_PREPARATION.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let mut journal = Journal {
            targets: Vec::new(),
            created_directories: Vec::new(),
        };
        for path in paths {
            let path = if path.exists() {
                path.canonicalize().map_err(|error| error.to_string())?
            } else {
                path.clone()
            };
            if journal.targets.iter().any(|target| target.path == path) {
                continue;
            }
            let mut parent = path.parent();
            while let Some(directory) = parent {
                if directory.exists() {
                    break;
                }
                if !journal
                    .created_directories
                    .contains(&directory.to_path_buf())
                {
                    journal.created_directories.push(directory.to_path_buf());
                }
                parent = directory.parent();
            }
            let existed = match fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => true,
                Ok(_) => return Err("A launch write target is not a regular file".to_string()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => return Err(error.to_string()),
            };
            if existed {
                fs::copy(&path, root.join(journal.targets.len().to_string()))
                    .map_err(|error| error.to_string())?;
            }
            journal.targets.push(SavedTarget { path, existed });
        }
        atomic_write(
            &root.join("journal.json"),
            &serde_json::to_vec(&journal).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            root,
            journal,
            settled: false,
        })
    }

    pub(crate) fn restore(&mut self) -> Result<(), String> {
        atomic_write(&self.root.join("restoring"), b"1").map_err(|error| error.to_string())?;
        for (index, target) in self.journal.targets.iter().enumerate().rev() {
            if target.existed {
                fs::copy(self.root.join(index.to_string()), &target.path).map_err(|error| {
                    format!("Launch preparation could not be restored: {error}")
                })?;
            } else if let Err(error) = fs::remove_file(&target.path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                return Err(format!("Launch preparation could not be restored: {error}"));
            }
        }
        self.journal
            .created_directories
            .sort_by_key(|directory| std::cmp::Reverse(directory.components().count()));
        for directory in &self.journal.created_directories {
            if let Err(error) = fs::remove_dir(directory)
                && error.kind() != std::io::ErrorKind::NotFound
                && error.kind() != std::io::ErrorKind::DirectoryNotEmpty
            {
                return Err(error.to_string());
            }
        }
        self.settled = true;
        fs::remove_dir_all(&self.root).map_err(|error| error.to_string())
    }

    pub(crate) fn commit(&mut self) -> Result<(), String> {
        self.settled = true;
        atomic_write(&self.root.join("handed_off"), b"1").map_err(|error| error.to_string())?;
        fs::remove_dir_all(&self.root).map_err(|error| error.to_string())
    }

    fn remember_directories(&mut self, directories: &[PathBuf]) -> Result<(), String> {
        for directory in directories {
            let mut current = Some(directory.as_path());
            while let Some(path) = current {
                if path.exists() {
                    break;
                }
                if !self
                    .journal
                    .created_directories
                    .contains(&path.to_path_buf())
                {
                    self.journal.created_directories.push(path.to_path_buf());
                }
                current = path.parent();
            }
        }
        atomic_write(
            &self.root.join("journal.json"),
            &serde_json::to_vec(&self.journal).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())
    }
}

impl Drop for Preparation {
    fn drop(&mut self) {
        if !self.settled
            && let Err(error) = self.restore()
        {
            log::error!("Launch preparation recovery remains pending: {error}");
        }
    }
}

pub(crate) fn recover(space_dir: &Path) -> Result<(), String> {
    let root = space_dir.join("launch_preparation");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    for entry in entries {
        let root = entry.map_err(|error| error.to_string())?.path();
        if !root.is_dir() {
            continue;
        }
        if root.join("handed_off").exists() || !root.join("journal.json").exists() {
            fs::remove_dir_all(root).map_err(|error| error.to_string())?;
            continue;
        }
        if root.join("dispatching").exists() && !root.join("restoring").exists() {
            let retained = space_dir.join("launch_preparation_retained");
            fs::create_dir_all(&retained).map_err(|error| error.to_string())?;
            let name = root
                .file_name()
                .ok_or_else(|| "Invalid launch recovery directory".to_string())?;
            fs::rename(&root, retained.join(name)).map_err(|error| error.to_string())?;
            log::warn!(
                "An interrupted launch has an unknown handoff outcome; prepared files and recovery copies were retained"
            );
            continue;
        }
        let journal = serde_json::from_slice(
            &fs::read(root.join("journal.json")).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("Launch preparation recovery journal is unreadable: {error}"))?;
        Preparation {
            root,
            journal,
            settled: false,
        }
        .restore()?;
    }
    Ok(())
}

fn extra_targets(
    space_dir: &Path,
    game_dir: &str,
) -> (
    Vec<PathBuf>,
    Vec<PathBuf>,
    Vec<extra_files::ExtraFileEntry>,
    Vec<String>,
) {
    fn collect(
        source: &Path,
        destination: &Path,
        targets: &mut Vec<PathBuf>,
        directories: &mut Vec<PathBuf>,
    ) -> Result<(), String> {
        if fs::symlink_metadata(source)
            .map_err(|error| error.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("A managed extra-file source contains a link".to_string());
        }
        if source.is_file() {
            targets.push(destination.to_path_buf());
        } else if source.is_dir() {
            directories.push(destination.to_path_buf());
            for entry in fs::read_dir(source).map_err(|error| error.to_string())? {
                let entry = entry.map_err(|error| error.to_string())?;
                if entry
                    .file_type()
                    .map_err(|error| error.to_string())?
                    .is_symlink()
                {
                    return Err("A managed extra-file source contains a link".to_string());
                }
                collect(
                    &entry.path(),
                    &destination.join(entry.file_name()),
                    targets,
                    directories,
                )?;
            }
        }
        Ok(())
    }
    let mut targets = Vec::new();
    let mut entries = Vec::new();
    let mut directories = Vec::new();
    let mut failures = Vec::new();
    let store = match extra_files::load_store(space_dir) {
        Ok(store) => store,
        Err(error) => return (targets, directories, entries, vec![error]),
    };
    for entry in store.entries.into_iter().filter(|entry| entry.enabled) {
        let mut entry_targets = Vec::new();
        let mut entry_directories = Vec::new();
        let result = (|| {
            extra_files::validate_entry(&entry)?;
            let destination = extra_files::resolve_destination(&entry.destination, game_dir)?;
            if destination.starts_with(space_dir)
                || destination.starts_with(crate::core::utils::app_paths::foxy_data_dir())
            {
                return Err("Destination points into Foxy's data directory".to_string());
            }
            collect(
                &extra_files::payload_dir(space_dir, &entry.id).join(&entry.source_name),
                &destination,
                &mut entry_targets,
                &mut entry_directories,
            )
        })();
        match result {
            Ok(()) => {
                targets.extend(entry_targets);
                directories.extend(entry_directories);
                entries.push(entry);
            }
            Err(error) => failures.push(format!("{}: {error}", entry.name)),
        }
    }
    (targets, directories, entries, failures)
}

pub(crate) fn missing_pins(
    space_dir: &Path,
    module: &dyn GameModule,
    settings: &SettingsViewState,
    plan: &LaunchPlan,
) -> Result<Vec<String>, String> {
    let mut missing = Vec::new();
    match module.addon_workshop(settings) {
        Some(AddonWorkshop::Steam(app_id)) => {
            for item in workshop::load_store(space_dir)?
                .entries
                .into_iter()
                .filter(|item| item.app_id == app_id)
            {
                if !item.frozen
                    || item
                        .frozen_path
                        .as_deref()
                        .is_some_and(|path| Path::new(path).is_dir())
                {
                    continue;
                }
                let live = workshop::resolve_installed_path(&item, &settings.steam_directory);
                let used = plan.mods.iter().any(|entry| {
                    entry.id == item.item_id
                        || live.as_ref().is_some_and(|live| {
                            entry
                                .path
                                .as_deref()
                                .is_some_and(|path| Path::new(path).starts_with(live))
                        })
                });
                if used {
                    missing.push(item.title.unwrap_or(item.item_id));
                }
            }
        }
        Some(AddonWorkshop::Reforger) => {
            for item in reforger::load_store(space_dir)?.entries {
                if !item.frozen
                    || item
                        .frozen_path
                        .as_deref()
                        .is_some_and(|path| Path::new(path).is_dir())
                {
                    continue;
                }
                if plan
                    .mods
                    .iter()
                    .any(|entry| entry.id.eq_ignore_ascii_case(&item.guid))
                {
                    missing.push(item.name.unwrap_or(item.guid));
                }
            }
        }
        None => {}
    }
    Ok(missing)
}

pub(crate) fn dispatch_plan(
    module: &dyn GameModule,
    settings: &SettingsViewState,
    plan: &LaunchPlan,
    approval: LaunchApproval,
    dispatch: impl FnOnce(&LaunchCommand) -> Result<u32, String>,
) -> Result<LaunchHandoff, LaunchFailure> {
    let space_dir = super::spaces::active_game_space_dir();
    dispatch_plan_in_space(&space_dir, module, settings, plan, approval, dispatch)
}

fn dispatch_plan_in_space(
    space_dir: &Path,
    module: &dyn GameModule,
    settings: &SettingsViewState,
    plan: &LaunchPlan,
    approval: LaunchApproval,
    dispatch: impl FnOnce(&LaunchCommand) -> Result<u32, String>,
) -> Result<LaunchHandoff, LaunchFailure> {
    let ctx = GameLaunchCtx {
        install_dir: module.install_dir_from_settings(settings),
        steam_directory: &settings.steam_directory,
        settings: Some(settings),
    };
    module
        .build_launch_preview(plan, &ctx)
        .map_err(|error| LaunchFailure::Failed(format!("{error:?}")))?;
    let missing = missing_pins(space_dir, module, settings, plan).map_err(LaunchFailure::Failed)?;
    if !missing.is_empty() && !approval.live_workshop_fallback {
        return Err(LaunchFailure::FrozenCopies(missing));
    }
    let mut targets = module.launch_write_targets(plan, &ctx);
    let (extra, directories, entries, failures) = extra_targets(space_dir, ctx.install_dir);
    if !failures.is_empty() && !approval.incomplete_extra_files {
        return Err(LaunchFailure::ExtraFiles(failures));
    }
    targets.extend(extra);
    let mut preparation = Preparation::begin(space_dir, &targets).map_err(LaunchFailure::Failed)?;
    preparation
        .remember_directories(&directories)
        .map_err(LaunchFailure::Failed)?;
    let mut activation = extra_files::ActivationSummary {
        failed: failures
            .into_iter()
            .map(|error| ("Extra file".to_string(), error))
            .collect(),
        ..Default::default()
    };
    let outcome = (|| {
        for entry in entries {
            match extra_files::activate_entry(space_dir, &entry, ctx.install_dir) {
                Ok(destination) => activation.activated.push((entry.name, destination)),
                Err(error) => activation.failed.push((entry.name, error)),
            }
        }
        if !activation.failed.is_empty() && !approval.incomplete_extra_files {
            return Err(LaunchFailure::ExtraFiles(
                activation
                    .failed
                    .iter()
                    .map(|(name, error)| format!("{name}: {error}"))
                    .collect(),
            ));
        }
        let command = module
            .build_launch(plan, &ctx)
            .map_err(|error| LaunchFailure::Failed(format!("{error:?}")))?;
        atomic_write(&preparation.root.join("dispatching"), b"1")
            .map_err(|error| LaunchFailure::Failed(error.to_string()))?;
        dispatch(&command)
            .map(|pid| (pid, command))
            .map_err(LaunchFailure::Failed)
    })();
    match outcome {
        Ok((pid, command)) => {
            if let Err(error) = preparation.commit() {
                log::warn!(
                    "Launch handoff succeeded; preparation cleanup remains pending: {error}"
                );
            }
            Ok(LaunchHandoff {
                pid,
                extra_files: activation,
                command,
                steam_status: None,
            })
        }
        Err(error) => {
            preparation.restore().map_err(LaunchFailure::Recovery)?;
            Err(error)
        }
    }
}

pub(crate) fn dispatch_repository(
    module: &dyn GameModule,
    settings: &SettingsViewState,
    repo: &Repository,
    server: Option<&RepositoryServer>,
    approval: LaunchApproval,
    mission: Option<&Path>,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<LaunchHandoff, LaunchFailure> {
    if cancelled.load(Ordering::Acquire) {
        return Err(LaunchFailure::Failed("Launch cancelled".to_string()));
    }
    let plan = module
        .build_repository_launch_plan(settings, repo, server)
        .map_err(|error| LaunchFailure::Failed(format!("{error:?}")))?;
    let steam_required =
        plan.game_build.is_none() && module.steam_app_id_from_settings(settings).is_some();
    let steam_status = if steam_required {
        Some(
            crate::core::steam::ensure_steam_running(&settings.steam_directory)
                .map_err(LaunchFailure::Failed)?,
        )
    } else {
        None
    };
    let mut handoff = dispatch_plan(module, settings, &plan, approval, |command| {
        if cancelled.load(Ordering::Acquire) {
            return Err("Launch cancelled".to_string());
        }
        if steam_required && !crate::core::steam::is_steam_running(&settings.steam_directory) {
            return Err("Steam is not running".to_string());
        }
        let mut args = command
            .args
            .iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>();
        if let Some(mission) = mission {
            args.push(crate::core::steam::arma3_mission_argument(
                &command.clone().into_process_command(),
                mission,
            ));
        }
        crate::core::utils::deelevate::spawn_unelevated(
            command.program.as_os_str(),
            &args,
            command.cwd.as_deref(),
        )
        .map_err(|error| error.to_string())
    })?;
    handoff.steam_status = steam_status;
    Ok(handoff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_failure_restores_manifest_and_extra_files_before_returning() {
        let root = tempfile::tempdir().unwrap();
        let space = root.path().join("space");
        let game = root.path().join("game");
        fs::create_dir_all(&space).unwrap();
        fs::create_dir_all(&game).unwrap();
        fs::write(
            game.join(super::super::twwh3::TWWH3_EXECUTABLE),
            b"test launcher",
        )
        .unwrap();
        let manifest = game.join(super::super::twwh3::TWWH3_USED_MODS_FILE);
        fs::write(&manifest, b"previous manifest").unwrap();
        let source = root.path().join("extra.cfg");
        let target = game.join("extra.cfg");
        fs::write(&source, b"prepared extra").unwrap();
        fs::write(&target, b"previous extra").unwrap();
        extra_files::add_entry(&space, "Config", &source, &target.to_string_lossy(), true).unwrap();
        let settings = SettingsViewState {
            twwh3_directory: game.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let plan = LaunchPlan {
            launch_args: Vec::new(),
            mods: Vec::new(),
            server: None,
            game_build: None,
        };
        let module = super::super::twwh3::TotalWarWarhammer3Module;
        let failure = dispatch_plan_in_space(
            &space,
            &module,
            &settings,
            &plan,
            LaunchApproval::default(),
            |_| {
                assert_eq!(fs::read(&target).unwrap(), b"prepared extra");
                assert_ne!(fs::read(&manifest).unwrap(), b"previous manifest");
                Err("dispatch refused".to_string())
            },
        )
        .unwrap_err();
        assert!(matches!(failure, LaunchFailure::Failed(error) if error == "dispatch refused"));
        assert_eq!(fs::read(&target).unwrap(), b"previous extra");
        assert_eq!(fs::read(&manifest).unwrap(), b"previous manifest");
        assert_eq!(
            fs::read_dir(space.join("launch_preparation"))
                .unwrap()
                .count(),
            0
        );

        let success = dispatch_plan_in_space(
            &space,
            &module,
            &settings,
            &plan,
            LaunchApproval::default(),
            |_| Ok(123),
        )
        .unwrap();
        assert_eq!(success.pid, 123);
        assert_eq!(success.extra_files.activated.len(), 1);
        assert_eq!(fs::read(target).unwrap(), b"prepared extra");
        assert_ne!(fs::read(manifest).unwrap(), b"previous manifest");
    }

    #[test]
    fn missing_extra_payload_requires_consent_and_does_not_write_manifest() {
        let root = tempfile::tempdir().unwrap();
        let space = root.path().join("space");
        let game = root.path().join("game");
        fs::create_dir_all(&space).unwrap();
        fs::create_dir_all(&game).unwrap();
        fs::write(
            game.join(super::super::twwh3::TWWH3_EXECUTABLE),
            b"test launcher",
        )
        .unwrap();
        let manifest = game.join(super::super::twwh3::TWWH3_USED_MODS_FILE);
        fs::write(&manifest, b"previous manifest").unwrap();
        let source = root.path().join("extra.cfg");
        fs::write(&source, b"payload").unwrap();
        let entry = extra_files::add_entry(
            &space,
            "Config",
            &source,
            &game.join("extra.cfg").to_string_lossy(),
            true,
        )
        .unwrap();
        fs::remove_dir_all(extra_files::payload_dir(&space, &entry.id)).unwrap();
        let settings = SettingsViewState {
            twwh3_directory: game.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let plan = LaunchPlan {
            launch_args: Vec::new(),
            mods: Vec::new(),
            server: None,
            game_build: None,
        };
        let module = super::super::twwh3::TotalWarWarhammer3Module;
        let failure = dispatch_plan_in_space(
            &space,
            &module,
            &settings,
            &plan,
            LaunchApproval::default(),
            |_| panic!("unapproved launch dispatched"),
        );
        assert!(matches!(failure, Err(LaunchFailure::ExtraFiles(_))));
        assert_eq!(fs::read(&manifest).unwrap(), b"previous manifest");
        let success = dispatch_plan_in_space(
            &space,
            &module,
            &settings,
            &plan,
            LaunchApproval {
                incomplete_extra_files: true,
                ..Default::default()
            },
            |_| Ok(123),
        )
        .unwrap();
        assert_eq!(success.extra_files.failed.len(), 1);
        assert_eq!(success.pid, 123);
    }

    #[test]
    fn failed_preparation_restores_original_and_removes_created_file() {
        let space = tempfile::tempdir().unwrap();
        let file = space.path().join("config.cfg");
        let new = space.path().join("new.cfg");
        fs::write(&file, b"original").unwrap();
        let mut transaction =
            Preparation::begin(space.path(), &[file.clone(), new.clone()]).unwrap();
        fs::write(&file, b"prepared").unwrap();
        fs::write(&new, b"created").unwrap();
        transaction.restore().unwrap();
        assert_eq!(fs::read(file).unwrap(), b"original");
        assert!(!new.exists());
    }

    #[test]
    fn restart_recovers_pre_handoff_files_but_retains_committed_files() {
        let space = tempfile::tempdir().unwrap();
        let file = space.path().join("config.cfg");
        fs::write(&file, b"original").unwrap();
        let transaction = Preparation::begin(space.path(), std::slice::from_ref(&file)).unwrap();
        fs::write(&file, b"prepared").unwrap();
        std::mem::forget(transaction);
        recover(space.path()).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"original");
        let mut transaction =
            Preparation::begin(space.path(), std::slice::from_ref(&file)).unwrap();
        fs::write(&file, b"committed").unwrap();
        transaction.commit().unwrap();
        recover(space.path()).unwrap();
        assert_eq!(fs::read(file).unwrap(), b"committed");
    }

    #[test]
    fn failed_dispatch_recovery_keeps_snapshot_until_restore_can_finish() {
        let space = tempfile::tempdir().unwrap();
        let file = space.path().join("config.cfg");
        fs::write(&file, b"original").unwrap();
        let mut transaction =
            Preparation::begin(space.path(), std::slice::from_ref(&file)).unwrap();
        fs::write(transaction.root.join("dispatching"), b"1").unwrap();
        fs::remove_file(&file).unwrap();
        fs::create_dir(&file).unwrap();
        assert!(transaction.restore().is_err());
        let snapshot = transaction.root.clone();
        std::mem::forget(transaction);
        fs::remove_dir(&file).unwrap();
        recover(space.path()).unwrap();
        assert_eq!(fs::read(file).unwrap(), b"original");
        assert!(!snapshot.exists());
    }

    #[test]
    fn rollback_removes_new_empty_preparation_directories() {
        let space = tempfile::tempdir().unwrap();
        let destination = space.path().join("game").join("empty");
        let mut transaction = Preparation::begin(space.path(), &[]).unwrap();
        transaction
            .remember_directories(std::slice::from_ref(&destination))
            .unwrap();
        fs::create_dir_all(&destination).unwrap();
        transaction.restore().unwrap();
        assert!(!space.path().join("game").exists());
    }

    #[test]
    fn missing_pins_only_reports_selected_steam_items_for_the_games_app_id() {
        let space = tempfile::tempdir().unwrap();
        let live = space.path().join("steam-live");
        fs::create_dir_all(&live).unwrap();
        let mut store = workshop::WorkshopFile::default();
        for (app_id, item_id, title) in [
            (107410, "111", "Selected missing pin"),
            (107410, "222", "Unselected missing pin"),
            (999999, "111", "Other game pin"),
        ] {
            store.entries.push(workshop::SteamWorkshopItem {
                source: workshop::STEAM_SOURCE.to_string(),
                app_id,
                item_id: item_id.to_string(),
                title: Some(title.to_string()),
                url: String::new(),
                enabled: true,
                frozen: true,
                load_order: None,
                version: None,
                installed_path: Some(live.to_string_lossy().into_owned()),
                frozen_path: None,
                size_bytes: None,
                time_updated: None,
                added_at: 0,
                updated_at: 0,
            });
        }
        workshop::save_store(space.path(), &store).unwrap();
        let settings = SettingsViewState::default();
        let plan = LaunchPlan {
            launch_args: Vec::new(),
            mods: vec![super::super::launch::ResolvedMod {
                id: "111".to_string(),
                path: None,
            }],
            server: None,
            game_build: None,
        };

        let missing = missing_pins(
            space.path(),
            &super::super::arma3::Arma3Module,
            &settings,
            &plan,
        )
        .unwrap();

        assert_eq!(missing, vec!["Selected missing pin"]);
    }

    #[test]
    fn missing_pins_checks_selected_reforger_frozen_guids_without_reading_steam_store() {
        let space = tempfile::tempdir().unwrap();
        let live = space.path().join("reforger/addons/live/596ABCDEF0123456");
        fs::create_dir_all(&live).unwrap();
        fs::write(space.path().join(workshop::WORKSHOP_FILE), "{broken").unwrap();
        let store = reforger::ReforgerAddonsFile {
            schema_version: 1,
            entries: vec![
                reforger::ReforgerAddonEntry {
                    source: reforger::REFORGER_SOURCE.to_string(),
                    guid: "596ABCDEF0123456".to_string(),
                    name: Some("Selected Reforger pin".to_string()),
                    enabled: true,
                    frozen: true,
                    version: None,
                    installed_path: None,
                    managed_path: Some(live.to_string_lossy().into_owned()),
                    frozen_path: Some(
                        space
                            .path()
                            .join("reforger/addons/frozen/596ABCDEF0123456")
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    size_bytes: None,
                    added_at: 0,
                    updated_at: 0,
                },
                reforger::ReforgerAddonEntry {
                    source: reforger::REFORGER_SOURCE.to_string(),
                    guid: "5A658E3BEE1D4AE2".to_string(),
                    name: Some("Unselected Reforger pin".to_string()),
                    enabled: true,
                    frozen: true,
                    version: None,
                    installed_path: None,
                    managed_path: None,
                    frozen_path: None,
                    size_bytes: None,
                    added_at: 0,
                    updated_at: 0,
                },
            ],
        };
        reforger::save_store(space.path(), &store).unwrap();
        let settings = SettingsViewState::default();
        let plan = LaunchPlan {
            launch_args: Vec::new(),
            mods: vec![super::super::launch::ResolvedMod {
                id: "596abcdef0123456".to_string(),
                path: Some(live.to_string_lossy().into_owned()),
            }],
            server: None,
            game_build: None,
        };

        let missing = missing_pins(
            space.path(),
            &super::super::reforger::ReforgerModule,
            &settings,
            &plan,
        )
        .unwrap();

        assert_eq!(missing, vec!["Selected Reforger pin"]);
    }
}
