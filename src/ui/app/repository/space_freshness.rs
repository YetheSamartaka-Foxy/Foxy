//! Refreshing repository spaces from their published manifest.
//!
//! A repository's own `repo.json` says whether that repository's payload moved.
//! It says nothing about the *space*: a server that adds, drops or re-flags a
//! repository inside `repository_space.json` changes what the space offers, and
//! no `repo.json` reports that.
//!
//! Every configured space is refreshed at launch, again every
//! [`REPOSITORY_SPACE_RECHECK_INTERVAL`] while Foxy runs, and on demand from the
//! space's toolbar and context menu. A refresh whose manifest checksum matches
//! the stored one leaves the space alone; otherwise it replaces the stored
//! manifest (entries, name, images, app update source) so new repositories show
//! up in the space. It never installs or removes a repository: that is a change
//! to the user's disk and stays their call.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use log::{info, warn};
use sha1::{Digest, Sha1};

use crate::ui::app::{FetchedRepositorySpace, Foxy, RepositorySpaceManifest};
use crate::ui::types::RepositorySpaceEntry;

/// How often a running Foxy rereads its spaces' manifests.
const REPOSITORY_SPACE_RECHECK_INTERVAL: Duration = Duration::from_secs(30 * 60);

/// SHA-1 (lowercase hex) of the compact JSON array
/// `[name, imageChecksum, iconChecksum, [[Name, Address, Requiered], ...]]`
/// over the manifest's values as published, entries in order. Matches the
/// `spaceChecksum` that `foxy-server-backend-cli create-space` writes; both
/// sides pin the same test vector.
pub fn repository_space_manifest_checksum(manifest: &RepositorySpaceManifest) -> String {
    let entries: Vec<serde_json::Value> = manifest
        .entries
        .iter()
        .map(|entry| serde_json::json!([entry.name, entry.address, entry.required]))
        .collect();
    let canonical = serde_json::json!([
        manifest.name,
        manifest.image_checksum,
        manifest.icon_checksum,
        entries
    ])
    .to_string();
    hex::encode(Sha1::digest(canonical.as_bytes()))
}

/// One space's published membership against the local copy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepositorySpaceRemoteDelta {
    /// Repositories the manifest publishes that this space does not list.
    pub added: Vec<String>,
    /// Repositories this space still lists that the manifest no longer has.
    pub removed: Vec<String>,
    /// Repositories whose required/optional flag differs from the manifest.
    pub required_changed: Vec<String>,
}

impl RepositorySpaceRemoteDelta {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.required_changed.is_empty()
    }

    pub fn total(&self) -> usize {
        self.added.len() + self.removed.len() + self.required_changed.len()
    }
}

/// A completed manifest fetch for one space.
pub struct RepositorySpaceFreshnessResult {
    pub space_id: String,
    pub space_name: String,
    /// The user asked for this refresh, so an unchanged or failed answer is
    /// reported instead of only logged.
    pub requested: bool,
    /// `Err` when the manifest could not be read. An unreachable manifest is
    /// unknown freshness, never "unchanged".
    pub outcome: Result<FetchedRepositorySpace, String>,
}

/// One repository as a space lists it, reduced to what a comparison needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceEntry {
    /// Normalized address; the identity.
    pub address: String,
    pub name: String,
    pub required: bool,
}

/// A space to fetch.
struct SpaceProbeTarget {
    id: String,
    name: String,
    source_address: String,
}

