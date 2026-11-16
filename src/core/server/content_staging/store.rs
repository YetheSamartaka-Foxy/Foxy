use std::fs;
use std::io::{Read, Write};

use serde::Deserialize;

use super::super::AddonSupply;
use super::super::{ServerInstanceId, ServerStore, ServerStoreError};
use super::validation::{expected_sources, fingerprint, valid_stage_id};
use super::{
    ContentStageControl, ContentStageList, ContentStageManifest, ContentStagePhase,
    ContentStagePlan, ContentStageReport, MANIFEST_FILE, MAX_MANIFEST_BYTES, SCHEMA_VERSION,
    StageEntryKind, tree,
};

impl ServerStore {
    pub fn plan_content_stage(
        &self,
        id: &ServerInstanceId,
        expected_binding_fingerprint: &str,
        control: &ContentStageControl,
    ) -> Result<ContentStagePlan, ServerStoreError> {
        let result = (|| {
            control.checkpoint()?;
            control.start_phase(ContentStagePhase::Preparing, 0, 0);
            let bindings = self.current_stage_bindings(id, expected_binding_fingerprint)?;
            let missions = self.selected_library_missions(id, control)?;
            let reforger_addon_selection = self.selected_reforger_addon_lock(id)?;
            let mut sources = expected_sources(&bindings)?;
            self.append_reforger_stage_sources(
                id,
                reforger_addon_selection.as_ref(),
                &mut sources,
            )?;
            self.append_mission_stage_sources(&mut sources, &missions)?;
            self.check_stage_separation(id, &sources)?;
            let entries = tree::inspect_sources(&sources, control)?;
            let total_bytes = entries
                .iter()
                .filter(|entry| entry.kind == StageEntryKind::File)
                .try_fold(0u64, |sum, entry| {
                    sum.checked_add(entry.bytes)
                        .ok_or(ServerStoreError::Corrupt)
                })?;
            let reforger_addons = self.inspect_reforger_stage_lock(id, &sources, control)?;
            let manifest = ContentStageManifest {
                schema_version: SCHEMA_VERSION,
                bindings_fingerprint: expected_binding_fingerprint.into(),
                bindings,
                sources,
                acquisitions: Vec::new(),
                missions,
                reforger_addons,
                entries,
                total_bytes,
            };
            self.validate_stage_manifest(id, &manifest)?;
            self.check_stage_inputs(&manifest)?;
            control.checkpoint()?;
            let proof = fingerprint(&manifest)?;
            let stage_id = format!("stage_{proof}");
            let directory = self.stage_root(id).join(&stage_id);
            super::super::store::check_directory_chain(&directory)?;
            Ok(ContentStagePlan {
                context: self.context().clone(),
                instance_id: id.clone(),
                proof: proof.clone(),
                manifest,
                fingerprint: proof,
                stage_id,
                directory,
            })
        })();
        control.finish(&result);
        result
    }

    pub fn apply_content_stage(
        &self,
        plan: ContentStagePlan,
        control: &ContentStageControl,
    ) -> Result<ContentStageReport, ServerStoreError> {
        let actual_sources = plan.manifest.sources.clone();
        self.apply_content_stage_from_sources(plan, actual_sources, control)
    }

