use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
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
static NEXT_IMPORT_ID: AtomicU64 = AtomicU64::new(0);

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BundleImportMode {
    #[default]
    Merge,
    Match,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleImportPreview {
    pub app_id: u32,
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub unlisted: Vec<String>,
    pub enabled_changes: Vec<String>,
    pub load_order_changes: Vec<String>,
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
#[cfg(test)]
pub fn import_bundle(
    space_dir: &Path,
    app_id: u32,
    path: &Path,
    restore_payloads: bool,
) -> Result<BundleImportSummary, String> {
    import_bundle_with_mode(
        space_dir,
        app_id,
        path,
        restore_payloads,
        BundleImportMode::Merge,
    )
}

pub fn preview_import_bundle(
    space_dir: &Path,
    app_id: u32,
    path: &Path,
) -> Result<BundleImportPreview, String> {
    let manifest = read_manifest(path)?;
    if manifest.app_id != app_id {
        return Err(format!(
            "Share bundle targets Steam app {} but the active game uses app {}",
            manifest.app_id, app_id
        ));
    }
    let items = normalized_bundle_items(&manifest)?;
    let store = super::load_store(space_dir)?;
    let incoming = items
        .iter()
        .map(|(item_id, _)| item_id.clone())
        .collect::<HashSet<_>>();
    let mut preview = BundleImportPreview {
        app_id,
        ..BundleImportPreview::default()
    };
    for (item_id, item) in &items {
        match store.entry(app_id, item_id) {
            Some(existing) => {
                preview.updated.push(item_id.clone());
                if existing.enabled != item.enabled {
                    preview.enabled_changes.push(item_id.clone());
                }
                if existing.load_order != item.load_order {
                    preview.load_order_changes.push(item_id.clone());
                }
            }
            None => preview.added.push(item_id.clone()),
        }
    }
    preview.unlisted = store
        .entries
        .iter()
        .filter(|entry| {
            entry.app_id == app_id
                && !super::normalize_workshop_id(&entry.item_id)
                    .is_some_and(|item_id| incoming.contains(&item_id))
        })
        .map(|entry| entry.item_id.clone())
        .collect();
    preview
        .unlisted
        .sort_by_key(|id| id.parse::<u64>().unwrap_or(u64::MAX));
    Ok(preview)
}

pub fn import_bundle_with_mode(
    space_dir: &Path,
    app_id: u32,
    path: &Path,
    restore_payloads: bool,
    mode: BundleImportMode,
) -> Result<BundleImportSummary, String> {
    let manifest = read_manifest(path)?;
    if manifest.app_id != app_id {
        return Err(format!(
            "Share bundle targets Steam app {} but the active game uses app {}",
            manifest.app_id, app_id
        ));
    }

    let items = normalized_bundle_items(&manifest)?;
    let mut summary = BundleImportSummary {
        app_id,
        ..BundleImportSummary::default()
    };
    let mut store = super::load_store(space_dir)?;
    let incoming = items
        .iter()
        .map(|(item_id, _)| item_id.clone())
        .collect::<HashSet<_>>();
    if mode == BundleImportMode::Match {
        for entry in store.entries.iter_mut().filter(|entry| {
            entry.app_id == app_id
                && !super::normalize_workshop_id(&entry.item_id)
                    .is_some_and(|item_id| incoming.contains(&item_id))
        }) {
            entry.enabled = false;
            entry.updated_at = super::unix_timestamp_now();
        }
    }

    let mut payload_import = if restore_payloads {
        Some(extract_payloads(space_dir, path, &items)?)
    } else {
        None
    };
    if let Some(payloads) = &mut payload_import {
        payloads.promote()?;
        summary.restored_payloads = payloads.restored_ids.clone();
    }

    for (item_id, item) in &items {
        let final_payload = payload_import
            .as_ref()
            .and_then(|payloads| payloads.final_paths.get(item_id));
        let restored = final_payload.is_some();
        let imported = SteamWorkshopItem {
            source: super::STEAM_SOURCE.to_string(),
            app_id,
            item_id: item_id.clone(),
            title: item.title.clone(),
            url: item
                .url
                .clone()
                .unwrap_or_else(|| super::workshop_url(item_id)),
            enabled: item.enabled,
            frozen: restored,
            load_order: item.load_order,
            version: item.version.clone(),
            installed_path: None,
            frozen_path: final_payload.map(|path| path.display().to_string()),
            size_bytes: item.size_bytes,
            time_updated: item.time_updated,
            added_at: super::unix_timestamp_now(),
            updated_at: super::unix_timestamp_now(),
        };
        let mut imported = imported;
        let added = store.entry(app_id, item_id).is_none();
        if let Some(existing) = store.entry(app_id, item_id) {
            imported.installed_path = existing.installed_path.clone();
            imported.added_at = existing.added_at;
        }
        if mode == BundleImportMode::Merge
            && let Some(existing) = store.entry(app_id, item_id)
            && !restored
        {
            imported.frozen = existing.frozen;
            imported.frozen_path = existing.frozen_path.clone();
        }
        let needs_download = !restored && !imported.frozen;
        if let Some(existing) = store.entry_mut(app_id, item_id) {
            *existing = imported;
        } else {
            store.entries.push(imported);
        }
        if added {
            summary.added.push(item_id.clone());
        } else {
            summary.updated.push(item_id.clone());
        }
        if needs_download {
            summary.needs_download.push(item_id.clone());
        }
    }
    if let Err(error) = super::save_store(space_dir, &store) {
        if let Some(payloads) = &mut payload_import
            && let Err(rollback_error) = payloads.rollback_promoted()
        {
            return Err(format!(
                "{error}; payload rollback failed: {rollback_error}"
            ));
        }
        return Err(error);
    }
    if let Some(payloads) = payload_import {
        payloads.commit();
    }
    Ok(summary)
}

fn normalized_bundle_items(
    manifest: &BundleManifest,
) -> Result<Vec<(String, &BundleItem)>, String> {
    let mut seen = HashSet::new();
    manifest
        .items
        .iter()
        .map(|item| {
            let item_id = super::normalize_workshop_id(&item.item_id)
                .ok_or_else(|| format!("Invalid Steam Workshop item id {}", item.item_id))?;
            if !seen.insert(item_id.clone()) {
                return Err(format!(
                    "Duplicate Steam Workshop item id {item_id} in bundle"
                ));
            }
            Ok((item_id, item))
        })
        .collect()
}

struct PayloadImport {
    space_dir: PathBuf,
    staging_root: PathBuf,
    restored_ids: Vec<String>,
    final_paths: HashMap<String, PathBuf>,
    promoted_paths: Vec<PathBuf>,
    committed: bool,
}

impl PayloadImport {
    fn promote(&mut self) -> Result<(), String> {
        let result = (|| {
            for item_id in &self.restored_ids {
                let source = self.staging_root.join(item_id);
                let base = imported_payload_dir(&self.space_dir, item_id);
                validate_payload_path(&self.space_dir, &base)?;
                fs::create_dir_all(&base).map_err(|err| {
                    format!("Failed to create {}: {err}", sanitize_log_path(&base))
                })?;
                validate_payload_path(&self.space_dir, &base)?;
                sync_parent(&base)
                    .map_err(|err| format!("Failed to sync payload directory: {err}"))?;
                let target = self
                    .final_paths
                    .get(item_id)
                    .expect("path prepared for item");
                if fs::symlink_metadata(target).is_ok() {
                    return Err(format!(
                        "Bundle payload destination already exists: {}",
                        sanitize_log_path(target)
                    ));
                }
                fs::rename(&source, target).map_err(|err| {
                    format!("Failed to place {}: {err}", sanitize_log_path(target))
                })?;
                self.promoted_paths.push(target.clone());
                sync_parent(target)
                    .map_err(|err| format!("Failed to sync payload directory: {err}"))?;
                sync_parent(&source)
                    .map_err(|err| format!("Failed to sync staging directory: {err}"))?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            return match self.rollback_promoted() {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(format!(
                    "{error}; payload rollback failed: {rollback_error}"
                )),
            };
        }
        Ok(())
    }

    fn rollback_promoted(&mut self) -> Result<(), String> {
        let mut errors = Vec::new();
        for path in self.promoted_paths.drain(..).rev() {
            if let Err(error) = fs::remove_dir_all(&path) {
                errors.push(format!("{}: {error}", sanitize_log_path(&path)));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for PayloadImport {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.staging_root);
        if !self.committed {
            let _ = self.rollback_promoted();
        }
    }
}

fn extract_payloads(
    space_dir: &Path,
    path: &Path,
    items: &[(String, &BundleItem)],
) -> Result<PayloadImport, String> {
    validate_payload_roots(space_dir, items)?;
    let frozen_root = space_dir.join(super::WORKSHOP_DIR).join(super::FROZEN_DIR);
    fs::create_dir_all(&frozen_root).map_err(|err| {
        format!(
            "Failed to create {}: {err}",
            sanitize_log_path(&frozen_root)
        )
    })?;
    let token = unique_import_token();
    let staging_root = frozen_root.join(format!(".bundle-import-{token}"));
    fs::create_dir(&staging_root)
        .map_err(|err| format!("Failed to create bundle staging directory: {err}"))?;
    let mut payload_import = PayloadImport {
        space_dir: space_dir.to_path_buf(),
        staging_root,
        restored_ids: Vec::new(),
        final_paths: HashMap::new(),
        promoted_paths: Vec::new(),
        committed: false,
    };
    let allowed_payloads = items
        .iter()
        .filter(|(_, item)| item.payload)
        .map(|(id, _)| id.as_str())
        .collect::<HashSet<_>>();
    let result = (|| {
        let mut archive = open_bundle(path)?;
        let mut extracted_bytes: u64 = 0;
        let mut restored = HashSet::new();
        for index in 0..archive.len() {
            let mut file = archive
                .by_index(index)
                .map_err(|err| format!("Failed to read zip entry {}: {}", index, err))?;
            let name = file
                .name()
                .map_err(|err| format!("Failed to read zip entry {} name: {}", index, err))?
                .replace('\\', "/");
            let Some(rest) = name.strip_prefix(FROZEN_PREFIX) else {
                continue;
            };
            let Some((item_id, relative)) = rest.split_once('/') else {
                continue;
            };
            let Some(item_id) = super::normalize_workshop_id(item_id) else {
                return Err(format!("Unsafe frozen payload path in bundle: {}", name));
            };
            if !allowed_payloads.contains(item_id.as_str()) {
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

            let target = payload_import.staging_root.join(&item_id).join(relative);
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
            restored.insert(item_id);
        }
        payload_import.restored_ids = items
            .iter()
            .filter_map(|(id, _)| restored.contains(id).then_some(id.clone()))
            .collect();
        sync_payload_tree(&payload_import.staging_root)
            .map_err(|err| format!("Failed to sync staged payloads: {err}"))?;
        let token = payload_import
            .staging_root
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(".bundle-import-"))
            .ok_or_else(|| "Invalid bundle staging directory name".to_string())?;
        for item_id in &payload_import.restored_ids {
            let base = imported_payload_dir(space_dir, item_id);
            validate_payload_path(space_dir, &base)?;
            payload_import
                .final_paths
                .insert(item_id.clone(), base.join(token));
        }
        Ok(())
    })();
    if let Err(error) = result {
        drop(payload_import);
        return Err(error);
    }
    Ok(payload_import)
}

fn imported_payload_dir(space_dir: &Path, item_id: &str) -> PathBuf {
    super::frozen_item_root(space_dir, item_id).join(IMPORTED_PAYLOAD_DIR)
}

fn unique_import_token() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = NEXT_IMPORT_ID.fetch_add(1, Ordering::Relaxed);
    format!("{}-{timestamp}-{counter}", std::process::id())
}

fn validate_payload_roots(space_dir: &Path, items: &[(String, &BundleItem)]) -> Result<(), String> {
    validate_payload_path(
        space_dir,
        &space_dir.join(super::WORKSHOP_DIR).join(super::FROZEN_DIR),
    )?;
    for (item_id, item) in items {
        if item.payload {
            validate_payload_path(space_dir, &imported_payload_dir(space_dir, item_id))?;
        }
    }
    Ok(())
}

fn validate_payload_path(space_dir: &Path, path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(space_dir)
        .map_err(|err| format!("Could not inspect game-space directory: {err}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("Workshop payload root must be a real game-space directory".to_string());
    }
    let relative = path
        .strip_prefix(space_dir)
        .map_err(|_| "Workshop payload path is outside the active game space".to_string())?;
    let mut cursor = space_dir.to_path_buf();
    for component in relative.components() {
        cursor.push(component.as_os_str());
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "Workshop payload paths must not contain symbolic links: {}",
                    sanitize_log_path(&cursor)
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!(
                    "Workshop payload path is not a directory: {}",
                    sanitize_log_path(&cursor)
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Could not inspect Workshop payload path {}: {error}",
                    sanitize_log_path(&cursor)
                ));
            }
        }
    }
    Ok(())
}

fn sync_payload_tree(path: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "staged Workshop payload contains a symbolic link",
            ));
        }
        if file_type.is_dir() {
            sync_payload_tree(&entry.path())?;
        } else if file_type.is_file() {
            fs::OpenOptions::new()
                .write(true)
                .open(entry.path())?
                .sync_all()?;
        }
    }
    sync_directory(path)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> std::io::Result<()> {
    Ok(())
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

    fn test_bundle(path: &Path, items: Vec<BundleItem>, payloads: &[(&str, &[u8])]) {
        let file = fs::File::create(path).expect("bundle file");
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        let manifest = BundleManifest {
            schema_version: BUNDLE_SCHEMA_VERSION,
            kind: BUNDLE_KIND.to_string(),
            game_id: "twwh3".to_string(),
            app_id: 1142710,
            created_at: 0,
            note: None,
            share_code: String::new(),
            state_checksum: None,
            items,
        };
        let bytes = serde_json::to_vec(&manifest).expect("manifest JSON");
        zip.start_file(BUNDLE_MANIFEST_NAME, options)
            .expect("manifest entry");
        zip.write_all(&bytes).expect("manifest bytes");
        for (name, bytes) in payloads {
            zip.start_file(name, options).expect("payload entry");
            zip.write_all(bytes).expect("payload bytes");
        }
        zip.finish().expect("finish bundle");
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
        let store = super::super::load_store(target_space.path()).expect("target store");
        let restored_root = PathBuf::from(
            store
                .entry(1142710, "111")
                .and_then(|entry| entry.frozen_path.as_deref())
                .expect("frozen path"),
        );
        let restored_pack = restored_root.join("alpha.pack");
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

    #[test]
    fn match_import_disables_unlisted_items_and_keeps_their_payloads() {
        let source = tempfile::tempdir().expect("source");
        let target = tempfile::tempdir().expect("target");
        let output = tempfile::tempdir().expect("output");
        managed_item(source.path(), "111", true);
        managed_item(target.path(), "111", false);
        managed_item(target.path(), "222", true);
        let payload = super::super::frozen_item_root(target.path(), "222");
        fs::create_dir_all(&payload).expect("payload folder");
        fs::write(payload.join("keep.pack"), b"payload").expect("payload");
        let mut store = super::super::load_store(target.path()).expect("store");
        let unlisted = store.entry_mut(1142710, "222").expect("entry");
        unlisted.frozen = true;
        unlisted.frozen_path = Some(payload.display().to_string());
        super::super::save_store(target.path(), &store).expect("save");

        let bundle_path = output.path().join("match.foxyshare");
        export_bundle(
            source.path(),
            "twwh3",
            1142710,
            &bundle_path,
            BundleExportOptions::default(),
            None,
            None,
        )
        .expect("export");
        let preview = preview_import_bundle(target.path(), 1142710, &bundle_path).expect("preview");
        assert_eq!(preview.updated, vec!["111"]);
        assert_eq!(preview.unlisted, vec!["222"]);
        import_bundle_with_mode(
            target.path(),
            1142710,
            &bundle_path,
            false,
            BundleImportMode::Match,
        )
        .expect("match");

        let store = super::super::load_store(target.path()).expect("store");
        assert!(store.entry(1142710, "111").expect("listed").enabled);
        let unlisted = store.entry(1142710, "222").expect("unlisted");
        assert!(!unlisted.enabled);
        assert!(unlisted.frozen);
        assert_eq!(
            fs::read(payload.join("keep.pack")).expect("kept payload"),
            b"payload"
        );
    }

    #[test]
    fn merge_import_applies_incoming_settings_and_retains_unlisted_selection() {
        let source = tempfile::tempdir().expect("source");
        let target = tempfile::tempdir().expect("target");
        let output = tempfile::tempdir().expect("output");
        managed_item(source.path(), "111", true);
        super::super::set_item_load_order(source.path(), 1142710, "111", Some(2)).expect("order");
        managed_item(target.path(), "111", false);
        managed_item(target.path(), "222", true);
        super::super::set_item_load_order(target.path(), 1142710, "111", Some(9)).expect("order");
        let bundle_path = output.path().join("merge.foxyshare");
        export_bundle(
            source.path(),
            "twwh3",
            1142710,
            &bundle_path,
            BundleExportOptions::default(),
            None,
            None,
        )
        .expect("export");

        import_bundle_with_mode(
            target.path(),
            1142710,
            &bundle_path,
            false,
            BundleImportMode::Merge,
        )
        .expect("merge");

        let store = super::super::load_store(target.path()).expect("store");
        let item = store.entry(1142710, "111").expect("item");
        assert!(item.enabled);
        assert_eq!(item.load_order, Some(2));
        assert!(store.entry(1142710, "222").unwrap().enabled);
    }

    #[test]
    fn preview_and_match_normalize_workshop_ids_before_comparing() {
        let target = tempfile::tempdir().expect("target");
        let output = tempfile::tempdir().expect("output");
        managed_item(target.path(), "111", false);
        let path = output.path().join("leading-zero-id.foxyshare");
        test_bundle(
            &path,
            vec![BundleItem {
                item_id: "000111".to_string(),
                title: Some("Imported".to_string()),
                url: None,
                enabled: true,
                load_order: Some(3),
                version: None,
                size_bytes: None,
                time_updated: None,
                payload: false,
            }],
            &[],
        );

        let preview = preview_import_bundle(target.path(), 1142710, &path).expect("preview");
        assert_eq!(preview.updated, vec!["111"]);
        assert!(preview.unlisted.is_empty());
        import_bundle_with_mode(
            target.path(),
            1142710,
            &path,
            false,
            BundleImportMode::Match,
        )
        .expect("match import");
        let store = super::super::load_store(target.path()).expect("store");
        assert_eq!(store.entries.len(), 1);
        assert!(store.entry(1142710, "111").expect("item").enabled);
    }

    #[test]
    fn malformed_late_payload_keeps_previous_imported_files_and_cleans_staging() {
        let target = tempfile::tempdir().expect("target");
        let output = tempfile::tempdir().expect("output");
        let previous = imported_payload_dir(target.path(), "111");
        fs::create_dir_all(&previous).expect("previous import");
        fs::write(previous.join("existing.pack"), b"previous complete payload")
            .expect("previous payload");
        let path = output.path().join("malformed-late.foxyshare");
        test_bundle(
            &path,
            vec![BundleItem {
                item_id: "111".to_string(),
                title: None,
                url: None,
                enabled: true,
                load_order: None,
                version: None,
                size_bytes: None,
                time_updated: None,
                payload: true,
            }],
            &[
                ("frozen/111/new.pack", b"new payload"),
                ("frozen/222/invalid.pack", b"not listed"),
            ],
        );

        let error =
            import_bundle_with_mode(target.path(), 1142710, &path, true, BundleImportMode::Merge)
                .expect_err("unlisted late entry is rejected");
        assert!(error.contains("unlisted Workshop item"));
        assert_eq!(
            fs::read(previous.join("existing.pack")).expect("prior file remains"),
            b"previous complete payload"
        );
        let frozen_root = target.path().join("workshop/frozen");
        assert!(
            !fs::read_dir(frozen_root)
                .expect("frozen root")
                .filter_map(Result::ok)
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".bundle-import-"))
        );
        assert!(
            super::super::load_store(target.path())
                .expect("unchanged store")
                .entries
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn payload_import_refuses_symlinked_destination_without_writing_outside_space() {
        use std::os::unix::fs::symlink;

        let target = tempfile::tempdir().expect("target");
        let output = tempfile::tempdir().expect("output");
        let external = output.path().join("external");
        fs::create_dir_all(&external).expect("external directory");
        fs::write(external.join("old.pack"), b"external data").expect("external file");
        let item_root = super::super::frozen_item_root(target.path(), "111");
        fs::create_dir_all(item_root.parent().expect("frozen root")).expect("frozen root");
        symlink(&external, imported_payload_dir(target.path(), "111"))
            .expect("imported path symlink");
        let path = output.path().join("symlink.foxyshare");
        test_bundle(
            &path,
            vec![BundleItem {
                item_id: "111".to_string(),
                title: None,
                url: None,
                enabled: true,
                load_order: None,
                version: None,
                size_bytes: None,
                time_updated: None,
                payload: true,
            }],
            &[("frozen/111/new.pack", b"replacement")],
        );

        let error =
            import_bundle_with_mode(target.path(), 1142710, &path, true, BundleImportMode::Merge)
                .expect_err("symlinked path is refused");
        assert!(error.contains("symbolic links"));
        assert_eq!(
            fs::read(external.join("old.pack")).expect("external remains"),
            b"external data"
        );
        assert!(!external.join("new.pack").exists());
    }
}