/// Compare a space against the entry list its manifest publishes.
///
/// Addresses are the identity, normalized the same way the add-repository path
/// normalizes them, so a trailing slash or a case difference is not a change.
/// Names are only used for the message.
pub fn repository_space_delta(
    local: &[SpaceEntry],
    remote: &[SpaceEntry],
) -> RepositorySpaceRemoteDelta {
    let index = |entries: &[SpaceEntry]| -> BTreeMap<String, (String, bool)> {
        entries
            .iter()
            .filter(|entry| !entry.address.is_empty())
            .map(|entry| {
                (
                    entry.address.to_ascii_lowercase(),
                    (entry.name.clone(), entry.required),
                )
            })
            .collect()
    };
    let local = index(local);
    let remote = index(remote);
    let label = |address: &str, name: &str| -> String {
        if name.trim().is_empty() {
            address.to_string()
        } else {
            name.trim().to_string()
        }
    };

    let mut delta = RepositorySpaceRemoteDelta::default();
    for (address, (name, required)) in &remote {
        match local.get(address) {
            None => delta.added.push(label(address, name)),
            Some((local_name, local_required)) if local_required != required => {
                delta.required_changed.push(label(address, local_name));
            }
            Some(_) => {}
        }
    }
    for (address, (name, _)) in &local {
        if !remote.contains_key(address) {
            delta.removed.push(label(address, name));
        }
    }
    delta
}

impl Foxy {
    fn space_entries(entries: &[RepositorySpaceEntry]) -> Vec<SpaceEntry> {
        entries
            .iter()
            .map(|entry| SpaceEntry {
                address: Self::normalize_repository_address_input(&entry.address),
                name: entry.name.clone(),
                required: entry.required,
            })
            .collect()
    }

    /// Refresh every configured space from its manifest. Runs once per launch,
    /// after the first frame, alongside the repository `repo.json` probes.
    pub(in crate::ui::app) fn start_repository_space_freshness_probe(&mut self) {
        self.start_repository_space_refresh(None);
    }

    /// Refresh one space from its manifest at the user's request.
    pub(crate) fn refresh_repository_space_from_server(&mut self, space_id: &str) {
        self.start_repository_space_refresh(Some(space_id));
    }

    /// Reread every space's manifest once the last full refresh is old enough,
    /// so a session left open still picks up a changed space.
    pub(in crate::ui::app) fn maybe_recheck_repository_spaces(&mut self) {
        if self
            .repository_space_last_refresh
            .is_some_and(|last| last.elapsed() < REPOSITORY_SPACE_RECHECK_INTERVAL)
        {
            return;
        }
        self.start_repository_space_refresh(None);
    }

    pub(crate) fn repository_space_refresh_in_flight(&self) -> bool {
        self.repository_space_freshness_rx.is_some()
    }

    fn start_repository_space_refresh(&mut self, only_space_id: Option<&str>) {
        if self.repository_space_refresh_in_flight() {
            return;
        }
        let requested = only_space_id.is_some();
        if !requested {
            self.repository_space_last_refresh = Some(Instant::now());
        }
        let spaces: Vec<SpaceProbeTarget> = self
            .repository_spaces
            .iter()
            .filter(|space| only_space_id.is_none_or(|id| space.id == id))
            .filter(|space| !space.source_address.trim().is_empty())
            .map(|space| SpaceProbeTarget {
                id: space.id.clone(),
                name: Self::repository_space_display_name(space).to_string(),
                source_address: space.source_address.trim().to_string(),
            })
            .collect();
        if spaces.is_empty() {
            return;
        }

        let (tx, rx) = std::sync::mpsc::channel::<RepositorySpaceFreshnessResult>();
        self.repository_space_freshness_rx = Some(rx);
        let repaint_ctx = self.repaint_ctx.clone();
        std::thread::spawn(move || {
            for target in spaces {
                let outcome = match Self::fetch_repository_space_manifest(&target.source_address) {
                    Ok(Some(fetched)) => Ok(fetched),
                    Ok(None) => {
                        Err("no repository space manifest at the configured address".into())
                    }
                    Err(err) => Err(err),
                };
                if tx
                    .send(RepositorySpaceFreshnessResult {
                        space_id: target.id,
                        space_name: target.name,
                        requested,
                        outcome,
                    })
                    .is_err()
                {
                    return;
                }
                Self::request_background_repaint(repaint_ctx.as_ref());
            }
        });
    }