    pub(super) fn apply_content_stage_from_sources(
        &self,
        plan: ContentStagePlan,
        actual_sources: Vec<super::StageSource>,
        control: &ContentStageControl,
    ) -> Result<ContentStageReport, ServerStoreError> {
        let result = (|| {
            control.checkpoint()?;
            let _pin = crate::core::server::owner::pin_write(&self.context().database_path())
                .map_err(|_| ServerStoreError::Ownership)?;
            self.validate_stage_context()?;
            if &plan.context != self.context()
                || plan.instance_id != plan.manifest.bindings.instance_id
                || plan.proof != plan.fingerprint
                || fingerprint(&plan.manifest)? != plan.proof
                || plan.stage_id != format!("stage_{}", plan.proof)
                || plan.directory != self.stage_root(&plan.instance_id).join(&plan.stage_id)
            {
                return Err(ServerStoreError::StaleRevision);
            }
            self.validate_stage_manifest(&plan.instance_id, &plan.manifest)?;
            self.check_stage_inputs(&plan.manifest)?;
            let acquisition_root = self
                .context()
                .space_dir()
                .join("server/instances")
                .join(plan.instance_id.as_str())
                .join("acquisition");
            validate_actual_sources(&plan.manifest, &actual_sources, &acquisition_root)?;
            self.check_stage_separation(&plan.instance_id, &actual_sources)?;
            let mut transfer_manifest = plan.manifest.clone();
            transfer_manifest.sources = actual_sources.clone();
            super::super::store::check_directory_chain(&plan.directory)?;
            if plan.directory.exists() {
                self.check_existing_stage(&plan, control)?;
                let entries = tree::inspect_sources(&actual_sources, control)?;
                if entries != plan.manifest.entries {
                    return Err(ServerStoreError::StaleRevision);
                }
                self.commit_stage_check(&plan, control, || Ok(()))?;
                return Ok(stage_report(&plan, false, true));
            }
            let root = self.stage_root(&plan.instance_id);
            super::super::store::check_directory_chain(&root)?;
            fs::create_dir_all(&root).map_err(|_| ServerStoreError::Io)?;
            super::super::store::check_directory_chain(&root)?;
            let temporary = tempfile::Builder::new()
                .prefix(".stage-build-")
                .tempdir_in(&root)
                .map_err(|_| ServerStoreError::Io)?;
            let staged = (|| {
                let source_snapshot =
                    tree::copy_sources(&transfer_manifest, temporary.path(), control)?;
                tree::verify_stage(&plan.manifest, temporary.path(), control)?;
                let manifest_bytes =
                    serde_json::to_vec(&plan.manifest).map_err(|_| ServerStoreError::Corrupt)?;
                let mut file = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(temporary.path().join(MANIFEST_FILE))
                    .map_err(|_| ServerStoreError::Io)?;
                file.write_all(&manifest_bytes)
                    .and_then(|()| file.sync_all())
                    .map_err(|_| ServerStoreError::Io)?;
                drop(file);
                tree::check_sources_unchanged(&actual_sources, &source_snapshot, control)?;
                let published = self.commit_stage_check(&plan, control, || {
                    publish_no_replace(temporary.path(), &plan.directory)
                })?;
                if !published {
                    self.check_existing_stage(&plan, control)?;
                    tree::check_sources_unchanged(&actual_sources, &source_snapshot, control)?;
                    self.commit_stage_check(&plan, control, || Ok(()))?;
                }
                Ok(published)
            })();
            match staged {
                Ok(true) => {
                    let _ = temporary.keep();
                    Ok(stage_report(&plan, true, false))
                }
                result => {
                    temporary
                        .close()
                        .map_err(|_| ServerStoreError::StageCleanupIncomplete)?;
                    result.map(|_| stage_report(&plan, false, true))
                }
            }
        })();
        control.finish(&result);
        result
    }

    fn commit_stage_check<T>(
        &self,
        plan: &ContentStagePlan,
        control: &ContentStageControl,
        operation: impl FnOnce() -> Result<T, ServerStoreError>,
    ) -> Result<T, ServerStoreError> {
        crate::core::server::owner::with_write(&self.context().database_path(), || {
            Ok((|| {
                self.validate_stage_context()?;
                self.check_stage_inputs(&plan.manifest)?;
                super::super::store::check_directory_chain(&plan.directory)?;
                control.checkpoint()?;
                operation()
            })())
        })
        .map_err(|_| ServerStoreError::Ownership)?
    }

    fn check_existing_stage(
        &self,
        plan: &ContentStagePlan,
        control: &ContentStageControl,
    ) -> Result<(), ServerStoreError> {
        let manifest = self.read_stage_manifest(&plan.instance_id, &plan.stage_id, control)?;
        if manifest != plan.manifest {
            return Err(ServerStoreError::Corrupt);
        }
        tree::verify_stage(&manifest, &plan.directory, control)
    }

    pub fn verify_content_stage(
        &self,
        id: &ServerInstanceId,
        stage_id: &str,
        control: &ContentStageControl,
    ) -> Result<ContentStageReport, ServerStoreError> {
        let result = self
            .inspect_verified_content_stage(id, stage_id, control)
            .map(|(_, report)| report);
        control.finish(&result);
        result
    }

    pub(in crate::core::server) fn inspect_verified_content_stage(
        &self,
        id: &ServerInstanceId,
        stage_id: &str,
        control: &ContentStageControl,
    ) -> Result<(ContentStageManifest, ContentStageReport), ServerStoreError> {
        (|| {
            control.checkpoint()?;
            self.validate_stage_context()?;
            self.show(id)?;
            let manifest = self.read_stage_manifest(id, stage_id, control)?;
            let proof = fingerprint(&manifest)?;
            let directory = self.stage_root(id).join(stage_id);
            tree::verify_stage(&manifest, &directory, control)?;
            if self.read_stage_manifest(id, stage_id, control)? != manifest {
                return Err(ServerStoreError::StaleRevision);
            }
            self.validate_stage_context()?;
            control.checkpoint()?;
            let report = ContentStageReport {
                stage_id: stage_id.into(),
                directory,
                fingerprint: proof,
                files: manifest
                    .entries
                    .iter()
                    .filter(|entry| entry.kind == StageEntryKind::File)
                    .count() as u64,
                bytes: manifest.total_bytes,
                verified: true,
                published: false,
                reused_existing: false,
                deployment_ready: false,
                active_deployment_changed: false,
                downloads_started: false,
            };
            Ok((manifest, report))
        })()
    }

