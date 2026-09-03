use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::core::steam;

use super::WorkshopFile;

const CHECKSUM_DOMAIN: &str = "foxy-state-checksum/v1";
const SHORT_CHECKSUM_LEN: usize = 8;
const UNPINNED_VERSION: &str = "latest";

/// One mod as it contributes to the state checksum. `version` is the frozen
/// content hash when the item is pinned, the Workshop update timestamp when it
/// is not, and `latest` when neither is known.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecksumMod {
    pub item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub version: String,
    #[serde(default)]
    pub frozen: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_order: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateChecksum {
    /// Short display form, the value two players read out to each other.
    pub checksum: String,
    /// Full hash, kept so a diff can tell "same short code" from "same state".
    pub full: String,
    pub game_id: String,
    pub app_id: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_build_id: Option<String>,
    pub mods: Vec<ChecksumMod>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub launch_args: Vec<String>,
}

pub struct StateChecksumInput<'a> {
    pub game_id: &'a str,
    pub app_id: u32,
    pub steam_directory: &'a str,
    pub store: &'a WorkshopFile,
    pub launch_args: &'a [String],
}

pub fn compute_state_checksum(input: &StateChecksumInput<'_>) -> StateChecksum {
    let game_build_id = steam::steam_app_build_id(input.steam_directory, input.app_id);
    let mut entries: Vec<&super::SteamWorkshopItem> = input
        .store
        .entries
        .iter()
        .filter(|entry| entry.app_id == input.app_id && entry.enabled)
        .collect();
    entries.sort_by_key(|entry| super::launch_order_key(entry));
    let mods: Vec<ChecksumMod> = entries
        .into_iter()
        .map(|entry| ChecksumMod {
            item_id: entry.item_id.clone(),
            title: entry.title.clone(),
            version: mod_version(entry),
            frozen: entry.frozen,
            load_order: entry.load_order,
        })
        .collect();
    let launch_args: Vec<String> = input
        .launch_args
        .iter()
        .map(|arg| arg.trim().to_string())
        .filter(|arg| !arg.is_empty())
        .collect();

    let mut hasher = blake3::Hasher::new();
    hasher.update(CHECKSUM_DOMAIN.as_bytes());
    hash_field(&mut hasher, input.game_id);
    hash_field(&mut hasher, &input.app_id.to_string());
    hash_field(&mut hasher, game_build_id.as_deref().unwrap_or_default());
    hash_field(&mut hasher, &mods.len().to_string());
    for item in &mods {
        hash_field(&mut hasher, &item.item_id);
        hash_field(&mut hasher, &item.version);
    }
    hash_field(&mut hasher, &launch_args.len().to_string());
    for arg in &launch_args {
        hash_field(&mut hasher, arg);
    }
    let full = hasher.finalize().to_hex().to_string();

    StateChecksum {
        checksum: full[..SHORT_CHECKSUM_LEN].to_uppercase(),
        full,
        game_id: input.game_id.to_string(),
        app_id: input.app_id,
        game_build_id,
        mods,
        launch_args,
    }
}

pub fn state_checksum_for_space(
    space_dir: &Path,
    game_id: &str,
    app_id: u32,
    steam_directory: &str,
    launch_args: &[String],
) -> Result<StateChecksum, String> {
    let store = super::load_store(space_dir)?;
    Ok(compute_state_checksum(&StateChecksumInput {
        game_id,
        app_id,
        steam_directory,
        store: &store,
        launch_args,
    }))
}

fn mod_version(entry: &super::SteamWorkshopItem) -> String {
    entry
        .version
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| entry.time_updated.map(|value| value.to_string()))
        .unwrap_or_else(|| UNPINNED_VERSION.to_string())
}