    pub(in crate::ui::app) fn poll_repository_space_freshness_results(
        &mut self,
        ctx: &egui::Context,
    ) {
        let mut results = Vec::new();
        let mut finished = false;
        if let Some(rx) = self.repository_space_freshness_rx.as_ref() {
            loop {
                match rx.try_recv() {
                    Ok(result) => results.push(result),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        finished = true;
                        break;
                    }
                }
            }
        }
        if finished {
            self.repository_space_freshness_rx = None;
        }
        for result in results {
            self.apply_repository_space_freshness_result(result, ctx);
            self.needs_repaint = true;
        }
    }

    fn apply_repository_space_freshness_result(
        &mut self,
        result: RepositorySpaceFreshnessResult,
        ctx: &egui::Context,
    ) {
        let RepositorySpaceFreshnessResult {
            space_id,
            space_name,
            requested,
            outcome,
        } = result;
        let fetched = match outcome {
            Ok(fetched) => fetched,
            Err(err) => {
                warn!(
                    "Could not read the published manifest for repository space {}: {}",
                    space_name, err
                );
                if requested {
                    let message = self.t_fmt(
                        "Could not refresh repository space {name} from the server",
                        &[("name", space_name)],
                    );
                    self.show_error_toast(message);
                }
                return;
            }
        };
        // The space may have been deleted while its manifest was in flight.
        let Some(stored) = self
            .repository_spaces
            .iter()
            .find(|space| space.id == space_id)
            .cloned()
        else {
            return;
        };

        // Info, not debug: "the space was checked" is the answer a user log has
        // to be able to show, and there is one line per space per refresh.
        if !stored.manifest_checksum.is_empty()
            && stored.manifest_checksum == fetched.manifest_checksum
        {
            info!(
                "Repository space {} matches its published manifest (checksum {})",
                space_name, stored.manifest_checksum
            );
            self.toast_repository_space_refreshed(requested, space_name);
            return;
        }

        let delta = repository_space_delta(
            &Self::space_entries(&stored.entries),
            &Self::space_entries(&fetched.entries),
        );
        let refreshed = Self::merged_repository_space(Some(&stored), &space_id, fetched, "");
        if refreshed != stored {
            self.store_repository_space(refreshed, ctx);
        }

        if delta.is_empty() {
            info!(
                "Repository space {} refreshed from its published manifest; its repository list is unchanged",
                space_name
            );
            self.toast_repository_space_refreshed(requested, space_name);
            return;
        }

        info!(
            "Repository space {} changed on the server: {} added, {} removed, {} required-flag changes; refreshed its repository list",
            space_name,
            delta.added.len(),
            delta.removed.len(),
            delta.required_changed.len()
        );
        let message = self.t_fmt(
            "Repository space {name} changed on the server ({count} differences). Open it to review.",
            &[
                ("name", space_name),
                ("count", delta.total().to_string()),
            ],
        );
        self.repository_space_remote_changes.insert(space_id, delta);
        self.show_success_toast(message);
    }

    fn toast_repository_space_refreshed(&mut self, requested: bool, space_name: String) {
        if requested {
            let message = self.t_fmt(
                "Repository space {name} refreshed from the server",
                &[("name", space_name)],
            );
            self.show_success_toast(message);
        }
    }

    pub(crate) fn repository_space_remote_delta(
        &self,
        space_id: &str,
    ) -> Option<&RepositorySpaceRemoteDelta> {
        self.repository_space_remote_changes.get(space_id)
    }
}

