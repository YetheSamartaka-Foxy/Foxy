use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::core::addon_metadata::AddonDisplayNameSnapshot;
use crate::core::arma3_server_query::ServerAddonRequirement;
use crate::core::utils::fs_safety::resolve_child_dir_case_insensitive;
use crate::ui::app::{
    Foxy, JoinPreflightAddonOrigin, JoinPreflightAddonSuggestion, JoinPreflightAmbiguousAddon,
    JoinPreflightKnownRemoteAddon, JoinPreflightMatchConfidence, JoinPreflightUnavailableAddon,
    PendingJoinPreflightState,
};
use crate::ui::types::{Repository, RepositoryServer};
use log::info;

struct KnownRemoteSearchContext<'a> {
    repo_name: &'a str,
    server: &'a RepositoryServer,
    requirement_by_name: &'a HashMap<String, &'a ServerAddonRequirement>,
    satisfied_or_actionable: &'a HashSet<String>,
    display_names: &'a AddonDisplayNameSnapshot,
}

struct KnownRemoteLogEntry<'a> {
    repo_name: &'a str,
    server: &'a RepositoryServer,
    requirement: &'a ServerAddonRequirement,
    matched_key: &'a str,
    source_repo: &'a Repository,
    source_addon_name: &'a str,
    source_display_name: Option<&'a String>,
    display_names: &'a AddonDisplayNameSnapshot,
    satisfied_or_actionable: &'a HashSet<String>,
}

impl Foxy {
    pub(crate) fn build_join_preflight_state(
        effective: &Repository,
        configured_repositories: &[Repository],
        server: &RepositoryServer,
        repo_name: &str,
        requirements: &[ServerAddonRequirement],
        display_names: &AddonDisplayNameSnapshot,
    ) -> Option<PendingJoinPreflightState> {
        let requirement_by_name = requirements
            .iter()
            .flat_map(|requirement| {
                requirement_match_keys_for_requirement(requirement)
                    .into_iter()
                    .map(move |normalized| (normalized, requirement))
            })
            .collect::<HashMap<_, _>>();

        if requirement_by_name.is_empty() {
            return None;
        }

        let mut candidates_by_requirement = collect_disabled_local_candidates(
            effective,
            configured_repositories,
            requirements,
            display_names,
        );
        let mut suggestions = Vec::new();
        let mut ambiguous = Vec::new();
        let mut satisfied_or_actionable = HashSet::new();

        for requirement in requirements {
            let match_keys = requirement_match_keys_for_requirement(requirement);
            if match_keys.is_empty()
                || match_keys.iter().any(|key| {
                    enabled_requirement_satisfied(
                        effective,
                        configured_repositories,
                        key,
                        display_names,
                    )
                })
            {
                mark_requirement_keys_handled(&mut satisfied_or_actionable, &match_keys);
                continue;
            }

            let candidate_entry = match_keys.iter().find_map(|key| {
                candidates_by_requirement
                    .remove(key)
                    .map(|candidates| (key, candidates))
            });
            let Some((_normalized_key, candidates)) = candidate_entry else {
                continue;
            };

            match candidates.as_slice() {
                [] => {}
                [candidate] => {
                    suggestions.push(candidate.clone());
                    mark_requirement_keys_handled(&mut satisfied_or_actionable, &match_keys);
                }
                _ => {
                    log_ambiguous_join_preflight_match(
                        effective,
                        repo_name,
                        server,
                        requirement,
                        &match_keys,
                        &candidates,
                        display_names,
                    );
                    ambiguous.push(JoinPreflightAmbiguousAddon {
                        reported_name: requirement.display_name.clone(),
                        candidates,
                        selected_candidate: None,
                    });
                    mark_requirement_keys_handled(&mut satisfied_or_actionable, &match_keys);
                }
            }
        }

        let known_remote = Self::collect_known_remote_addons(
            effective,
            configured_repositories,
            KnownRemoteSearchContext {
                repo_name,
                server,
                requirement_by_name: &requirement_by_name,
                satisfied_or_actionable: &satisfied_or_actionable,
                display_names,
            },
        );
        let extra_enabled = collect_extra_enabled_addons(
            effective,
            configured_repositories,
            &requirement_by_name,
            display_names,
        );
        let unavailable_enabled = collect_unavailable_enabled_external_addons(
            effective,
            configured_repositories,
            &requirement_by_name,
            display_names,
        );

        if suggestions.is_empty()
            && ambiguous.is_empty()
            && known_remote.is_empty()
            && extra_enabled.is_empty()
            && unavailable_enabled.is_empty()
        {
            return None;
        }

        suggestions.sort_by(|left, right| {
            origin_sort_key(&left.origin)
                .cmp(&origin_sort_key(&right.origin))
                .then_with(|| left.addon_name.cmp(&right.addon_name))
        });
        suggestions.dedup_by(|left, right| {
            left.origin == right.origin && left.addon_name.eq_ignore_ascii_case(&right.addon_name)
        });

        Some(PendingJoinPreflightState {
            repo_name: repo_name.to_string(),
            server: server.clone(),
            original_repository: effective.clone(),
            suggestions,
            ambiguous,
            known_remote,
            extra_enabled,
            unavailable_enabled,
            // Filled in by `present_join_preflight` once the gate is evaluated.
            ts3_required: false,
            ts3_running: false,
            steam_required: false,
            steam_running: false,
            launch_only: false,
        })
    }