    pub(in crate::core::server) fn read_stage_manifest(
        &self,
        id: &ServerInstanceId,
        stage_id: &str,
        control: &ContentStageControl,
    ) -> Result<ContentStageManifest, ServerStoreError> {
        self.read_stage_manifest_at(id, stage_id, &self.stage_root(id).join(stage_id), control)
    }

    pub(in crate::core::server) fn read_stage_manifest_at(
        &self,
        id: &ServerInstanceId,
        stage_id: &str,
        directory: &std::path::Path,
        control: &ContentStageControl,
    ) -> Result<ContentStageManifest, ServerStoreError> {
        if !valid_stage_id(stage_id) {
            return Err(ServerStoreError::Invalid("Invalid content stage ID".into()));
        }
        super::super::store::check_directory_chain(directory)?;
        if !directory.exists() {
            return Err(ServerStoreError::NotFound);
        }
        let path = directory.join(MANIFEST_FILE);
        super::super::store::check_regular_file(&path)?;
        if !path.exists() {
            return Err(ServerStoreError::Corrupt);
        }
        let bytes = read_bounded_controlled(&path, MAX_MANIFEST_BYTES, control)?;
        #[derive(Deserialize)]
        struct Version {
            schema_version: u32,
        }
        let version: Version =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        if version.schema_version != SCHEMA_VERSION {
            return Err(ServerStoreError::UnsupportedVersion);
        }
        let manifest: ContentStageManifest =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        self.validate_stage_manifest(id, &manifest)
            .map_err(|error| {
                if error == ServerStoreError::UnsupportedVersion {
                    error
                } else {
                    ServerStoreError::Corrupt
                }
            })?;
        if stage_id != format!("stage_{}", fingerprint(&manifest)?) {
            return Err(ServerStoreError::Corrupt);
        }
        Ok(manifest)
    }

    pub fn content_stages(
        &self,
        id: &ServerInstanceId,
    ) -> Result<ContentStageList, ServerStoreError> {
        self.validate_stage_context()?;
        self.show(id)?;
        let root = self.stage_root(id);
        super::super::store::check_directory_chain(&root)?;
        let directory = match fs::read_dir(&root) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ContentStageList {
                    stage_ids: vec![],
                    unrecognized_entries: 0,
                    integrity_verified: false,
                    writes_performed: false,
                });
            }
            Err(_) => return Err(ServerStoreError::Io),
        };
        let mut stage_ids = Vec::new();
        let mut unrecognized_entries = 0;
        for (index, entry) in directory.enumerate() {
            if index >= 1024 {
                return Err(ServerStoreError::Invalid(
                    "Content stage listing exceeds 1024 entries".into(),
                ));
            }
            let entry = entry.map_err(|_| ServerStoreError::Io)?;
            let name = entry.file_name().into_string().ok();
            if let Some(name) = name.filter(|name| valid_stage_id(name))
                && super::super::store::check_directory_chain(&entry.path()).is_ok()
            {
                stage_ids.push(name);
            } else {
                unrecognized_entries += 1;
            }
        }
        stage_ids.sort();
        Ok(ContentStageList {
            stage_ids,
            unrecognized_entries,
            integrity_verified: false,
            writes_performed: false,
        })
    }
}

