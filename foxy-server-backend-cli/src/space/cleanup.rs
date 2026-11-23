use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use super::{LoadedSpace, SharedMod};

const INVENTORY_FILE: &str = ".foxy-generated-pool.json";

#[derive(Serialize, Deserialize)]
struct PoolInventory {
    pool_id: String,
    mods: Vec<String>,
    repositories: Vec<String>,
}

pub struct CleanupPlan {
    links: Vec<PathBuf>,
    mods: Vec<PathBuf>,
}

impl CleanupPlan {
    pub fn links(&self) -> &[PathBuf] {
        &self.links
    }

    pub fn mods(&self) -> &[PathBuf] {
        &self.mods
    }

    pub fn print(&self, preview: bool) {
        if self.links.is_empty() && self.mods.is_empty() {
            println!("Pool cleanup: nothing to remove");
            return;
        }
        if preview {
            println!("Pool cleanup would remove:");
        } else {
            println!("Pool cleanup removed:");
        }
        for link in &self.links {
            println!("  symlink: {}", link.display());
        }
        for dir in &self.mods {
            println!("  mod folder: {}", dir.display());
        }
    }

    pub fn execute(&self) -> Result<()> {
        for link in &self.links {
            super::remove_symlink(link)
                .with_context(|| format!("Failed to remove symlink {}", link.display()))?;
        }
        for dir in &self.mods {
            std::fs::remove_dir_all(dir)
                .with_context(|| format!("Failed to remove pool mod {}", dir.display()))?;
        }
        Ok(())
    }
}

pub fn plan_cleanup(
    output_dir: &Path,
    pool_dir: &Path,
    repos: &[super::SpaceRepo],
    groups: &[SharedMod],
) -> Result<CleanupPlan> {
    refuse_symlink_dir(output_dir)?;
    refuse_symlink_dir(pool_dir)?;
    let expected: BTreeSet<&str> = groups.iter().map(|group| group.name.as_str()).collect();
    let pool_id = normalized_absolute(pool_dir)?;
    let pool_is_internal = pool_id.starts_with(normalized_absolute(output_dir)?);
    let inventory = read_inventory(output_dir)?;
    let previous: BTreeSet<String> = inventory
        .filter(|record| record.pool_id == path_id(&pool_id))
        .map(|record| record.mods.into_iter().collect())
        .unwrap_or_default();
    if !pool_is_internal && previous.is_empty() {
        bail!("Cleanup of a pool outside the output requires a prior generated pool inventory");
    }
    let mut orphan_names = BTreeSet::new();
    let mut mod_dirs = Vec::new();

    if pool_dir.exists() {
        for entry in std::fs::read_dir(pool_dir)
            .with_context(|| format!("Failed to read pool {}", pool_dir.display()))?
        {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if contains_name(&expected, &name) {
                continue;
            }
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                continue;
            }
            let generated = contains_name(&previous, &name)
                || (pool_is_internal
                    && (path.join("foxy_addon.json").is_file() || path.join("mod.srf").is_file()));
            if generated {
                orphan_names.insert(name);
                mod_dirs.push(path);
            }
        }
    }

    let mut links = Vec::new();
    if output_dir.exists() {
        for repo in std::fs::read_dir(output_dir)
            .with_context(|| format!("Failed to read output {}", output_dir.display()))?
        {
            let repo = repo?;
            let repo_path = repo.path();
            let metadata = std::fs::symlink_metadata(&repo_path)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                continue;
            }
            if normalized_absolute(&repo_path)? == pool_id {
                continue;
            }
            let repo_folder = repo.file_name().to_string_lossy().into_owned();
            let repo_expected: Option<BTreeSet<&str>> = repos
                .iter()
                .find(|configured| same_name(&configured.folder, &repo_folder))
                .map(|configured| {
                    configured
                        .mods
                        .iter()
                        .map(|item| item.mod_name.as_str())
                        .collect()
                });
            for entry in std::fs::read_dir(&repo_path)? {
                let entry = entry?;
                let path = entry.path();
                if !std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
                    continue;
                }
                let target = std::fs::read_link(&path)?;
                let target = if target.is_absolute() {
                    target
                } else {
                    repo_path.join(target)
                };
                let target_id = normalized_absolute(&target)?;
                if target_id.parent() != Some(pool_id.as_path()) {
                    continue;
                }
                let Some(name) = target_id.file_name().and_then(|value| value.to_str()) else {
                    continue;
                };
                if !same_name(&path.file_name().unwrap().to_string_lossy(), name) {
                    continue;
                }
                if let Some(repo_mods) = &repo_expected {
                    if !contains_name(repo_mods, name) {
                        links.push(path);
                    }
                    continue;
                }
                if contains_name(&expected, name) {
                    continue;
                }
                if contains_name(&orphan_names, name)
                    || contains_name(&previous, name)
                    || !target_id.exists()
                {
                    links.push(path);
                }
            }
        }
    }
    links.sort();
    mod_dirs.sort();
    Ok(CleanupPlan {
        links,
        mods: mod_dirs,
    })
}

