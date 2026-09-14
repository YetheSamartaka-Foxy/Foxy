use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use log::warn;

use crate::core::game::spaces;
use crate::core::models::game_space_stats::{AddonGroupStats, addon_group_stats};
use crate::core::ts3_plugin;
use crate::ui::app::Foxy;
use crate::ui::views::settings::ts3_plugins::Ts3PluginRowStatus;

/// One repository space (or the standalone group) as summarized for the
/// active game space overview.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepositoryGroupSummary {
    pub space_id: Option<String>,
    pub name: String,
    pub repository_count: usize,
    pub stats: AddonGroupStats,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamSpeakPluginSummary {
    pub addon_name: String,
    pub version: Option<String>,
    pub status: Ts3PluginRowStatus,
}

/// Present only when a TeamSpeak 3 client was found on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamSpeakSummary {
    pub directory: PathBuf,
    pub plugins: Vec<TeamSpeakPluginSummary>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GameSpaceOverviewReport {
    pub total: AddonGroupStats,
    pub groups: Vec<RepositoryGroupSummary>,
    pub workspace_bytes: u64,
    pub teamspeak: Option<TeamSpeakSummary>,
}

/// Background-loaded numbers behind the game space overview panel. The DB
/// aggregates and the workspace walk run off the UI thread; the panel shows
/// the last report until a fresh one lands.
#[derive(Default)]
pub struct GameSpaceOverviewState {
    pub report: Option<GameSpaceOverviewReport>,
    rx: Option<Receiver<GameSpaceOverviewReport>>,
    loaded_key: Option<(u64, u64, u64)>,
    dirty: bool,
}

/// TeamSpeak inputs for the background load, gathered on the UI thread from
/// the settings and the last plugin scan; `None` when the game has no TS3
/// integration.
struct TeamSpeakRequest {
    configured_dir: String,
    plugins: Vec<(String, PathBuf, Ts3PluginRowStatus)>,
}

struct GroupRequest {
    space_id: Option<String>,
    name: String,
    members: Vec<(String, String)>,
}

pub(crate) fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn dir_size_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if let Ok(metadata) = entry.metadata() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    total
}

fn teamspeak_summary(request: TeamSpeakRequest) -> Option<TeamSpeakSummary> {
    let directory = ts3_plugin::resolve_teamspeak_directory(&request.configured_dir)?;
    let plugins = request
        .plugins
        .into_iter()
        .map(|(addon_name, path, status)| TeamSpeakPluginSummary {
            addon_name,
            version: ts3_plugin::read_package_version(&path),
            status,
        })
        .collect();
    Some(TeamSpeakSummary { directory, plugins })
}

fn spawn_overview_load(
    groups: Vec<GroupRequest>,
    workspace_dir: PathBuf,
    teamspeak: Option<TeamSpeakRequest>,
) -> Receiver<GameSpaceOverviewReport> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let runtime = crate::core::api::background_runtime();
        let stats_for = |members: &[(String, String)]| -> AddonGroupStats {
            let Some(runtime) = runtime else {
                return AddonGroupStats::default();
            };
            match runtime.block_on(addon_group_stats(members)) {
                Ok(stats) => stats,
                Err(err) => {
                    warn!(
                        "Game space overview could not read repository stats: {}",
                        err
                    );
                    AddonGroupStats::default()
                }
            }
        };
        let all_members: Vec<(String, String)> = groups
            .iter()
            .flat_map(|group| group.members.iter().cloned())
            .collect();
        let report = GameSpaceOverviewReport {
            total: stats_for(&all_members),
            groups: groups
                .iter()
                .map(|group| RepositoryGroupSummary {
                    space_id: group.space_id.clone(),
                    name: group.name.clone(),
                    repository_count: group.members.len(),
                    stats: stats_for(&group.members),
                })
                .collect(),
            workspace_bytes: dir_size_bytes(&workspace_dir),
            teamspeak: teamspeak.and_then(teamspeak_summary),
        };
        let _ = tx.send(report);
    });
    rx
}

