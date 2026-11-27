use crate::ui::app::EditorLaunchExternalAddon;
use crate::ui::types::Repository;
use std::path::Path;

pub(super) fn join_preflight_fact_keys(
    state: &crate::ui::app::PendingJoinPreflightState,
) -> Vec<String> {
    let mut facts = Vec::new();
    for item in &state.suggestions {
        facts.push(format!(
            "suggestion:{:?}:{}:{}:{:?}",
            item.origin,
            item.addon_name.to_lowercase(),
            item.reported_name,
            item.confidence
        ));
    }
    for item in &state.ambiguous {
        let mut candidates = item
            .candidates
            .iter()
            .map(|candidate| {
                format!(
                    "{:?}:{}:{:?}",
                    candidate.origin,
                    candidate.addon_name.to_lowercase(),
                    candidate.confidence
                )
            })
            .collect::<Vec<_>>();
        candidates.sort();
        facts.push(format!(
            "ambiguous:{}:{}",
            item.reported_name,
            candidates.join(",")
        ));
    }
    for item in &state.known_remote {
        facts.push(format!(
            "remote:{}:{}:{}:{}:{}:{:?}",
            item.repository_url,
            item.repository_path,
            item.addon_name.to_lowercase(),
            item.reported_name,
            item.available,
            item.confidence
        ));
    }
    for item in &state.extra_enabled {
        facts.push(format!(
            "extra:{:?}:{}:{}:{:?}",
            item.origin,
            item.addon_name.to_lowercase(),
            item.reported_name,
            item.confidence
        ));
    }
    for item in &state.unavailable_enabled {
        facts.push(format!(
            "unavailable:{}:{}",
            item.addon_name.to_lowercase(),
            crate::core::utils::content_hash::normalize_path(&item.path)
        ));
    }
    for item in state.dlc_enable.iter().chain(&state.dlc_disable) {
        facts.push(format!("dlc:{}:{}", item.code, item.name));
    }
    facts.sort();
    facts
}

pub(super) fn merge_join_preflight_choices(
    previous: &crate::ui::app::PendingJoinPreflightState,
    refreshed: &mut crate::ui::app::PendingJoinPreflightState,
) {
    refreshed.ts3_required = previous.ts3_required;
    refreshed.ts3_running = previous.ts3_running;
    refreshed.steam_required = previous.steam_required;
    refreshed.steam_running = previous.steam_running;
    refreshed.server_addons_checked = previous.server_addons_checked;
    for item in &mut refreshed.suggestions {
        if let Some(old) = previous.suggestions.iter().find(|old| {
            old.origin == item.origin && old.addon_name.eq_ignore_ascii_case(&item.addon_name)
        }) {
            item.selected = old.selected;
        }
    }
    for item in &mut refreshed.extra_enabled {
        if let Some(old) = previous.extra_enabled.iter().find(|old| {
            old.origin == item.origin && old.addon_name.eq_ignore_ascii_case(&item.addon_name)
        }) {
            item.selected = old.selected;
        }
    }
    for item in &mut refreshed.known_remote {
        if let Some(old) = previous.known_remote.iter().find(|old| {
            old.addon_name.eq_ignore_ascii_case(&item.addon_name)
                && old.repository_url == item.repository_url
                && old.repository_path == item.repository_path
        }) {
            item.selected = old.selected;
        }
    }
    for item in &mut refreshed.ambiguous {
        if let Some(old) = previous
            .ambiguous
            .iter()
            .find(|old| old.reported_name.eq_ignore_ascii_case(&item.reported_name))
        {
            item.selected_candidate = old
                .selected_candidate
                .filter(|index| *index < item.candidates.len());
            if let Some(index) = item.selected_candidate {
                item.candidates[index].selected = true;
            }
        }
    }
    for item in &mut refreshed.dlc_enable {
        if let Some(old) = previous.dlc_enable.iter().find(|old| old.code == item.code) {
            item.selected = old.selected;
        }
    }
    for item in &mut refreshed.dlc_disable {
        if let Some(old) = previous
            .dlc_disable
            .iter()
            .find(|old| old.code == item.code)
        {
            item.selected = old.selected;
        }
    }
    if !previous.suggestions.is_empty() || !previous.ambiguous.is_empty() {
        refreshed.save_local_addon_selections = previous.save_local_addon_selections;
    }
}

