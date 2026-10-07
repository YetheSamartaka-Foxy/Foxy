use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::core::utils::format::sanitize_log_path;
use crate::core::utils::fs_safety::is_safe_child_path;

use super::checksum::StateChecksum;
use super::share::{self, ShareCodeOptions, SharedItem};
use super::{SteamWorkshopItem, WorkshopFile};

pub const BUNDLE_EXTENSION: &str = "foxyshare";
pub const BUNDLE_MANIFEST_NAME: &str = "share.json";
const BUNDLE_KIND: &str = "foxy-workshop-share";
const BUNDLE_SCHEMA_VERSION: u32 = 1;
const FROZEN_PREFIX: &str = "frozen/";
/// Restored payloads land in their own folder so they never sit beside (or
/// inside) the `<hash>_<name>` snapshots `freeze_item` writes.
const IMPORTED_PAYLOAD_DIR: &str = "imported";
/// Bundles carry whole frozen mod folders, so the ceiling is generous compared
/// with a `.foxypack`; it exists to stop a hostile archive, not to size a
/// realistic mod list.
pub const MAX_BUNDLE_UNCOMPRESSED_BYTES: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleItem {
    pub item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_order: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_updated: Option<u64>,
    /// Set when the bundle carries this mod's files under `frozen/<item_id>/`.
    #[serde(default)]
    pub payload: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleManifest {
    pub schema_version: u32,
    pub kind: String,
    pub game_id: String,
    pub app_id: u32,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Pipe-separated share code for the same selection, so a recipient without
    /// the bundle file can still be handed a paste-able list.
    pub share_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_checksum: Option<StateChecksum>,
    pub items: Vec<BundleItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleExportSummary {
    pub path: String,
    pub app_id: u32,
    pub item_count: usize,
    pub payload_count: usize,
    pub share_code: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleImportSummary {
    pub app_id: u32,
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub restored_payloads: Vec<String>,
    /// Items the bundle references without carrying files for; the caller
    /// downloads these from the Workshop.
    pub needs_download: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BundleExportOptions {
    pub include_disabled: bool,
    /// Copy the frozen snapshot of every pinned item into the archive.
    pub include_frozen_payloads: bool,
}

pub fn build_manifest(
    store: &WorkshopFile,
    game_id: &str,
    app_id: u32,
    options: BundleExportOptions,
    state_checksum: Option<StateChecksum>,
    note: Option<String>,
) -> BundleManifest {
    let items: Vec<BundleItem> = bundle_entries(store, app_id, options)
        .into_iter()
        .map(|(item, _)| item)
        .collect();
    let shared: Vec<SharedItem> = items
        .iter()
        .map(|item| SharedItem {
            item_id: item.item_id.clone(),
            name: item.title.clone(),
            load_order: item.load_order,
            version: item.version.clone(),
        })
        .collect();
    BundleManifest {
        schema_version: BUNDLE_SCHEMA_VERSION,
        kind: BUNDLE_KIND.to_string(),
        game_id: game_id.to_string(),
        app_id,
        created_at: super::unix_timestamp_now(),
        note,
        share_code: share::render_share_code(
            &shared,
            ShareCodeOptions {
                include_load_order: true,
                include_versions: false,
            },
        ),
        state_checksum,
        items,
    }
}

fn bundle_entries(
    store: &WorkshopFile,
    app_id: u32,
    options: BundleExportOptions,
) -> Vec<(BundleItem, Option<PathBuf>)> {
    let mut entries: Vec<&SteamWorkshopItem> = store
        .entries
        .iter()
        .filter(|entry| entry.app_id == app_id)
        .filter(|entry| options.include_disabled || entry.enabled)
        .collect();
    entries.sort_by_key(|entry| super::launch_order_key(entry));
    entries
        .into_iter()
        .map(|entry| {
            let payload_dir = options
                .include_frozen_payloads
                .then(|| frozen_payload_dir(entry))
                .flatten();
            (
                BundleItem {
                    item_id: entry.item_id.clone(),
                    title: entry.title.clone(),
                    url: Some(entry.url.clone()).filter(|url| !url.is_empty()),
                    enabled: entry.enabled,
                    load_order: entry.load_order,
                    version: entry.version.clone(),
                    size_bytes: entry.size_bytes,
                    time_updated: entry.time_updated,
                    payload: payload_dir.is_some(),
                },
                payload_dir,
            )
        })
        .collect()
}

fn frozen_payload_dir(entry: &SteamWorkshopItem) -> Option<PathBuf> {
    if !entry.frozen {
        return None;
    }
    let path = PathBuf::from(entry.frozen_path.as_deref()?);
    path.is_dir().then_some(path)
}

pub fn export_bundle(
    space_dir: &Path,
    game_id: &str,
    app_id: u32,
    output_path: &Path,
    options: BundleExportOptions,
    state_checksum: Option<StateChecksum>,
    note: Option<String>,
) -> Result<BundleExportSummary, String> {
    let store = super::load_store(space_dir)?;
    let entries = bundle_entries(&store, app_id, options);
    let manifest = build_manifest(&store, game_id, app_id, options, state_checksum, note);

    let file = fs::File::create(output_path).map_err(|err| {
        format!(
            "Failed to create share bundle {}: {}",
            sanitize_log_path(output_path),
            err
        )
    })?;
    let mut zip = ZipWriter::new(file);
    let zip_options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let data = serde_json::to_vec_pretty(&manifest)
        .map_err(|err| format!("Failed to serialize share bundle manifest: {}", err))?;
    zip.start_file(BUNDLE_MANIFEST_NAME, zip_options)
        .map_err(|err| format!("Failed to write {}: {}", BUNDLE_MANIFEST_NAME, err))?;
    zip.write_all(&data)
        .map_err(|err| format!("Failed to write {}: {}", BUNDLE_MANIFEST_NAME, err))?;

    let mut payload_count = 0;
    for (item, payload_dir) in &entries {
        let Some(payload_dir) = payload_dir else {
            continue;
        };
        add_dir_to_zip(
            &mut zip,
            zip_options,
            payload_dir,
            &format!("{}{}", FROZEN_PREFIX, item.item_id),
        )?;
        payload_count += 1;
    }

    zip.finish()
        .map_err(|err| format!("Failed to finish share bundle: {}", err))?;

    Ok(BundleExportSummary {
        path: output_path.display().to_string(),
        app_id,
        item_count: manifest.items.len(),
        payload_count,
        share_code: manifest.share_code,
    })
}

fn add_dir_to_zip(
    zip: &mut ZipWriter<fs::File>,
    options: SimpleFileOptions,
    source: &Path,
    archive_name: &str,
) -> Result<(), String> {
    let metadata = fs::metadata(source)
        .map_err(|err| format!("Failed to read {}: {}", sanitize_log_path(source), err))?;
    if metadata.is_file() {
        zip.start_file(archive_name, options)
            .map_err(|err| format!("Failed to add {}: {}", archive_name, err))?;
        let mut file = fs::File::open(source)
            .map_err(|err| format!("Failed to open {}: {}", sanitize_log_path(source), err))?;
        std::io::copy(&mut file, zip)
            .map_err(|err| format!("Failed to copy {}: {}", sanitize_log_path(source), err))?;
        return Ok(());
    }

    zip.add_directory(format!("{}/", archive_name.trim_end_matches('/')), options)
        .map_err(|err| format!("Failed to add {}: {}", archive_name, err))?;
    let mut children = fs::read_dir(source)
        .map_err(|err| format!("Failed to read {}: {}", sanitize_log_path(source), err))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("Failed to read {}: {}", sanitize_log_path(source), err))?;
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let child_name = child.file_name().to_string_lossy().to_string();
        if !is_safe_child_path(&child_name) {
            return Err(format!("Unsafe frozen payload name {}", child_name));
        }
        add_dir_to_zip(
            zip,
            options,
            &child.path(),
            &format!("{}/{}", archive_name.trim_end_matches('/'), child_name),
        )?;
    }
    Ok(())
}

pub fn read_manifest(path: &Path) -> Result<BundleManifest, String> {
    let mut archive = open_bundle(path)?;
    let entry = archive
        .by_name(BUNDLE_MANIFEST_NAME)
        .map_err(|_| format!("{} is not a Foxy share bundle", sanitize_log_path(path)))?;
    let manifest: BundleManifest = serde_json::from_reader(entry)
        .map_err(|err| format!("Failed to parse share bundle manifest: {}", err))?;
    if manifest.kind != BUNDLE_KIND {
        return Err(format!("Unsupported share bundle kind {}", manifest.kind));
    }
    if manifest.schema_version != BUNDLE_SCHEMA_VERSION {
        return Err(format!(
            "Unsupported share bundle schema version {}",
            manifest.schema_version
        ));
    }
    Ok(manifest)
}

/// Import a share bundle into the active space. Frozen payloads are restored
/// into the space's own snapshot store; items without a payload are recorded as
/// managed and reported in `needs_download`.
pub fn import_bundle(
    space_dir: &Path,
    app_id: u32,
    path: &Path,
    restore_payloads: bool,
) -> Result<BundleImportSummary, String> {
    let manifest = read_manifest(path)?;
    if manifest.app_id != app_id {
        return Err(format!(
            "Share bundle targets Steam app {} but the active game uses app {}",
            manifest.app_id, app_id
        ));
    }

    let mut summary = BundleImportSummary {
        app_id,
        ..BundleImportSummary::default()
    };
    if restore_payloads {
        summary.restored_payloads = extract_payloads(space_dir, path, &manifest)?;
    }

    for item in &manifest.items {
        let item_id = super::normalize_workshop_id(&item.item_id)
            .ok_or_else(|| format!("Invalid Steam Workshop item id {}", item.item_id))?;
        let restored = summary.restored_payloads.contains(&item_id);
        let frozen_path = restored.then(|| {
            imported_payload_dir(space_dir, &item_id)
                .display()
                .to_string()
        });
        let imported = SteamWorkshopItem {
            source: super::STEAM_SOURCE.to_string(),
            app_id,
            item_id: item_id.clone(),
            title: item.title.clone(),
            url: item
                .url
                .clone()
                .unwrap_or_else(|| super::workshop_url(&item_id)),
            enabled: item.enabled,
            frozen: restored,
            load_order: item.load_order,
            version: item.version.clone(),
            installed_path: None,
            frozen_path,
            size_bytes: item.size_bytes,
            time_updated: item.time_updated,
            added_at: 0,
            updated_at: 0,
        };
        let result = super::upsert_bundle_item(space_dir, imported)?;
        if result.added {
            summary.added.push(item_id.clone());
        } else {
            summary.updated.push(item_id.clone());
        }
        if !restored {
            summary.needs_download.push(item_id);
        }
    }
    Ok(summary)
}

fn extract_payloads(
    space_dir: &Path,
    path: &Path,
    manifest: &BundleManifest,
) -> Result<Vec<String>, String> {
    let mut archive = open_bundle(path)?;
    let mut extracted_bytes: u64 = 0;
    let mut restored = Vec::new();
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|err| format!("Failed to read zip entry {}: {}", index, err))?;
        let name = file.name().replace('\\', "/");
        let Some(rest) = name.strip_prefix(FROZEN_PREFIX) else {
            continue;
        };
        let Some((item_id, relative)) = rest.split_once('/') else {
            continue;
        };
        let Some(item_id) = super::normalize_workshop_id(item_id) else {
            return Err(format!("Unsafe frozen payload path in bundle: {}", name));
        };
        if !manifest
            .items
            .iter()
            .any(|item| item.payload && item.item_id == item_id)
        {
            return Err(format!(
                "Share bundle carries files for unlisted Workshop item {}",
                item_id
            ));
        }
        let relative = relative.trim_end_matches('/');
        if relative.is_empty() {
            continue;
        }
        if !is_safe_child_path(relative) {
            return Err(format!("Unsafe frozen payload path in bundle: {}", name));
        }
        extracted_bytes = extracted_bytes.saturating_add(file.size());
        if extracted_bytes > MAX_BUNDLE_UNCOMPRESSED_BYTES {
            return Err(format!(
                "Share bundle payloads exceed the {} GiB import limit",
                MAX_BUNDLE_UNCOMPRESSED_BYTES / (1024 * 1024 * 1024)
            ));
        }

        let target = imported_payload_dir(space_dir, &item_id).join(relative);
        if file.is_dir() || name.ends_with('/') {
            fs::create_dir_all(&target).map_err(|err| {
                format!("Failed to create {}: {}", sanitize_log_path(&target), err)
            })?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|err| {
                    format!("Failed to create {}: {}", sanitize_log_path(parent), err)
                })?;
            }
            let mut output = fs::File::create(&target).map_err(|err| {
                format!("Failed to create {}: {}", sanitize_log_path(&target), err)
            })?;
            std::io::copy(&mut file, &mut output).map_err(|err| {
                format!(
                    "Failed to extract {} to {}: {}",
                    name,
                    sanitize_log_path(&target),
                    err
                )
            })?;
        }
        if !restored.contains(&item_id) {
            restored.push(item_id);
        }
    }
    Ok(restored)
}

