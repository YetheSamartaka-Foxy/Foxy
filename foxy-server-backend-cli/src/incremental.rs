use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::UNIX_EPOCH;

use crate::cli::GenerationMode;
use crate::types::{DiscoveredFile, ProcessedMod, ResolvedMod};

const CACHE_FILE: &str = ".foxy-hash-cache.json";

#[derive(Default, Deserialize, Serialize)]
pub struct HashCache {
    entries: BTreeMap<String, CacheEntry>,
}

#[derive(Deserialize, Serialize)]
struct CacheEntry {
    fingerprint: String,
    processed: ProcessedMod,
    #[serde(default)]
    output_stamps: Vec<u128>,
}

impl HashCache {
    pub fn load(root: &Path) -> Self {
        std::fs::read(root.join(CACHE_FILE))
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    pub fn reusable(
        &self,
        item: &ResolvedMod,
        root: &Path,
        fingerprint: &str,
    ) -> Option<ProcessedMod> {
        let entry = self.entries.get(&item.mod_name)?;
        if entry.fingerprint != fingerprint {
            return None;
        }
        if !root.join(&item.mod_name).is_dir() {
            return None;
        }
        if entry.output_stamps.len() != entry.processed.files.len() {
            return None;
        }
        if !entry
            .processed
            .files
            .iter()
            .zip(&entry.output_stamps)
            .all(|(file, stamp)| {
                let path = root.join(&item.mod_name).join(&file.relative_path);
                std::fs::symlink_metadata(&path).is_ok_and(|meta| {
                    !meta.file_type().is_symlink()
                        && meta.is_file()
                        && meta.len() == file.length
                        && modified_stamp(&meta).ok() == Some(*stamp)
                })
            })
        {
            return None;
        }
        let mut processed = entry.processed.clone();
        processed.is_required = item.is_required;
        processed.enabled = item.enabled;
        processed.client_side = item.client_side;
        Some(processed)
    }

    pub fn store(
        &mut self,
        name: &str,
        fingerprint: String,
        processed: ProcessedMod,
        root: &Path,
    ) -> Result<()> {
        let output_stamps = processed
            .files
            .iter()
            .map(|file| {
                modified_stamp(&std::fs::metadata(
                    root.join(name).join(&file.relative_path),
                )?)
            })
            .collect::<Result<Vec<_>>>()?;
        self.entries.insert(
            name.to_string(),
            CacheEntry {
                fingerprint,
                processed,
                output_stamps,
            },
        );
        Ok(())
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let path = root.join(CACHE_FILE);
        let data = serde_json::to_vec(self)?;
        std::fs::write(&path, data)
            .with_context(|| format!("Failed to write hash cache {}", path.display()))
    }
}

pub fn fingerprint(
    item: &ResolvedMod,
    files: &[DiscoveredFile],
    mode: GenerationMode,
    prune: bool,
) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(
        std::fs::canonicalize(&item.source_path)?
            .to_string_lossy()
            .as_bytes(),
    );
    hasher.update(&[mode as u8, u8::from(prune)]);
    for file in files {
        hasher.update(file.relative_path.as_bytes());
        hasher.update(&file.file_size.to_le_bytes());
        let modified = modified_stamp(&std::fs::metadata(&file.absolute_path)?)?;
        hasher.update(&modified.to_le_bytes());
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn modified_stamp(meta: &std::fs::Metadata) -> Result<u128> {
    Ok(meta.modified()?.duration_since(UNIX_EPOCH)?.as_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_changes_when_source_file_length_changes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("@mod");
        std::fs::create_dir(&source).unwrap();
        let file = source.join("data.txt");
        std::fs::write(&file, b"one").unwrap();
        let item = ResolvedMod {
            mod_name: "@mod".to_string(),
            source_path: source.clone(),
            is_required: true,
            enabled: true,
            client_side: false,
        };
        let before = fingerprint(
            &item,
            &crate::discover::discover_files(&source).unwrap(),
            GenerationMode::Foxy,
            false,
        )
        .unwrap();
        std::fs::write(&file, b"longer").unwrap();
        let after = fingerprint(
            &item,
            &crate::discover::discover_files(&source).unwrap(),
            GenerationMode::Foxy,
            false,
        )
        .unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn cache_reuses_existing_output_and_rejects_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("@mod");
        let output = dir.path().join("output");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&output).unwrap();
        std::fs::write(source.join("data.txt"), b"data").unwrap();
        let item = ResolvedMod {
            mod_name: "@mod".to_string(),
            source_path: source.clone(),
            is_required: true,
            enabled: true,
            client_side: false,
        };
        crate::hash::process_mods(
            std::slice::from_ref(&item),
            Some(&output),
            &indicatif::ProgressBar::hidden(),
            GenerationMode::Foxy,
            false,
            true,
        )
        .unwrap();
        let cache = HashCache::load(&output);
        let fingerprint = fingerprint(
            &item,
            &crate::discover::discover_files(&source).unwrap(),
            GenerationMode::Foxy,
            false,
        )
        .unwrap();
        assert!(cache.reusable(&item, &output, &fingerprint).is_some());
        std::fs::remove_file(output.join("@mod").join("data.txt")).unwrap();
        assert!(cache.reusable(&item, &output, &fingerprint).is_none());
    }
}