    pub(crate) fn log_join_preflight_modal_contents(
        preflight: &PendingJoinPreflightState,
        requirement_count: usize,
    ) {
        let mut reasons = Vec::new();
        if !preflight.suggestions.is_empty() {
            reasons.push(format!("local_disabled={}", preflight.suggestions.len()));
        }
        if !preflight.ambiguous.is_empty() {
            reasons.push(format!("ambiguous_local={}", preflight.ambiguous.len()));
        }
        if !preflight.known_remote.is_empty() {
            reasons.push(format!("known_remote={}", preflight.known_remote.len()));
        }
        if !preflight.extra_enabled.is_empty() {
            reasons.push(format!("extra_enabled={}", preflight.extra_enabled.len()));
        }
        if !preflight.unavailable_enabled.is_empty() {
            reasons.push(format!(
                "unavailable_enabled={}",
                preflight.unavailable_enabled.len()
            ));
        }

        info!(
            "Join addon preflight modal content for repository {} server {}:{}: requirements={}, reason={}",
            preflight.repo_name,
            preflight.server.address,
            preflight.server.port,
            requirement_count,
            reasons.join(", ")
        );

        if preflight.suggestions.is_empty() {
            info!(
                "Join addon preflight modal local enable section for repository {} server {}:{}: empty",
                preflight.repo_name, preflight.server.address, preflight.server.port
            );
        } else {
            for (idx, suggestion) in preflight.suggestions.iter().enumerate() {
                info!(
                    "Join addon preflight modal local enable[{}] for repository {} server {}:{}: addon_name={:?}, server_name={:?}, origin={}, match_type={:?}, selected={}",
                    idx,
                    preflight.repo_name,
                    preflight.server.address,
                    preflight.server.port,
                    suggestion.addon_name,
                    suggestion.reported_name,
                    origin_log_label(&suggestion.origin),
                    suggestion.confidence,
                    suggestion.selected
                );
            }
        }

        if preflight.ambiguous.is_empty() {
            info!(
                "Join addon preflight modal ambiguous section for repository {} server {}:{}: empty",
                preflight.repo_name, preflight.server.address, preflight.server.port
            );
        } else {
            for (idx, ambiguous) in preflight.ambiguous.iter().enumerate() {
                let candidates = ambiguous
                    .candidates
                    .iter()
                    .map(|candidate| {
                        format!(
                            "{} ({}, {:?}, selected={})",
                            candidate.addon_name,
                            origin_log_label(&candidate.origin),
                            candidate.confidence,
                            ambiguous
                                .selected_candidate
                                .and_then(|selected| ambiguous.candidates.get(selected))
                                .is_some_and(|selected| {
                                    selected
                                        .addon_name
                                        .eq_ignore_ascii_case(&candidate.addon_name)
                                        && selected.origin == candidate.origin
                                })
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                info!(
                    "Join addon preflight modal ambiguous[{}] for repository {} server {}:{}: server_name={:?}, candidates={}",
                    idx,
                    preflight.repo_name,
                    preflight.server.address,
                    preflight.server.port,
                    ambiguous.reported_name,
                    candidates
                );
            }
        }

        if preflight.known_remote.is_empty() {
            info!(
                "Join addon preflight modal known remote section for repository {} server {}:{}: empty",
                preflight.repo_name, preflight.server.address, preflight.server.port
            );
        } else {
            for (idx, remote) in preflight.known_remote.iter().enumerate() {
                info!(
                    "Join addon preflight modal known remote[{}] for repository {} server {}:{}: server_name={:?}, source_repository={:?}, addon_name={:?}, available={}, match_type={:?}, selected={}",
                    idx,
                    preflight.repo_name,
                    preflight.server.address,
                    preflight.server.port,
                    remote.reported_name,
                    remote.repository_name,
                    remote.addon_name,
                    remote.available,
                    remote.confidence,
                    remote.selected
                );
            }
        }

        if preflight.extra_enabled.is_empty() {
            info!(
                "Join addon preflight modal extra enabled section for repository {} server {}:{}: empty",
                preflight.repo_name, preflight.server.address, preflight.server.port
            );
        } else {
            for (idx, extra) in preflight.extra_enabled.iter().enumerate() {
                info!(
                    "Join addon preflight modal extra enabled[{}] for repository {} server {}:{}: addon_name={:?}, origin={}, keep_enabled={}",
                    idx,
                    preflight.repo_name,
                    preflight.server.address,
                    preflight.server.port,
                    extra.addon_name,
                    origin_log_label(&extra.origin),
                    extra.selected
                );
            }
        }

        if preflight.unavailable_enabled.is_empty() {
            info!(
                "Join addon preflight modal unavailable enabled section for repository {} server {}:{}: empty",
                preflight.repo_name, preflight.server.address, preflight.server.port
            );
        } else {
            for (idx, unavailable) in preflight.unavailable_enabled.iter().enumerate() {
                info!(
                    "Join addon preflight modal unavailable enabled[{}] for repository {} server {}:{}: addon_name={:?}, path={:?}",
                    idx,
                    preflight.repo_name,
                    preflight.server.address,
                    preflight.server.port,
                    unavailable.addon_name,
                    unavailable.path
                );
            }
        }
    }

    pub(crate) fn repository_with_join_preflight_selections(
        pending: &PendingJoinPreflightState,
    ) -> Repository {
        let mut repository = pending.original_repository.clone();
        for suggestion in &pending.suggestions {
            if suggestion.selected {
                enable_suggestion(&mut repository, suggestion);
            }
        }
        for ambiguous in &pending.ambiguous {
            let Some(candidate_idx) = ambiguous.selected_candidate else {
                continue;
            };
            if let Some(candidate) = ambiguous.candidates.get(candidate_idx) {
                enable_suggestion(&mut repository, candidate);
            }
        }
        apply_extra_enabled_disables(&mut repository, pending);
        for remote in &pending.known_remote {
            if remote.selected {
                enable_known_remote_addon(&mut repository, remote);
            }
        }
        repository
    }

    /// Builds the launch repository for the "launch without suggested addons" path.
    ///
    /// This path enables no suggested/known-remote addons and always strips every
    /// detected extra enabled addon, regardless of its tick state. The tick state
    /// only governs the "launch with selected addons" path; choosing to launch
    /// without the detected addons means none of them are loaded.
    pub(crate) fn repository_without_join_preflight_suggestions(
        pending: &PendingJoinPreflightState,
    ) -> Repository {
        let mut repository = pending.original_repository.clone();
        for extra in &pending.extra_enabled {
            disable_suggestion(&mut repository, extra);
        }
        repository
    }

    /// Enabled external addons that won't resolve at launch, for launch paths
    /// that have no server requirement list (the plain launch and the editor
    /// launch). Mirrors the `unavailable_enabled` set the join preflight builds;
    /// with no requirements there is nothing to exclude, so every enabled
    /// external addon with an unresolvable path is reported.
    pub(crate) fn unavailable_enabled_external_addons(
        effective: &Repository,
        configured_repositories: &[Repository],
    ) -> Vec<JoinPreflightUnavailableAddon> {
        collect_unavailable_enabled_external_addons(
            effective,
            configured_repositories,
            &HashMap::new(),
            &AddonDisplayNameSnapshot::new(),
        )
    }

    fn collect_known_remote_addons(
        effective: &Repository,
        configured_repositories: &[Repository],
        context: KnownRemoteSearchContext<'_>,
    ) -> Vec<JoinPreflightKnownRemoteAddon> {
        let current_repo_address = Self::normalize_repo_url(&effective.address);
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        let KnownRemoteSearchContext {
            repo_name,
            server,
            requirement_by_name,
            satisfied_or_actionable,
            display_names,
        } = context;

        for (repo_idx, repo) in configured_repositories.iter().enumerate() {
            if repo.address.trim().is_empty()
                || repo.path.trim().is_empty()
                || Self::normalize_repo_url(&repo.address) == current_repo_address
            {
                continue;
            }

            for addon_name in repo
                .addons
                .iter()
                .chain(repo.optional_addons.iter())
                .map(|(name, _)| name)
            {
                let repo_url = Self::normalize_repo_url(&repo.address);
                let display_name = addon_display_name(display_names, &repo_url, addon_name);
                let Some((normalized, requirement)) = addon_match_keys(addon_name, display_name)
                    .into_iter()
                    .find_map(|key| {
                        if key.is_empty() || satisfied_or_actionable.contains(&key) {
                            None
                        } else {
                            requirement_by_name
                                .get(&key)
                                .map(|requirement| (key, *requirement))
                        }
                    })
                else {
                    continue;
                };
                if !seen.insert((normalized.clone(), repo_idx)) {
                    continue;
                }
                log_known_remote_join_preflight_match(
                    effective,
                    KnownRemoteLogEntry {
                        repo_name,
                        server,
                        requirement,
                        matched_key: &normalized,
                        source_repo: repo,
                        source_addon_name: addon_name,
                        source_display_name: display_name,
                        display_names,
                        satisfied_or_actionable,
                    },
                );
                rows.push(JoinPreflightKnownRemoteAddon {
                    reported_name: requirement.display_name.clone(),
                    addon_name: addon_name.clone(),
                    repository_name: repo.name.clone(),
                    repository_url: repo_url,
                    repository_path: repo.path.clone(),
                    available: source_repository_addon_path_available(repo, addon_name),
                    confidence: JoinPreflightMatchConfidence::ExactNormalizedName,
                    selected: true,
                });
            }
        }

        rows.sort_by(|left, right| {
            left.reported_name
                .cmp(&right.reported_name)
                .then_with(|| left.repository_name.cmp(&right.repository_name))
        });
        rows
    }
}

fn mark_requirement_keys_handled(handled: &mut HashSet<String>, match_keys: &[String]) {
    handled.extend(match_keys.iter().cloned());
}

fn log_ambiguous_join_preflight_match(
    effective: &Repository,
    repo_name: &str,
    server: &RepositoryServer,
    requirement: &ServerAddonRequirement,
    requirement_keys: &[String],
    candidates: &[JoinPreflightAddonSuggestion],
    display_names: &AddonDisplayNameSnapshot,
) {
    info!(
        "Join addon preflight ambiguous local match for repository {} server {}:{}: server_name={:?}, raw_identity={:?}, requirement_keys=[{}], candidate_count={}",
        repo_name,
        server.address,
        server.port,
        requirement.display_name,
        requirement.raw_identity,
        requirement_keys.join(", "),
        candidates.len()
    );

    for (candidate_idx, candidate) in candidates.iter().enumerate() {
        let display_name = candidate_display_name(effective, candidate, display_names);
        let candidate_keys = addon_match_keys(&candidate.addon_name, display_name);
        info!(
            "Join addon preflight ambiguous candidate[{}] for repository {} server {}:{}: server_name={:?}, addon_name={:?}, origin={}, display_name={:?}, candidate_keys=[{}], match_type={:?}",
            candidate_idx,
            repo_name,
            server.address,
            server.port,
            requirement.display_name,
            candidate.addon_name,
            origin_log_label(&candidate.origin),
            display_name,
            candidate_keys.join(", "),
            candidate.confidence
        );
    }
}

fn log_known_remote_join_preflight_match(effective: &Repository, entry: KnownRemoteLogEntry<'_>) {
    let requirement_keys = requirement_match_keys(&entry.requirement.display_name);
    let current_matches =
        current_repo_requirement_match_details(effective, &requirement_keys, entry.display_names);
    let handled_keys = requirement_keys
        .iter()
        .filter(|key| entry.satisfied_or_actionable.contains(*key))
        .cloned()
        .collect::<Vec<_>>();

    info!(
        "Join addon preflight remote suggestion accepted for repository {} server {}:{}: server_name={:?}, raw_identity={:?}, matched_key={}, requirement_keys=[{}], handled_requirement_keys=[{}], source_repository={:?}, source_addon={:?}, source_display_name={:?}",
        entry.repo_name,
        entry.server.address,
        entry.server.port,
        entry.requirement.display_name,
        entry.requirement.raw_identity,
        entry.matched_key,
        requirement_keys.join(", "),
        handled_keys.join(", "),
        entry.source_repo.name,
        entry.source_addon_name,
        entry.source_display_name
    );

    if current_matches.is_empty() {
        info!(
            "Join addon preflight current repository has no local addon whose active match keys hit server requirement {:?} for repository {} server {}:{}",
            entry.requirement.display_name,
            entry.repo_name,
            entry.server.address,
            entry.server.port
        );
    } else {
        for (idx, detail) in current_matches.iter().enumerate() {
            info!(
                "Join addon preflight current repository match candidate[{}] for server requirement {:?} repository {} server {}:{}: addon_name={:?}, origin={}, enabled={}, display_name={:?}, match_keys=[{}]",
                idx,
                entry.requirement.display_name,
                entry.repo_name,
                entry.server.address,
                entry.server.port,
                detail.addon_name,
                detail.origin,
                detail.enabled,
                detail.display_name,
                detail.match_keys.join(", ")
            );
        }
    }
}

#[derive(Debug)]
struct LocalRequirementMatchDetail {
    addon_name: String,
    origin: &'static str,
    enabled: bool,
    display_name: Option<String>,
    match_keys: Vec<String>,
}

fn current_repo_requirement_match_details(
    effective: &Repository,
    requirement_keys: &[String],
    display_names: &AddonDisplayNameSnapshot,
) -> Vec<LocalRequirementMatchDetail> {
    let repo_display_names = addon_display_names_for_repo(display_names, effective);
    let requirement_keys = requirement_keys.iter().cloned().collect::<HashSet<_>>();
    let mut details = Vec::new();

    for (addon_name, enabled) in &effective.addons {
        let display_name = repo_display_names.and_then(|names| names.get(addon_name));
        push_current_match_detail(
            &mut details,
            addon_name,
            "required",
            *enabled,
            display_name,
            &requirement_keys,
            display_names,
        );
    }
    for (addon_name, enabled) in &effective.optional_addons {
        let display_name = repo_display_names.and_then(|names| names.get(addon_name));
        push_current_match_detail(
            &mut details,
            addon_name,
            "optional",
            *enabled,
            display_name,
            &requirement_keys,
            display_names,
        );
    }
    for (addon_name, enabled, path) in &effective.external_addons {
        push_current_match_detail(
            &mut details,
            addon_name,
            "external",
            *enabled,
            Some(path),
            &requirement_keys,
            display_names,
        );
    }

    details
}

fn push_current_match_detail(
    details: &mut Vec<LocalRequirementMatchDetail>,
    addon_name: &str,
    origin: &'static str,
    enabled: bool,
    display_or_path_name: Option<&String>,
    requirement_keys: &HashSet<String>,
    display_names: &AddonDisplayNameSnapshot,
) {
    let match_keys = if origin == "external" {
        external_addon_match_keys(
            addon_name,
            display_or_path_name.map(String::as_str),
            &[],
            display_names,
        )
    } else {
        addon_match_keys(addon_name, display_or_path_name)
    }
    .into_iter()
    .filter(|key| requirement_keys.contains(key))
    .collect::<Vec<_>>();
    if match_keys.is_empty() {
        return;
    }

    details.push(LocalRequirementMatchDetail {
        addon_name: addon_name.to_string(),
        origin,
        enabled,
        display_name: display_or_path_name.cloned(),
        match_keys,
    });
}

fn candidate_display_name<'a>(
    effective: &'a Repository,
    candidate: &JoinPreflightAddonSuggestion,
    display_names: &'a AddonDisplayNameSnapshot,
) -> Option<&'a String> {
    match candidate.origin {
        JoinPreflightAddonOrigin::Required | JoinPreflightAddonOrigin::Optional => {
            addon_display_names_for_repo(display_names, effective)
                .and_then(|names| names.get(&candidate.addon_name))
        }
        JoinPreflightAddonOrigin::External => None,
    }
}

fn origin_log_label(origin: &JoinPreflightAddonOrigin) -> &'static str {
    match origin {
        JoinPreflightAddonOrigin::Required => "required",
        JoinPreflightAddonOrigin::Optional => "optional",
        JoinPreflightAddonOrigin::External => "external",
    }
}