/// Runtime-only record of what the last refresh of each space changed, keyed by
/// space id.
pub type RepositorySpaceRemoteChanges = HashMap<String, RepositorySpaceRemoteDelta>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::RepositorySpaceManifestEntry;
    use crate::ui::types::RepositorySpace;

    fn entry(address: &str, name: &str, required: bool) -> SpaceEntry {
        SpaceEntry {
            address: address.to_string(),
            name: name.to_string(),
            required,
        }
    }

    fn space_entry(name: &str, address: &str, required: bool) -> RepositorySpaceEntry {
        RepositorySpaceEntry {
            name: name.to_string(),
            address: address.to_string(),
            required,
        }
    }

    fn fetched(entries: Vec<RepositorySpaceEntry>) -> FetchedRepositorySpace {
        FetchedRepositorySpace {
            source_address: "http://repo.example.invalid:8080/space/repository_space.json"
                .to_string(),
            source_base_url: "http://repo.example.invalid:8080/space".to_string(),
            space_id: "space-from-url".to_string(),
            manifest_name: "MainSpace".to_string(),
            icon_image_path: "icon.png".to_string(),
            icon_image_checksum: "new-icon".to_string(),
            repo_image_path: "space.png".to_string(),
            repo_image_checksum: "new-image".to_string(),
            app_update_url: "http://repo.example.invalid:8080/updates/".to_string(),
            manifest_checksum: "new-checksum".to_string(),
            entries,
        }
    }

    fn manifest_entry(name: &str, address: &str, required: bool) -> RepositorySpaceManifestEntry {
        RepositorySpaceManifestEntry {
            name: name.to_string(),
            address: address.to_string(),
            required,
        }
    }

    fn checksum_manifest() -> RepositorySpaceManifest {
        RepositorySpaceManifest {
            name: "MainSpace".to_string(),
            image: "space.png".to_string(),
            image_checksum: "aa".to_string(),
            icon: "icon.png".to_string(),
            icon_checksum: "bb".to_string(),
            space_checksum: String::new(),
            app_update_url: String::new(),
            entries: vec![
                manifest_entry(
                    "RepoAlpha",
                    "http://repo.example.invalid:8080/space/RepoAlpha/",
                    true,
                ),
                manifest_entry(
                    "RepoBeta",
                    "http://repo.example.invalid:8080/space/RepoBeta/",
                    false,
                ),
            ],
        }
    }

    #[test]
    fn manifest_checksum_matches_the_pinned_server_vector() {
        assert_eq!(
            repository_space_manifest_checksum(&checksum_manifest()),
            "580e1de5482e306432a52e2850ba74eda311c69d"
        );
    }

    #[test]
    fn manifest_checksum_ignores_fields_outside_the_covered_set() {
        let base = repository_space_manifest_checksum(&checksum_manifest());
        let mut other = checksum_manifest();
        other.image = "elsewhere.png".to_string();
        other.app_update_url = "http://repo.example.invalid:8080/updates/".to_string();
        other.space_checksum = "stale".to_string();
        assert_eq!(repository_space_manifest_checksum(&other), base);

        let mut added = checksum_manifest();
        added.entries.push(manifest_entry(
            "RepoGamma",
            "http://repo.example.invalid:8080/space/RepoGamma/",
            false,
        ));
        assert_ne!(repository_space_manifest_checksum(&added), base);
    }

    #[test]
    fn published_example_space_checksum_matches_its_content() {
        let manifest: RepositorySpaceManifest = serde_json::from_str(include_str!(
            "../../../../examples/json/remote_repositories/repository_space.json"
        ))
        .unwrap();
        assert_eq!(
            repository_space_manifest_checksum(&manifest),
            manifest.space_checksum
        );
    }

    #[test]
    fn manifests_without_a_space_checksum_still_parse() {
        let manifest: RepositorySpaceManifest = serde_json::from_str(
            r#"{"Name":"MainSpace","imageChecksum":"aa","iconChecksum":"bb","entries":[{"Name":"RepoAlpha","Address":"http://repo.example.invalid:8080/space/RepoAlpha/","Requiered":true}]}"#,
        )
        .unwrap();
        assert!(manifest.space_checksum.is_empty());
        assert_eq!(manifest.entries.len(), 1);
    }

    #[test]
    fn refresh_takes_published_entries_and_keeps_local_state() {
        let stored = RepositorySpace {
            id: "space-stored".to_string(),
            name: "MainSpace".to_string(),
            local_name_override: Some("Our space".to_string()),
            collapsed: true,
            source_address: "http://repo.example.invalid:8080/space/repository_space.json"
                .to_string(),
            source_base_url: "http://repo.example.invalid:8080/space".to_string(),
            shared_path: "R:/Mods/space".to_string(),
            icon_image_path: "icon.png".to_string(),
            icon_image_checksum: "old-icon".to_string(),
            repo_image_path: "space.png".to_string(),
            repo_image_checksum: "old-image".to_string(),
            app_update_url: String::new(),
            manifest_checksum: "old-checksum".to_string(),
            entries: vec![space_entry(
                "RepoAlpha",
                "http://repo.example.invalid:8080/space/RepoAlpha",
                true,
            )],
        };
        let published = vec![
            space_entry(
                "RepoAlpha",
                "http://repo.example.invalid:8080/space/RepoAlpha",
                true,
            ),
            space_entry(
                "RepoBeta",
                "http://repo.example.invalid:8080/space/RepoBeta",
                false,
            ),
        ];

        let refreshed = Foxy::merged_repository_space(
            Some(&stored),
            &stored.id,
            fetched(published.clone()),
            "R:/Mods/elsewhere",
        );

        assert_eq!(refreshed.id, "space-stored");
        assert_eq!(refreshed.entries, published);
        assert_eq!(refreshed.manifest_checksum, "new-checksum");
        assert_eq!(refreshed.icon_image_checksum, "new-icon");
        assert_eq!(refreshed.repo_image_checksum, "new-image");
        assert_eq!(
            refreshed.app_update_url,
            "http://repo.example.invalid:8080/updates/"
        );
        assert_eq!(refreshed.shared_path, "R:/Mods/space");
        assert!(refreshed.collapsed);
        assert_eq!(refreshed.local_name_override.as_deref(), Some("Our space"));
    }

    #[test]
    fn first_import_uses_the_preferred_folder() {
        let imported = Foxy::merged_repository_space(
            None,
            "space-from-url",
            fetched(vec![space_entry(
                "RepoAlpha",
                "http://repo.example.invalid:8080/space/RepoAlpha",
                true,
            )]),
            "R:/Mods/space",
        );

        assert_eq!(imported.id, "space-from-url");
        assert_eq!(imported.name, "MainSpace");
        assert_eq!(imported.shared_path, "R:/Mods/space");
        assert!(!imported.collapsed);
        assert!(imported.local_name_override.is_none());
    }

    #[test]
    fn identical_membership_is_no_change() {
        let local = vec![
            entry("http://x/a", "A", true),
            entry("http://x/b", "B", false),
        ];
        let remote = vec![
            entry("HTTP://X/A", "A", true),
            entry("http://x/b", "B", false),
        ];
        assert!(repository_space_delta(&local, &remote).is_empty());
    }

    #[test]
    fn added_removed_and_reflagged_entries_are_reported_separately() {
        let local = vec![
            entry("http://x/a", "A", true),
            entry("http://x/b", "B", false),
        ];
        let remote = vec![
            entry("http://x/a", "A", false),
            entry("http://x/c", "C", true),
        ];
        let delta = repository_space_delta(&local, &remote);
        assert_eq!(delta.added, vec!["C".to_string()]);
        assert_eq!(delta.removed, vec!["B".to_string()]);
        assert_eq!(delta.required_changed, vec!["A".to_string()]);
        assert_eq!(delta.total(), 3);
    }

    #[test]
    fn entries_without_an_address_are_ignored_and_names_fall_back_to_the_address() {
        let local = vec![entry("", "ghost", true)];
        let remote = vec![entry("http://x/a", "   ", true)];
        let delta = repository_space_delta(&local, &remote);
        assert_eq!(delta.added, vec!["http://x/a".to_string()]);
        assert!(delta.removed.is_empty());
    }
}
