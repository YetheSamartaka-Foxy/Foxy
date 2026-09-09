use super::mirror::{prepare_target, resolved_target, write_json};
use anyhow::{Result, bail};
use clap::Args;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Args)]
pub struct SyntheticOptions {
    #[arg(long)]
    pub output: PathBuf,
    #[arg(long, default_value_t = 32)]
    pub mods: u32,
    #[arg(long, default_value_t = 500)]
    pub files_per_mod: u32,
    #[arg(long, default_value_t = 4096)]
    pub file_bytes: usize,
    /// Emit PBOs with this many entries instead of flat `.bin` files. Each entry
    /// becomes its own manifest part, so part rows scale without payload bytes.
    #[arg(long, default_value_t = 0)]
    pub pbo_entries: u32,
    #[arg(long, default_value_t = 64)]
    pub entry_bytes: usize,
    #[arg(long, default_value_t = 20260909)]
    pub seed: i32,
    #[arg(long, default_value = "TestKit Synthetic Writes")]
    pub repo_name: String,
    #[arg(long)]
    pub force: bool,
}

struct DotNetRandom {
    values: [i32; 56],
    next: usize,
    next_prime: usize,
}

impl DotNetRandom {
    fn new(seed: i32) -> Self {
        let mut values = [0; 56];
        let mut previous = 161803398_i32.wrapping_sub(seed.checked_abs().unwrap_or(i32::MAX));
        values[55] = previous;
        let mut current: i32 = 1;
        for index in 1..55 {
            let slot = 21 * index % 55;
            values[slot] = current;
            current = previous.wrapping_sub(current);
            if current < 0 {
                current = current.wrapping_add(i32::MAX);
            }
            previous = values[slot];
        }
        for _ in 0..4 {
            for index in 1..56 {
                values[index] = values[index].wrapping_sub(values[1 + (index + 30) % 55]);
                if values[index] < 0 {
                    values[index] = values[index].wrapping_add(i32::MAX);
                }
            }
        }
        Self {
            values,
            next: 0,
            next_prime: 21,
        }
    }

    fn fill(&mut self, bytes: &mut [u8]) {
        for byte in bytes {
            self.next = self.next % 55 + 1;
            self.next_prime = self.next_prime % 55 + 1;
            let mut sample = self.values[self.next].wrapping_sub(self.values[self.next_prime]);
            if sample == i32::MAX {
                sample -= 1;
            }
            if sample < 0 {
                sample = sample.wrapping_add(i32::MAX);
            }
            self.values[self.next] = sample;
            *byte = (sample % 256) as u8;
        }
    }
}

impl SyntheticOptions {
    pub fn execute(&self, repo_root: &Path) -> Result<Value> {
        if self.mods == 0 || self.files_per_mod == 0 || self.file_bytes == 0 {
            bail!("synthetic dimensions must be positive");
        }
        if self.pbo_entries > 0 && self.entry_bytes == 0 {
            bail!("entry-bytes must be positive when generating PBOs");
        }
        let output = resolved_target(&self.output, repo_root)?;
        let mut source_name = output.as_os_str().to_owned();
        source_name.push("-src");
        let source = resolved_target(Path::new(&source_name), repo_root)?;
        if !self.force && (output.exists() || source.exists()) {
            bail!("output or source exists; pass --force to rebuild it");
        }
        prepare_target(&output, self.force)?;
        fs::remove_dir(&output)?;
        prepare_target(&source, self.force)?;
        let mut random = DotNetRandom::new(self.seed);
        let mut buffer = vec![0; self.file_bytes];
        let mut names = Vec::new();
        for index in 0..self.mods {
            let name = format!("@synthetic_{index:03}");
            let directory = source.join(&name);
            fs::create_dir_all(directory.join("addons"))?;
            for file in 0..self.files_per_mod {
                if self.pbo_entries > 0 {
                    let bytes = synthetic_pbo(&mut random, self.pbo_entries, self.entry_bytes);
                    fs::write(
                        directory.join("addons").join(format!("part_{file:05}.pbo")),
                        &bytes,
                    )?;
                } else {
                    random.fill(&mut buffer);
                    fs::write(
                        directory.join("addons").join(format!("part_{file:05}.bin")),
                        &buffer,
                    )?;
                }
            }
            fs::write(directory.join("meta.cpp"), format!("name = \"{name}\";\n"))?;
            names.push(name);
        }
        let mut config_name = output.as_os_str().to_owned();
        config_name.push("-config.json");
        let config_path = PathBuf::from(config_name);
        let required: Vec<_> = names
            .iter()
            .map(|name| json!({"modName": name, "enabled": true}))
            .collect();
        write_json(
            &config_path,
            &json!({"repoName": self.repo_name, "basePath": source, "appUpdateUrl": "", "requiredMods": required, "optionalMods": [], "iconImagePath": "", "repoImagePath": "", "clientParameters": "", "repoBasicAuthentication": {"username": "", "password": ""}, "version": "3.2.0.0", "servers": []}),
        )?;
        let generator = repo_root.join("target/release").join(format!(
            "foxy-server-backend-cli{}",
            std::env::consts::EXE_SUFFIX
        ));
        if !generator.exists()
            && !Command::new("cargo")
                .args(["build", "-p", "foxy-server-backend-cli", "--release"])
                .current_dir(repo_root)
                .status()?
                .success()
        {
            bail!("failed to build repository generator");
        }
        let threads = std::thread::available_parallelism()?.get().to_string();
        let status = Command::new(generator)
            .args(["--no-progress", "create"])
            .arg(&config_path)
            .arg(&output)
            .args(["--threads", &threads])
            .status()?;
        if !status.success() {
            bail!("repository generation failed: {status}");
        }
        let (mut files, mut parts) = (0_u64, 0_u64);
        for name in &names {
            let manifest: Value =
                serde_json::from_slice(&fs::read(output.join(name).join("foxy_addon.json"))?)?;
            if let Some(entries) = manifest["files"].as_array() {
                files += entries.len() as u64;
                parts += entries
                    .iter()
                    .map(|entry| {
                        entry["parts"]
                            .as_array()
                            .map_or(0, |parts| parts.len() as u64)
                    })
                    .sum::<u64>();
            }
        }
        let bytes = walkdir::WalkDir::new(&output).into_iter().try_fold(
            0_u64,
            |sum, entry| -> Result<_> {
                let entry = entry?;
                Ok(sum
                    + if entry.file_type().is_file() {
                        entry.metadata()?.len()
                    } else {
                        0
                    })
            },
        )?;
        let summary = json!({"kind": "synthetic", "generated_utc": chrono::Utc::now().to_rfc3339(), "seed": self.seed, "mods": names.len(), "files": files, "parts": parts, "payload_bytes": bytes});
        write_json(&output.join("origin.json"), &summary)?;
        fs::remove_dir_all(&source)?;
        Ok(summary)
    }
}

