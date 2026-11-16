use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::thread::JoinHandle;

use crate::core::profile_backups::{self, BackupEntry};

#[derive(Default)]
pub(crate) struct ProfileBackupState {
    pub entries: Vec<BackupEntry>,
    pub busy: bool,
    pub status: Option<String>,
    pub confirm: Option<ProfileBackupConfirm>,
    pub confirm_initial_focus: bool,
    rx: Option<Receiver<ProfileBackupResult>>,
    worker: Option<JoinHandle<()>>,
    pub(crate) started_spaces: HashSet<PathBuf>,
    last_automatic_check: Option<std::time::Instant>,
    pending_start_space: Option<PathBuf>,
    recovery_checked: bool,
    pub list_loaded_for: Option<PathBuf>,
}

#[derive(Clone)]
pub(crate) enum ProfileBackupConfirm {
    Restore(String),
    DeleteAll,
}

enum ProfileBackupResult {
    List(PathBuf, Result<Vec<BackupEntry>, String>),
    Automatic(PathBuf, bool, Result<MutationResult, String>),
    Mutation(PathBuf, bool, Result<MutationResult, String>),
}

pub(crate) enum MutationResult {
    Entries(Vec<BackupEntry>),
    Restored(Vec<crate::core::arma3_profiles::Arma3Profile>),
    Deferred,
    Done,
}

impl ProfileBackupState {
    fn automatic_requested(
        &self,
        root: &std::path::Path,
        policy: &profile_backups::BackupPolicy,
    ) -> bool {
        policy.enabled && (policy.interval_days != 0 || !self.started_spaces.contains(root))
    }

    fn start(
        &mut self,
        work: impl FnOnce() -> ProfileBackupResult + Send + 'static,
        ctx: &eframe::egui::Context,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        let (tx, rx) = channel();
        self.rx = Some(rx);
        let repaint = ctx.clone();
        self.worker = Some(std::thread::spawn(move || {
            let _ = tx.send(work());
            repaint.request_repaint();
        }));
    }

    pub(crate) fn reset_for_space(&mut self) {
        let started_spaces = std::mem::take(&mut self.started_spaces);
        *self = Self {
            started_spaces,
            ..Self::default()
        };
    }
}

