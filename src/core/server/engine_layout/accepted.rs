use serde::Serialize;

use crate::core::server::{ServerDefinitionKind, ServerInstallation, ServerPlatform};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TestedEngineLayout {
    pub tested_runtime_version: &'static str,
    pub evidence_source: &'static str,
    pub shared_root_relative_directories: bool,
    pub isolated_copy_fallback: bool,
    pub base_keys_retained: bool,
}

pub(super) fn observe(installation: &ServerInstallation) -> Option<TestedEngineLayout> {
    if !installation.validate(installation.game) {
        return None;
    }
    let (bytes, fingerprint, version) = match (installation.game, installation.platform) {
        (ServerDefinitionKind::Arma3, ServerPlatform::WindowsX64) => (
            32_200_952,
            "ed0204a505ee8094356ce07ee9ce6736a570245e9098c0784771da7daf2d48ab",
            "2.22.154089",
        ),
        (ServerDefinitionKind::Arma3, ServerPlatform::LinuxX64) => (
            41_681_008,
            "3bca4c88ec42759b85fdda6c5be77cbc11ef9db46812d9f864dd191a0935ffbc",
            "2.22.154089",
        ),
        (ServerDefinitionKind::Reforger, ServerPlatform::WindowsX64) => (
            36_232_248,
            "3c13311b4e07d81d83db62de239ba883d297378da2c03185d31f2a6d3d007926",
            "1.9.0.37",
        ),
        (ServerDefinitionKind::Reforger, ServerPlatform::LinuxX64) => (
            51_291_168,
            "5c09b8847f33d9c9bb65d678471c10d1f55d206fd6d2f3fc5f599ed7f41ac8d2",
            "1.9.0.37",
        ),
    };
    if installation.evidence.bytes != bytes || installation.evidence.blake3 != fingerprint {
        return None;
    }
    Some(TestedEngineLayout {
        tested_runtime_version: version,
        evidence_source: "compiled_native_acceptance_matrix",
        shared_root_relative_directories: installation.game == ServerDefinitionKind::Arma3,
        isolated_copy_fallback: true,
        base_keys_retained: installation.game == ServerDefinitionKind::Arma3,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::installation::{
        BuildEvidence, ExecutableEvidence, InstallationId, InstallationOwnership,
    };

    fn installation(platform: ServerPlatform, bytes: u64, fingerprint: &str) -> ServerInstallation {
        ServerInstallation {
            id: InstallationId::new(),
            game: ServerDefinitionKind::Arma3,
            platform,
            directory: std::env::temp_dir().join("native-accepted-fixture"),
            executable: match platform {
                ServerPlatform::WindowsX64 => "arma3server_x64.exe",
                ServerPlatform::LinuxX64 => "arma3server_x64",
            }
            .into(),
            evidence: ExecutableEvidence {
                bytes,
                blake3: fingerprint.into(),
            },
            ownership: InstallationOwnership::Adopted,
            build_evidence: BuildEvidence::ExecutableFingerprintOnly,
        }
    }

    #[test]
    fn native_layout_compatibility_requires_the_exact_tested_executable() {
        for (platform, bytes, fingerprint) in [
            (
                ServerPlatform::WindowsX64,
                32_200_952,
                "ed0204a505ee8094356ce07ee9ce6736a570245e9098c0784771da7daf2d48ab",
            ),
            (
                ServerPlatform::LinuxX64,
                41_681_008,
                "3bca4c88ec42759b85fdda6c5be77cbc11ef9db46812d9f864dd191a0935ffbc",
            ),
        ] {
            let accepted = installation(platform, bytes, fingerprint);
            let observation = observe(&accepted).unwrap();
            assert_eq!(observation.tested_runtime_version, "2.22.154089");
            assert!(observation.shared_root_relative_directories);
            assert!(observation.isolated_copy_fallback);
            assert!(observation.base_keys_retained);
            let mut unknown = accepted.clone();
            unknown.evidence.bytes += 1;
            assert!(observe(&unknown).is_none());
            unknown = accepted.clone();
            unknown.evidence.blake3.replace_range(..1, "0");
            assert!(observe(&unknown).is_none());
            unknown = accepted.clone();
            unknown.platform = match platform {
                ServerPlatform::WindowsX64 => ServerPlatform::LinuxX64,
                ServerPlatform::LinuxX64 => ServerPlatform::WindowsX64,
            };
            assert!(observe(&unknown).is_none());
            unknown = accepted;
            unknown.game = ServerDefinitionKind::Reforger;
            assert!(observe(&unknown).is_none());
        }
    }

    #[test]
    fn reforger_layout_accepts_only_the_observed_game_platform_and_executable() {
        for (platform, bytes, digest) in [
            (
                ServerPlatform::WindowsX64,
                36_232_248,
                "3c13311b4e07d81d83db62de239ba883d297378da2c03185d31f2a6d3d007926",
            ),
            (
                ServerPlatform::LinuxX64,
                51_291_168,
                "5c09b8847f33d9c9bb65d678471c10d1f55d206fd6d2f3fc5f599ed7f41ac8d2",
            ),
        ] {
            let mut accepted = installation(platform, bytes, digest);
            accepted.game = ServerDefinitionKind::Reforger;
            accepted.executable = match platform {
                ServerPlatform::WindowsX64 => "ArmaReforgerServer.exe",
                ServerPlatform::LinuxX64 => "ArmaReforgerServer",
            }
            .into();
            let layout = observe(&accepted).unwrap();
            assert_eq!(layout.tested_runtime_version, "1.9.0.37");
            assert!(layout.isolated_copy_fallback);
            assert!(!layout.shared_root_relative_directories);
            assert!(!layout.base_keys_retained);
            let mut unknown = accepted.clone();
            unknown.evidence.blake3 = "f".repeat(64);
            assert!(observe(&unknown).is_none());
            unknown = accepted.clone();
            unknown.evidence.bytes += 1;
            assert!(observe(&unknown).is_none());
            unknown = accepted.clone();
            unknown.game = ServerDefinitionKind::Arma3;
            assert!(observe(&unknown).is_none());
            unknown = accepted.clone();
            unknown.platform = match platform {
                ServerPlatform::WindowsX64 => ServerPlatform::LinuxX64,
                ServerPlatform::LinuxX64 => ServerPlatform::WindowsX64,
            };
            assert!(observe(&unknown).is_none());
            accepted.executable = "OtherServer.exe".into();
            assert!(observe(&accepted).is_none());
        }
    }
}