fn hash_field(hasher: &mut blake3::Hasher, value: &str) {
    hasher.update(value.as_bytes());
    hasher.update(&[0]);
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecksumVersionMismatch {
    pub item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub local_version: String,
    pub remote_version: String,
    /// Which side pinned the mod. `remote_pinned` without `local_pinned` is the
    /// common case worth surfacing: the other player froze a build Steam no
    /// longer serves, so only their bundle can reproduce it.
    #[serde(default)]
    pub local_pinned: bool,
    #[serde(default)]
    pub remote_pinned: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateChecksumDiff {
    pub matches: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_build_mismatch: Option<(String, String)>,
    /// Enabled remotely but not locally.
    pub missing_mods: Vec<ChecksumMod>,
    /// Enabled locally but not remotely.
    pub extra_mods: Vec<ChecksumMod>,
    pub version_mismatches: Vec<ChecksumVersionMismatch>,
    pub load_order_differs: bool,
    pub launch_args_differ: bool,
}

pub fn diff_state_checksums(local: &StateChecksum, remote: &StateChecksum) -> StateChecksumDiff {
    let local_by_id: BTreeMap<&str, &ChecksumMod> = local
        .mods
        .iter()
        .map(|item| (item.item_id.as_str(), item))
        .collect();
    let remote_by_id: BTreeMap<&str, &ChecksumMod> = remote
        .mods
        .iter()
        .map(|item| (item.item_id.as_str(), item))
        .collect();

    let missing_mods = remote
        .mods
        .iter()
        .filter(|item| !local_by_id.contains_key(item.item_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let extra_mods = local
        .mods
        .iter()
        .filter(|item| !remote_by_id.contains_key(item.item_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let version_mismatches = local
        .mods
        .iter()
        .filter_map(|item| {
            let remote_item = remote_by_id.get(item.item_id.as_str())?;
            (remote_item.version != item.version).then(|| ChecksumVersionMismatch {
                item_id: item.item_id.clone(),
                title: item.title.clone().or_else(|| remote_item.title.clone()),
                local_version: item.version.clone(),
                remote_version: remote_item.version.clone(),
                local_pinned: item.frozen,
                remote_pinned: remote_item.frozen,
            })
        })
        .collect::<Vec<_>>();

    let game_build_mismatch = match (
        local.game_build_id.as_deref(),
        remote.game_build_id.as_deref(),
    ) {
        (Some(local_build), Some(remote_build)) if local_build != remote_build => {
            Some((local_build.to_string(), remote_build.to_string()))
        }
        _ => None,
    };

    let shared_local_order = local
        .mods
        .iter()
        .filter(|item| remote_by_id.contains_key(item.item_id.as_str()))
        .map(|item| item.item_id.as_str())
        .collect::<Vec<_>>();
    let shared_remote_order = remote
        .mods
        .iter()
        .filter(|item| local_by_id.contains_key(item.item_id.as_str()))
        .map(|item| item.item_id.as_str())
        .collect::<Vec<_>>();

    StateChecksumDiff {
        matches: local.full == remote.full,
        game_build_mismatch,
        missing_mods,
        extra_mods,
        version_mismatches,
        load_order_differs: shared_local_order != shared_remote_order,
        launch_args_differ: local.launch_args != remote.launch_args,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::game::workshop::SteamWorkshopItem;

    fn item(item_id: &str, load_order: Option<u32>, version: Option<&str>) -> SteamWorkshopItem {
        SteamWorkshopItem {
            source: super::super::STEAM_SOURCE.to_string(),
            app_id: 1142710,
            item_id: item_id.to_string(),
            title: Some(format!("Mod {}", item_id)),
            url: String::new(),
            enabled: true,
            frozen: version.is_some(),
            load_order,
            version: version.map(str::to_string),
            installed_path: None,
            frozen_path: None,
            size_bytes: None,
            time_updated: Some(1700),
            added_at: 0,
            updated_at: 0,
        }
    }

    fn checksum_of(entries: Vec<SteamWorkshopItem>) -> StateChecksum {
        let store = WorkshopFile {
            schema_version: 1,
            entries,
        };
        compute_state_checksum(&StateChecksumInput {
            game_id: "twwh3",
            app_id: 1142710,
            steam_directory: "",
            store: &store,
            launch_args: &[],
        })
    }

    #[test]
    fn same_mod_set_produces_the_same_checksum_regardless_of_store_order() {
        let first = checksum_of(vec![item("111", Some(1), None), item("222", Some(2), None)]);
        let second = checksum_of(vec![item("222", Some(2), None), item("111", Some(1), None)]);

        assert_eq!(first.full, second.full);
        assert_eq!(first.checksum.len(), SHORT_CHECKSUM_LEN);
    }

    #[test]
    fn load_order_and_pinned_version_change_the_checksum() {
        let base = checksum_of(vec![item("111", Some(1), None), item("222", Some(2), None)]);
        let reordered = checksum_of(vec![item("111", Some(2), None), item("222", Some(1), None)]);
        let pinned = checksum_of(vec![
            item("111", Some(1), Some("abc")),
            item("222", Some(2), None),
        ]);

        assert_ne!(base.full, reordered.full);
        assert_ne!(base.full, pinned.full);
    }

    #[test]
    fn disabled_items_are_excluded() {
        let mut disabled = item("333", Some(3), None);
        disabled.enabled = false;
        let with_disabled = checksum_of(vec![item("111", Some(1), None), disabled]);
        let without = checksum_of(vec![item("111", Some(1), None)]);

        assert_eq!(with_disabled.full, without.full);
    }

    #[test]
    fn diff_reports_missing_extra_and_version_mismatches() {
        let local = checksum_of(vec![
            item("111", Some(1), Some("aaa")),
            item("999", Some(2), None),
        ]);
        let remote = checksum_of(vec![
            item("111", Some(1), Some("bbb")),
            item("222", Some(2), None),
        ]);

        let diff = diff_state_checksums(&local, &remote);

        assert!(!diff.matches);
        assert_eq!(diff.missing_mods.len(), 1);
        assert_eq!(diff.missing_mods[0].item_id, "222");
        assert_eq!(diff.extra_mods.len(), 1);
        assert_eq!(diff.extra_mods[0].item_id, "999");
        assert_eq!(diff.version_mismatches.len(), 1);
        assert_eq!(diff.version_mismatches[0].remote_version, "bbb");
        assert!(diff.version_mismatches[0].remote_pinned);
        assert!(diff.version_mismatches[0].local_pinned);
    }
}
