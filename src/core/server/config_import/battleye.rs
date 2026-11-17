use std::fs;
use std::io::Write;
use std::net::Ipv4Addr;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use super::runtime::{check_private_dir, create_protected_directory, validate_state_marker};
use crate::core::server::{
    ContentStageControl, ServerInstanceId, ServerPlatform, ServerStore, ServerStoreError,
};

const MAX_FILES: usize = 64;
const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_CONFIG_BYTES: usize = 64 * 1024;
const OWNED_KEYS: &[&str] = &["rconpassword", "rconport", "rconip"];

/// BattlEye writes its active configuration, bans and updates beside its library, so the
/// immutable runtime copy is mirrored into private instance state and passed as `-bepath`.
#[derive(Clone, Debug, Serialize)]
pub struct NativeBattlEyeSeed {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub configuration: String,
    pub library: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rcon: Option<BattlEyeRcon>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BattlEyeRcon {
    pub address: Ipv4Addr,
    pub port: u16,
    pub password_reference: String,
}

pub(crate) fn battleye_names(platform: ServerPlatform) -> (&'static str, &'static str) {
    match platform {
        ServerPlatform::WindowsX64 => ("BEServer_x64.cfg", "beserver_x64.dll"),
        ServerPlatform::LinuxX64 => ("beserver_x64.cfg", "beserver_x64.so"),
    }
}

/// Keeps vendor lines, replacing only the RCon keys Foxy owns.
pub(crate) fn merged_configuration(vendor: &str, rcon: Option<(&BattlEyeRcon, &str)>) -> String {
    let mut lines: Vec<String> = vendor
        .lines()
        .filter(|line| {
            let key = line
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            !OWNED_KEYS.contains(&key.as_str())
        })
        .map(str::to_owned)
        .collect();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    if let Some((settings, password)) = rcon {
        lines.push(format!("RConPassword {password}"));
        lines.push(format!("RConPort {}", settings.port));
        lines.push(format!("RConIP {}", settings.address));
    }
    let mut text = lines.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

fn refreshed_every_start(lower_name: &str) -> bool {
    lower_name.ends_with(".dll") || lower_name.ends_with(".so")
}

fn stale_active_copy(name: &str, configuration: &str) -> bool {
    let stem = configuration
        .strip_suffix(".cfg")
        .unwrap_or(configuration)
        .to_ascii_lowercase();
    let lower = name.to_ascii_lowercase();
    lower
        .strip_prefix(&format!("{stem}_active_"))
        .and_then(|tail| tail.strip_suffix(".cfg"))
        .is_some_and(|tail| !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_alphanumeric()))
}

fn write_replacing(directory: &Path, name: &str, bytes: &[u8]) -> Result<(), ServerStoreError> {
    let mut temp = tempfile::Builder::new()
        .prefix(".foxy-battleye-")
        .tempfile_in(directory)
        .map_err(|_| ServerStoreError::Io)?;
    temp.write_all(bytes).map_err(|_| ServerStoreError::Io)?;
    temp.as_file()
        .sync_all()
        .map_err(|_| ServerStoreError::Io)?;
    temp.persist(directory.join(name))
        .map_err(|_| ServerStoreError::Io)?;
    Ok(())
}

impl ServerStore {
    pub(in crate::core::server) fn prepare_battleye_state(
        &self,
        id: &ServerInstanceId,
        seed: &NativeBattlEyeSeed,
        control: &ContentStageControl,
    ) -> Result<(), ServerStoreError> {
        control.checkpoint()?;
        self.validate_record_context()?;
        let record_root = self.record_root(id);
        let state_root = record_root.join("runtime-state");
        validate_state_marker(self, id, &state_root, control)?;
        if seed.destination != state_root.join("home/battleye") {
            return Err(ServerStoreError::UnsafePath);
        }
        let relative = seed
            .source
            .strip_prefix(&record_root)
            .map_err(|_| ServerStoreError::UnsafePath)?;
        let parts: Vec<_> = relative.components().collect();
        let runtime_name = match parts.as_slice() {
            [
                Component::Normal(runtimes),
                Component::Normal(runtime),
                Component::Normal(engine),
                Component::Normal(battleye),
            ] if *runtimes == "runtimes"
                && *engine == "engine"
                && battleye
                    .to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case("battleye")) =>
            {
                runtime.to_str().ok_or(ServerStoreError::UnsafePath)?
            }
            _ => return Err(ServerStoreError::UnsafePath),
        };
        if !runtime_name.starts_with("runtime_") {
            return Err(ServerStoreError::UnsafePath);
        }
        crate::core::server::store::check_directory_chain(&seed.source)?;
        let home = state_root.join("home");
        crate::core::server::store::check_directory_chain(&home)?;
        create_protected_directory(&seed.destination)?;
        check_private_dir(&seed.destination)?;
        let mut names = Vec::new();
        for entry in fs::read_dir(&seed.source).map_err(|_| ServerStoreError::Io)? {
            let entry = entry.map_err(|_| ServerStoreError::Io)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| ServerStoreError::UnsafePath)?;
            names.push(name);
            if names.len() > MAX_FILES {
                return Err(ServerStoreError::Invalid(
                    "The BattlEye folder has too many entries".into(),
                ));
            }
        }
        names.sort();
        let mut vendor = String::new();
        let mut library = false;
        for name in &names {
            control.checkpoint()?;
            let path = seed.source.join(name);
            let metadata = fs::symlink_metadata(&path).map_err(|_| ServerStoreError::Io)?;
            if metadata.file_type().is_symlink() {
                return Err(ServerStoreError::UnsafePath);
            }
            if !metadata.is_file() || name.starts_with('.') {
                continue;
            }
            let lower = name.to_ascii_lowercase();
            if lower == seed.configuration.to_ascii_lowercase() {
                let bytes = crate::core::server::content_staging::read_bounded_controlled(
                    &path,
                    MAX_CONFIG_BYTES,
                    control,
                )?;
                vendor = String::from_utf8_lossy(&bytes).into_owned();
                continue;
            }
            if lower.starts_with("beserver") && lower.ends_with(".cfg") {
                continue;
            }
            let is_library = lower == seed.library.to_ascii_lowercase();
            library |= is_library;
            // BattlEye keeps bans and filters beside its library; only libraries are refreshed.
            if !refreshed_every_start(&lower)
                && fs::symlink_metadata(seed.destination.join(name)).is_ok()
            {
                continue;
            }
            let bytes = crate::core::server::content_staging::read_bounded_controlled(
                &path,
                MAX_FILE_BYTES,
                control,
            )?;
            write_replacing(&seed.destination, name, &bytes)?;
        }
        if !library {
            return Err(ServerStoreError::Invalid(
                "This managed Arma runtime has no BattlEye server library for its platform.".into(),
            ));
        }
        let password = match &seed.rcon {
            Some(rcon) => {
                let secret = self.resolve_config_secret(id, &rcon.password_reference)?;
                if !(3..=64).contains(&secret.len())
                    || !secret.bytes().all(|b| b.is_ascii_graphic())
                {
                    return Err(ServerStoreError::Corrupt);
                }
                Some(zeroize::Zeroizing::new(secret))
            }
            None => None,
        };
        let text = zeroize::Zeroizing::new(merged_configuration(
            &vendor,
            seed.rcon
                .as_ref()
                .zip(password.as_ref().map(|value| value.as_str())),
        ));
        control.checkpoint()?;
        write_replacing(&seed.destination, &seed.configuration, text.as_bytes())?;
        for entry in fs::read_dir(&seed.destination).map_err(|_| ServerStoreError::Io)? {
            let entry = entry.map_err(|_| ServerStoreError::Io)?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if stale_active_copy(&name, &seed.configuration)
                && fs::symlink_metadata(entry.path()).is_ok_and(|metadata| metadata.is_file())
            {
                fs::remove_file(entry.path()).map_err(|_| ServerStoreError::Io)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rcon() -> BattlEyeRcon {
        BattlEyeRcon {
            address: Ipv4Addr::LOCALHOST,
            port: 2320,
            password_reference: format!("secret_{}", "a".repeat(32)),
        }
    }

    #[test]
    fn owned_rcon_lines_replace_only_their_keys_and_keep_vendor_settings() {
        let vendor = "MaxPing 250\nrconpassword old\nRConPort 1\nRestrictRCon 1\n\n";
        assert_eq!(
            merged_configuration(vendor, Some((&rcon(), "synthetic-pass"))),
            "MaxPing 250\nRestrictRCon 1\nRConPassword synthetic-pass\nRConPort 2320\nRConIP 127.0.0.1\n"
        );
        assert_eq!(
            merged_configuration(vendor, None),
            "MaxPing 250\nRestrictRCon 1\n"
        );
        assert_eq!(merged_configuration("", None), "");
    }

    #[test]
    fn only_libraries_replace_existing_battleye_state() {
        assert!(refreshed_every_start("beserver_x64.dll"));
        assert!(refreshed_every_start("beserver_x64.so"));
        for kept in ["bans.txt", "eula.txt", "scripts.txt", "beserver_x64.so.bak"] {
            assert!(!refreshed_every_start(kept), "{kept}");
        }
    }

    #[test]
    fn only_exact_active_copies_of_the_owned_configuration_are_stale() {
        assert!(stale_active_copy(
            "BEServer_x64_active_55da.cfg",
            "BEServer_x64.cfg"
        ));
        assert!(stale_active_copy(
            "beserver_x64_active_0f.cfg",
            "beserver_x64.cfg"
        ));
        for kept in [
            "BEServer_x64.cfg",
            "BEServer_x64_active_.cfg",
            "BEServer_active_55da.cfg",
            "BEServer_x64_active_5-5.cfg",
            "bans.txt",
        ] {
            assert!(!stale_active_copy(kept, "BEServer_x64.cfg"), "{kept}");
        }
    }
}