fn same_name(left: &str, right: &str) -> bool {
    left == right || (cfg!(windows) && left.eq_ignore_ascii_case(right))
}

fn contains_name<S: AsRef<str> + Ord>(names: &BTreeSet<S>, name: &str) -> bool {
    names.iter().any(|known| same_name(known.as_ref(), name))
}

pub fn write_inventory(
    output_dir: &Path,
    pool_dir: &Path,
    space: &LoadedSpace,
    groups: &[SharedMod],
    cleaned: bool,
) -> Result<()> {
    let pool_id = path_id(&normalized_absolute(pool_dir)?);
    let mut mods: BTreeSet<String> = groups.iter().map(|group| group.name.clone()).collect();
    if !cleaned
        && let Some(previous) = read_inventory(output_dir)?
        && previous.pool_id == pool_id
    {
        mods.extend(previous.mods);
    }
    let inventory = PoolInventory {
        pool_id,
        mods: mods.into_iter().collect(),
        repositories: space.repos.iter().map(|repo| repo.folder.clone()).collect(),
    };
    let path = output_dir.join(INVENTORY_FILE);
    let data = serde_json::to_vec_pretty(&inventory)?;
    std::fs::write(&path, data).with_context(|| format!("Failed to write {}", path.display()))
}

fn path_id(path: &Path) -> String {
    blake3::hash(path.to_string_lossy().as_bytes())
        .to_hex()
        .to_string()
}

fn read_inventory(output_dir: &Path) -> Result<Option<PoolInventory>> {
    let path = output_dir.join(INVENTORY_FILE);
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("Failed to read {}", path.display())),
    };
    serde_json::from_slice(&data)
        .with_context(|| format!("Failed to parse {}", path.display()))
        .map(Some)
}

fn refuse_symlink_dir(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("Cleanup refuses symlink directory {}", path.display())
        }
        Ok(metadata) if !metadata.is_dir() => {
            bail!("Cleanup requires a directory: {}", path.display())
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("Failed to inspect {}", path.display())),
    }
}

