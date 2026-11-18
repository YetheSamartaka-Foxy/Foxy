macro_rules! println {
    ($($arg:tt)*) => {
        if crate::output::json_mode() {
            std::eprintln!($($arg)*);
        } else {
            std::println!($($arg)*);
        }
    };
}

#[doc(hidden)]
pub mod artifacts;
#[doc(hidden)]
pub mod build_info;
#[doc(hidden)]
pub mod changelog_parser;
#[doc(hidden)]
pub mod cli;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod discover;
#[doc(hidden)]
pub mod hash;
#[doc(hidden)]
pub mod incremental;
#[doc(hidden)]
pub mod keys;
#[doc(hidden)]
pub mod mod_line;
#[doc(hidden)]
pub mod mod_line_files;
#[doc(hidden)]
pub mod operations;
#[doc(hidden)]
pub mod output;
#[doc(hidden)]
pub mod planner;
#[doc(hidden)]
pub mod published;
#[doc(hidden)]
pub mod report;
#[doc(hidden)]
pub mod space;
#[doc(hidden)]
pub mod srf;
#[doc(hidden)]
pub mod staging;
#[doc(hidden)]
pub mod types;
#[doc(hidden)]
pub mod update_manifest;
#[doc(hidden)]
pub mod verify;

use anyhow::{Context, bail};
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

pub struct KeyCollectionRequest {
    pub enabled: bool,
    pub dest: Option<PathBuf>,
    pub additional_sources: Vec<PathBuf>,
}

#[doc(hidden)]
pub fn configure_thread_pool(threads: usize) -> anyhow::Result<()> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global()
        .context("Failed to configure thread pool")
}

#[doc(hidden)]
pub fn progress_bar(no_progress: bool) -> ProgressBar {
    if no_progress {
        return ProgressBar::hidden();
    }
    let progress = ProgressBar::new(0);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} files ({per_sec})")
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("=> "),
    );
    progress
}

#[derive(Debug, Clone)]
pub struct PlayerFile {
    pub relative_path: PathBuf,
    pub bytes: u64,
    pub blake3: String,
}

#[derive(Debug, Clone)]
pub struct PlayerMod {
    pub name: String,
    pub required: bool,
    pub root: PathBuf,
    pub files: Vec<PlayerFile>,
}

pub use verify::VerificationReport;

pub fn verify_repository(output: &Path) -> anyhow::Result<VerificationReport> {
    verify::verify_report(output)
}

