use anyhow::{Context, Result, bail};
use clap::Args;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Args)]
pub struct MirrorOptions {
    #[arg(long)]
    pub upstream: String,
    #[arg(long)]
    pub output: PathBuf,
    #[arg(long, default_value = "Test Kit Origin")]
    pub repo_name: String,
    #[arg(long, value_delimiter = ',')]
    pub mods: Vec<String>,
    #[arg(long, default_value_t = 0)]
    pub limit: usize,
    #[arg(long)]
    pub payload: bool,
    #[arg(long)]
    pub force: bool,
}

pub(super) fn resolved_target(path: &Path, repo_root: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let parent = absolute
        .parent()
        .context("output cannot be a filesystem root")?;
    fs::create_dir_all(parent)?;
    let target = if absolute.exists() {
        absolute.canonicalize()?
    } else {
        parent.canonicalize()?.join(
            absolute
                .file_name()
                .context("output needs a directory name")?,
        )
    };
    let repo = repo_root.canonicalize()?;
    if target.parent().is_none()
        || repo.starts_with(&target)
        || target == repo.join("src")
        || target == repo.join("testkit")
    {
        bail!("refusing unsafe origin output directory");
    }
    if absolute
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        bail!("origin output must not be a symbolic link");
    }
    Ok(target)
}

pub(super) fn prepare_target(path: &Path, force: bool) -> Result<()> {
    if path.exists() {
        if !force {
            bail!("output already exists; pass --force to rebuild it");
        }
        fs::remove_dir_all(path)?;
    }
    fs::create_dir_all(path)?;
    Ok(())
}

fn relative_path(base: &Path, path: &str) -> Result<PathBuf> {
    let mut result = base.to_path_buf();
    for part in path.split(['/', '\\']) {
        if part.is_empty() || part == "." || part == ".." || part.contains(':') {
            bail!("unsafe manifest path");
        }
        result.push(part);
    }
    Ok(result)
}

pub(super) fn write_json(path: &Path, value: &Value) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

impl MirrorOptions {
    pub fn execute(&self, repo_root: &Path) -> Result<Value> {
        let base = format!("{}/", self.upstream.trim_end_matches('/'));
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()?;
        let mut mods = self.mods.clone();
        if mods.is_empty() {
            let html = client.get(&base).send()?.error_for_status()?.text()?;
            let expression = regex::Regex::new(r#"href="([^"]+)/""#)?;
            for capture in expression.captures_iter(&html) {
                let name = percent_encoding::percent_decode_str(&capture[1])
                    .decode_utf8()?
                    .into_owned();
                if name.starts_with('@') {
                    mods.push(name);
                }
            }
            mods.sort_by_key(|name| name.to_lowercase());
            mods.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        }
        if self.limit > 0 {
            mods.truncate(self.limit);
        }
        if mods.is_empty() {
            bail!("no mods found upstream");
        }
        let output = resolved_target(&self.output, repo_root)?;
        prepare_target(&output, self.force)?;
        let mut required = Vec::new();
        let mut skipped = Vec::new();
        let (mut files, mut parts, mut bytes) = (0_u64, 0_u64, 0_u64);
        for name in mods {
            let mod_dir = relative_path(&output, &name)?;
            let mut url = reqwest::Url::parse(&base)?;
            url.path_segments_mut()
                .map_err(|()| anyhow::anyhow!("invalid upstream base URL"))?
                .pop_if_empty()
                .push(&name)
                .push("foxy_addon.json");
            let raw = match client
                .get(url)
                .send()
                .and_then(|response| response.error_for_status())
                .and_then(|response| response.text())
            {
                Ok(raw) => raw,
                Err(_) => {
                    skipped.push(name);
                    continue;
                }
            };
            let manifest: Value = serde_json::from_str(&raw)?;
            fs::create_dir_all(&mod_dir)?;
            fs::write(mod_dir.join("foxy_addon.json"), raw)?;
            for file in manifest["files"]
                .as_array()
                .context("manifest files must be an array")?
            {
                files += 1;
                bytes += file["length"].as_u64().context("manifest file length")?;
                parts += file["parts"]
                    .as_array()
                    .map_or(0, |parts| parts.len() as u64);
                if self.payload {
                    let path = file["path"].as_str().context("manifest file path")?;
                    let target = relative_path(&mod_dir, path)?;
                    fs::create_dir_all(target.parent().unwrap())?;
                    let mut url = reqwest::Url::parse(&base)?;
                    url.path_segments_mut()
                        .map_err(|()| anyhow::anyhow!("invalid upstream base URL"))?
                        .pop_if_empty()
                        .push(&name)
                        .extend(path.split(['/', '\\']));
                    client
                        .get(url)
                        .send()?
                        .error_for_status()?
                        .copy_to(&mut fs::File::create(target)?)?;
                }
            }
            required.push(json!({"modName": name, "checkSum": manifest["checksum"].as_str().unwrap_or(""), "enabled": true}));
        }
        if required.is_empty() {
            bail!("no mod manifests could be mirrored");
        }
        let fingerprint: String = required
            .iter()
            .filter_map(|entry| entry["checkSum"].as_str())
            .collect();
        let checksum = hex::encode_upper(Sha256::digest(fingerprint.as_bytes()));
        write_json(
            &output.join("repo.json"),
            &json!({"repoName": self.repo_name, "checksum": checksum, "foxyMode": "FoxyModeV1", "requiredMods": required, "optionalMods": [], "iconImagePath": "", "iconImageChecksum": "", "repoImagePath": "", "repoImageChecksum": "", "appUpdateUrl": "", "clientParameters": "", "version": "3.2.0.0", "servers": []}),
        )?;
        write_json(
            &output.join("foxy_addons.json"),
            &json!({"version": "FoxyModeV1", "hashAlgorithm": "BLAKE3", "checksum": checksum, "requiredMods": required, "optionalMods": []}),
        )?;
        let summary = json!({"upstream": base, "repo_name": self.repo_name, "generated_utc": chrono::Utc::now().to_rfc3339(), "payload_mirrored": self.payload, "mods": required.len(), "files": files, "parts": parts, "payload_bytes": bytes, "skipped": skipped});
        write_json(&output.join("origin.json"), &summary)?;
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_payload_traversal() {
        for path in ["../file", "C:\\file", "/file", "a\\..\\file"] {
            assert!(relative_path(Path::new("base"), path).is_err());
        }
        assert_eq!(
            relative_path(Path::new("base"), "addons/a.bin").unwrap(),
            Path::new("base").join("addons/a.bin")
        );
    }
}
