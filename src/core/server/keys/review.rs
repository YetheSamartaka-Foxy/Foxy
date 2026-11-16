use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::{
    BASE_KEY_HASH, ServerKeyCandidate, ServerKeyOrigin, ServerKeyReview, ServerKeyTrust,
    TrustedServerKey,
};
use crate::core::server::{
    Arma3MissionSource, ContentStageControl, ServerDefinition, ServerInstance, ServerInstanceId,
    ServerStore, ServerStoreError,
};

const MAX_KEYS: usize = 128;
const MAX_KEY_BYTES: u64 = 16 * 1024;

#[derive(serde::Serialize)]
struct FingerprintInput<'a> {
    context_root: &'a Path,
    space_id: &'a str,
    game_id: &'a str,
    space_name: &'a str,
    catalog_revision: u64,
    instance: &'a ServerInstance,
    stage_id: Option<&'a str>,
    stage_bytes: &'a [(String, String)],
    operator_bytes: &'a [(String, String)],
}

fn fingerprint(input: &FingerprintInput<'_>) -> Result<String, ServerStoreError> {
    let bytes = serde_json::to_vec(input).map_err(|_| ServerStoreError::Corrupt)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn read_key(path: &Path) -> Result<Vec<u8>, ServerStoreError> {
    if let Some(parent) = path.parent() {
        super::super::store::check_directory_chain(parent)?;
    }
    super::super::store::check_regular_file(path)?;
    let file = fs::File::open(path).map_err(|_| ServerStoreError::Io)?;
    let metadata = file.metadata().map_err(|_| ServerStoreError::Io)?;
    if !metadata.is_file() || metadata.len() > MAX_KEY_BYTES {
        return Err(ServerStoreError::Invalid(
            "Public key exceeds the 16 KiB limit".into(),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_KEY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ServerStoreError::Io)?;
    if bytes.len() as u64 != metadata.len() || bytes.len() as u64 > MAX_KEY_BYTES {
        return Err(ServerStoreError::StaleRevision);
    }
    Ok(bytes)
}

fn add_candidate(
    candidates: &mut BTreeMap<(String, String), TrustedServerKey>,
    issues: &mut Vec<String>,
    name: String,
    bytes: Vec<u8>,
    origin: ServerKeyOrigin,
) {
    match TrustedServerKey::new(name.clone(), bytes.clone(), origin.clone()) {
        Ok(mut key) => {
            let id = (name.to_ascii_lowercase(), key.blake3.clone());
            if let Some(existing) = candidates.get_mut(&id) {
                if existing.material == bytes {
                    if !existing.origins.contains(&origin) {
                        existing.origins.push(origin);
                    }
                    existing
                        .origins
                        .sort_by_key(|o| serde_json::to_string(o).unwrap_or_default());
                }
            } else {
                key.origins
                    .sort_by_key(|o| serde_json::to_string(o).unwrap_or_default());
                candidates.insert(id, key);
            }
        }
        Err(message) => issues.push(format!("{name}: {message}")),
    }
}

fn collect_folder(
    root: &Path,
    control: &ContentStageControl,
) -> Result<Vec<(String, Vec<u8>)>, ServerStoreError> {
    let folder = root.join("keys");
    super::super::store::check_directory_chain(&folder)?;
    if !folder.exists() {
        return Ok(Vec::new());
    }
    let mut entries = fs::read_dir(&folder)
        .map_err(|_| ServerStoreError::Io)?
        .map(|entry| entry.map_err(|_| ServerStoreError::Io))
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    if entries.len() > MAX_KEYS {
        return Err(ServerStoreError::Invalid(
            "Installation has too many public keys".into(),
        ));
    }
    let mut result = Vec::new();
    for entry in entries {
        control.checkpoint()?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| ServerStoreError::UnsafePath)?;
        if !name.to_ascii_lowercase().ends_with(".bikey") {
            continue;
        }
        result.push((name.clone(), read_key(&entry.path())?));
    }
    Ok(result)
}

type Collected = (
    Vec<ServerKeyCandidate>,
    Vec<String>,
    Vec<(String, String)>,
    Vec<(String, String)>,
);

fn collect(
    store: &ServerStore,
    instance: &ServerInstance,
    stage_id: Option<&str>,
    operators: &[PathBuf],
    control: &ContentStageControl,
) -> Result<Collected, ServerStoreError> {
    let mut candidates = BTreeMap::new();
    let mut issues = Vec::new();
    let mut stage_fingerprint = Vec::new();
    let mut operator_fingerprint = Vec::new();
    let mut base_keys = Vec::new();
    if let Some(installation_id) = &instance.installation_id {
        let catalog = store.load()?;
        let installation = catalog
            .installations
            .iter()
            .find(|item| &item.id == installation_id)
            .ok_or(ServerStoreError::InstallationNotFound)?;
        store.check_installation_root(installation)?;
        installation.verify()?;
        base_keys = collect_folder(&installation.directory, control)?;
        let base_keys_after = collect_folder(&installation.directory, control)?;
        installation.verify()?;
        if base_keys != base_keys_after {
            return Err(ServerStoreError::StaleRevision);
        }
        for (name, bytes) in &base_keys {
            let parsed = TrustedServerKey::new(
                name.clone(),
                bytes.clone(),
                ServerKeyOrigin::Base {
                    installation_id: installation_id.as_str().into(),
                    path: format!("keys/{name}"),
                },
            )
            .map_err(ServerStoreError::Invalid)?;
            stage_fingerprint.push((format!("base/{name}"), parsed.blake3));
            add_candidate(
                &mut candidates,
                &mut issues,
                name.clone(),
                bytes.clone(),
                ServerKeyOrigin::Base {
                    installation_id: installation_id.as_str().into(),
                    path: format!("keys/{name}"),
                },
            );
        }
    }
    if let Some(stage_id) = stage_id {
        let (manifest, report) =
            store.inspect_verified_content_stage(&instance.id, stage_id, control)?;
        let mission_profiles: BTreeSet<_> = manifest
            .missions
            .iter()
            .filter_map(|mission| match mission {
                Arma3MissionSource::Library { mission_id, .. } => Some(mission_id.as_str()),
                Arma3MissionSource::AddonClass => None,
            })
            .collect();
        let sources = manifest
            .sources
            .iter()
            .filter(|source| !mission_profiles.contains(source.profile_name.as_str()))
            .collect::<Vec<_>>();
        let mut count = 0;
        for entry in manifest
            .entries
            .iter()
            .filter(|entry| entry.kind == crate::core::server::StageEntryKind::File)
        {
            control.checkpoint()?;
            let Some(name) = entry.path.rsplit('/').next() else {
                continue;
            };
            if !name.to_ascii_lowercase().ends_with(".bikey") {
                continue;
            }
            count += 1;
            if count > MAX_KEYS {
                return Err(ServerStoreError::Invalid(
                    "Content stage has too many public keys".into(),
                ));
            }
            let Some(source) = sources
                .iter()
                .find(|source| entry.path.starts_with(&format!("{}/", source.destination)))
            else {
                issues.push(format!("{name}: staged key has no source provenance"));
                continue;
            };
            let path = report
                .directory
                .join(crate::core::server::content_staging::parse_relative(
                    &entry.path,
                )?);
            let bytes = read_key(&path)?;
            if bytes.len() as u64 != entry.bytes
                || entry.blake3.as_deref() != Some(blake3::hash(&bytes).to_hex().as_str())
            {
                return Err(ServerStoreError::StaleRevision);
            }
            let source_id = manifest
                .bindings
                .bindings
                .iter()
                .find(|b| b.profile.name == source.profile_name)
                .map(|b| b.binding.source_id.clone())
                .unwrap_or_default();
            add_candidate(
                &mut candidates,
                &mut issues,
                name.into(),
                bytes.clone(),
                ServerKeyOrigin::Stage {
                    source_id,
                    path: entry.path.clone(),
                },
            );
            stage_fingerprint.push((
                entry.path.clone(),
                blake3::hash(&bytes).to_hex().to_string(),
            ));
        }
        stage_fingerprint.sort();
    }
    for path in operators {
        control.checkpoint()?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(ServerStoreError::UnsafePath)?
            .to_owned();
        if !super::key_name_valid(&name) {
            return Err(ServerStoreError::Invalid(
                "Expected an ordinary public BIKEY file".into(),
            ));
        }
        let bytes = read_key(path)?;
        operator_fingerprint.push((name.clone(), blake3::hash(&bytes).to_hex().to_string()));
        add_candidate(
            &mut candidates,
            &mut issues,
            name.clone(),
            bytes,
            ServerKeyOrigin::Operator { filename: name },
        );
    }
    let mut result = Vec::new();
    for (_, key) in candidates {
        let effective_base = base_keys
            .iter()
            .any(|(name, bytes)| name.eq_ignore_ascii_case(&key.name) && bytes == &key.material);
        let implicit_tested_base = key.name.eq_ignore_ascii_case("a3.bikey")
            && key.material.len() == 155
            && key.blake3 == BASE_KEY_HASH;
        result.push(ServerKeyCandidate {
            id: format!("{}:{}", key.name.to_ascii_lowercase(), key.blake3),
            key,
            trusted: false,
            effective_base,
            implicit_tested_base,
        });
    }
    result.sort_by(|a, b| a.id.cmp(&b.id));
    let mut names = BTreeMap::<String, BTreeSet<String>>::new();
    for candidate in &result {
        names
            .entry(candidate.key.name.to_ascii_lowercase())
            .or_default()
            .insert(candidate.key.blake3.clone());
    }
    for (name, hashes) in names {
        if hashes.len() > 1 {
            issues.push(format!(
                "{name}: different key bytes share this destination filename"
            ));
        }
    }
    issues.sort();
    if result.len() > MAX_KEYS
        || result
            .iter()
            .map(|candidate| candidate.key.material.len())
            .sum::<usize>()
            > 512 * 1024
    {
        return Err(ServerStoreError::Invalid(
            "Public-key review exceeds its size limit".into(),
        ));
    }
    Ok((result, issues, stage_fingerprint, operator_fingerprint))
}

impl ServerStore {
    pub fn review_server_keys(
        &self,
        id: &ServerInstanceId,
        stage_id: Option<&str>,
        operator_files: &[PathBuf],
        control: &ContentStageControl,
    ) -> Result<ServerKeyReview, ServerStoreError> {
        control.checkpoint()?;
        validate_review_context(self)?;
        if operator_files.len() > MAX_KEYS {
            return Err(ServerStoreError::Invalid(
                "Select at most 128 operator public keys".into(),
            ));
        }
        let catalog = self.load()?;
        let instance = catalog
            .instances
            .iter()
            .find(|instance| &instance.id == id)
            .ok_or(ServerStoreError::NotFound)?;
        if !matches!(instance.definition, ServerDefinition::Arma3(_)) {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let (mut candidates, mut issues, stage_bytes, operator_bytes) =
            collect(self, instance, stage_id, operator_files, control)?;
        if let ServerDefinition::Arma3(arma) = &instance.definition {
            if instance
                .configuration_import
                .as_ref()
                .is_some_and(|imported| {
                    imported.files.iter().any(|file| {
                        file.role == crate::core::server::ConfigurationRole::Server
                            && file.ownership
                                == crate::core::server::ConfigurationOwnership::Operator
                    })
                })
            {
                issues.push("Operator server configuration controls signature checking; the typed signature setting may differ.".into());
            }
            if let Some(policy) = &arma.key_trust {
                if policy.instance_id != instance.id {
                    issues.push("Saved key trust belongs to another instance. Review and apply trust for this instance before deployment.".into());
                }
                if policy.instance_id == instance.id {
                    let current_ids: BTreeSet<_> = candidates
                        .iter()
                        .map(|candidate| candidate.id.clone())
                        .collect();
                    for key in &policy.keys {
                        key.validate().map_err(ServerStoreError::Invalid)?;
                        let key_id = format!("{}:{}", key.name.to_ascii_lowercase(), key.blake3);
                        if !current_ids.contains(&key_id) {
                            candidates.push(ServerKeyCandidate {
                                id: key_id,
                                key: key.clone(),
                                trusted: true,
                                effective_base: false,
                                implicit_tested_base: key.name.eq_ignore_ascii_case("a3.bikey")
                                    && key.material.len() == 155
                                    && key.blake3 == BASE_KEY_HASH,
                            });
                        }
                    }
                    candidates.sort_by(|a, b| a.id.cmp(&b.id));
                    for candidate in &mut candidates {
                        candidate.trusted = policy.keys.iter().any(|key| {
                            key.name.eq_ignore_ascii_case(&candidate.key.name)
                                && key.identity == candidate.key.identity
                                && key.material == candidate.key.material
                        });
                    }
                    let mut by_name = BTreeMap::<String, BTreeSet<String>>::new();
                    for candidate in &candidates {
                        by_name
                            .entry(candidate.key.name.to_ascii_lowercase())
                            .or_default()
                            .insert(candidate.key.blake3.clone());
                    }
                    for (name, hashes) in by_name {
                        if hashes.len() > 1
                            && !issues
                                .iter()
                                .any(|issue| issue.starts_with(&format!("{name}:")))
                        {
                            issues.push(format!(
                                "{name}: different key bytes share this destination filename"
                            ));
                        }
                    }
                    issues.sort();
                }
            }
        }
        let fingerprint = fingerprint(&FingerprintInput {
            context_root: self.context().data_root(),
            space_id: self.context().space_id(),
            game_id: self.context().game_id(),
            space_name: self.context().display_name(),
            catalog_revision: catalog.revision,
            instance,
            stage_id,
            stage_bytes: &stage_bytes,
            operator_bytes: &operator_bytes,
        })?;
        let signature_verification = match &instance.definition {
            ServerDefinition::Arma3(arma) => match arma.verify_signatures {
                crate::core::server::SignatureVerification::Disabled => "disabled",
                crate::core::server::SignatureVerification::V2 => "v2",
            },
            _ => return Err(ServerStoreError::UnsupportedGame),
        };
        validate_review_context(self)?;
        Ok(ServerKeyReview {
            context: self.context().clone(),
            operator_files: operator_files.to_vec(),
            instance_id: id.clone(),
            instance_revision: instance.revision,
            stage_id: stage_id.map(str::to_owned),
            candidates,
            issues,
            fingerprint,
            signature_verification: signature_verification.into(),
            cryptographic_verification: false,
            writes_performed: false,
        })
    }

    pub fn apply_server_key_review(
        &self,
        review: &ServerKeyReview,
        selected: &[String],
        control: &ContentStageControl,
    ) -> Result<ServerInstance, ServerStoreError> {
        control.checkpoint()?;
        if review.context != *self.context() {
            return Err(ServerStoreError::StaleRevision);
        }
        let current = self.review_server_keys(
            &review.instance_id,
            review.stage_id.as_deref(),
            &review.operator_files,
            control,
        )?;
        if current.fingerprint != review.fingerprint
            || current.instance_revision != review.instance_revision
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let known: BTreeMap<_, _> = current
            .candidates
            .iter()
            .map(|candidate| (candidate.id.as_str(), candidate))
            .collect();
        let mut selected_ids = BTreeSet::new();
        if selected
            .iter()
            .any(|id| !selected_ids.insert(id.as_str()) || !known.contains_key(id.as_str()))
        {
            return Err(ServerStoreError::Invalid(
                "Selected public-key candidate is unknown or duplicated".into(),
            ));
        }
        let mut keys = Vec::new();
        for candidate in &current.candidates {
            if candidate.implicit_tested_base || selected_ids.contains(candidate.id.as_str()) {
                keys.push(candidate.key.clone());
            } else if candidate.effective_base {
                return Err(ServerStoreError::Invalid(
                    "Select every additional effective installation key".into(),
                ));
            }
        }
        keys.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        });
        let policy = ServerKeyTrust {
            instance_id: review.instance_id.clone(),
            keys,
        };
        policy.validate().map_err(ServerStoreError::Invalid)?;
        let mut instance = self.show(&review.instance_id)?;
        if let ServerDefinition::Arma3(arma) = &mut instance.definition {
            arma.key_trust = Some(Box::new(policy));
        } else {
            return Err(ServerStoreError::UnsupportedGame);
        }
        control.checkpoint()?;
        let plan = self.plan(crate::core::server::DraftChange::Replace {
            instance: instance.clone(),
            expected_revision: review.instance_revision,
        })?;
        crate::core::server::owner::with_write(&self.context().database_path(), || {
            Ok((|| {
                control.checkpoint()?;
                validate_review_context(self)?;
                let latest = self.review_server_keys(
                    &review.instance_id,
                    review.stage_id.as_deref(),
                    &review.operator_files,
                    control,
                )?;
                if latest.fingerprint != review.fingerprint
                    || latest.instance_revision != review.instance_revision
                {
                    return Err(ServerStoreError::StaleRevision);
                }
                self.apply_owned(plan)
            })())
        })
        .map_err(|_| ServerStoreError::Ownership)??;
        instance.revision = review
            .instance_revision
            .checked_add(1)
            .ok_or(ServerStoreError::Corrupt)?;
        Ok(instance)
    }
}

fn validate_review_context(store: &ServerStore) -> Result<(), ServerStoreError> {
    let current = crate::core::server::ServerContext::capture(
        store.context().data_root(),
        Some(store.context().space_id()),
    )
    .map_err(ServerStoreError::Invalid)?;
    if &current != store.context() {
        return Err(ServerStoreError::StaleRevision);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::{DraftChange, ServerContext};
    use std::fs;

    fn public_bikey() -> Vec<u8> {
        let mut blob = vec![
            6, 2, 0, 0, 0x00, 0x24, 0x00, 0x00, b'R', b'S', b'A', b'1', 0x00, 0x08, 0x00, 0x00,
            0x01, 0x00, 0x01, 0x00,
        ];
        blob.extend(std::iter::repeat_n(0x5a, 256));
        let mut bytes = b"FoxyReviewKey".to_vec();
        bytes.push(0);
        bytes.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        bytes.extend(blob);
        bytes
    }

    #[test]
    fn key_review_is_read_only_and_apply_rechecks_selection_and_sources() {
        const CHILD: &str = "FOXY_SERVER_KEY_REVIEW_CLAIM_TEST";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "core::server::keys::review::tests::key_review_is_read_only_and_apply_rechecks_selection_and_sources",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("games.json"),
            include_bytes!("../../../../examples/json/appdata/games.json"),
        )
        .unwrap();
        let context = ServerContext::capture(root.path(), Some("arma3")).unwrap();
        let store = ServerStore::new(context.clone()).unwrap();
        let claim = context.database_path();
        assert_eq!(
            crate::core::tasks::db_process_lock::acquire(&claim),
            crate::core::tasks::db_process_lock::LockOutcome::Acquired
        );
        let plan = store
            .plan(DraftChange::Add {
                name: "KeyReview".into(),
            })
            .unwrap();
        let instance = plan.instance.clone();
        store.apply(plan).unwrap();

        let source = root.path().join("FoxyReview.bikey");
        let bytes = public_bikey();
        fs::write(&source, &bytes).unwrap();
        let before = store.load().unwrap();
        let control = ContentStageControl::default();
        let review = store
            .review_server_keys(&instance.id, None, std::slice::from_ref(&source), &control)
            .unwrap();
        assert_eq!(review.signature_verification, "v2");
        assert!(!review.cryptographic_verification);
        assert!(!review.writes_performed);
        assert_eq!(store.load().unwrap(), before);
        assert_eq!(fs::read(&source).unwrap(), bytes);
        let candidate = review.candidates.first().unwrap().id.clone();
        let unknown = store
            .apply_server_key_review(&review, &["unknown".into()], &control)
            .unwrap_err();
        assert!(matches!(unknown, ServerStoreError::Invalid(_)));
        assert_eq!(store.load().unwrap(), before);

        let applied = store
            .apply_server_key_review(&review, &[candidate], &control)
            .unwrap();
        assert_eq!(applied.revision, instance.revision + 1);
        let ServerDefinition::Arma3(arma) = applied.definition else {
            panic!()
        };
        assert_eq!(arma.key_trust.unwrap().keys.len(), 1);

        let stale = store
            .review_server_keys(&instance.id, None, std::slice::from_ref(&source), &control)
            .unwrap();
        fs::write(&source, b"changed bytes").unwrap();
        assert_eq!(
            store.apply_server_key_review(&stale, &[stale.candidates[0].id.clone()], &control),
            Err(ServerStoreError::StaleRevision)
        );
        crate::core::tasks::db_process_lock::release_for(&claim).unwrap();
    }
}