/// A PBO the repository generator will split into `entries + 2` manifest parts
/// (header, one per entry, trailer). Layout mirrors `foxy_formats::pbo`: a zero
/// byte, the `sreV` tag, sixteen skipped bytes, a zero extension marker, the
/// entry table terminated by an empty name, then the entry payloads and a
/// trailer.
fn synthetic_pbo(random: &mut DotNetRandom, entries: u32, entry_bytes: usize) -> Vec<u8> {
    fn push_entry(bytes: &mut Vec<u8>, name: &str, length: u32) {
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
        for _ in 0..4 {
            bytes.extend_from_slice(&0u32.to_le_bytes());
        }
        bytes.extend_from_slice(&length.to_le_bytes());
    }

    let mut bytes = Vec::with_capacity(entries as usize * (entry_bytes + 32) + 64);
    bytes.push(0);
    bytes.extend_from_slice(b"sreV");
    bytes.extend_from_slice(&[0; 16]);
    bytes.push(0);
    for entry in 0..entries {
        push_entry(
            &mut bytes,
            &format!("data/e_{entry:05}.bin"),
            entry_bytes as u32,
        );
    }
    push_entry(&mut bytes, "", 0);
    let mut payload = vec![0; entry_bytes];
    for _ in 0..entries {
        random.fill(&mut payload);
        bytes.extend_from_slice(&payload);
    }
    let mut trailer = [0; 21];
    random.fill(&mut trailer[1..]);
    bytes.extend_from_slice(&trailer);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode the generated PBO the way `foxy_formats::pbo` does, without
    /// importing it: the kit stays independent of the crates it measures.
    #[test]
    fn synthetic_pbo_lays_out_one_span_per_entry() {
        let mut random = DotNetRandom::new(7);
        let bytes = synthetic_pbo(&mut random, 5, 16);

        assert_eq!(bytes[0], 0);
        assert_eq!(&bytes[1..5], b"sreV");
        let mut cursor = 21;
        assert_eq!(bytes[cursor], 0);
        cursor += 1;

        let mut lengths = Vec::new();
        loop {
            let end = cursor + bytes[cursor..].iter().position(|b| *b == 0).unwrap();
            let name = String::from_utf8(bytes[cursor..end].to_vec()).unwrap();
            cursor = end + 1;
            let size = u32::from_le_bytes(bytes[cursor + 16..cursor + 20].try_into().unwrap());
            cursor += 20;
            if name.is_empty() {
                break;
            }
            lengths.push(size as usize);
        }

        assert_eq!(lengths, vec![16; 5]);
        let header_len = cursor;
        assert_eq!(bytes.len(), header_len + 5 * 16 + 21);
    }
    use base64::Engine;
    #[test]
    fn matches_windows_powershell_seeded_random_next_bytes() {
        let mut random = DotNetRandom::new(20260909);
        let mut bytes = [0; 32];
        random.fill(&mut bytes);
        assert_eq!(
            base64::engine::general_purpose::STANDARD.encode(bytes),
            "lbMtirsuy+29LziF600ZRbXiVwPRK3FYiA8IMpWz9gI="
        );
        let mut split = DotNetRandom::new(20260909);
        let mut divided = [0; 32];
        split.fill(&mut divided[..5]);
        split.fill(&mut divided[5..]);
        assert_eq!(bytes, divided);
    }
}