impl Foxy {
    /// Show the active game space overview in the main panel by clearing
    /// every sidebar selection; the panel is what renders when nothing is
    /// selected.
    pub fn open_game_space_overview(&mut self) {
        self.repository_view_state.selected_repository = None;
        self.selected_repository_space_id = None;
        self.selected_repository_visual_folder_id = None;
        self.needs_repaint = true;
    }

    pub(crate) fn mark_game_space_overview_dirty(&mut self) {
        self.game_space_overview.dirty = true;
    }

    /// Kick off a reload when the overview is stale and none is in flight,
    /// then drain any finished report. Called from the overview panel.
    pub(crate) fn poll_game_space_overview(&mut self) {
        let key = (
            self.repositories_revision,
            self.repository_spaces_version,
            self.settings_revision,
        );
        let stale = self.game_space_overview.dirty
            || self.game_space_overview.loaded_key != Some(key)
            || self.game_space_overview.report.is_none();
        if stale && self.game_space_overview.rx.is_none() {
            let mut groups: Vec<GroupRequest> = self
                .repository_spaces
                .iter()
                .map(|space| GroupRequest {
                    space_id: Some(space.id.clone()),
                    name: space.name.clone(),
                    members: Vec::new(),
                })
                .collect();
            let mut standalone = GroupRequest {
                space_id: None,
                name: String::new(),
                members: Vec::new(),
            };
            for repo in &self.repository_view_state.repositories {
                let member = (repo.address.clone(), repo.path.clone());
                let group = repo.repository_space_id.as_deref().and_then(|space_id| {
                    groups
                        .iter_mut()
                        .find(|group| group.space_id.as_deref() == Some(space_id))
                });
                match group {
                    Some(group) => group.members.push(member),
                    None => standalone.members.push(member),
                }
            }
            if !standalone.members.is_empty() {
                groups.push(standalone);
            }
            let teamspeak = crate::core::game::registry()
                .active()
                .capabilities()
                .teamspeak3_plugins
                .then(|| TeamSpeakRequest {
                    configured_dir: self.settings_view_state.teamspeak3_directory.clone(),
                    plugins: self.ts3_plugin_overview_entries(),
                });
            self.game_space_overview.rx = Some(spawn_overview_load(
                groups,
                spaces::active_game_space_dir(),
                teamspeak,
            ));
            self.game_space_overview.loaded_key = Some(key);
            self.game_space_overview.dirty = false;
        }
        let Some(rx) = self.game_space_overview.rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(report) => {
                self.game_space_overview.report = Some(report);
                self.game_space_overview.rx = None;
                self.needs_repaint = true;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.game_space_overview.rx = None;
            }
        }
    }

    fn repository_instance_index(&self, address: &str, path: &str) -> Option<usize> {
        let key = Self::repo_instance_key(address, path);
        self.repository_view_state
            .repositories
            .iter()
            .position(|repo| Self::repo_instance_key(&repo.address, &repo.path) == key)
    }

    pub(crate) fn mark_repository_launched(&mut self, address: &str, path: &str) {
        if let Some(index) = self.repository_instance_index(address, path) {
            self.repository_view_state.repositories[index].last_launched_at = Some(unix_now_secs());
            self.mark_repositories_dirty();
        }
    }

    pub(crate) fn mark_repository_updated(&mut self, index: usize) {
        if let Some(repo) = self.repository_view_state.repositories.get_mut(index) {
            repo.last_updated_at = Some(unix_now_secs());
            self.mark_repositories_dirty();
            self.mark_game_space_overview_dirty();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_size_sums_nested_files_and_skips_missing_dirs() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("a.bin"), [0u8; 10]).unwrap();
        std::fs::create_dir_all(dir.path().join("nested/deeper")).unwrap();
        std::fs::write(dir.path().join("nested/deeper/b.bin"), [0u8; 32]).unwrap();
        assert_eq!(dir_size_bytes(dir.path()), 42);
        assert_eq!(dir_size_bytes(&dir.path().join("missing")), 0);
    }
}
