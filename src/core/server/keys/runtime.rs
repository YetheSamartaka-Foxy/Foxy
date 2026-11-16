use std::collections::BTreeSet;

use super::super::runtime_payload::PayloadInventory;
use super::{BASE_KEY_HASH, ServerKeyOrigin, ServerKeyTrust, TrustedServerKey};
use crate::core::server::{
    ServerInstallation, ServerInstanceId, ServerStoreError,
    content_staging::{ContentStageManifest, StageEntryKind},
};

const BASE_KEY_NAME: &str = "a3.bikey";
const BASE_KEY_BYTES: usize = 155;

pub(in crate::core::server) fn private_keys(
    policy: Option<&ServerKeyTrust>,
    id: &ServerInstanceId,
    installation: &ServerInstallation,
    payload: &PayloadInventory,
    stage: Option<&ContentStageManifest>,
) -> Result<Vec<TrustedServerKey>, ServerStoreError> {
    private_keys_with_baseline(
        policy,
        id,
        installation,
        payload,
        stage,
        BASE_KEY_HASH,
        BASE_KEY_BYTES,
    )
}

fn private_keys_with_baseline(
    policy: Option<&ServerKeyTrust>,
    id: &ServerInstanceId,
    installation: &ServerInstallation,
    payload: &PayloadInventory,
    stage: Option<&ContentStageManifest>,
    baseline_hash: &str,
    baseline_bytes: usize,
) -> Result<Vec<TrustedServerKey>, ServerStoreError> {
    if installation.id.as_str().is_empty() || payload.entries.len() > 100_000 {
        return Err(ServerStoreError::Corrupt);
    }
    if stage.is_some_and(|manifest| {
        manifest.bindings.instance_id != *id || manifest.schema_version != 1
    }) {
        return Err(ServerStoreError::StaleRevision);
    }

    let bikey_entries = payload
        .entries
        .iter()
        .filter(|entry| {
            entry.kind == StageEntryKind::File
                && entry
                    .path
                    .split('/')
                    .next()
                    .is_some_and(|part| part.eq_ignore_ascii_case("keys"))
                && entry
                    .path
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| name.to_ascii_lowercase().ends_with(".bikey"))
        })
        .collect::<Vec<_>>();
    if bikey_entries.len() > 128 {
        return Err(ServerStoreError::Invalid(
            "Runtime payload contains too many public keys".into(),
        ));
    }
    for entry in &bikey_entries {
        if entry.path.split('/').count() != 2
            || entry.path.split('/').nth(1).is_none_or(str::is_empty)
        {
            return Err(ServerStoreError::Invalid(
                "Runtime payload contains a nested public key under its keys directory".into(),
            ));
        }
    }

    let Some(policy) = policy else {
        let baseline = bikey_entries.iter().any(|entry| {
            entry.path.eq_ignore_ascii_case("keys/a3.bikey")
                && entry.bytes == baseline_bytes as u64
                && entry.blake3.as_deref() == Some(baseline_hash)
        });
        if !baseline {
            return Err(ServerStoreError::Invalid(
                "Runtime payload does not contain the tested base public key".into(),
            ));
        }
        if bikey_entries.len() != 1 {
            return Err(ServerStoreError::Invalid(
                "Additional runtime public keys require an explicit trust policy".into(),
            ));
        }
        return Ok(Vec::new());
    };

    if &policy.instance_id != id {
        return Err(ServerStoreError::StaleRevision);
    }
    policy.validate().map_err(ServerStoreError::Invalid)?;

    let installed_baseline = bikey_entries.iter().any(|entry| {
        entry.path.eq_ignore_ascii_case("keys/a3.bikey")
            && entry.bytes == baseline_bytes as u64
            && entry.blake3.as_deref() == Some(baseline_hash)
    });
    let baseline = installed_baseline
        && policy.keys.iter().any(|key| {
            key.name.eq_ignore_ascii_case(BASE_KEY_NAME)
                && key.material.len() == baseline_bytes
                && key.blake3 == baseline_hash
                && (stage.is_none()
                    || key.origins.iter().any(|origin| {
                        matches!(origin, ServerKeyOrigin::Base { installation_id, path }
                        if installation_id == installation.id.as_str()
                            && path.eq_ignore_ascii_case("keys/a3.bikey"))
                            && origin_matches(origin, key, installation, payload, stage, true)
                    }))
        });
    if !baseline {
        return Err(ServerStoreError::Invalid(
            "Trust policy is missing the exact tested base public key".into(),
        ));
    }

    let mut represented_paths = BTreeSet::new();
    let mut selected = Vec::new();
    for key in &policy.keys {
        let key_origins = if stage.is_none() {
            key.origins.iter().collect::<Vec<_>>()
        } else {
            key.origins
                .iter()
                .filter(|origin| origin_matches(origin, key, installation, payload, stage, false))
                .collect::<Vec<_>>()
        };
        if key_origins.is_empty() {
            return Err(ServerStoreError::StaleRevision);
        }
        for origin in key_origins {
            match origin {
                ServerKeyOrigin::Base { path, .. } | ServerKeyOrigin::Stage { path, .. } => {
                    if !represented_paths.insert(path.to_ascii_lowercase()) {
                        return Err(ServerStoreError::Invalid(
                            "Trust policy has colliding public-key origins".into(),
                        ));
                    }
                }
                ServerKeyOrigin::Operator { .. } => {}
            }
        }
        let already_in_base = payload.entries.iter().any(|entry| {
            entry.kind == StageEntryKind::File
                && entry
                    .path
                    .eq_ignore_ascii_case(&format!("keys/{}", key.name))
                && entry.bytes == key.material.len() as u64
                && entry.blake3.as_deref() == Some(key.blake3.as_str())
        });
        let tested_base = key.name.eq_ignore_ascii_case(BASE_KEY_NAME)
            && key.material.len() == baseline_bytes
            && key.blake3 == baseline_hash;
        if !(already_in_base || tested_base) {
            selected.push(key.clone());
        }
    }

    for entry in bikey_entries {
        if !represented_paths.contains(&entry.path.to_ascii_lowercase()) {
            return Err(ServerStoreError::Invalid(
                "Runtime payload contains an untrusted public key".into(),
            ));
        }
    }
    Ok(selected)
}