fn imported_payload_dir(space_dir: &Path, item_id: &str) -> PathBuf {
    super::frozen_item_root(space_dir, item_id).join(IMPORTED_PAYLOAD_DIR)
}

fn open_bundle(path: &Path) -> Result<ZipArchive<fs::File>, String> {
    let file = fs::File::open(path).map_err(|err| {
        format!(
            "Failed to open share bundle {}: {}",
            sanitize_log_path(path),
            err
        )
    })?;
    ZipArchive::new(file).map_err(|err| format!("Failed to read share bundle as zip: {}", err))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn managed_item(space: &Path, item_id: &str, enabled: bool) {
        super::super::upsert_item(
            space,
            1142710,
            item_id,
            Some(format!("Mod {}", item_id)),
            None,
            None,
            enabled,
        )
        .expect("upsert");
    }

    #[test]
    fn export_and_import_round_trips_items_and_frozen_payloads() {
        let source_space = tempfile::tempdir().expect("source space");
        let target_space = tempfile::tempdir().expect("target space");
        let output = tempfile::tempdir().expect("output");
        managed_item(source_space.path(), "111", true);
        managed_item(source_space.path(), "222", true);
        super::super::set_item_load_order(source_space.path(), 1142710, "111", Some(2))
            .expect("order");
        super::super::set_item_load_order(source_space.path(), 1142710, "222", Some(1))
            .expect("order");

        let frozen_root = super::super::frozen_item_root(source_space.path(), "111");
        fs::create_dir_all(&frozen_root).expect("frozen root");
        fs::write(frozen_root.join("alpha.pack"), "pack bytes").expect("pack");
        let mut store = super::super::load_store(source_space.path()).expect("store");
        let entry = store.entry_mut(1142710, "111").expect("entry");
        entry.frozen = true;
        entry.frozen_path = Some(frozen_root.display().to_string());
        entry.version = Some("abc123".to_string());
        super::super::save_store(source_space.path(), &store).expect("save");

        let bundle_path = output.path().join("share.foxyshare");
        let summary = export_bundle(
            source_space.path(),
            "twwh3",
            1142710,
            &bundle_path,
            BundleExportOptions {
                include_disabled: false,
                include_frozen_payloads: true,
            },
            None,
            None,
        )
        .expect("export");

        assert_eq!(summary.item_count, 2);
        assert_eq!(summary.payload_count, 1);
        assert_eq!(summary.share_code, "222;1|111;2");

        let imported =
            import_bundle(target_space.path(), 1142710, &bundle_path, true).expect("import");

        assert_eq!(imported.added.len(), 2);
        assert_eq!(imported.restored_payloads, vec!["111".to_string()]);
        assert_eq!(imported.needs_download, vec!["222".to_string()]);
        let restored_pack = imported_payload_dir(target_space.path(), "111").join("alpha.pack");
        assert_eq!(
            fs::read_to_string(restored_pack).expect("restored pack"),
            "pack bytes"
        );
        let store = super::super::load_store(target_space.path()).expect("target store");
        let entry = store.entry(1142710, "111").expect("entry");
        assert!(entry.frozen);
        assert_eq!(entry.version.as_deref(), Some("abc123"));
        assert_eq!(
            store.entry(1142710, "222").expect("entry").load_order,
            Some(1)
        );
    }

    #[test]
    fn import_refuses_a_bundle_for_another_game() {
        let space = tempfile::tempdir().expect("space");
        let output = tempfile::tempdir().expect("output");
        managed_item(space.path(), "111", true);
        let bundle_path = output.path().join("share.foxyshare");
        export_bundle(
            space.path(),
            "twwh3",
            1142710,
            &bundle_path,
            BundleExportOptions::default(),
            None,
            None,
        )
        .expect("export");

        let err = import_bundle(space.path(), 107410, &bundle_path, false).expect_err("mismatch");

        assert!(err.contains("targets Steam app"));
    }
}