fn normalized_absolute(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_only_lists_generated_orphan_pool_mods() {
        let dir = tempfile::tempdir().unwrap();
        let pool = dir.path().join("pool");
        std::fs::create_dir_all(pool.join("@old")).unwrap();
        std::fs::create_dir_all(pool.join("@keep")).unwrap();
        std::fs::create_dir_all(pool.join("user-data")).unwrap();
        std::fs::write(pool.join("@old").join("foxy_addon.json"), b"{}").unwrap();
        std::fs::write(pool.join("@keep").join("foxy_addon.json"), b"{}").unwrap();

        let plan = plan_cleanup(
            dir.path(),
            &pool,
            &[],
            &[SharedMod {
                name: "@keep".to_string(),
                uses: vec![],
            }],
        )
        .unwrap();
        assert_eq!(plan.mods, vec![pool.join("@old")]);
        assert!(plan.links.is_empty());
        plan.execute().unwrap();
        assert!(!pool.join("@old").exists());
        assert!(pool.join("@keep").exists());
        assert!(pool.join("user-data").exists());
    }

    #[test]
    fn cleanup_removes_orphan_links_without_following_them() {
        let dir = tempfile::tempdir().unwrap();
        let pool = dir.path().join("pool");
        let old = pool.join("@old");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(old.join("foxy_addon.json"), b"{}").unwrap();
        let link = repo.join("@old");
        let target = super::super::symlink_target(&link, &old);
        if let Err(err) = super::super::create_dir_symlink(&target, &link) {
            if cfg!(windows) && err.kind() == std::io::ErrorKind::PermissionDenied {
                return;
            }
            panic!("failed to create test symlink: {err}");
        }

        let plan = plan_cleanup(dir.path(), &pool, &[], &[]).unwrap();
        assert_eq!(plan.links, vec![link.clone()]);
        assert_eq!(plan.mods, vec![old.clone()]);
        plan.execute().unwrap();
        assert!(std::fs::symlink_metadata(&link).is_err());
        assert!(!old.exists());
    }

    #[test]
    fn cleanup_removes_shared_mod_link_from_repo_that_dropped_it() {
        let dir = tempfile::tempdir().unwrap();
        let pool = dir.path().join("pool");
        let shared = pool.join("@shared");
        let repo_one = dir.path().join("one");
        let repo_two = dir.path().join("two");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::create_dir_all(&repo_one).unwrap();
        std::fs::create_dir_all(&repo_two).unwrap();
        std::fs::write(shared.join("foxy_addon.json"), b"{}").unwrap();

        let link_one = repo_one.join("@shared");
        let link_two = repo_two.join("@shared");
        for link in [&link_one, &link_two] {
            let target = super::super::symlink_target(link, &shared);
            if let Err(err) = super::super::create_dir_symlink(&target, link) {
                if cfg!(windows) && err.kind() == std::io::ErrorKind::PermissionDenied {
                    return;
                }
                panic!("failed to create test symlink: {err}");
            }
        }

        let make_repo =
            |folder: &str, mods: Vec<crate::types::ResolvedMod>| super::super::SpaceRepo {
                folder: folder.to_string(),
                name: folder.to_string(),
                address: format!("https://repo.example.invalid/{folder}/"),
                required: true,
                config: serde_json::from_str(r#"{"repoName":"Test","basePath":"."}"#).unwrap(),
                config_path: PathBuf::from("config.json"),
                mods,
            };
        let repos = vec![
            make_repo("one", vec![]),
            make_repo(
                "two",
                vec![crate::types::ResolvedMod {
                    mod_name: "@shared".to_string(),
                    source_path: shared.clone(),
                    is_required: true,
                    enabled: true,
                    client_side: false,
                    chunking: crate::types::Chunking::Fixed,
                }],
            ),
        ];
        let groups = super::super::group_shared_mods(&repos);
        let plan = plan_cleanup(dir.path(), &pool, &repos, &groups).unwrap();
        assert_eq!(plan.links, vec![link_one.clone()]);
        assert!(plan.mods.is_empty());
        plan.execute().unwrap();
        assert!(std::fs::symlink_metadata(&link_one).is_err());
        assert!(std::fs::symlink_metadata(&link_two).is_ok());
        assert!(shared.exists());
    }

    #[test]
    fn external_pool_cleanup_uses_only_its_inventory() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("output");
        let pool = dir.path().join("shared-pool");
        std::fs::create_dir_all(&output).unwrap();
        for name in ["@owned", "@other"] {
            let mod_dir = pool.join(name);
            std::fs::create_dir_all(&mod_dir).unwrap();
            std::fs::write(mod_dir.join("foxy_addon.json"), b"{}").unwrap();
        }
        let inventory = PoolInventory {
            pool_id: path_id(&normalized_absolute(&pool).unwrap()),
            mods: vec!["@owned".to_string()],
            repositories: vec![],
        };
        std::fs::write(
            output.join(INVENTORY_FILE),
            serde_json::to_vec(&inventory).unwrap(),
        )
        .unwrap();

        let plan = plan_cleanup(&output, &pool, &[], &[]).unwrap();
        assert_eq!(plan.mods, vec![pool.join("@owned")]);
        assert!(pool.join("@other").exists());
    }
}