pub(super) fn paths_overlap(left: &str, right: &str) -> bool {
    fn key(path: &str) -> String {
        crate::core::utils::content_hash::normalize_path(path.trim())
            .trim_end_matches('/')
            .to_string()
    }
    let left = key(left);
    let right = key(right);
    if left.is_empty() || right.is_empty() {
        return false;
    }
    left == right
        || left
            .strip_prefix(&right)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(&left)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

pub(super) fn repository_launch_paths(
    repository: &Repository,
    shared_path: Option<&str>,
) -> Vec<String> {
    let mut paths = vec![repository.path.clone()];
    if let Some(shared_path) = shared_path.filter(|path| !path.trim().is_empty()) {
        paths.push(shared_path.to_string());
        paths.extend(
            repository
                .addons
                .iter()
                .chain(&repository.optional_addons)
                .map(|(name, _)| {
                    Path::new(shared_path)
                        .join(name)
                        .to_string_lossy()
                        .into_owned()
                }),
        );
    }
    paths.extend(
        repository
            .addons
            .iter()
            .chain(&repository.optional_addons)
            .map(|(name, _)| {
                Path::new(&repository.path)
                    .join(name)
                    .to_string_lossy()
                    .into_owned()
            }),
    );
    paths.extend(
        repository
            .external_addons
            .iter()
            .filter(|(_, enabled, _)| *enabled)
            .map(|(_, _, path)| path.clone()),
    );
    paths
}

pub(super) fn editor_launch_external_addons(
    repository: &Repository,
) -> Vec<EditorLaunchExternalAddon> {
    let mut addons: Vec<EditorLaunchExternalAddon> = repository
        .external_addons
        .iter()
        .filter(|(_, enabled, _)| *enabled)
        .map(|(name, _, path)| EditorLaunchExternalAddon {
            name: name.trim().to_string(),
            path: path.trim().to_string(),
            keep: true,
        })
        .filter(|addon| !addon.name.is_empty() || !addon.path.is_empty())
        .collect();
    addons.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.path.cmp(&right.path))
    });
    addons.dedup_by(|left, right| left.name == right.name && left.path == right.path);
    addons
}

/// Disables only the external addons the user unticked; every other addon keeps its configured state.
pub(super) fn editor_launch_repository(
    repository: &Repository,
    external_addons: &[EditorLaunchExternalAddon],
) -> Repository {
    let mut launch = repository.clone();
    for (name, enabled, path) in &mut launch.external_addons {
        if *enabled
            && external_addons
                .iter()
                .any(|addon| !addon.keep && addon.name == name.trim() && addon.path == path.trim())
        {
            *enabled = false;
        }
    }
    launch
}

#[cfg(test)]
mod tests {
    use super::{
        editor_launch_external_addons, editor_launch_repository, join_preflight_fact_keys,
        merge_join_preflight_choices, paths_overlap,
    };
    use crate::ui::app::{
        JoinPreflightKnownRemoteAddon, JoinPreflightMatchConfidence, PendingJoinPreflightState,
    };
    use crate::ui::types::{Repository, RepositoryServer};

    fn editor_repository() -> Repository {
        let mut repository = Repository {
            path: "R:/Mods".to_string(),
            ..Repository::default()
        };
        repository.addons = vec![("@mod_01".to_string(), true)];
        repository.optional_addons = vec![("@mod_02".to_string(), true)];
        repository.external_addons = vec![
            ("@mod_10".to_string(), true, "R:/Mods/@mod_10".to_string()),
            ("@mod_11".to_string(), true, "R:/Mods/@mod_11".to_string()),
            ("@mod_20".to_string(), true, "R:/Other/@mod_20".to_string()),
            ("@mod_21".to_string(), true, "R:/Other/@mod_21".to_string()),
            ("@mod_30".to_string(), false, "R:/Other/@mod_30".to_string()),
        ];
        repository
    }

    #[test]
    fn editor_launch_lists_every_enabled_external_addon_ticked() {
        let listed = editor_launch_external_addons(&editor_repository());
        let names: Vec<&str> = listed.iter().map(|addon| addon.name.as_str()).collect();
        assert_eq!(names, ["@mod_10", "@mod_11", "@mod_20", "@mod_21"]);
        assert!(listed.iter().all(|addon| addon.keep));
    }

