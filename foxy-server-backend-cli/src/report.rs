use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;

use crate::{output, srf};

#[derive(Clone)]
pub struct ModState {
    checksum: String,
    bytes: u64,
}

pub type Snapshot = BTreeMap<String, ModState>;

#[derive(Serialize)]
pub struct Change {
    pub kind: &'static str,
    pub mod_name: String,
    pub download_bytes: u64,
}

#[derive(Serialize)]
pub struct ChangeReport {
    pub changes: Vec<Change>,
    pub estimated_download_bytes: u64,
}

pub fn snapshot(output: &Path) -> Result<Snapshot> {
    let mut result = Snapshot::new();
    if output.join("repo.json").is_file() {
        read_repo(output, "", &mut result)?;
    } else if output.is_dir() {
        for entry in std::fs::read_dir(output)? {
            let path = entry?.path();
            if path.join("repo.json").is_file() {
                let folder = path.file_name().unwrap().to_string_lossy().into_owned();
                read_repo(&path, &folder, &mut result)?;
            }
        }
    }
    Ok(result)
}

fn read_repo(dir: &Path, prefix: &str, result: &mut Snapshot) -> Result<()> {
    let repo = srf::read_manifest(&dir.join("repo.json"))?;
    let manifest = if repo["foxyMode"].is_string() {
        dir.join("foxy_addons.json")
    } else {
        dir.join("repo.json")
    };
    let value = srf::read_manifest(&manifest)?;
    for list in ["requiredMods", "optionalMods"] {
        for item in value[list].as_array().into_iter().flatten() {
            let Some(name) = item["modName"].as_str() else {
                continue;
            };
            let checksum = item["checkSum"].as_str().unwrap_or_default().to_string();
            let mod_dir = dir.join(name);
            let bytes = if mod_dir.join("foxy_addon.json").is_file() {
                let addon = srf::read_manifest(&mod_dir.join("foxy_addon.json"))?;
                addon["files"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|file| file["length"].as_u64())
                    .sum()
            } else if mod_dir.join("mod.srf").is_file() {
                let addon = srf::read_manifest(&mod_dir.join("mod.srf"))?;
                addon["Files"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|file| file["Length"].as_u64())
                    .sum()
            } else {
                0
            };
            let key = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            result.insert(key, ModState { checksum, bytes });
        }
    }
    Ok(())
}

pub fn compare(old: &Snapshot, new: &Snapshot) -> ChangeReport {
    let mut changes = Vec::new();
    for (name, state) in new {
        let kind = match old.get(name) {
            None => "added",
            Some(previous) if previous.checksum != state.checksum => "changed",
            Some(_) => continue,
        };
        changes.push(Change {
            kind,
            mod_name: name.clone(),
            download_bytes: state.bytes,
        });
    }
    for name in old.keys() {
        if !new.contains_key(name) {
            changes.push(Change {
                kind: "removed",
                mod_name: name.clone(),
                download_bytes: 0,
            });
        }
    }
    let estimated_download_bytes = changes.iter().map(|change| change.download_bytes).sum();
    ChangeReport {
        changes,
        estimated_download_bytes,
    }
}

pub fn print(report: &ChangeReport) {
    for change in &report.changes {
        println!("{}: {}", change.kind, change.mod_name);
    }
    println!(
        "Changes: {}, estimated client download: {:.2} MB",
        report.changes.len(),
        report.estimated_download_bytes as f64 / 1_048_576.0
    );
    output::set_details(json!(report));
}

pub fn diff(old: &Path, new: &Path) -> Result<()> {
    if !old.exists() || !new.exists() {
        bail!("Both diff inputs must exist");
    }
    let old_state = snapshot(old)?;
    let new_state = snapshot(new)?;
    if old_state.is_empty() || new_state.is_empty() {
        bail!("Both diff inputs must contain generated repositories");
    }
    let result = compare(&old_state, &new_state);
    print(&result);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_report_counts_only_new_downloads() {
        let old = Snapshot::from([
            (
                "@gone".to_string(),
                ModState {
                    checksum: "a".into(),
                    bytes: 5,
                },
            ),
            (
                "@changed".to_string(),
                ModState {
                    checksum: "b".into(),
                    bytes: 6,
                },
            ),
        ]);
        let new = Snapshot::from([
            (
                "@changed".to_string(),
                ModState {
                    checksum: "c".into(),
                    bytes: 7,
                },
            ),
            (
                "@new".to_string(),
                ModState {
                    checksum: "d".into(),
                    bytes: 8,
                },
            ),
        ]);
        let report = compare(&old, &new);
        assert_eq!(report.changes.len(), 3);
        assert_eq!(report.estimated_download_bytes, 15);
        assert!(
            report
                .changes
                .iter()
                .any(|item| item.kind == "removed" && item.download_bytes == 0)
        );
    }
}
