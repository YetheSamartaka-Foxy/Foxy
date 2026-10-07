use anyhow::Result;
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;

use crate::cli::{GenerationMode, SpaceLayout};
use crate::types::ResolvedMod;
use crate::{discover, incremental, keys, output, published, space};

#[derive(Serialize)]
pub struct PlannedAction {
    pub action: &'static str,
    pub path: String,
    pub bytes: u64,
}

#[derive(Default, Serialize)]
pub struct BuildPlan {
    pub actions: Vec<PlannedAction>,
    pub copy_bytes: u64,
}

impl BuildPlan {
    pub fn add(&mut self, action: &'static str, path: &Path, bytes: u64) {
        self.actions.push(PlannedAction {
            action,
            path: path.display().to_string(),
            bytes,
        });
        if action == "copy" || action == "overwrite" {
            self.copy_bytes += bytes;
        }
    }

    fn add_mod(
        &mut self,
        item: &ResolvedMod,
        root: &Path,
        mode: GenerationMode,
        prune: bool,
        use_incremental: bool,
    ) -> Result<()> {
        let mod_dir = root.join(&item.mod_name);
        if prune {
            for path in published::optionals_in(&mod_dir)? {
                self.add("remove", &path, 0);
            }
        }
        let files = discover::discover_files_with_pruning(&item.source_path, prune)?;
        let reusable = if use_incremental {
            let fingerprint = incremental::fingerprint(item, &files, mode, prune)?;
            incremental::HashCache::load(root)
                .reusable(item, root, &fingerprint)
                .is_some()
        } else {
            false
        };
        if reusable {
            self.add("reuse", &mod_dir, 0);
        } else {
            for file in files {
                let dest = root.join(&item.mod_name).join(&file.relative_path);
                self.add(
                    if dest.exists() { "overwrite" } else { "copy" },
                    &dest,
                    file.file_size,
                );
            }
        }
        if mode != GenerationMode::Swifty {
            self.add("write", &mod_dir.join("foxy_addon.json"), 0);
        }
        if mode != GenerationMode::Foxy {
            self.add("write", &mod_dir.join("mod.srf"), 0);
        }
        Ok(())
    }

    pub fn show(&self) {
        for item in &self.actions {
            println!("{}: {}", item.action, item.path);
        }
        println!(
            "Plan: {} actions, {:.2} MB to copy",
            self.actions.len(),
            self.copy_bytes as f64 / 1_048_576.0
        );
        output::set_details(json!(self));
    }

    pub fn add_images(&mut self, images: &[(&str, &Path)], output_dir: &Path) -> Result<()> {
        for (image, base) in images {
            if image.is_empty() {
                continue;
            }
            let source = base.join(image);
            if source.is_file() {
                self.add(
                    "copy",
                    &output_dir.join(image),
                    std::fs::metadata(source)?.len(),
                );
            }
        }
        Ok(())
    }

    pub fn add_keys(
        &mut self,
        mods: &[ResolvedMod],
        dest: &Path,
        additional: &[std::path::PathBuf],
        prune: bool,
    ) -> Result<()> {
        let mut taken = BTreeMap::<String, std::path::PathBuf>::new();
        let mut sources = Vec::new();
        for item in mods {
            for file in discover::discover_files_with_pruning(&item.source_path, prune)? {
                if keys::is_key_file(&file.relative_path) {
                    sources.push(file.absolute_path);
                }
            }
        }
        for source in additional {
            sources.extend(keys::additional_key_paths(source)?);
        }
        for source in sources {
            let Some(name) = source.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if let Some(first) = taken.get(name) {
                self.add(
                    if std::fs::read(first)? == std::fs::read(&source)? {
                        "skip-duplicate-key"
                    } else {
                        "conflicting-key"
                    },
                    &dest.join(name),
                    0,
                );
            } else {
                self.add("copy", &dest.join(name), std::fs::metadata(&source)?.len());
                taken.insert(name.to_string(), source);
            }
        }
        Ok(())
    }
}