fn origin_matches(
    origin: &ServerKeyOrigin,
    key: &TrustedServerKey,
    installation: &ServerInstallation,
    payload: &PayloadInventory,
    stage: Option<&ContentStageManifest>,
    require_base: bool,
) -> bool {
    match origin {
        ServerKeyOrigin::Base {
            installation_id,
            path,
        } => {
            installation_id == installation.id.as_str()
                && payload.entries.iter().any(|entry| {
                    entry.path == *path
                        && entry.kind == StageEntryKind::File
                        && entry.bytes == key.material.len() as u64
                        && entry.blake3.as_deref() == Some(key.blake3.as_str())
                        && filename(path).is_some_and(|name| name.eq_ignore_ascii_case(&key.name))
                })
        }
        ServerKeyOrigin::Stage { source_id, path } => {
            let Some(stage) = stage else {
                return false;
            };
            stage.sources.iter().any(|source| {
                path.starts_with(&format!("{}/", source.destination))
                    && stage.bindings.bindings.iter().any(|binding| {
                        binding.binding.source_id == *source_id
                            && binding.profile.name == source.profile_name
                    })
            }) && stage.entries.iter().any(|entry| {
                entry.path == *path
                    && entry.kind == StageEntryKind::File
                    && entry.bytes == key.material.len() as u64
                    && entry.blake3.as_deref() == Some(key.blake3.as_str())
                    && filename(path).is_some_and(|name| name.eq_ignore_ascii_case(&key.name))
            })
        }
        ServerKeyOrigin::Operator { filename } => {
            !require_base && filename.eq_ignore_ascii_case(&key.name)
        }
    }
}

fn filename(path: &str) -> Option<&str> {
    path.rsplit('/').next()
}

#[cfg(test)]
mod tests {
    use super::super::super::{
        content_bindings::ReviewedContentBinding, runtime_payload::PayloadEntry,
    };
    use super::*;
    use crate::core::server::{
        ContentBindingsDocument, InstallationId, ServerAddonBinding, ServerAddonRole,
        ServerDefinitionKind, ServerKeyOrigin, ServerPlatform,
        content_sources::{AddonFolder, RepositoryProfile},
        content_staging::{StageEntry, StageSource},
        installation::{BuildEvidence, ExecutableEvidence, InstallationOwnership},
    };
    use std::path::PathBuf;

    fn material(seed: u8) -> Vec<u8> {
        let mut blob = vec![
            6, 2, 0, 0, 0, 0x24, 0, 0, b'R', b'S', b'A', b'1', 0, 8, 0, 0, 1, 0, 1, 0,
        ];
        blob.extend(std::iter::repeat_n(seed, 256));
        let mut bytes = format!("Authority{seed}").into_bytes();
        bytes.push(0);
        bytes.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        bytes.extend(blob);
        bytes
    }