impl crate::ui::app::Foxy {
    pub(crate) fn start_automatic_profile_backups(&mut self, ctx: &eframe::egui::Context) {
        let active = crate::core::game::spaces::active_game_space();
        if !crate::core::game::registry()
            .get(&active.game_id)
            .is_some_and(|module| module.capabilities().profiles)
        {
            return;
        }
        let root = crate::core::game::spaces::active_game_space_dir().join("profile_backups");
        let policy = self.settings_view_state.profile_backups.clone();
        let state = &mut self.profile_backup_state;
        let should_backup = state.automatic_requested(&root, &policy);
        if should_backup || !state.recovery_checked {
            ctx.request_repaint_after(std::time::Duration::from_secs(60));
        }
        if (!should_backup && state.recovery_checked)
            || state
                .last_automatic_check
                .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(60))
            || state.busy
            || state.confirm.is_some()
            || self.pending_game_space_switch.is_some()
            || self.close_requested_at.is_some()
            || self.launch_action.is_pending()
            || self.pending_arma3_profile_action.is_some()
            || self.settings_view_state.debug_mode
        {
            return;
        }
        state.last_automatic_check = Some(std::time::Instant::now());
        state.pending_start_space =
            (should_backup && policy.interval_days == 0).then(|| root.clone());
        let custom_dir = self
            .settings_view_state
            .arma3_profiles_directory
            .trim()
            .to_owned();
        state.start(
            move || {
                let custom = (!custom_dir.is_empty()).then(|| PathBuf::from(custom_dir));
                let roots = profile_backups::restore_roots(custom.as_deref());
                if profile_backups::recover_interrupted_restores(&root, &roots).is_err() {
                    return ProfileBackupResult::Automatic(
                        root,
                        false,
                        Err("Profile backup operation failed.".to_owned()),
                    );
                }
                let result = (|| {
                    if crate::core::arma3_profiles::is_arma3_running() {
                        return Ok(MutationResult::Deferred);
                    }
                    let entries = if should_backup {
                        let profiles =
                            crate::core::arma3_profiles::detect_all_profiles(custom.as_deref());
                        profile_backups::run_automatic(
                            &root,
                            &profiles,
                            &policy,
                            chrono::Local::now(),
                        )?
                    } else {
                        Vec::new()
                    };
                    Ok::<_, anyhow::Error>(MutationResult::Entries(entries))
                })()
                .map_err(|_| "Profile backup operation failed.".to_owned());
                ProfileBackupResult::Automatic(root, true, result)
            },
            ctx,
        );
    }

    pub(crate) fn poll_profile_backups(&mut self, ctx: &eframe::egui::Context) {
        let result = {
            let state = &self.profile_backup_state;
            let Some(rx) = state.rx.as_ref() else {
                return;
            };
            match rx.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    return;
                }
                Err(TryRecvError::Disconnected) => None,
            }
        };
        let Some(result) = result else {
            self.profile_backup_state.busy = false;
            self.profile_backup_state.rx = None;
            self.profile_backup_state.worker = None;
            self.profile_backup_state.status = Some("Profile backup operation failed.".to_owned());
            return;
        };
        let active_root =
            crate::core::game::spaces::active_game_space_dir().join("profile_backups");
        let state = &mut self.profile_backup_state;
        let automatic = matches!(&result, ProfileBackupResult::Automatic(_, _, _));
        match result {
            ProfileBackupResult::List(root, result) => {
                if root == active_root {
                    match result {
                        Ok(entries) => {
                            state.entries = entries;
                            state.list_loaded_for = Some(root);
                        }
                        Err(_) => {
                            state.list_loaded_for = Some(root);
                            state.status = Some("Profile backup operation failed.".to_owned())
                        }
                    }
                }
            }
            ProfileBackupResult::Automatic(root, recovered, result)
            | ProfileBackupResult::Mutation(root, recovered, result) => {
                let start_succeeded = matches!(&result, Ok(MutationResult::Entries(_)));
                if root == active_root {
                    state.recovery_checked = recovered;
                }
                if root == active_root {
                    match result {
                        Ok(MutationResult::Entries(entries)) if !entries.is_empty() => {
                            state.entries = entries;
                            state.list_loaded_for = None;
                            state.status = Some("Profile backup operation completed.".to_owned());
                        }
                        Ok(MutationResult::Entries(_)) => {
                            state.list_loaded_for = None;
                            state.status = Some("Profile backup operation completed.".to_owned());
                        }
                        Ok(MutationResult::Done) => {
                            state.list_loaded_for = None;
                            state.status = Some("Profile backup operation completed.".to_owned());
                        }
                        Ok(MutationResult::Restored(profiles)) => {
                            self.detected_arma3_profiles = profiles;
                            self.detected_active_arma3_profile = None;
                            state.list_loaded_for = None;
                            state.status = Some("Profile backup operation completed.".to_owned());
                        }
                        Ok(MutationResult::Deferred) => state.status = None,
                        Err(reason) => {
                            state.status = Some(reason);
                            state.list_loaded_for = None;
                            if !automatic {
                                state.last_automatic_check = None;
                            }
                        }
                    }
                }
                if state.pending_start_space.as_ref() == Some(&root) {
                    if start_succeeded {
                        state.started_spaces.insert(root);
                    }
                    state.pending_start_space = None;
                }
            }
        }
        state.busy = false;
        state.rx = None;
        state.worker = None;
        ctx.request_repaint();
    }

    pub(crate) fn request_profile_backup_list(&mut self, ctx: &eframe::egui::Context) {
        let root = crate::core::game::spaces::active_game_space_dir().join("profile_backups");
        if self.profile_backup_state.busy
            || self.profile_backup_state.list_loaded_for.as_ref() == Some(&root)
        {
            return;
        }
        self.profile_backup_state.start(
            move || {
                ProfileBackupResult::List(
                    root.clone(),
                    profile_backups::list_backups(&root)
                        .map_err(|_| "Profile backup operation failed.".to_owned()),
                )
            },
            ctx,
        );
    }

    pub(crate) fn start_profile_backup_mutation(
        &mut self,
        ctx: &eframe::egui::Context,
        work: impl FnOnce(PathBuf) -> Result<MutationResult, String> + Send + 'static,
    ) {
        let root = crate::core::game::spaces::active_game_space_dir().join("profile_backups");
        if self.launch_action.is_pending()
            || self.pending_game_space_switch.is_some()
            || self.close_requested_at.is_some()
            || self.pending_arma3_profile_action.is_some()
            || self.settings_view_state.debug_mode
        {
            return;
        }
        let custom_dir = self
            .settings_view_state
            .arma3_profiles_directory
            .trim()
            .to_owned();
        self.profile_backup_state.start(
            move || {
                let custom = (!custom_dir.is_empty()).then(|| PathBuf::from(custom_dir));
                let roots = profile_backups::restore_roots(custom.as_deref());
                let recovery = profile_backups::recover_interrupted_restores(&root, &roots);
                let recovered = recovery.is_ok();
                let result = recovery
                    .map_err(|_| "Profile backup operation failed.".to_owned())
                    .and_then(|()| work(root.clone()));
                ProfileBackupResult::Mutation(root, recovered, result)
            },
            ctx,
        );
    }

    pub(crate) fn profile_backup_work_in_flight(&self) -> bool {
        self.profile_backup_state.busy
    }

    pub(crate) fn profile_backup_recovery_pending(&self) -> bool {
        !self.profile_backup_state.recovery_checked
            && crate::core::game::registry()
                .get(&crate::core::game::spaces::active_game_space().game_id)
                .is_some_and(|module| module.capabilities().profiles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_start_runs_once_per_space_across_switches() {
        let root = PathBuf::from("games/arma3/profile_backups");
        let other = PathBuf::from("games/other/profile_backups");
        let policy = profile_backups::BackupPolicy {
            enabled: true,
            interval_days: 0,
        };
        let mut state = ProfileBackupState::default();
        assert!(state.automatic_requested(&root, &policy));
        state.started_spaces.insert(root.clone());
        state.status = Some("Working...".to_owned());
        state.confirm = Some(ProfileBackupConfirm::DeleteAll);
        state.reset_for_space();
        assert!(!state.automatic_requested(&root, &policy));
        assert!(state.automatic_requested(&other, &policy));
        assert!(state.status.is_none());
        assert!(state.confirm.is_none());
        assert!(state.automatic_requested(&root, &profile_backups::BackupPolicy::default()));
        assert!(!state.automatic_requested(
            &other,
            &profile_backups::BackupPolicy {
                enabled: false,
                ..policy
            }
        ));
    }
}
