use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{ServerDefinitionKind, ServerStoreError};

mod manifest;
mod observation;
mod version;

pub use observation::InstallationBuildObservation;
pub(super) use observation::observe_build;
pub use version::ExecutableVersion;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct InstallationId(String);

impl InstallationId {
    pub fn new() -> Self {
        Self(format!("install_{:032x}", rand::random::<u128>()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for InstallationId {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let suffix = value.strip_prefix("install_").unwrap_or_default();
        if suffix.len() != 32
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("Invalid server installation id".into());
        }
        Ok(Self(value))
    }
}

impl Default for InstallationId {
    fn default() -> Self {
        Self::new()
    }
}

impl From<InstallationId> for String {
    fn from(value: InstallationId) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerPlatform {
    WindowsX64,
    LinuxX64,
}

impl ServerPlatform {
    pub fn current() -> Result<Self, ServerStoreError> {
        if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            Ok(Self::WindowsX64)
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Ok(Self::LinuxX64)
        } else {
            Err(ServerStoreError::Invalid(
                "Native server adoption requires Windows or Linux x64".into(),
            ))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutableEvidence {
    pub bytes: u64,
    pub blake3: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerInstallation {
    pub id: InstallationId,
    pub game: ServerDefinitionKind,
    pub platform: ServerPlatform,
    pub directory: PathBuf,
    pub executable: String,
    pub evidence: ExecutableEvidence,
    pub ownership: InstallationOwnership,
    pub build_evidence: BuildEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationOwnership {
    Adopted,
    Managed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildEvidence {
    ExecutableFingerprintOnly,
    NativeDepotManifest,
}

impl ServerInstallation {
    pub fn inspect(directory: &Path, game: ServerDefinitionKind) -> Result<Self, ServerStoreError> {
        let directory = std::path::absolute(directory).map_err(|_| ServerStoreError::UnsafePath)?;
        super::store::check_directory_chain(&directory)?;
        let directory = directory
            .canonicalize()
            .map_err(|_| invalid_installation())?;
        super::store::check_directory_chain(&directory)?;
        let platform = ServerPlatform::current()?;
        let executable = executable_name(game, platform).to_string();
        let path = directory.join(&executable);
        super::store::check_regular_file(&path)?;
        let mut file = File::open(path).map_err(|_| invalid_installation())?;
        let metadata = file.metadata().map_err(|_| invalid_installation())?;
        if !metadata.is_file() || metadata.len() > 1024 * 1024 * 1024 {
            return Err(invalid_installation());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(invalid_installation());
            }
        }
        let mut header = [0u8; 64];
        file.read_exact(&mut header)
            .map_err(|_| invalid_installation())?;
        let native = match platform {
            ServerPlatform::LinuxX64 => valid_elf(&header),
            ServerPlatform::WindowsX64 => {
                let offset = u32::from_le_bytes(header[60..64].try_into().unwrap());
                let mut pe = [0u8; 26];
                header.starts_with(b"MZ")
                    && offset >= 64
                    && u64::from(offset) + 26 <= metadata.len()
                    && file.seek(SeekFrom::Start(u64::from(offset))).is_ok()
                    && file.read_exact(&mut pe).is_ok()
                    && valid_pe(&pe)
            }
        };
        if !native {
            return Err(invalid_installation());
        }
        file.rewind().map_err(|_| invalid_installation())?;
        let mut hasher = blake3::Hasher::new();
        let bytes = std::io::copy(
            &mut Read::by_ref(&mut file).take(metadata.len() + 1),
            &mut hasher,
        )
        .map_err(|_| invalid_installation())?;
        if bytes != metadata.len()
            || file.metadata().map_err(|_| invalid_installation())?.len() != bytes
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let evidence = ExecutableEvidence {
            bytes,
            blake3: hasher.finalize().to_hex().to_string(),
        };
        let managed = super::native_install::managed_manifest_matches(
            &directory,
            game,
            platform,
            &executable,
            &evidence,
        );
        Ok(Self {
            id: InstallationId::new(),
            game,
            platform,
            directory,
            executable,
            evidence,
            ownership: if managed {
                InstallationOwnership::Managed
            } else {
                InstallationOwnership::Adopted
            },
            build_evidence: if managed {
                BuildEvidence::NativeDepotManifest
            } else {
                BuildEvidence::ExecutableFingerprintOnly
            },
        })
    }

    pub fn validate(&self, game: ServerDefinitionKind) -> bool {
        self.game == game
            && self.directory.is_absolute()
            && !self
                .directory
                .components()
                .any(|part| matches!(part, Component::ParentDir))
            && self.executable == executable_name(game, self.platform)
            && (64..=1024 * 1024 * 1024).contains(&self.evidence.bytes)
            && self.evidence.blake3.len() == 64
            && self
                .evidence
                .blake3
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    pub fn verify(&self) -> Result<(), ServerStoreError> {
        let observed = Self::inspect(&self.directory, self.game)?;
        if self.platform != observed.platform
            || self.executable != observed.executable
            || self.directory != observed.directory
            || self.evidence != observed.evidence
        {
            return Err(ServerStoreError::StaleRevision);
        }
        Ok(())
    }
}

pub(super) fn executable_name(
    game: ServerDefinitionKind,
    platform: ServerPlatform,
) -> &'static str {
    match (game, platform) {
        (ServerDefinitionKind::Arma3, ServerPlatform::WindowsX64) => "arma3server_x64.exe",
        (ServerDefinitionKind::Arma3, ServerPlatform::LinuxX64) => "arma3server_x64",
        (ServerDefinitionKind::Reforger, ServerPlatform::WindowsX64) => "ArmaReforgerServer.exe",
        (ServerDefinitionKind::Reforger, ServerPlatform::LinuxX64) => "ArmaReforgerServer",
    }
}

fn valid_pe(header: &[u8; 26]) -> bool {
    header.starts_with(b"PE\0\0")
        && header[4..6] == [0x64, 0x86]
        && u16::from_le_bytes([header[6], header[7]]) > 0
        && u16::from_le_bytes([header[20], header[21]]) >= 2
        && header[24..26] == [0x0b, 0x02]
}

fn valid_elf(header: &[u8; 64]) -> bool {
    header.starts_with(b"\x7fELF")
        && header[4..7] == [2, 1, 1]
        && matches!(u16::from_le_bytes([header[16], header[17]]), 2 | 3)
        && header[18..20] == [62, 0]
}

fn invalid_installation() -> ServerStoreError {
    ServerStoreError::Invalid("Expected a readable native x64 dedicated-server executable in an ordinary installation directory".into())
}

pub(super) fn paths_overlap(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        let normalize = |path: &Path| {
            let path = path.to_string_lossy().to_lowercase().replace('/', "\\");
            if let Some(tail) = path.strip_prefix("\\\\?\\unc\\") {
                format!("\\\\{tail}")
            } else {
                path.strip_prefix("\\\\?\\").unwrap_or(&path).to_string()
            }
        };
        let left = normalize(left);
        let right = normalize(right);
        Path::new(&left).starts_with(Path::new(&right))
            || Path::new(&right).starts_with(Path::new(&left))
    }
    #[cfg(not(windows))]
    {
        left.starts_with(right) || right.starts_with(left)
    }
}

#[cfg(test)]
pub(crate) mod tests;
