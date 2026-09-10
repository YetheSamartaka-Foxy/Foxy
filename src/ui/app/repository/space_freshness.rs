//! Startup freshness probe for repository spaces.
//!
//! A repository's own `repo.json` says whether that repository's payload moved.
//! It says nothing about the *space*: a server that adds, drops or re-flags a
//! repository inside `repository_space.json` changes what the user is supposed
//! to have, and Foxy only ever read that manifest when the user imported the
//! space by hand. Until then a new required repository is invisible.
//!
//! The probe answers "did this space's published membership move" and stops
//! there. Applying it is the user's call: adding or removing repositories
//! behind their back is a change to their disk, not a notification.

use std::collections::{BTreeMap, HashMap};

use log::{info, warn};

use crate::ui::app::Foxy;
use crate::ui::types::RepositorySpace;

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

/// A completed probe for one space.
pub struct RepositorySpaceFreshnessResult {
    pub space_id: String,
    pub space_name: String,
    /// `Ok(delta)` when the manifest was read, `Err` when it could not be.
    /// An unreachable manifest is unknown freshness, never "unchanged".
    pub outcome: Result<RepositorySpaceRemoteDelta, String>,
}

/// One repository as a space lists it, reduced to what a comparison needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceEntry {
    /// Normalized address; the identity.
    pub address: String,
    pub name: String,
    pub required: bool,
}

/// A space to probe, with the local membership to compare the answer against.
struct SpaceProbeTarget {
    id: String,
    name: String,
    source_address: String,
    entries: Vec<SpaceEntry>,
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
    fn space_entries(space: &RepositorySpace) -> Vec<SpaceEntry> {
        space
            .entries
            .iter()
            .map(|entry| SpaceEntry {
                address: Self::normalize_repository_address_input(&entry.address),
                name: entry.name.clone(),
                required: entry.required,
            })
            .collect()
    }

    /// Fetch every configured space's manifest on a worker thread and report
    /// how its published membership differs from the local copy.
    ///
    /// Runs once per launch, after the first frame, alongside the repository
    /// `repo.json` probes rather than before them: the space answer changes what
    /// the user should install, not what the current launch is about to check.
    pub(in crate::ui::app) fn start_repository_space_freshness_probe(&mut self) {
        if self.repository_space_freshness_rx.is_some() {
            return;
        }
        let spaces: Vec<SpaceProbeTarget> = self
            .repository_spaces
            .iter()
            .filter(|space| !space.source_address.trim().is_empty())
            .map(|space| SpaceProbeTarget {
                id: space.id.clone(),
                name: Self::repository_space_display_name(space).to_string(),
                source_address: space.source_address.trim().to_string(),
                entries: Self::space_entries(space),
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
                    Ok(Some(fetched)) => {
                        let remote: Vec<SpaceEntry> = fetched
                            .entries
                            .iter()
                            .map(|entry| SpaceEntry {
                                address: Self::normalize_repository_address_input(&entry.address),
                                name: entry.name.clone(),
                                required: entry.required,
                            })
                            .collect();
                        Ok(repository_space_delta(&target.entries, &remote))
                    }
                    Ok(None) => {
                        Err("no repository space manifest at the configured address".into())
                    }
                    Err(err) => Err(err),
                };
                if tx
                    .send(RepositorySpaceFreshnessResult {
                        space_id: target.id,
                        space_name: target.name,
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

    pub(in crate::ui::app) fn poll_repository_space_freshness_results(&mut self) {
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
            match result.outcome {
                Ok(delta) if delta.is_empty() => {
                    // Info, not debug: "the space was checked and matches" is the
                    // answer a user log has to be able to show, and there is one
                    // line per space per launch.
                    info!(
                        "Repository space {} matches its published manifest",
                        result.space_name
                    );
                    self.repository_space_remote_changes
                        .remove(&result.space_id);
                }
                Ok(delta) => {
                    info!(
                        "Repository space {} changed on the server: {} added, {} removed, {} required-flag changes",
                        result.space_name,
                        delta.added.len(),
                        delta.removed.len(),
                        delta.required_changed.len()
                    );
                    let message = self.t_fmt(
                        "Repository space {name} changed on the server ({count} differences). Open it to review.",
                        &[
                            ("name", result.space_name.clone()),
                            ("count", delta.total().to_string()),
                        ],
                    );
                    self.repository_space_remote_changes
                        .insert(result.space_id, delta);
                    self.show_success_toast(message);
                }
                Err(err) => {
                    warn!(
                        "Could not read the published manifest for repository space {}: {}",
                        result.space_name, err
                    );
                    self.repository_space_remote_changes
                        .remove(&result.space_id);
                }
            }
            self.needs_repaint = true;
        }
    }

    pub(crate) fn repository_space_remote_delta(
        &self,
        space_id: &str,
    ) -> Option<&RepositorySpaceRemoteDelta> {
        self.repository_space_remote_changes.get(space_id)
    }
}

/// Runtime-only record of what each space's manifest says, keyed by space id.
pub type RepositorySpaceRemoteChanges = HashMap<String, RepositorySpaceRemoteDelta>;

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(address: &str, name: &str, required: bool) -> SpaceEntry {
        SpaceEntry {
            address: address.to_string(),
            name: name.to_string(),
            required,
        }
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
