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

#[cfg(test)]
mod tests {
    use super::{join_preflight_fact_keys, merge_join_preflight_choices, paths_overlap};
    use crate::ui::app::{
        JoinPreflightKnownRemoteAddon, JoinPreflightMatchConfidence, PendingJoinPreflightState,
    };
    use crate::ui::types::{Repository, RepositoryServer};

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
        let mut refreshed = preflight();
        refreshed.known_remote = old.known_remote.clone();
        refreshed.known_remote[0].selected = false;
        merge_join_preflight_choices(&old, &mut refreshed);
        assert!(refreshed.known_remote[0].selected);
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
}