pub(in crate::core::server) fn read_bounded_controlled(
    path: &std::path::Path,
    limit: usize,
    control: &ContentStageControl,
) -> Result<Vec<u8>, ServerStoreError> {
    control.checkpoint()?;
    super::super::store::check_directory_chain(path.parent().ok_or(ServerStoreError::UnsafePath)?)?;
    let mut file = tree::open_read(path)?;
    let before = file.metadata().map_err(|_| ServerStoreError::Io)?;
    if before.len() > limit as u64 {
        return Err(ServerStoreError::Corrupt);
    }
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        control.checkpoint()?;
        let read = file.read(&mut buffer).map_err(|_| ServerStoreError::Io)?;
        if read == 0 {
            break;
        }
        if bytes.len().saturating_add(read) > limit {
            return Err(ServerStoreError::Corrupt);
        }
        if bytes.len().saturating_add(read) as u64 > before.len() {
            return Err(ServerStoreError::StaleRevision);
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    let after = file.metadata().map_err(|_| ServerStoreError::Io)?;
    super::super::store::check_regular_file(path)?;
    let named = fs::symlink_metadata(path).map_err(|_| ServerStoreError::Io)?;
    if before.len() != after.len()
        || before.len() != named.len()
        || before.len() != bytes.len() as u64
        || before.modified().ok() != after.modified().ok()
        || before.modified().ok() != named.modified().ok()
    {
        return Err(ServerStoreError::StaleRevision);
    }
    control.checkpoint()?;
    Ok(bytes)
}

fn stage_report(
    plan: &ContentStagePlan,
    published: bool,
    reused_existing: bool,
) -> ContentStageReport {
    ContentStageReport {
        stage_id: plan.stage_id.clone(),
        directory: plan.directory.clone(),
        fingerprint: plan.fingerprint.clone(),
        files: plan
            .manifest
            .entries
            .iter()
            .filter(|entry| entry.kind == StageEntryKind::File)
            .count() as u64,
        bytes: plan.manifest.total_bytes,
        verified: true,
        published,
        reused_existing,
        deployment_ready: false,
        active_deployment_changed: false,
        downloads_started: false,
    }
}

fn validate_actual_sources(
    manifest: &ContentStageManifest,
    actual_sources: &[super::StageSource],
    acquisition_root: &std::path::Path,
) -> Result<(), ServerStoreError> {
    use std::collections::HashSet;

    if actual_sources.len() != manifest.sources.len() {
        return Err(ServerStoreError::StaleRevision);
    }
    let mut seen = HashSet::new();
    for (logical, actual) in manifest.sources.iter().zip(actual_sources) {
        if logical.profile_name != actual.profile_name
            || logical.destination != actual.destination
            || !seen.insert((actual.profile_name.as_str(), actual.destination.as_str()))
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let download = manifest.bindings.bindings.iter().any(|binding| {
            binding.profile.name == logical.profile_name
                && binding.profile.supply == AddonSupply::Download
                && if binding.profile.addons.is_empty() {
                    manifest
                        .acquisitions
                        .iter()
                        .find(|snapshot| snapshot.profile_name == binding.profile.name)
                        .is_some_and(|snapshot| {
                            snapshot.addons.iter().any(|addon| {
                                logical.source == binding.profile.local_path.join(&addon.name)
                            })
                        })
                } else {
                    binding
                        .profile
                        .addons
                        .iter()
                        .any(|addon| logical.source == binding.profile.local_path.join(addon))
                }
        });
        if !download && logical.source != actual.source {
            return Err(ServerStoreError::StaleRevision);
        }
        if download {
            let acquisition_root_physical =
                super::validation::physical_directory(acquisition_root)?;
            let logical_addon = logical
                .source
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or(ServerStoreError::UnsafePath)?;
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
                || components[3] != manifest.bindings.game_space_id
                || components[4] != "content"
                || components[5] != logical_addon
            {
                return Err(ServerStoreError::UnsafePath);
            }
            let actual_physical = super::validation::physical_directory(&actual.source)?;
            if !actual_physical.starts_with(&acquisition_root_physical) {
                return Err(ServerStoreError::UnsafePath);
            }
        }
    }
    Ok(())
}

pub(in crate::core::server) fn publish_no_replace(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> Result<bool, ServerStoreError> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        super::super::store::check_directory_chain(source)?;
        let source = source.canonicalize().map_err(|_| ServerStoreError::Io)?;
        let destination = destination
            .parent()
            .ok_or(ServerStoreError::UnsafePath)?
            .canonicalize()
            .map_err(|_| ServerStoreError::Io)?
            .join(
                destination
                    .file_name()
                    .ok_or(ServerStoreError::UnsafePath)?,
            );
        let from = source
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let to = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        if unsafe { winapi::um::winbase::MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } != 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(80 | 183)) {
            return Ok(false);
        }
        Err(ServerStoreError::Io)
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let from = std::ffi::CString::new(source.as_os_str().as_bytes())
            .map_err(|_| ServerStoreError::UnsafePath)?;
        let to = std::ffi::CString::new(destination.as_os_str().as_bytes())
            .map_err(|_| ServerStoreError::UnsafePath)?;
        if unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } == 0
        {
            return Ok(true);
        }
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::EEXIST) {
            return Ok(false);
        }
        Err(ServerStoreError::Io)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (source, destination);
        Err(ServerStoreError::UnsupportedGame)
    }
}