pub fn verify_repository_controlled(
    output: &Path,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> anyhow::Result<VerificationReport> {
    verify::verify_report_controlled(output, cancelled)
}

pub fn write_player_repository(
    stage: &Path,
    repo_name: &str,
    mods: &[PlayerMod],
    cancelled: &(dyn Fn() -> bool + Sync),
    progress: &(dyn Fn(u64) + Sync),
) -> anyhow::Result<()> {
    use cli::GenerationMode;
    use types::{Chunking, DiscoveredFile, ProcessedMod, ResolvedMod};

    let mut processed = Vec::with_capacity(mods.len());
    let mut sources = Vec::with_capacity(mods.len());
    let mut mod_names = BTreeSet::new();
    for item in mods {
        if cancelled() {
            bail!("Repository generation cancelled");
        }
        if item.name.is_empty()
            || Path::new(&item.name).components().count() != 1
            || !matches!(
                Path::new(&item.name).components().next(),
                Some(Component::Normal(_))
            )
        {
            bail!("Invalid player mod name");
        }
        if !mod_names.insert(item.name.to_ascii_lowercase()) {
            bail!("Duplicate player mod name: {}", item.name);
        }
        let root = std::fs::canonicalize(&item.root).with_context(|| {
            format!("Failed to inspect player mod root {}", item.root.display())
        })?;
        sources.push(ResolvedMod {
            mod_name: item.name.clone(),
            source_path: root.clone(),
            is_required: item.required,
            enabled: item.required,
            client_side: false,
            chunking: Chunking::Fixed,
        });
        let mut files = Vec::with_capacity(item.files.len());
        let mut relative_paths = BTreeSet::new();
        for (order, file) in item.files.iter().enumerate() {
            if cancelled() {
                bail!("Repository generation cancelled");
            }
            if file
                .relative_path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
                || file.relative_path.as_os_str().is_empty()
            {
                bail!("Invalid allowlisted relative path");
            }
            if !relative_paths.insert(file.relative_path.to_string_lossy().to_ascii_lowercase()) {
                bail!(
                    "Duplicate allowlisted relative path: {}",
                    file.relative_path.display()
                );
            }
            let source = root.join(&file.relative_path);
            let metadata = std::fs::symlink_metadata(&source).with_context(|| {
                format!(
                    "Failed to inspect allowlisted file {}",
                    file.relative_path.display()
                )
            })?;
            if !metadata.file_type().is_file() || metadata.len() != file.bytes {
                bail!(
                    "Allowlisted file length or type mismatch: {}",
                    file.relative_path.display()
                );
            }
            let canonical_source = std::fs::canonicalize(&source)?;
            if !canonical_source.starts_with(&root) {
                bail!(
                    "Allowlisted path escapes player mod root: {}",
                    file.relative_path.display()
                );
            }
            let relative = file.relative_path.to_string_lossy().replace('\\', "/");
            let discovered = DiscoveredFile {
                absolute_path: source.clone(),
                relative_path: relative,
                file_size: file.bytes,
            };
            let dest = stage.join(&item.name).join(&file.relative_path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let hashed = hash::copy_and_hash_file_controlled(
                &source,
                Some(&dest),
                &discovered,
                order,
                GenerationMode::Foxy,
                Chunking::Fixed,
                hash::HashControl {
                    cancelled,
                    progress,
                },
            )?;
            let staged_metadata = std::fs::metadata(&dest)?;
            let staged_blake3 = hash::hash_file_blake3_controlled(&dest, cancelled)?;
            if staged_metadata.len() != file.bytes
                || !staged_blake3.eq_ignore_ascii_case(&file.blake3)
                || hashed.length != file.bytes
            {
                bail!(
                    "Published allowlisted file failed checksum or length verification: {}",
                    file.relative_path.display()
                );
            }
            files.push(hashed);
        }
        let checksums = hash::compute_mod_checksums(&files, GenerationMode::Foxy);
        processed.push(ProcessedMod {
            mod_name: item.name.clone(),
            checksums,
            files,
            is_required: item.required,
            enabled: item.required,
            client_side: false,
        });
    }

    if cancelled() {
        bail!("Repository generation cancelled");
    }

    artifacts::write_mod_manifests(&processed, stage, GenerationMode::Foxy)?;
    let config: types::RepoConfig = serde_json::from_value(serde_json::json!({
        "repoName": repo_name,
        "basePath": "",
        "requiredMods": [],
        "optionalMods": []
    }))?;
    artifacts::write_repo_manifests(&config, &processed, stage, GenerationMode::Foxy, None)?;
    let server_line = crate::mod_line::render_launch_params(&crate::mod_line::build_launch_params(
        &config,
        &processed,
        &sources,
        crate::mod_line::ModLineOptions {
            prefix: "",
            include_optional: false,
        },
    ));
    published::write_server_mod_line(stage, &server_line)?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ReforgerModReference {
    pub id: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub required: Option<bool>,
}

pub fn reforger_addon_fragment(mods: &[ReforgerModReference]) -> anyhow::Result<Vec<u8>> {
    let entries: Vec<serde_json::Value> = mods
        .iter()
        .map(|item| {
            let mut entry = serde_json::Map::new();
            entry.insert("modId".into(), item.id.clone().into());
            if let Some(name) = &item.name {
                entry.insert("name".into(), name.clone().into());
            }
            if let Some(version) = &item.version {
                entry.insert("version".into(), version.clone().into());
            }
            if let Some(required) = item.required {
                entry.insert("required".into(), required.into());
            }
            serde_json::Value::Object(entry)
        })
        .collect();
    Ok(serde_json::to_vec_pretty(
        &serde_json::json!({"game":{"mods":entries}}),
    )?)
}

#[cfg(test)]
mod api_tests {
    use super::*;

    #[test]
    fn reforger_fragment_keeps_optional_fields_absent() {
        let bytes = reforger_addon_fragment(&[ReforgerModReference {
            id: "guid".into(),
            name: None,
            version: None,
            required: None,
        }])
        .unwrap();
        assert_eq!(
            bytes,
            br#"{
  "game": {
    "mods": [
      {
        "modId": "guid"
      }
    ]
  }
}"#
        );
    }

    #[test]
    fn writer_copies_only_allowlisted_files_and_verifies_output() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let stage = dir.path().join("stage");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(source.join("allowed.pbo"), b"allowed").unwrap();
        std::fs::write(source.join("secret.bikey"), b"not allowlisted").unwrap();
        let digest =
            hash::hash_file_blake3_controlled(&source.join("allowed.pbo"), &|| false).unwrap();
        write_player_repository(
            &stage,
            "MainRepo",
            &[PlayerMod {
                name: "@mod_01".into(),
                required: false,
                root: source,
                files: vec![PlayerFile {
                    relative_path: "allowed.pbo".into(),
                    bytes: 7,
                    blake3: digest,
                }],
            }],
            &|| false,
            &|_| {},
        )
        .unwrap();
        let published = stage.join("@mod_01");
        assert!(published.join("allowed.pbo").is_file());
        assert!(!published.join("secret.bikey").exists());
        let addon: serde_json::Value =
            serde_json::from_slice(&std::fs::read(published.join("foxy_addon.json")).unwrap())
                .unwrap();
        assert_eq!(addon["files"].as_array().unwrap().len(), 1);
        let repo: serde_json::Value =
            serde_json::from_slice(&std::fs::read(stage.join("repo.json")).unwrap()).unwrap();
        assert_eq!(repo["foxyMode"], "FoxyModeV1");
        assert_eq!(verify_repository(&stage).unwrap().mods, 1);
    }

    #[test]
    fn writer_rejects_cancelled_or_modified_allowlisted_input() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let stage = dir.path().join("stage");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&stage).unwrap();
        let file = source.join("file.pbo");
        std::fs::write(&file, b"content").unwrap();
        let item = PlayerMod {
            name: "@mod_02".into(),
            required: true,
            root: source,
            files: vec![PlayerFile {
                relative_path: "file.pbo".into(),
                bytes: 7,
                blake3: hash::hash_file_blake3_controlled(&file, &|| false).unwrap(),
            }],
        };
        assert!(
            write_player_repository(
                &stage,
                "MainRepo",
                std::slice::from_ref(&item),
                &|| true,
                &|_| {}
            )
            .is_err()
        );
        std::fs::write(&file, b"changed").unwrap();
        assert!(write_player_repository(&stage, "MainRepo", &[item], &|| false, &|_| {}).is_err());
    }
}