pub fn create(
    mods: &[ResolvedMod],
    output_dir: &Path,
    mode: GenerationMode,
    prune: bool,
    use_incremental: bool,
) -> Result<BuildPlan> {
    let mut plan = BuildPlan::default();
    for item in mods {
        plan.add_mod(item, output_dir, mode, prune, use_incremental)?;
    }
    plan.add("write", &output_dir.join("repo.json"), 0);
    if mode != GenerationMode::Swifty {
        plan.add("write", &output_dir.join("foxy_addons.json"), 0);
    }
    plan.add("write", &output_dir.join("server_mod_line.txt"), 0);
    Ok(plan)
}

pub fn create_space(
    loaded: &space::LoadedSpace,
    output_dir: &Path,
    layout: SpaceLayout,
    pool_dir: &Path,
    mode: GenerationMode,
    prune: bool,
    use_incremental: bool,
) -> Result<BuildPlan> {
    let mut plan = BuildPlan::default();
    match layout {
        SpaceLayout::Copy => {
            for repo in &loaded.repos {
                let root = output_dir.join(&repo.folder);
                for item in &repo.mods {
                    plan.add_mod(item, &root, mode, prune, use_incremental)?;
                }
            }
        }
        SpaceLayout::Pool => {
            let groups = space::group_shared_mods(&loaded.repos);
            for group in &groups {
                let (r, m) = group.uses[0];
                plan.add_mod(
                    &loaded.repos[r].mods[m],
                    pool_dir,
                    mode,
                    prune,
                    use_incremental,
                )?;
                for &(r, _) in &group.uses {
                    plan.add(
                        "link",
                        &output_dir.join(&loaded.repos[r].folder).join(&group.name),
                        0,
                    );
                }
            }
        }
        SpaceLayout::Link => {
            for repo in &loaded.repos {
                for item in &repo.mods {
                    plan.add(
                        "link",
                        &output_dir.join(&repo.folder).join(&item.mod_name),
                        0,
                    );
                    if mode != GenerationMode::Swifty {
                        plan.add(
                            "write-source-manifest",
                            &item.source_path.join("foxy_addon.json"),
                            0,
                        );
                    }
                    if mode != GenerationMode::Foxy {
                        plan.add(
                            "write-source-manifest",
                            &item.source_path.join("mod.srf"),
                            0,
                        );
                    }
                }
            }
        }
    }
    for repo in &loaded.repos {
        let root = output_dir.join(&repo.folder);
        plan.add("write", &root.join("repo.json"), 0);
        if mode != GenerationMode::Swifty {
            plan.add("write", &root.join("foxy_addons.json"), 0);
        }
        plan.add("write", &root.join("server_mod_line.txt"), 0);
        let base = Path::new(&repo.config.base_path);
        plan.add_images(
            &[
                (&repo.config.icon_image_path, base),
                (&repo.config.repo_image_path, base),
            ],
            &root,
        )?;
    }
    plan.add("write", &output_dir.join(space::SPACE_MANIFEST), 0);
    plan.add_images(
        &[
            (&loaded.config.icon_image_path, &loaded.config_dir),
            (&loaded.config.repo_image_path, &loaded.config_dir),
        ],
        output_dir,
    )?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_plan_lists_copy_and_prune_without_writes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source").join("@mod");
        let output = dir.path().join("output");
        std::fs::create_dir_all(source.join("optionals")).unwrap();
        std::fs::create_dir_all(output.join("@mod").join("optionals")).unwrap();
        std::fs::write(source.join("main.pbo"), b"main").unwrap();
        std::fs::write(source.join("optionals").join("unused.pbo"), b"unused").unwrap();
        let mods = [ResolvedMod {
            mod_name: "@mod".into(),
            source_path: source,
            is_required: true,
            enabled: true,
            client_side: false,
        }];
        let plan = create(&mods, &output, GenerationMode::Foxy, true, false).unwrap();
        assert!(plan.actions.iter().any(|action| action.action == "copy"));
        assert!(plan.actions.iter().any(|action| action.action == "remove"));
        assert!(!output.join("@mod").join("main.pbo").exists());
        assert!(output.join("@mod").join("optionals").exists());
    }
}