fn collect_disabled_local_candidates(
    effective: &Repository,
    configured_repositories: &[Repository],
    requirements: &[ServerAddonRequirement],
    display_names: &AddonDisplayNameSnapshot,
) -> HashMap<String, Vec<JoinPreflightAddonSuggestion>> {
    let requirement_by_name = requirements
        .iter()
        .flat_map(|requirement| {
            requirement_match_keys_for_requirement(requirement)
                .into_iter()
                .map(move |key| (key, requirement))
        })
        .collect::<HashMap<_, _>>();
    let mut candidates = HashMap::<String, Vec<JoinPreflightAddonSuggestion>>::new();

    collect_disabled_addon_candidates(
        &effective.addons,
        addon_display_names_for_repo(display_names, effective),
        &requirement_by_name,
        JoinPreflightAddonOrigin::Required,
        &mut candidates,
    );
    collect_disabled_addon_candidates(
        &effective.optional_addons,
        addon_display_names_for_repo(display_names, effective),
        &requirement_by_name,
        JoinPreflightAddonOrigin::Optional,
        &mut candidates,
    );
    for (addon_name, enabled, path) in &effective.external_addons {
        if !*enabled && external_addon_path_available(addon_name, path) {
            for normalized in external_addon_match_keys(
                addon_name,
                Some(path),
                configured_repositories,
                display_names,
            ) {
                if let Some(requirement) = requirement_by_name.get(&normalized) {
                    candidates
                        .entry(normalized)
                        .or_default()
                        .push(JoinPreflightAddonSuggestion {
                            addon_name: addon_name.clone(),
                            origin: JoinPreflightAddonOrigin::External,
                            reported_name: requirement.display_name.clone(),
                            confidence: JoinPreflightMatchConfidence::ExactNormalizedName,
                            selected: true,
                        });
                    break;
                }
            }
        }
    }

    candidates
}

fn collect_extra_enabled_addons(
    effective: &Repository,
    configured_repositories: &[Repository],
    requirements: &HashMap<String, &ServerAddonRequirement>,
    display_names: &AddonDisplayNameSnapshot,
) -> Vec<JoinPreflightAddonSuggestion> {
    let mut extras = Vec::new();
    let optional_client_side = effective
        .optional_addon_client_side
        .iter()
        .chain(effective.remote_client_side_addons.iter())
        .map(|name| normalize_addon_name(name))
        .collect::<HashSet<_>>();
    collect_extra_enabled_addon_candidates(
        &effective.optional_addons,
        addon_display_names_for_repo(display_names, effective),
        requirements,
        JoinPreflightAddonOrigin::Optional,
        &optional_client_side,
        &mut extras,
    );
    let external_client_side = effective
        .external_addon_client_side
        .iter()
        .map(|path| normalize_client_side_path_key(path))
        .collect::<HashSet<_>>();
    for (addon_name, enabled, path) in &effective.external_addons {
        if *enabled
            && external_addon_path_available(addon_name, path)
            && !external_client_side.contains(&normalize_client_side_path_key(path))
            && !external_addon_is_repo_defined_client_side(
                addon_name,
                path,
                configured_repositories,
            )
            && !external_addon_matches_any_requirement(
                addon_name,
                path,
                configured_repositories,
                requirements,
                display_names,
            )
        {
            extras.push(JoinPreflightAddonSuggestion {
                addon_name: addon_name.clone(),
                origin: JoinPreflightAddonOrigin::External,
                reported_name: addon_name.clone(),
                confidence: JoinPreflightMatchConfidence::ExactNormalizedName,
                selected: true,
            });
        }
    }

    extras.sort_by(|left, right| {
        origin_sort_key(&left.origin)
            .cmp(&origin_sort_key(&right.origin))
            .then_with(|| left.addon_name.cmp(&right.addon_name))
    });
    extras.dedup_by(|left, right| {
        left.origin == right.origin && left.addon_name.eq_ignore_ascii_case(&right.addon_name)
    });
    extras
}

/// Enabled external addons whose configured path cannot be resolved on disk.
///
/// The launcher (`resolve_launch_mod_paths`) silently skips these with only an
/// error log the user never sees, so the preflight surfaces them as a warning.
/// The availability check is the same one the launcher resolution relies on, so
/// an addon flagged here is exactly an addon that would not be loaded.
///
/// Server-required addons are deliberately excluded: those are already routed
/// through the suggestion/known-remote flow (e.g. offered from another repo),
/// so reporting them here too would double-list the same addon.
fn collect_unavailable_enabled_external_addons(
    effective: &Repository,
    configured_repositories: &[Repository],
    requirements: &HashMap<String, &ServerAddonRequirement>,
    display_names: &AddonDisplayNameSnapshot,
) -> Vec<JoinPreflightUnavailableAddon> {
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    for (addon_name, enabled, path) in &effective.external_addons {
        if !*enabled || external_addon_path_available(addon_name, path) {
            continue;
        }
        if external_addon_matches_any_requirement(
            addon_name,
            path,
            configured_repositories,
            requirements,
            display_names,
        ) {
            continue;
        }
        if !seen.insert((
            addon_name.to_ascii_lowercase(),
            normalize_client_side_path_key(path),
        )) {
            continue;
        }
        rows.push(JoinPreflightUnavailableAddon {
            addon_name: addon_name.clone(),
            path: path.clone(),
        });
    }
    rows.sort_by(|left, right| left.addon_name.cmp(&right.addon_name));
    rows
}

fn collect_extra_enabled_addon_candidates(
    addon_states: &[(String, bool)],
    display_names: Option<&HashMap<String, String>>,
    requirements: &HashMap<String, &ServerAddonRequirement>,
    origin: JoinPreflightAddonOrigin,
    client_side: &HashSet<String>,
    extras: &mut Vec<JoinPreflightAddonSuggestion>,
) {
    for (addon_name, enabled) in addon_states {
        let display_name = display_names.and_then(|names| names.get(addon_name));
        if *enabled
            && !client_side.contains(&normalize_addon_name(addon_name))
            && !addon_matches_any_requirement(addon_name, display_name, requirements)
        {
            extras.push(JoinPreflightAddonSuggestion {
                addon_name: addon_name.clone(),
                origin: origin.clone(),
                reported_name: display_name.cloned().unwrap_or_else(|| addon_name.clone()),
                confidence: JoinPreflightMatchConfidence::ExactNormalizedName,
                selected: true,
            });
        }
    }
}

fn normalize_client_side_path_key(path: &str) -> String {
    crate::core::utils::content_hash::normalize_path(path.trim())
}

fn external_addon_is_repo_defined_client_side(
    addon_name: &str,
    addon_path: &str,
    configured_repositories: &[Repository],
) -> bool {
    let addon_name = normalize_addon_name(addon_name);
    if addon_name.is_empty() {
        return false;
    }
    let addon_path = normalize_client_side_path_key(addon_path);
    configured_repositories.iter().any(|repo| {
        let repo_path = normalize_client_side_path_key(&repo.path);
        if repo_path.is_empty() || !path_is_within_root(&addon_path, &repo_path) {
            return false;
        }
        repo.remote_client_side_addons
            .iter()
            .any(|name| normalize_addon_name(name) == addon_name)
    })
}