    #[test]
    fn editor_launch_leaves_out_only_unticked_external_addons() {
        let repository = editor_repository();
        let mut listed = editor_launch_external_addons(&repository);
        for addon in &mut listed {
            addon.keep = !addon.path.starts_with("R:/Mods/");
        }

        let launch = editor_launch_repository(&repository, &listed);

        let enabled: Vec<&str> = launch
            .external_addons
            .iter()
            .filter(|(_, enabled, _)| *enabled)
            .map(|(name, _, _)| name.as_str())
            .collect();
        assert_eq!(enabled, ["@mod_20", "@mod_21"]);
        assert_eq!(launch.external_addons.len(), 5);
        assert_eq!(launch.addons, repository.addons);
        assert_eq!(launch.optional_addons, repository.optional_addons);
    }

    #[test]
    fn editor_launch_with_all_ticked_keeps_the_configured_repository() {
        let repository = editor_repository();
        let listed = editor_launch_external_addons(&repository);
        let launch = editor_launch_repository(&repository, &listed);
        assert_eq!(launch.external_addons, repository.external_addons);
    }

    #[test]
    fn launch_conflict_paths_include_ancestors_but_not_sibling_folders() {
        assert!(paths_overlap("C:\\Games\\Shared", "c:/games/shared/@mod_a"));
        assert!(paths_overlap("R:/Mods", "r:/mods"));
        assert!(!paths_overlap("R:/Mods/A", "R:/Mods/AB"));
        assert!(!paths_overlap("R:/Mods/A", "R:/Mods/B"));
        assert!(!paths_overlap("", "R:/Mods/B"));
    }

    fn preflight() -> PendingJoinPreflightState {
        PendingJoinPreflightState::empty(
            "RepoAlpha",
            RepositoryServer::default(),
            Repository::default(),
            false,
        )
    }

    #[test]
    fn refreshed_join_facts_retain_choice_and_detect_availability_change() {
        let mut old = preflight();
        old.known_remote.push(JoinPreflightKnownRemoteAddon {
            reported_name: "mod-a".to_string(),
            addon_name: "@mod_a".to_string(),
            repository_name: "RepoBeta".to_string(),
            repository_url: "https://repo.example.invalid/".to_string(),
            repository_path: "R:/Mods/Beta".to_string(),
            available: false,
            confidence: JoinPreflightMatchConfidence::ExactNormalizedName,
            selected: true,
        });
        old.steam_required = true;
        old.steam_running = true;
        old.ts3_required = true;
        old.server_addons_checked = true;
        let mut refreshed = preflight();
        refreshed.known_remote = old.known_remote.clone();
        refreshed.known_remote[0].selected = false;
        merge_join_preflight_choices(&old, &mut refreshed);
        assert!(refreshed.known_remote[0].selected);
        assert!(refreshed.steam_required && refreshed.steam_running);
        assert!(refreshed.ts3_required && !refreshed.ts3_running);
        assert!(refreshed.server_addons_checked);
        assert_eq!(
            join_preflight_fact_keys(&old),
            join_preflight_fact_keys(&refreshed)
        );

        refreshed.known_remote[0].available = true;
        assert_ne!(
            join_preflight_fact_keys(&old),
            join_preflight_fact_keys(&refreshed)
        );
    }

    #[test]
    fn refreshed_join_facts_preserve_save_local_addon_selections_flag() {
        let mut old = preflight();
        old.suggestions
            .push(crate::ui::app::JoinPreflightAddonSuggestion {
                addon_name: "@mod_01".to_string(),
                origin: crate::ui::app::JoinPreflightAddonOrigin::Required,
                reported_name: "@mod_01".to_string(),
                confidence: JoinPreflightMatchConfidence::ExactNormalizedName,
                selected: true,
            });
        old.save_local_addon_selections = false;

        let mut refreshed = preflight();
        refreshed.suggestions = old.suggestions.clone();
        refreshed.save_local_addon_selections = true;

        merge_join_preflight_choices(&old, &mut refreshed);
        assert!(!refreshed.save_local_addon_selections);
    }
}