    fn install() -> ServerInstallation {
        ServerInstallation {
            id: InstallationId::new(),
            game: ServerDefinitionKind::Arma3,
            platform: ServerPlatform::WindowsX64,
            directory: PathBuf::from("C:/fixture/install"),
            executable: "arma3server_x64.exe".into(),
            evidence: ExecutableEvidence {
                bytes: 64,
                blake3: "0".repeat(64),
            },
            ownership: InstallationOwnership::Adopted,
            build_evidence: BuildEvidence::ExecutableFingerprintOnly,
        }
    }

    fn entry(path: &str, bytes: &[u8]) -> PayloadEntry {
        PayloadEntry {
            path: path.into(),
            kind: StageEntryKind::File,
            bytes: bytes.len() as u64,
            blake3: Some(blake3::hash(bytes).to_hex().to_string()),
            executable: false,
        }
    }

    fn base_payload(base: &[u8], extras: &[(&str, &[u8])]) -> PayloadInventory {
        let mut entries = vec![entry("keys/a3.bikey", base)];
        entries.extend(extras.iter().map(|(path, bytes)| entry(path, bytes)));
        PayloadInventory {
            platform: ServerPlatform::WindowsX64,
            entries,
            total_bytes: 0,
        }
    }

    fn key(name: &str, bytes: Vec<u8>, origin: ServerKeyOrigin) -> TrustedServerKey {
        TrustedServerKey::new(name.into(), bytes, origin).unwrap()
    }

    fn policy(id: &ServerInstanceId, keys: Vec<TrustedServerKey>) -> ServerKeyTrust {
        ServerKeyTrust {
            instance_id: id.clone(),
            keys,
        }
    }

    fn synthetic_baseline() -> (Vec<u8>, String) {
        let bytes = material(0x31);
        let hash = blake3::hash(&bytes).to_hex().to_string();
        (bytes, hash)
    }

    #[test]
    fn refuses_unreviewed_base_key_and_accepts_explicit_exact_material() {
        let installation = install();
        let id = ServerInstanceId::new();
        let (base, base_hash) = synthetic_baseline();
        let extra = material(0x32);
        let payload = base_payload(&base, &[("keys/extra.bikey", &extra)]);
        assert!(
            private_keys_with_baseline(
                None,
                &id,
                &installation,
                &payload,
                None,
                &base_hash,
                base.len(),
            )
            .is_err()
        );

        let trust = policy(
            &id,
            vec![
                key(
                    "a3.bikey",
                    base.clone(),
                    ServerKeyOrigin::Base {
                        installation_id: installation.id.as_str().into(),
                        path: "keys/a3.bikey".into(),
                    },
                ),
                key(
                    "extra.bikey",
                    extra,
                    ServerKeyOrigin::Base {
                        installation_id: installation.id.as_str().into(),
                        path: "keys/extra.bikey".into(),
                    },
                ),
            ],
        );
        let selected = private_keys_with_baseline(
            Some(&trust),
            &id,
            &installation,
            &payload,
            Some(&empty_stage(id.clone())),
            &base_hash,
            base.len(),
        )
        .unwrap();
        assert!(selected.is_empty());
    }