fn path_is_within_root(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn addon_matches_any_requirement(
    addon_name: &str,
    display_name: Option<&String>,
    requirements: &HashMap<String, &ServerAddonRequirement>,
) -> bool {
    addon_match_keys(addon_name, display_name)
        .iter()
        .any(|key| requirements.contains_key(key))
}

fn collect_disabled_addon_candidates(
    addon_states: &[(String, bool)],
    display_names: Option<&HashMap<String, String>>,
    requirements: &HashMap<String, &ServerAddonRequirement>,
    origin: JoinPreflightAddonOrigin,
    candidates: &mut HashMap<String, Vec<JoinPreflightAddonSuggestion>>,
) {
    for (addon_name, enabled) in addon_states {
        if !*enabled {
            let display_name = display_names.and_then(|names| names.get(addon_name));
            for normalized in addon_match_keys(addon_name, display_name) {
                if let Some(requirement) = requirements.get(&normalized) {
                    candidates
                        .entry(normalized)
                        .or_default()
                        .push(JoinPreflightAddonSuggestion {
                            addon_name: addon_name.clone(),
                            origin: origin.clone(),
                            reported_name: requirement.display_name.clone(),
                            confidence: JoinPreflightMatchConfidence::ExactNormalizedName,
                            selected: true,
                        });
                    break;
                }
            }
        }
    }
}

fn enabled_requirement_satisfied(
    effective: &Repository,
    configured_repositories: &[Repository],
    normalized_requirement: &str,
    display_names: &AddonDisplayNameSnapshot,
) -> bool {
    let repo_display_names = addon_display_names_for_repo(display_names, effective);
    effective
        .addons
        .iter()
        .chain(effective.optional_addons.iter())
        .any(|(name, enabled)| {
            let display_name = repo_display_names.and_then(|names| names.get(name));
            *enabled
                && repository_addon_path_available(effective, name)
                && addon_match_keys(name, display_name)
                    .iter()
                    .any(|key| key == normalized_requirement)
        })
        || effective
            .external_addons
            .iter()
            .any(|(name, enabled, path)| {
                *enabled
                    && external_addon_path_available(name, path)
                    && external_addon_match_keys(
                        name,
                        Some(path),
                        configured_repositories,
                        display_names,
                    )
                    .iter()
                    .any(|key| key == normalized_requirement)
            })
}

fn repository_addon_path_available(repository: &Repository, addon_name: &str) -> bool {
    let repo_path = repository.path.trim();
    if repo_path.is_empty() {
        return true;
    }

    let root_path = Path::new(repo_path);
    if !root_path.exists() {
        return true;
    }

    resolve_child_dir_case_insensitive(root_path, addon_name).is_some()
}

fn source_repository_addon_path_available(repository: &Repository, addon_name: &str) -> bool {
    let repo_path = repository.path.trim();
    if repo_path.is_empty() {
        return false;
    }

    resolve_child_dir_case_insensitive(Path::new(repo_path), addon_name).is_some()
}

fn external_addon_path_available(addon_name: &str, path: &str) -> bool {
    resolve_external_addon_path(addon_name, path).is_some()
}

fn resolve_external_addon_path(addon_name: &str, path: &str) -> Option<std::path::PathBuf> {
    let trimmed_path = path.trim();
    if trimmed_path.is_empty() {
        return None;
    }

    let base_path = Path::new(trimmed_path);
    if let Some(nested_path) = resolve_child_dir_case_insensitive(base_path, addon_name) {
        return Some(nested_path);
    }

    if base_path.is_dir() {
        if workshop_id_from_external_path(trimmed_path).is_some() {
            return Some(base_path.to_path_buf());
        }
        let base_name = base_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if base_name.trim_start().starts_with('@') {
            return Some(base_path.to_path_buf());
        }
        let base_name = normalize_addon_name(base_name);
        let addon_key = normalize_addon_name(addon_name);
        if !base_name.is_empty() && base_name == addon_key {
            return Some(base_path.to_path_buf());
        }
    }

    None
}

fn addon_match_keys(name: &str, display_name: Option<&String>) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(display_name) = display_name {
        push_normalized_addon_keys(&mut keys, display_name);
        if !keys.is_empty() {
            return keys;
        }
    }
    push_normalized_addon_keys(&mut keys, name);
    keys
}

fn external_addon_matches_any_requirement(
    addon_name: &str,
    path: &str,
    configured_repositories: &[Repository],
    requirements: &HashMap<String, &ServerAddonRequirement>,
    display_names: &AddonDisplayNameSnapshot,
) -> bool {
    external_addon_match_keys(
        addon_name,
        Some(path),
        configured_repositories,
        display_names,
    )
    .iter()
    .any(|key| requirements.contains_key(key))
}

fn external_addon_match_keys(
    name: &str,
    path: Option<&str>,
    configured_repositories: &[Repository],
    display_names: &AddonDisplayNameSnapshot,
) -> Vec<String> {
    let mut keys = addon_match_keys(name, None);
    if let Some(path) = path
        && let Some(path_name) = external_addon_path_name(path)
    {
        push_unique_key(&mut keys, normalize_addon_name(path_name));
    }
    push_external_repo_defined_match_keys(
        &mut keys,
        name,
        path,
        configured_repositories,
        display_names,
    );
    if let Some(workshop_id) = path.and_then(workshop_id_from_external_path) {
        push_unique_key(&mut keys, workshop_match_key(&workshop_id));
    }
    keys
}

fn push_external_repo_defined_match_keys(
    keys: &mut Vec<String>,
    external_name: &str,
    external_path: Option<&str>,
    configured_repositories: &[Repository],
    display_names: &AddonDisplayNameSnapshot,
) {
    let Some(external_path) = external_path else {
        return;
    };
    let external_path_key = normalize_client_side_path_key(external_path);
    if external_path_key.is_empty() {
        return;
    }

    let external_name_key = normalize_addon_name(external_name);
    let external_folder_key = external_addon_path_name(external_path)
        .map(normalize_addon_name)
        .unwrap_or_default();

    for repo in configured_repositories {
        let repo_path_key = normalize_client_side_path_key(&repo.path);
        if repo_path_key.is_empty() || !path_is_within_root(&external_path_key, &repo_path_key) {
            continue;
        }

        let repo_display_names = addon_display_names_for_repo(display_names, repo);
        for (repo_addon_name, _) in repo.addons.iter().chain(repo.optional_addons.iter()) {
            let repo_addon_key = normalize_addon_name(repo_addon_name);
            let repo_display_name = repo_display_names.and_then(|names| names.get(repo_addon_name));
            let repo_display_key = repo_display_name
                .map(|name| normalize_addon_name(name))
                .unwrap_or_default();

            if external_name_key == repo_addon_key
                || external_folder_key == repo_addon_key
                || (!repo_display_key.is_empty() && external_name_key == repo_display_key)
            {
                for key in addon_match_keys(repo_addon_name, repo_display_name) {
                    push_unique_key(keys, key);
                }
            }
        }
    }
}

fn external_addon_path_name(path: &str) -> Option<&str> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }
    Path::new(trimmed)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
}

fn addon_display_names_for_repo<'a>(
    display_names: &'a AddonDisplayNameSnapshot,
    repository: &Repository,
) -> Option<&'a HashMap<String, String>> {
    display_names.get(&Foxy::normalize_repo_url(&repository.address))
}

fn addon_display_name<'a>(
    display_names: &'a AddonDisplayNameSnapshot,
    repo_url: &str,
    addon_name: &str,
) -> Option<&'a String> {
    display_names
        .get(repo_url)
        .and_then(|names| names.get(addon_name))
}

fn requirement_match_keys(name: &str) -> Vec<String> {
    let mut keys = Vec::new();
    push_normalized_addon_keys(&mut keys, name);
    push_requirement_suffix_stripped_keys(&mut keys, name);
    for acronym in addon_acronym_keys(name) {
        push_unique_key(&mut keys, acronym);
    }
    keys
}

fn requirement_match_keys_for_requirement(requirement: &ServerAddonRequirement) -> Vec<String> {
    let mut keys = requirement_match_keys(&requirement.display_name);
    for workshop_id in &requirement.workshop_ids {
        push_unique_key(&mut keys, workshop_match_key(workshop_id));
    }
    keys
}

fn workshop_match_key(workshop_id: &str) -> String {
    format!("workshop:{}", workshop_id.trim())
}

fn workshop_id_from_external_path(path: &str) -> Option<String> {
    let normalized = path.trim().replace('\\', "/");
    let parts = normalized
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    for window in parts.windows(4) {
        if window[0].eq_ignore_ascii_case("workshop")
            && window[1].eq_ignore_ascii_case("content")
            && window[2] == "107410"
            && window[3].chars().all(|ch| ch.is_ascii_digit())
        {
            return Some(window[3].to_string());
        }
    }

    for pair in parts.windows(2) {
        if pair[0] == "107410" && pair[1].chars().all(|ch| ch.is_ascii_digit()) {
            return Some(pair[1].to_string());
        }
    }

    None
}

fn push_unique_key(keys: &mut Vec<String>, key: String) {
    if !key.is_empty() && !keys.iter().any(|existing| existing == &key) {
        keys.push(key);
    }
}

fn push_normalized_addon_keys(keys: &mut Vec<String>, name: &str) {
    let normalized = normalize_addon_name(name);
    push_unique_key(keys, normalized.clone());

    let compact = normalized.replace('_', "");
    if compact != normalized {
        push_unique_key(keys, compact);
    }

    let token_sorted = sorted_addon_token_key(&normalized);
    if token_sorted != normalized {
        push_unique_key(keys, token_sorted);
    }
}

