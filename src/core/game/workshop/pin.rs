use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::core::utils::content_hash::calculate_addon_folder_content_hash;

use super::{FreezeSummary, SteamWorkshopItem};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinState {
    /// Launches from Steam's live folder and follows every Workshop update.
    NotFrozen,
    /// Pinned, and Steam's copy still matches the pin.
    InSync,
    /// Pinned, but Steam has since updated the item. Foxy still launches the
    /// pinned copy; this is what a player needs to see before sharing a code.
    Drifted,
    /// Pinned, and Steam no longer has the item installed, so drift cannot be
    /// judged. The pinned copy still launches.
    LiveMissing,
    /// Marked pinned but the snapshot folder is gone, so the next launch falls
    /// back to Steam's live copy.
    FrozenMissing,
}

impl PinState {
    pub fn as_str(&self) -> &'static str {
        match self {
            PinState::NotFrozen => "not-frozen",
            PinState::InSync => "in-sync",
            PinState::Drifted => "drifted",
            PinState::LiveMissing => "live-missing",
            PinState::FrozenMissing => "frozen-missing",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinStatus {
    pub item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    pub enabled: bool,
    pub frozen: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_order: Option<u32>,
    pub state: PinState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen_path: Option<String>,
}

/// Pin state of every managed item of one app, in launch order.
pub fn pin_status(
    space_dir: &Path,
    app_id: u32,
    steam_directory: &str,
) -> Result<Vec<PinStatus>, String> {
    let store = super::load_store(space_dir)?;
    let mut entries: Vec<&SteamWorkshopItem> = store
        .entries
        .iter()
        .filter(|entry| entry.app_id == app_id)
        .collect();
    entries.sort_by_key(|entry| super::launch_order_key(entry));
    Ok(entries
        .into_iter()
        .map(|entry| pin_status_for_entry(entry, steam_directory))
        .collect())
}

fn pin_status_for_entry(entry: &SteamWorkshopItem, steam_directory: &str) -> PinStatus {
    let live_path = super::resolve_installed_path(entry, steam_directory);
    // A metadata hash of the live folder, compared against the hash recorded
    // when the item was frozen. Same folder plus no Steam update means the same
    // hash, so drift is detected without re-reading every mod file.
    let live_version = live_path
        .as_deref()
        .and_then(|path| calculate_addon_folder_content_hash(path).ok())
        .filter(|hash| !hash.is_empty());
    let frozen_dir_exists = entry
        .frozen_path
        .as_deref()
        .map(Path::new)
        .is_some_and(Path::is_dir);

    let state = if !entry.frozen {
        PinState::NotFrozen
    } else if !frozen_dir_exists {
        PinState::FrozenMissing
    } else {
        match (entry.version.as_deref(), live_version.as_deref()) {
            (Some(pinned), Some(live)) if pinned.eq_ignore_ascii_case(live) => PinState::InSync,
            (Some(_), Some(_)) => PinState::Drifted,
            _ => PinState::LiveMissing,
        }
    };

    PinStatus {
        item_id: entry.item_id.clone(),
        title: entry.title.clone(),
        url: entry.url.clone(),
        enabled: entry.enabled,
        frozen: entry.frozen,
        load_order: entry.load_order,
        state,
        pinned_version: entry.version.clone(),
        live_version,
        live_path: live_path.map(|path| path.display().to_string()),
        frozen_path: entry.frozen_path.clone(),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreezeAllSummary {
    pub frozen: Vec<FreezeSummary>,
    /// Already pinned and left alone because `refresh` was not requested.
    pub skipped: Vec<String>,
    pub failed: Vec<FreezeFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreezeFailure {
    pub item_id: String,
    pub error: String,
}

/// Pin every managed item of one app in one pass. `refresh` re-pins items that
/// are already frozen, which is how a player moves a pin onto a newer build.
pub fn freeze_all(
    space_dir: &Path,
    app_id: u32,
    steam_directory: &str,
    include_disabled: bool,
    refresh: bool,
) -> Result<FreezeAllSummary, String> {
    let store = super::load_store(space_dir)?;
    let mut entries: Vec<SteamWorkshopItem> = store
        .entries
        .iter()
        .filter(|entry| entry.app_id == app_id)
        .filter(|entry| include_disabled || entry.enabled)
        .cloned()
        .collect();
    entries.sort_by_key(super::launch_order_key);

    let mut summary = FreezeAllSummary::default();
    for entry in entries {
        if entry.frozen && !refresh {
            summary.skipped.push(entry.item_id.clone());
            continue;
        }
        match super::freeze_item(space_dir, app_id, &entry.item_id, steam_directory) {
            Ok(frozen) => summary.frozen.push(frozen),
            Err(error) => summary.failed.push(FreezeFailure {
                item_id: entry.item_id.clone(),
                error,
            }),
        }
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn workshop_item_dir(steam_root: &Path, app_id: u32, item_id: &str) -> std::path::PathBuf {
        let dir = steam_root
            .join("steamapps")
            .join("workshop")
            .join("content")
            .join(app_id.to_string())
            .join(item_id);
        fs::create_dir_all(&dir).expect("item dir");
        dir
    }

    #[test]
    fn pin_status_reports_not_frozen_in_sync_and_drifted() {
        let space = tempfile::tempdir().expect("space");
        let steam = tempfile::tempdir().expect("steam");
        let app_id = 1142710;
        let pinned_dir = workshop_item_dir(steam.path(), app_id, "111");
        fs::write(pinned_dir.join("alpha.pack"), "one").expect("pack");
        let loose_dir = workshop_item_dir(steam.path(), app_id, "222");
        fs::write(loose_dir.join("beta.pack"), "two").expect("pack");
        let steam_dir = steam.path().display().to_string();
        for item_id in ["111", "222"] {
            super::super::upsert_item(space.path(), app_id, item_id, None, None, None, true)
                .expect("upsert");
        }
        super::super::freeze_item(space.path(), app_id, "111", &steam_dir).expect("freeze");

        let statuses = pin_status(space.path(), app_id, &steam_dir).expect("status");
        let pinned = statuses
            .iter()
            .find(|status| status.item_id == "111")
            .expect("pinned");
        let loose = statuses
            .iter()
            .find(|status| status.item_id == "222")
            .expect("loose");

        assert_eq!(loose.state, PinState::NotFrozen);
        assert_eq!(pinned.state, PinState::InSync);

        fs::write(pinned_dir.join("beta.pack"), "added by a Workshop update").expect("update");

        let statuses = pin_status(space.path(), app_id, &steam_dir).expect("status");
        let pinned = statuses
            .iter()
            .find(|status| status.item_id == "111")
            .expect("pinned");

        assert_eq!(pinned.state, PinState::Drifted);
        assert_ne!(pinned.live_version, pinned.pinned_version);
    }

    #[test]
    fn freeze_all_skips_pinned_items_unless_refreshing() {
        let space = tempfile::tempdir().expect("space");
        let steam = tempfile::tempdir().expect("steam");
        let app_id = 1142710;
        for item_id in ["111", "222"] {
            let dir = workshop_item_dir(steam.path(), app_id, item_id);
            fs::write(dir.join("alpha.pack"), item_id).expect("pack");
            super::super::upsert_item(space.path(), app_id, item_id, None, None, None, true)
                .expect("upsert");
        }
        let steam_dir = steam.path().display().to_string();

        let first = freeze_all(space.path(), app_id, &steam_dir, false, false).expect("freeze all");
        assert_eq!(first.frozen.len(), 2);
        assert!(first.skipped.is_empty());
        assert!(first.failed.is_empty());

        let second =
            freeze_all(space.path(), app_id, &steam_dir, false, false).expect("freeze all");
        assert!(second.frozen.is_empty());
        assert_eq!(second.skipped.len(), 2);

        let refreshed = freeze_all(space.path(), app_id, &steam_dir, false, true).expect("refresh");
        assert_eq!(refreshed.frozen.len(), 2);
    }
}