    #[test]
    fn rejects_foreign_instance_and_stale_staged_key_bytes() {
        let installation = install();
        let id = ServerInstanceId::new();
        let foreign = ServerInstanceId::new();
        let (base, base_hash) = synthetic_baseline();
        let extra = material(0x33);
        let payload = base_payload(&base, &[]);
        let base_key = key(
            "a3.bikey",
            base.clone(),
            ServerKeyOrigin::Base {
                installation_id: installation.id.as_str().into(),
                path: "keys/a3.bikey".into(),
            },
        );
        let extra_key = key(
            "stage.bikey",
            extra.clone(),
            ServerKeyOrigin::Stage {
                source_id: "repo".into(),
                path: "addons/@mod_01/keys/stage.bikey".into(),
            },
        );
        let trust = policy(&id, vec![base_key.clone(), extra_key]);
        let selected = private_keys_with_baseline(
            Some(&trust),
            &id,
            &installation,
            &payload,
            Some(&stage(&id, &extra)),
            &base_hash,
            base.len(),
        )
        .unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|key| key.name.as_str())
                .collect::<Vec<_>>(),
            vec!["stage.bikey"]
        );
        assert!(
            private_keys_with_baseline(
                Some(&policy(&foreign, trust.keys.clone())),
                &id,
                &installation,
                &payload,
                Some(&stage(&id, &extra)),
                &base_hash,
                base.len(),
            )
            .is_err()
        );

        assert!(matches!(
            private_keys_with_baseline(
                Some(&trust),
                &id,
                &installation,
                &payload,
                Some(&stage(&id, &material(0x34))),
                &base_hash,
                base.len(),
            ),
            Err(ServerStoreError::StaleRevision)
        ));
    }

    #[test]
    fn accepts_an_owned_operator_snapshot_without_external_files() {
        let installation = install();
        let id = ServerInstanceId::new();
        let (base, base_hash) = synthetic_baseline();
        let operator = material(0x35);
        let payload = base_payload(&base, &[]);
        let trust = policy(
            &id,
            vec![
                key(
                    "a3.bikey",
                    base.clone(),
                    ServerKeyOrigin::Base {
                        installation_id: installation.id.as_str().into(),
                        path: "keys/a3.bikey".into(),
                    },
                ),
                key(
                    "operator.bikey",
                    operator,
                    ServerKeyOrigin::Operator {
                        filename: "operator.bikey".into(),
                    },
                ),
            ],
        );
        let selected = private_keys_with_baseline(
            Some(&trust),
            &id,
            &installation,
            &payload,
            Some(&empty_stage(id.clone())),
            &base_hash,
            base.len(),
        )
        .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "operator.bikey");
    }

    #[test]
    fn ignores_other_payload_bikey_files_and_rejects_nested_key_files() {
        let installation = install();
        let id = ServerInstanceId::new();
        let (base, base_hash) = synthetic_baseline();
        let elsewhere = material(0x36);
        let payload = base_payload(&base, &[("dta/vendor.bikey", &elsewhere)]);
        assert!(
            private_keys_with_baseline(
                None,
                &id,
                &installation,
                &payload,
                None,
                &base_hash,
                base.len(),
            )
            .unwrap()
            .is_empty()
        );

        let nested = base_payload(&base, &[("keys/sub/nested.bikey", &elsewhere)]);
        assert!(
            private_keys_with_baseline(
                None,
                &id,
                &installation,
                &nested,
                None,
                &base_hash,
                base.len(),
            )
            .is_err()
        );
    }

    fn empty_stage(id: ServerInstanceId) -> ContentStageManifest {
        ContentStageManifest {
            reforger_addons: None,
            schema_version: 1,
            bindings_fingerprint: "0".repeat(64),
            bindings: ContentBindingsDocument {
                schema_version: 1,
                game_space_id: "space".into(),
                game_id: "arma3".into(),
                instance_id: id,
                generation: 1,
                instance_revision: 1,
                catalog_revision: 1,
                catalog_fingerprint: "0".repeat(64),
                content_sources_fingerprint: Some("0".repeat(64)),
                bindings: vec![binding()],
            },
            sources: vec![StageSource {
                profile_name: "repo".into(),
                source: PathBuf::from("source"),
                destination: "addons/@mod_01".into(),
            }],
            missions: Vec::new(),
            acquisitions: Vec::new(),
            entries: Vec::new(),
            total_bytes: 0,
        }
    }

    fn stage(id: &ServerInstanceId, bytes: &[u8]) -> ContentStageManifest {
        let mut stage = empty_stage(id.clone());
        stage.entries.push(StageEntry {
            path: "addons/@mod_01/keys/stage.bikey".into(),
            kind: StageEntryKind::File,
            bytes: bytes.len() as u64,
            blake3: Some(blake3::hash(bytes).to_hex().to_string()),
        });
        stage.total_bytes = bytes.len() as u64;
        stage
    }

    fn binding() -> ReviewedContentBinding {
        let profile = RepositoryProfile {
            name: "repo".into(),
            remote_url: "https://repo.example.invalid/".into(),
            local_path: PathBuf::from("repo"),
            folder_id: "folder".into(),
            supply: crate::core::server::AddonSupply::AlreadyPresent,
            addons: Vec::new(),
        };
        ReviewedContentBinding {
            binding: ServerAddonBinding {
                source_id: "repo".into(),
                role: ServerAddonRole::Required,
                order: 0,
            },
            folder: AddonFolder {
                id: "folder".into(),
                path: PathBuf::from("folder"),
            },
            profile,
        }
    }
}