fn sorted_addon_token_key(normalized: &str) -> String {
    let mut tokens = normalized
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if tokens.len() < 2 {
        return normalized.to_string();
    }
    tokens.sort_unstable();
    tokens.join("_")
}

fn push_requirement_suffix_stripped_keys(keys: &mut Vec<String>, name: &str) {
    let normalized = normalize_addon_name(name);
    let parts = normalized
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.len() < 2 {
        return;
    }

    let suffixes = ["support", "compat", "compatibility"];
    if parts.last().is_some_and(|last| suffixes.contains(last)) {
        let stripped = parts[..parts.len() - 1].join("_");
        push_normalized_addon_keys(keys, &stripped);
    }
}

fn addon_acronym_keys(name: &str) -> Vec<String> {
    let words = name
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    let letters = words
        .iter()
        .filter(|word| word.chars().next().is_some_and(char::is_alphabetic))
        .filter_map(|word| word.chars().next())
        .collect::<String>();
    if letters.len() < 2 {
        return Vec::new();
    }

    let base_key = letters.to_ascii_lowercase();
    let mut keys = Vec::new();
    if let Some(version_major) = words
        .iter()
        .find(|word| word.chars().all(|ch| ch.is_ascii_digit()))
    {
        let mut key = base_key.clone();
        key.push_str(version_major);
        push_unique_key(&mut keys, key);
    }
    push_unique_key(&mut keys, base_key);
    keys
}

fn enable_suggestion(repository: &mut Repository, suggestion: &JoinPreflightAddonSuggestion) {
    match suggestion.origin {
        JoinPreflightAddonOrigin::Required => {
            set_addon_enabled(&mut repository.addons, &suggestion.addon_name);
        }
        JoinPreflightAddonOrigin::Optional => {
            set_addon_enabled(&mut repository.optional_addons, &suggestion.addon_name);
        }
        JoinPreflightAddonOrigin::External => {
            for (name, enabled, _) in &mut repository.external_addons {
                if name.eq_ignore_ascii_case(&suggestion.addon_name) {
                    *enabled = true;
                }
            }
        }
    }
}

fn apply_extra_enabled_disables(repository: &mut Repository, pending: &PendingJoinPreflightState) {
    // `selected` means "keep this addon loaded". Anything the user unticked is
    // stripped for the launch; ticked extras are left enabled.
    for extra in &pending.extra_enabled {
        if !extra.selected {
            disable_suggestion(repository, extra);
        }
    }
}

fn disable_suggestion(repository: &mut Repository, suggestion: &JoinPreflightAddonSuggestion) {
    match suggestion.origin {
        JoinPreflightAddonOrigin::Required => {
            set_addon_disabled(&mut repository.addons, &suggestion.addon_name);
        }
        JoinPreflightAddonOrigin::Optional => {
            set_addon_disabled(&mut repository.optional_addons, &suggestion.addon_name);
        }
        JoinPreflightAddonOrigin::External => {
            for (name, enabled, _) in &mut repository.external_addons {
                if name.eq_ignore_ascii_case(&suggestion.addon_name) {
                    *enabled = false;
                }
            }
        }
    }
}

fn enable_known_remote_addon(repository: &mut Repository, remote: &JoinPreflightKnownRemoteAddon) {
    for (name, enabled, path) in &mut repository.external_addons {
        if name.eq_ignore_ascii_case(&remote.addon_name)
            && normalize_client_side_path_key(path)
                == normalize_client_side_path_key(&remote.repository_path)
        {
            *enabled = true;
            return;
        }
    }

    repository.external_addons.push((
        remote.addon_name.clone(),
        true,
        remote.repository_path.clone(),
    ));
}

fn set_addon_enabled(addons: &mut [(String, bool)], addon_name: &str) {
    for (name, enabled) in addons {
        if name.eq_ignore_ascii_case(addon_name) {
            *enabled = true;
        }
    }
}

fn set_addon_disabled(addons: &mut [(String, bool)], addon_name: &str) {
    for (name, enabled) in addons {
        if name.eq_ignore_ascii_case(addon_name) {
            *enabled = false;
        }
    }
}

