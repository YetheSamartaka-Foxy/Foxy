use std::collections::{HashMap, HashSet};

use super::super::{
    AddonSupply, ContentBindingsDocument, ServerInstanceId, ServerStore, ServerStoreError,
};
use super::validation::{expected_sources_with_acquisitions, fingerprint};
use super::{
    ContentStageControl, ContentStageManifest, ContentStagePhase, ContentStagePlan,
    ContentStageReport, SCHEMA_VERSION, StageEntryKind, StageRepositorySnapshot, StageSource, tree,
};

impl ServerStore {
    pub(in crate::core::server) fn publish_acquired_content_stage(
        &self,
        id: &ServerInstanceId,
        expected_bindings_fingerprint: &str,
        repositories: Vec<StageRepositorySnapshot>,
        acquired_sources: Vec<StageSource>,
        control: &ContentStageControl,
    ) -> Result<ContentStageReport, ServerStoreError> {
        let result = (|| {
            control.checkpoint()?;
            control.start_phase(ContentStagePhase::Preparing, 0, 0);
            if self.selected_reforger_addon_lock(id)?.is_some() {
                return Err(ServerStoreError::Invalid(
                    "Repository acquisition cannot be combined with native Reforger addon revisions.".into(),
                ));
            }
            let bindings = self.current_stage_bindings(id, expected_bindings_fingerprint)?;
            let missions = self.selected_library_missions(id, control)?;
            let logical_sources = expected_sources_with_acquisitions(&bindings, &repositories)?;
            let mut sources = logical_sources;
            self.append_mission_stage_sources(&mut sources, &missions)?;
            let download_keys = download_source_keys(&bindings, &repositories, &sources)?;
            if download_keys.len() != acquired_sources.len() || acquired_sources.is_empty() {
                return Err(ServerStoreError::StaleRevision);
            }
            let mut acquired_by_key = HashMap::new();
            for source in acquired_sources {
                let key = (source.profile_name.clone(), source.destination.clone());
                if !download_keys.contains(&key) || acquired_by_key.insert(key, source).is_some() {
                    return Err(ServerStoreError::StaleRevision);
                }
            }
            let actual_sources = sources
                .iter()
                .map(|logical| {
                    acquired_by_key
                        .remove(&(logical.profile_name.clone(), logical.destination.clone()))
                        .unwrap_or_else(|| logical.clone())
                })
                .collect::<Vec<_>>();
            if !acquired_by_key.is_empty() {
                return Err(ServerStoreError::StaleRevision);
            }
            let acquisition_root = self
                .context()
                .space_dir()
                .join("server/instances")
                .join(id.as_str())
                .join("acquisition");
            validate_actual_sources_against_downloads(
                &bindings,
                &sources,
                &actual_sources,
                &download_keys,
                &acquisition_root,
            )?;
            self.check_stage_separation(id, &actual_sources)?;
            let entries = tree::inspect_sources(&actual_sources, control)?;
            let total_bytes = entries
                .iter()
                .filter(|entry| entry.kind == StageEntryKind::File)
                .try_fold(0u64, |sum, entry| {
                    sum.checked_add(entry.bytes)
                        .ok_or(ServerStoreError::Corrupt)
                })?;
            let reforger_addons = self.inspect_reforger_stage_lock(id, &actual_sources, control)?;
            let manifest = ContentStageManifest {
                schema_version: SCHEMA_VERSION,
                bindings_fingerprint: expected_bindings_fingerprint.into(),
                bindings,
                sources,
                missions,
                reforger_addons,
                entries,
                total_bytes,
                acquisitions: repositories,
            };
            self.validate_stage_manifest(id, &manifest)?;
            self.check_stage_inputs(&manifest)?;
            control.checkpoint()?;
            let proof = fingerprint(&manifest)?;
            let stage_id = format!("stage_{proof}");
            let directory = self.stage_root(id).join(&stage_id);
            super::super::store::check_directory_chain(&directory)?;
            let plan = ContentStagePlan {
                context: self.context().clone(),
                instance_id: id.clone(),
                proof: proof.clone(),
                manifest,
                fingerprint: proof,
                stage_id,
                directory,
            };
            let mut report =
                self.apply_content_stage_from_sources(plan, actual_sources, control)?;
            report.downloads_started = true;
            Ok(report)
        })();
        control.finish(&result);
        result
    }
}

fn download_source_keys(
    bindings: &ContentBindingsDocument,
    repositories: &[StageRepositorySnapshot],
    sources: &[StageSource],
) -> Result<HashSet<(String, String)>, ServerStoreError> {
    let mut keys = HashSet::new();
    for binding in bindings
        .bindings
        .iter()
        .filter(|binding| binding.profile.supply == AddonSupply::Download)
    {
        let expected: Vec<std::path::PathBuf> = if binding.profile.addons.is_empty() {
            let snapshot = repositories
                .iter()
                .find(|snapshot| snapshot.profile_name == binding.profile.name)
                .ok_or(ServerStoreError::StaleRevision)?;
            snapshot
                .addons
                .iter()
                .map(|addon| binding.profile.local_path.join(&addon.name))
                .collect()
        } else {
            binding
                .profile
                .addons
                .iter()
                .map(|addon| binding.profile.local_path.join(addon))
                .collect()
        };
        for source in sources.iter().filter(|source| {
            source.profile_name == binding.profile.name && expected.contains(&source.source)
        }) {
            if !keys.insert((source.profile_name.clone(), source.destination.clone())) {
                return Err(ServerStoreError::Corrupt);
            }
        }
    }
    Ok(keys)
}

fn validate_actual_sources_against_downloads(
    bindings: &ContentBindingsDocument,
    logical_sources: &[StageSource],
    actual_sources: &[StageSource],
    download_keys: &HashSet<(String, String)>,
    acquisition_root: &std::path::Path,
) -> Result<(), ServerStoreError> {
    if logical_sources.len() != actual_sources.len() {
        return Err(ServerStoreError::StaleRevision);
    }
    let acquisition_root_physical = super::validation::physical_directory(acquisition_root)?;
    for (logical, actual) in logical_sources.iter().zip(actual_sources) {
        if logical.profile_name != actual.profile_name || logical.destination != actual.destination
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let key = (logical.profile_name.clone(), logical.destination.clone());
        if download_keys.contains(&key) {
            let relative = actual
                .source
                .strip_prefix(acquisition_root)
                .map_err(|_| ServerStoreError::UnsafePath)?;
            let components = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>();
            if components.len() != 6
                || !components[0].starts_with(".acquire-")
                || !components[1].strip_prefix("repo-").is_some_and(|number| {
                    !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
                })
                || components[2] != "games"
                || components[3] != bindings.game_space_id
                || components[4] != "content"
                || components[5] != logical.destination.rsplit('/').next().unwrap_or_default()
            {
                return Err(ServerStoreError::UnsafePath);
            }
            let physical = super::validation::physical_directory(&actual.source)?;
            if !physical.starts_with(&acquisition_root_physical) {
                return Err(ServerStoreError::UnsafePath);
            }
            for binding in &bindings.bindings {
                for source_root in [&binding.folder.path, &binding.profile.local_path] {
                    let source_physical = super::validation::physical_directory(source_root)?;
                    if super::super::installation::paths_overlap(&physical, &source_physical) {
                        return Err(ServerStoreError::UnsafePath);
                    }
                }
            }
        } else if logical.source != actual.source {
            return Err(ServerStoreError::StaleRevision);
        }
    }
    Ok(())
}