fn normalize_addon_name(name: &str) -> String {
    name.trim()
        .trim_matches('"')
        .trim_matches('\'')
        .chars()
        .filter_map(|ch| {
            if ch.is_whitespace() || matches!(ch, '-' | '_' | '.') {
                Some('_')
            } else if ch == '@' {
                None
            } else if ch.is_ascii() {
                Some(ch.to_ascii_lowercase())
            } else {
                ch.to_lowercase().next()
            }
        })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

fn origin_sort_key(origin: &JoinPreflightAddonOrigin) -> u8 {
    match origin {
        JoinPreflightAddonOrigin::Required => 0,
        JoinPreflightAddonOrigin::Optional => 1,
        JoinPreflightAddonOrigin::External => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requirement(name: &str) -> ServerAddonRequirement {
        ServerAddonRequirement {
            display_name: name.to_string(),
            required: true,
            raw_identity: Some(name.to_string()),
            workshop_ids: Vec::new(),
        }
    }

    fn workshop_requirement(name: &str, workshop_id: &str) -> ServerAddonRequirement {
        ServerAddonRequirement {
            display_name: name.to_string(),
            required: true,
            raw_identity: Some(name.to_string()),
            workshop_ids: vec![workshop_id.to_string()],
        }
    }

    fn create_temp_addon_dir(addon_name: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(dir.path().join(addon_name)).expect("addon dir");
        dir
    }

    fn create_temp_workshop_addon_dir(
        workshop_id: &str,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let addon_dir = dir
            .path()
            .join("steamapps")
            .join("workshop")
            .join("content")
            .join("107410")
            .join(workshop_id);
        std::fs::create_dir_all(&addon_dir).expect("workshop addon dir");
        (dir, addon_dir)
    }

    #[test]
    fn preflight_enables_disabled_optional_addon_for_temporary_repository() {
        let repo = Repository {
            optional_addons: vec![("@ace".to_string(), false)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[],
            &server,
            "Repo",
            &[requirement("@ACE")],
            &AddonDisplayNameSnapshot::new(),
        )
        .expect("disabled matching addon should create preflight");

        assert_eq!(state.suggestions.len(), 1);
        assert!(state.suggestions[0].selected);
        assert!(!state.original_repository.optional_addons[0].1);
        assert!(Foxy::repository_with_join_preflight_selections(&state).optional_addons[0].1);
    }

    #[test]
    fn preflight_matches_display_name_acronym_to_disabled_at_folder() {
        let repo = Repository {
            optional_addons: vec![("@ace3".to_string(), false)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };
        let requirements = vec![requirement("Advanced Combat Environment 3.21.0")];

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[],
            &server,
            "Repo",
            &requirements,
            &AddonDisplayNameSnapshot::new(),
        )
        .expect("disabled @ace3 should match ACE display name");

        assert_eq!(state.suggestions.len(), 1);
        assert_eq!(state.suggestions[0].addon_name, "@ace3");
        assert!(state.suggestions[0].selected);
        assert!(Foxy::repository_with_join_preflight_selections(&state).optional_addons[0].1);
    }

    fn pending_with_single_extra_enabled(keep_loaded: bool) -> PendingJoinPreflightState {
        PendingJoinPreflightState {
            repo_name: "Repo".to_string(),
            server: RepositoryServer {
                name: "Main".to_string(),
                address: "127.0.0.1".to_string(),
                port: "2302".to_string(),
                password: String::new(),
                battle_eye: false,
            },
            original_repository: Repository {
                external_addons: vec![(
                    "@bagigi_restrict_markers".to_string(),
                    true,
                    "C:\\mods\\@bagigi_restrict_markers".to_string(),
                )],
                ..Repository::default()
            },
            suggestions: Vec::new(),
            ambiguous: Vec::new(),
            known_remote: Vec::new(),
            extra_enabled: vec![JoinPreflightAddonSuggestion {
                addon_name: "@bagigi_restrict_markers".to_string(),
                origin: JoinPreflightAddonOrigin::External,
                reported_name: "@bagigi_restrict_markers".to_string(),
                confidence: JoinPreflightMatchConfidence::ExactNormalizedName,
                selected: keep_loaded,
            }],
            unavailable_enabled: Vec::new(),
            ts3_required: false,
            ts3_running: false,
            steam_required: false,
            steam_running: false,
            launch_only: false,
        }
    }

    #[test]
    fn launch_without_suggestions_strips_extra_enabled_regardless_of_tick() {
        for keep_loaded in [true, false] {
            let pending = pending_with_single_extra_enabled(keep_loaded);
            let launch_repository = Foxy::repository_without_join_preflight_suggestions(&pending);
            assert!(
                !launch_repository.external_addons[0].1,
                "extra addon must be stripped on the launch-without path (keep_loaded={keep_loaded})"
            );
            // The original repository must remain untouched.
            assert!(pending.original_repository.external_addons[0].1);
        }
    }

    #[test]
    fn launch_with_selected_keeps_ticked_extra_and_strips_unticked_extra() {
        // Ticked extra ("keep loaded") stays enabled.
        let kept = pending_with_single_extra_enabled(true);
        let kept_launch = Foxy::repository_with_join_preflight_selections(&kept);
        assert!(kept_launch.external_addons[0].1);

        // Unticked extra is stripped on the launch-with-selected path.
        let stripped = pending_with_single_extra_enabled(false);
        let stripped_launch = Foxy::repository_with_join_preflight_selections(&stripped);
        assert!(!stripped_launch.external_addons[0].1);
    }

    #[test]
    fn unchecked_local_preflight_suggestion_is_not_enabled_for_launch() {
        let repo = Repository {
            optional_addons: vec![("@ace3".to_string(), false)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let mut state = Foxy::build_join_preflight_state(
            &repo,
            &[],
            &server,
            "Repo",
            &[requirement("Advanced Combat Environment 3.21.0")],
            &AddonDisplayNameSnapshot::new(),
        )
        .expect("disabled addon should create selectable preflight suggestion");
        state.suggestions[0].selected = false;

        let launch_repository = Foxy::repository_with_join_preflight_selections(&state);

        assert!(!launch_repository.optional_addons[0].1);
    }

    #[test]
    fn preflight_prefers_db_display_name_over_addon_folder_name() {
        let repo = Repository {
            address: "https://example.invalid/repo".to_string(),
            optional_addons: vec![("@ace3".to_string(), false)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };
        let mut display_names = AddonDisplayNameSnapshot::new();
        display_names.insert(
            "https://example.invalid/repo/".to_string(),
            HashMap::from([(
                "@ace3".to_string(),
                "Advanced Combat Environment 3.21.0".to_string(),
            )]),
        );

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[],
            &server,
            "Repo",
            &[requirement("Advanced Combat Environment 3.21.0")],
            &display_names,
        )
        .expect("display-name match should create preflight");

        assert_eq!(state.suggestions.len(), 1);
        assert_eq!(state.suggestions[0].addon_name, "@ace3");
    }

    #[test]
    fn preflight_ignores_already_enabled_addon() {
        let repo = Repository {
            addons: vec![("@cba_a3".to_string(), true)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[],
            &server,
            "Repo",
            &[requirement("@cba_a3")],
            &AddonDisplayNameSnapshot::new(),
        );

        assert!(state.is_none());
    }

    #[test]
    fn preflight_does_not_satisfy_enabled_repo_addon_when_folder_is_missing() {
        let repo_dir = tempfile::tempdir().expect("temp dir");
        let repo = Repository {
            name: "Current".to_string(),
            address: "http://example.com/current".to_string(),
            path: repo_dir.path().to_string_lossy().to_string(),
            optional_addons: vec![("@burnem_redux".to_string(), true)],
            ..Repository::default()
        };
        let source_repo = Repository {
            name: "Source".to_string(),
            address: "http://example.com/source".to_string(),
            path: "C:\\Source".to_string(),
            optional_addons: vec![("@burnem_redux".to_string(), true)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[repo.clone(), source_repo],
            &server,
            "Repo",
            &[requirement("Burn Em Redux")],
            &AddonDisplayNameSnapshot::new(),
        )
        .expect("missing enabled addon should create known-remote preflight");

        assert!(state.suggestions.is_empty());
        assert_eq!(state.known_remote.len(), 1);
        assert_eq!(state.known_remote[0].addon_name, "@burnem_redux");
        assert!(!state.known_remote[0].available);
        assert!(state.known_remote[0].selected);
    }

    #[test]
    fn preflight_keeps_duplicate_disabled_matches_ambiguous() {
        let repo = Repository {
            addons: vec![("@ace".to_string(), false)],
            optional_addons: vec![("@ACE".to_string(), false)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[],
            &server,
            "Repo",
            &[requirement("@ace")],
            &AddonDisplayNameSnapshot::new(),
        )
        .expect("ambiguous matching addons should create preflight");

        assert!(state.suggestions.is_empty());
        assert_eq!(state.ambiguous.len(), 1);
        assert_eq!(state.ambiguous[0].candidates.len(), 2);
        assert_eq!(state.ambiguous[0].selected_candidate, None);
        assert!(!state.original_repository.addons[0].1);
        assert!(!state.original_repository.optional_addons[0].1);
    }

    #[test]
    fn selected_ambiguous_candidate_is_enabled_only_in_launch_repository() {
        let repo = Repository {
            addons: vec![("@ace".to_string(), false)],
            optional_addons: vec![("@ACE".to_string(), false)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let mut state = Foxy::build_join_preflight_state(
            &repo,
            &[],
            &server,
            "Repo",
            &[requirement("@ace")],
            &AddonDisplayNameSnapshot::new(),
        )
        .expect("ambiguous matching addons should create preflight");
        state.ambiguous[0].selected_candidate = Some(1);

        let launch_repository = Foxy::repository_with_join_preflight_selections(&state);

        assert!(!state.original_repository.addons[0].1);
        assert!(!state.original_repository.optional_addons[0].1);
        assert!(!launch_repository.addons[0].1);
        assert!(launch_repository.optional_addons[0].1);
    }

    #[test]
    fn preflight_finds_missing_addon_in_other_configured_repository() {
        let repo = Repository {
            name: "Current".to_string(),
            address: "http://example.com/current".to_string(),
            ..Repository::default()
        };
        let source_repo = Repository {
            name: "Source".to_string(),
            address: "http://example.com/source".to_string(),
            path: "C:\\Source".to_string(),
            addons: vec![("@ace".to_string(), true)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[repo.clone(), source_repo],
            &server,
            "Repo",
            &[requirement("@ACE")],
            &AddonDisplayNameSnapshot::new(),
        )
        .expect("known remote addon should create preflight");

        assert!(state.suggestions.is_empty());
        assert_eq!(state.known_remote.len(), 1);
        assert_eq!(state.known_remote[0].repository_name, "Source");
        assert_eq!(state.known_remote[0].repository_path, "C:\\Source");
        assert!(!state.known_remote[0].available);
        assert!(state.known_remote[0].selected);

        let launch_repository = Foxy::repository_with_join_preflight_selections(&state);
        assert_eq!(
            launch_repository.external_addons,
            vec![("@ace".to_string(), true, "C:\\Source".to_string())]
        );
    }

    #[test]
    fn preflight_does_not_offer_remote_download_when_disabled_local_addon_is_actionable() {
        let repo = Repository {
            name: "Current".to_string(),
            address: "http://example.com/current".to_string(),
            optional_addons: vec![("@ace3".to_string(), false)],
            ..Repository::default()
        };
        let source_repo = Repository {
            name: "Source".to_string(),
            address: "http://example.com/source".to_string(),
            path: "C:\\Source".to_string(),
            addons: vec![("@ace3".to_string(), true)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };
        let mut display_names = AddonDisplayNameSnapshot::new();
        display_names.insert(
            "http://example.com/source/".to_string(),
            HashMap::from([(
                "@ace3".to_string(),
                "Advanced Combat Environment 3.21.0".to_string(),
            )]),
        );

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[repo.clone(), source_repo],
            &server,
            "Repo",
            &[requirement("Advanced Combat Environment 3.21.0")],
            &display_names,
        )
        .expect("disabled local addon should create preflight");

        assert_eq!(state.suggestions.len(), 1);
        assert_eq!(state.suggestions[0].addon_name, "@ace3");
        assert!(state.known_remote.is_empty());
    }

    #[test]
    fn preflight_matches_versioned_server_name_to_unversioned_local_acronym() {
        let repo = Repository {
            name: "Current".to_string(),
            address: "http://example.com/current".to_string(),
            optional_addons: vec![("@ace".to_string(), false)],
            ..Repository::default()
        };
        let source_repo = Repository {
            name: "Source".to_string(),
            address: "http://example.com/source".to_string(),
            path: "C:\\Source".to_string(),
            addons: vec![("@ace3".to_string(), true)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };
        let mut display_names = AddonDisplayNameSnapshot::new();
        display_names.insert(
            "http://example.com/source/".to_string(),
            HashMap::from([(
                "@ace3".to_string(),
                "Advanced Combat Environment 3.21.0".to_string(),
            )]),
        );

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[repo.clone(), source_repo],
            &server,
            "Repo",
            &[requirement("Advanced Combat Environment 3.21.0")],
            &display_names,
        )
        .expect("disabled local acronym match should create preflight");

        assert_eq!(state.suggestions.len(), 1);
        assert_eq!(state.suggestions[0].addon_name, "@ace");
        assert!(state.known_remote.is_empty());
    }

    #[test]
    fn preflight_finds_known_remote_addon_with_compact_folder_name_match() {
        let repo = Repository {
            name: "Current".to_string(),
            address: "http://example.com/current".to_string(),
            ..Repository::default()
        };
        let source_repo = Repository {
            name: "Source".to_string(),
            address: "http://example.com/source".to_string(),
            path: "C:\\Source".to_string(),
            optional_addons: vec![("@burnem_redux".to_string(), true)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[repo.clone(), source_repo],
            &server,
            "Repo",
            &[requirement("Burn Em Redux")],
            &AddonDisplayNameSnapshot::new(),
        )
        .expect("known remote addon should match compact folder name");

        assert!(state.suggestions.is_empty());
        assert_eq!(state.known_remote.len(), 1);
        assert_eq!(state.known_remote[0].addon_name, "@burnem_redux");
        assert!(!state.known_remote[0].available);
        assert!(state.known_remote[0].selected);
    }

    #[test]
    fn preflight_finds_known_remote_addon_with_server_support_suffix() {
        let repo = Repository {
            name: "Current".to_string(),
            address: "http://example.com/current".to_string(),
            ..Repository::default()
        };
        let source_repo = Repository {
            name: "Source".to_string(),
            address: "http://example.com/source".to_string(),
            path: "C:\\Source".to_string(),
            optional_addons: vec![("@burnem_tiow".to_string(), true)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[repo.clone(), source_repo],
            &server,
            "Repo",
            &[requirement("Burn Em TIOW SUPPORT")],
            &AddonDisplayNameSnapshot::new(),
        )
        .expect("known remote addon should match server support suffix");

        assert!(state.suggestions.is_empty());
        assert_eq!(state.known_remote.len(), 1);
        assert_eq!(state.known_remote[0].addon_name, "@burnem_tiow");
        assert!(!state.known_remote[0].available);
        assert!(state.known_remote[0].selected);
    }

    #[test]
    fn preflight_marks_downloaded_known_remote_addon_available_for_launch() {
        let source_dir = create_temp_addon_dir("@burnem_tiow");
        let repo = Repository {
            name: "Current".to_string(),
            address: "http://example.com/current".to_string(),
            ..Repository::default()
        };
        let source_repo = Repository {
            name: "Source".to_string(),
            address: "http://example.com/source".to_string(),
            path: source_dir.path().to_string_lossy().to_string(),
            optional_addons: vec![("@burnem_tiow".to_string(), true)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[repo.clone(), source_repo],
            &server,
            "Repo",
            &[requirement("Burn Em TIOW SUPPORT")],
            &AddonDisplayNameSnapshot::new(),
        )
        .expect("downloaded known remote addon should create launch preflight");

        assert!(state.suggestions.is_empty());
        assert_eq!(state.known_remote.len(), 1);
        assert_eq!(state.known_remote[0].addon_name, "@burnem_tiow");
        assert!(state.known_remote[0].available);
        assert!(state.known_remote[0].selected);

        let launch_repository = Foxy::repository_with_join_preflight_selections(&state);
        assert_eq!(
            launch_repository.external_addons,
            vec![(
                "@burnem_tiow".to_string(),
                true,
                source_dir.path().to_string_lossy().to_string()
            )]
        );
    }

    #[test]
    fn preflight_treats_enabled_external_acronym_match_as_satisfied() {
        let addon_dir = create_temp_addon_dir("@ace");
        let addon_path = addon_dir.path().join("@ace");
        let repo = Repository {
            name: "Current".to_string(),
            address: "http://example.com/current".to_string(),
            external_addons: vec![(
                "@ace".to_string(),
                true,
                addon_path.to_string_lossy().to_string(),
            )],
            ..Repository::default()
        };
        let source_repo = Repository {
            name: "Source".to_string(),
            address: "http://example.com/source".to_string(),
            path: "C:\\Source".to_string(),
            addons: vec![("@ace3".to_string(), true)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };
        let mut display_names = AddonDisplayNameSnapshot::new();
        display_names.insert(
            "http://example.com/source/".to_string(),
            HashMap::from([(
                "@ace3".to_string(),
                "Advanced Combat Environment 3.21.0".to_string(),
            )]),
        );

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[repo.clone(), source_repo],
            &server,
            "Repo",
            &[requirement("Advanced Combat Environment 3.21.0")],
            &display_names,
        );

        assert!(state.is_none());
    }

    #[test]
    fn preflight_treats_enabled_external_path_folder_match_as_satisfied() {
        let source_dir = tempfile::tempdir().expect("temp dir");
        let addon_path = source_dir.path().join("@burnem_redux");
        std::fs::create_dir(&addon_path).expect("addon dir");
        let repo = Repository {
            name: "Current".to_string(),
            address: "http://example.com/current".to_string(),
            external_addons: vec![(
                "Burn Em Redux".to_string(),
                true,
                addon_path.to_string_lossy().to_string(),
            )],
            ..Repository::default()
        };
        let source_repo = Repository {
            name: "Source".to_string(),
            address: "http://example.com/source".to_string(),
            path: source_dir.path().to_string_lossy().to_string(),
            addons: vec![("@burnem_redux".to_string(), true)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[repo.clone(), source_repo],
            &server,
            "Repo",
            &[requirement("@burnem_redux")],
            &AddonDisplayNameSnapshot::new(),
        );

        assert!(state.is_none());
    }

    #[test]
    fn preflight_treats_reordered_enabled_external_folder_tokens_as_satisfied() {
        let source_dir = tempfile::tempdir().expect("temp dir");
        let addon_path = source_dir.path().join("@bagigi_restrict_markers");
        std::fs::create_dir(&addon_path).expect("addon dir");
        let repo = Repository {
            name: "Current".to_string(),
            external_addons: vec![(
                "@bagigi_restrict_markers".to_string(),
                true,
                addon_path.to_string_lossy().to_string(),
            )],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[],
            &server,
            "Repo",
            &[requirement("Restrict Markers Bagigi")],
            &AddonDisplayNameSnapshot::new(),
        );

        assert!(state.is_none());
    }

    #[test]
    fn preflight_treats_enabled_external_workshop_id_match_as_satisfied() {
        let (_dir, addon_path) = create_temp_workshop_addon_dir("463939057");
        let repo = Repository {
            name: "Current".to_string(),
            external_addons: vec![(
                "Local Display Name".to_string(),
                true,
                addon_path.to_string_lossy().to_string(),
            )],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[],
            &server,
            "Repo",
            &[workshop_requirement("Server Only Name", "463939057")],
            &AddonDisplayNameSnapshot::new(),
        );

        assert!(state.is_none());
    }

    #[test]
    fn preflight_offers_disabled_external_workshop_id_match_as_local_enable() {
        let (_dir, addon_path) = create_temp_workshop_addon_dir("463939057");
        let repo = Repository {
            name: "Current".to_string(),
            external_addons: vec![(
                "Local Display Name".to_string(),
                false,
                addon_path.to_string_lossy().to_string(),
            )],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[],
            &server,
            "Repo",
            &[workshop_requirement("Server Only Name", "463939057")],
            &AddonDisplayNameSnapshot::new(),
        )
        .expect("disabled external workshop addon should create local enable preflight");

        assert_eq!(state.suggestions.len(), 1);
        assert_eq!(
            state.suggestions[0].origin,
            JoinPreflightAddonOrigin::External
        );
        assert!(state.extra_enabled.is_empty());

        let launch_repository = Foxy::repository_with_join_preflight_selections(&state);
        assert!(launch_repository.external_addons[0].1);
    }

    #[test]
    fn preflight_treats_enabled_external_repo_display_name_match_as_satisfied() {
        let source_dir = tempfile::tempdir().expect("temp dir");
        let addon_path = source_dir.path().join("@burnem_redux");
        std::fs::create_dir(&addon_path).expect("addon dir");
        let repo = Repository {
            name: "Current".to_string(),
            address: "http://example.com/current".to_string(),
            external_addons: vec![(
                "@burnem_redux".to_string(),
                true,
                addon_path.to_string_lossy().to_string(),
            )],
            ..Repository::default()
        };
        let source_repo = Repository {
            name: "Source".to_string(),
            address: "http://example.com/source".to_string(),
            path: source_dir.path().to_string_lossy().to_string(),
            addons: vec![("@burnem_redux".to_string(), true)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };
        let mut display_names = AddonDisplayNameSnapshot::new();
        display_names.insert(
            "http://example.com/source/".to_string(),
            HashMap::from([("@burnem_redux".to_string(), "Burn Em Redux".to_string())]),
        );

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[repo.clone(), source_repo],
            &server,
            "Repo",
            &[requirement("Burn Em Redux")],
            &display_names,
        );

        assert!(state.is_none());
    }

    #[test]
    fn preflight_offers_disabled_external_repo_display_name_match_as_local_enable() {
        let source_dir = tempfile::tempdir().expect("temp dir");
        let addon_path = source_dir.path().join("@burnem_redux");
        std::fs::create_dir(&addon_path).expect("addon dir");
        let repo = Repository {
            name: "Current".to_string(),
            address: "http://example.com/current".to_string(),
            external_addons: vec![(
                "@burnem_redux".to_string(),
                false,
                addon_path.to_string_lossy().to_string(),
            )],
            ..Repository::default()
        };
        let source_repo = Repository {
            name: "Source".to_string(),
            address: "http://example.com/source".to_string(),
            path: source_dir.path().to_string_lossy().to_string(),
            addons: vec![("@burnem_redux".to_string(), true)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };
        let mut display_names = AddonDisplayNameSnapshot::new();
        display_names.insert(
            "http://example.com/source/".to_string(),
            HashMap::from([("@burnem_redux".to_string(), "Burn Em Redux".to_string())]),
        );

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[repo.clone(), source_repo],
            &server,
            "Repo",
            &[requirement("Burn Em Redux")],
            &display_names,
        )
        .expect("disabled external addon should create local enable preflight");

        assert_eq!(state.suggestions.len(), 1);
        assert_eq!(
            state.suggestions[0].origin,
            JoinPreflightAddonOrigin::External
        );
        assert!(state.known_remote.is_empty());
        assert!(state.extra_enabled.is_empty());

        let launch_repository = Foxy::repository_with_join_preflight_selections(&state);
        assert!(launch_repository.external_addons[0].1);
    }

    #[test]
    fn preflight_flags_enabled_external_addon_not_reported_by_server() {
        let addon_dir = create_temp_addon_dir("@1mmers_better_inventory");
        let addon_path = addon_dir.path().join("@1mmers_better_inventory");
        let repo = Repository {
            name: "Current".to_string(),
            external_addons: vec![(
                "@1mmers_better_inventory".to_string(),
                true,
                addon_path.to_string_lossy().to_string(),
            )],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[],
            &server,
            "Repo",
            &[requirement("@cba_a3")],
            &AddonDisplayNameSnapshot::new(),
        )
        .expect("extra enabled external addon should create preflight");

        assert!(state.suggestions.is_empty());
        assert_eq!(state.extra_enabled.len(), 1);
        assert_eq!(
            state.extra_enabled[0].addon_name,
            "@1mmers_better_inventory"
        );
        // Extra enabled addons default to ticked ("keep loaded"), so the
        // launch-with-selected path leaves them enabled until the user unticks.
        assert!(state.extra_enabled[0].selected);

        let launch_repository = Foxy::repository_with_join_preflight_selections(&state);
        assert!(launch_repository.external_addons[0].1);
        assert!(state.original_repository.external_addons[0].1);

        // The launch-without path strips the extra regardless of tick state.
        let without_repository = Foxy::repository_without_join_preflight_suggestions(&state);
        assert!(!without_repository.external_addons[0].1);
    }

    #[test]
    fn preflight_ignores_missing_enabled_external_addon_and_offers_known_remote_download() {
        let deleted_source_dir = tempfile::tempdir().expect("temp dir");
        let repo = Repository {
            name: "Current".to_string(),
            address: "http://example.com/current".to_string(),
            external_addons: vec![(
                "@burnem_redux".to_string(),
                true,
                deleted_source_dir.path().to_string_lossy().to_string(),
            )],
            ..Repository::default()
        };
        let source_repo = Repository {
            name: "Source".to_string(),
            address: "http://example.com/source".to_string(),
            path: deleted_source_dir.path().to_string_lossy().to_string(),
            addons: vec![("@burnem_redux".to_string(), true)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };
        let mut display_names = AddonDisplayNameSnapshot::new();
        display_names.insert(
            "http://example.com/source/".to_string(),
            HashMap::from([("@burnem_redux".to_string(), "Burn Em Redux".to_string())]),
        );

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[repo.clone(), source_repo],
            &server,
            "Repo",
            &[requirement("Burn Em Redux")],
            &display_names,
        )
        .expect("missing enabled external addon should create known remote preflight");

        assert!(state.suggestions.is_empty());
        assert!(state.extra_enabled.is_empty());
        assert_eq!(state.known_remote.len(), 1);
        assert_eq!(state.known_remote[0].addon_name, "@burnem_redux");
        assert!(state.known_remote[0].selected);
    }

    #[test]
    fn preflight_ignores_client_side_external_addon_not_reported_by_server() {
        let dir = tempfile::tempdir().expect("temp dir");
        let addon_dir = dir.path().join("@1mmers_better_inventory");
        std::fs::create_dir(&addon_dir).expect("addon dir");
        let path = addon_dir.to_string_lossy().to_string();
        let repo = Repository {
            name: "Current".to_string(),
            external_addons: vec![("@1mmers_better_inventory".to_string(), true, path.clone())],
            external_addon_client_side: vec![path],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[],
            &server,
            "Repo",
            &[requirement("@cba_a3")],
            &AddonDisplayNameSnapshot::new(),
        );

        assert!(state.is_none());
    }

    #[test]
    fn preflight_reports_enabled_external_addon_with_missing_path() {
        let repo = Repository {
            name: "Current".to_string(),
            external_addons: vec![(
                "@Enhanced GPS".to_string(),
                true,
                "C:\\Steam\\steamapps\\workshop\\content\\107410\\2480263219".to_string(),
            )],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[],
            &server,
            "Repo",
            &[requirement("@cba_a3")],
            &AddonDisplayNameSnapshot::new(),
        )
        .expect("an enabled external addon with a missing path should open a warning preflight");

        assert!(state.suggestions.is_empty());
        assert!(state.ambiguous.is_empty());
        assert!(state.known_remote.is_empty());
        assert!(state.extra_enabled.is_empty());
        assert_eq!(state.unavailable_enabled.len(), 1);
        assert_eq!(state.unavailable_enabled[0].addon_name, "@Enhanced GPS");
    }

    #[test]
    fn preflight_ignores_disabled_external_addon_with_missing_path() {
        let repo = Repository {
            name: "Current".to_string(),
            external_addons: vec![(
                "@Enhanced GPS".to_string(),
                false,
                "C:\\Steam\\steamapps\\workshop\\content\\107410\\2480263219".to_string(),
            )],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[],
            &server,
            "Repo",
            &[requirement("@cba_a3")],
            &AddonDisplayNameSnapshot::new(),
        );

        assert!(state.is_none());
    }

    #[test]
    fn preflight_ignores_client_side_optional_addon_not_reported_by_server() {
        let repo = Repository {
            optional_addons: vec![("@soundmod".to_string(), true)],
            optional_addon_client_side: vec!["@soundmod".to_string()],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[],
            &server,
            "Repo",
            &[requirement("@cba_a3")],
            &AddonDisplayNameSnapshot::new(),
        );

        assert!(state.is_none());
    }

    #[test]
    fn unavailable_enabled_helper_reports_missing_and_skips_present_and_disabled() {
        let dir = tempfile::tempdir().expect("temp dir");
        let present = dir.path().join("@present_mod");
        std::fs::create_dir(&present).expect("addon dir");
        let repo = Repository {
            external_addons: vec![
                (
                    "@present_mod".to_string(),
                    true,
                    present.to_string_lossy().to_string(),
                ),
                (
                    "@missing_mod".to_string(),
                    true,
                    "C:\\nope\\@missing_mod".to_string(),
                ),
                (
                    "@disabled_missing".to_string(),
                    false,
                    "C:\\nope\\@disabled_missing".to_string(),
                ),
            ],
            ..Repository::default()
        };

        // No server requirements: this is the pure-launch / editor-launch path.
        let unavailable = Foxy::unavailable_enabled_external_addons(&repo, &[]);

        assert_eq!(unavailable.len(), 1);
        assert_eq!(unavailable[0].addon_name, "@missing_mod");
    }

    #[test]
    fn preflight_ignores_repository_defined_client_side_optional_addon_not_reported_by_server() {
        let repo = Repository {
            optional_addons: vec![("@soundmod".to_string(), true)],
            remote_client_side_addons: vec!["@soundmod".to_string()],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[],
            &server,
            "Repo",
            &[requirement("@cba_a3")],
            &AddonDisplayNameSnapshot::new(),
        );

        assert!(state.is_none());
    }

    #[test]
    fn preflight_ignores_repository_defined_client_side_external_addon_not_reported_by_server() {
        let repo_dir = tempfile::tempdir().expect("temp dir");
        let addon_dir = repo_dir.path().join("@1mmers_aaren_sound");
        std::fs::create_dir(&addon_dir).expect("addon dir");
        let external_path = addon_dir.to_string_lossy().to_string();
        let repo = Repository {
            name: "Current".to_string(),
            external_addons: vec![(
                "@1mmers_aaren_sound".to_string(),
                true,
                external_path.clone(),
            )],
            ..Repository::default()
        };
        let source_repo = Repository {
            name: "Immers".to_string(),
            path: repo_dir.path().to_string_lossy().to_string(),
            addons: vec![("@1mmers_aaren_sound".to_string(), true)],
            remote_client_side_addons: vec!["@1mmers_aaren_sound".to_string()],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[repo.clone(), source_repo],
            &server,
            "Repo",
            &[requirement("@cba_a3")],
            &AddonDisplayNameSnapshot::new(),
        );

        assert!(state.is_none());
    }

    #[test]
    fn unticked_extra_enabled_addon_is_stripped_for_launch() {
        let repo = Repository {
            optional_addons: vec![("@soundmod".to_string(), true)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };

        let mut state = Foxy::build_join_preflight_state(
            &repo,
            &[],
            &server,
            "Repo",
            &[requirement("@cba_a3")],
            &AddonDisplayNameSnapshot::new(),
        )
        .expect("extra enabled optional addon should create preflight");
        // Unticking the addon means "do not keep it loaded".
        state.extra_enabled[0].selected = false;

        let launch_repository = Foxy::repository_with_join_preflight_selections(&state);

        assert!(!launch_repository.optional_addons[0].1);
    }

    #[test]
    fn preflight_does_not_flag_enabled_optional_display_name_reported_by_server() {
        let repo = Repository {
            name: "Current".to_string(),
            address: "http://example.com/current".to_string(),
            optional_addons: vec![("@ace3".to_string(), true)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };
        let mut display_names = AddonDisplayNameSnapshot::new();
        display_names.insert(
            "http://example.com/current/".to_string(),
            HashMap::from([(
                "@ace3".to_string(),
                "Advanced Combat Environment 3.21.0".to_string(),
            )]),
        );

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[],
            &server,
            "Repo",
            &[requirement("Advanced Combat Environment 3.21.0")],
            &display_names,
        );

        assert!(state.is_none());
    }

    #[test]
    fn preflight_does_not_offer_remote_download_when_enabled_local_display_name_matches() {
        let repo = Repository {
            name: "Current".to_string(),
            address: "http://example.com/current".to_string(),
            addons: vec![("@ace3".to_string(), true)],
            ..Repository::default()
        };
        let source_repo = Repository {
            name: "Source".to_string(),
            address: "http://example.com/source".to_string(),
            path: "C:\\Source".to_string(),
            addons: vec![("@ace3".to_string(), true)],
            ..Repository::default()
        };
        let server = RepositoryServer {
            name: "Main".to_string(),
            address: "127.0.0.1".to_string(),
            port: "2302".to_string(),
            password: String::new(),
            battle_eye: false,
        };
        let mut display_names = AddonDisplayNameSnapshot::new();
        display_names.insert(
            "http://example.com/current/".to_string(),
            HashMap::from([(
                "@ace3".to_string(),
                "Advanced Combat Environment 3.21.0".to_string(),
            )]),
        );

        let state = Foxy::build_join_preflight_state(
            &repo,
            &[repo.clone(), source_repo],
            &server,
            "Repo",
            &[requirement("Advanced Combat Environment 3.21.0")],
            &display_names,
        );

        assert!(state.is_none());
    }
}
