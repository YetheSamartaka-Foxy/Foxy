use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{
    MissionImportPlan, MissionLibraryEntry, MissionLibrarySnapshot, MissionOrigin, acquisition,
    payload, valid_id, valid_template,
};
use crate::core::server::content_staging::{
    StageEntry, StageEntryKind, StageSource, read_bounded_controlled, verify_subtree,
};
use crate::core::server::{
    Arma3MissionSource, ContentStageControl, ServerContext, ServerDefinition, ServerInstanceId,
    ServerStore, ServerStoreError,
};

fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, ServerStoreError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ServerStoreError::Corrupt)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(ServerStoreError::Invalid(
            "Mission metadata exceeds 4 MiB".into(),
        ));
    }
    Ok(bytes)
}

fn revision(entry: &MissionLibraryEntry) -> Result<String, ServerStoreError> {
    Ok(format!(
        "revision_{}",
        blake3::hash(&encode(&(&entry.origin, &entry.payload))?).to_hex()
    ))
}

fn mission_id(origin: &MissionOrigin) -> Result<String, ServerStoreError> {
    let key = match origin {
        MissionOrigin::Local { path } => format!("local:{}", path.to_string_lossy()),
        MissionOrigin::Http { url, .. } => format!("http:{url}"),
    };
    Ok(format!(
        "mission_{}",
        &blake3::hash(key.as_bytes()).to_hex()[..32]
    ))
}

impl ServerStore {
    fn validate_mission_context(&self) -> Result<(), ServerStoreError> {
        if !self.capabilities().mission_library {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let current =
            ServerContext::capture(self.context().data_root(), Some(self.context().space_id()))
                .map_err(ServerStoreError::Invalid)?;
        if &current != self.context() {
            return Err(ServerStoreError::StaleRevision);
        }
        Ok(())
    }

    fn mission_root(&self) -> PathBuf {
        self.context().space_dir().join("server/mission-library")
    }

    fn validate_mission_destination(&self) -> Result<(), ServerStoreError> {
        let root = crate::core::server::content_staging::physical_directory(&self.mission_root())?;
        let catalog = self.load()?;
        let sources = self.content_sources()?;
        for path in catalog
            .installations
            .iter()
            .map(|item| &item.directory)
            .chain(sources.sources.folders.iter().map(|item| &item.path))
        {
            let other = crate::core::server::content_staging::physical_directory(path)?;
            if crate::core::server::installation::paths_overlap(&root, &other) {
                return Err(ServerStoreError::UnsafePath);
            }
        }
        Ok(())
    }

    fn mission_directory(&self, id: &str) -> Result<PathBuf, ServerStoreError> {
        if !valid_id(id, "mission_", 32) {
            return Err(ServerStoreError::Corrupt);
        }
        Ok(self.mission_root().join(id))
    }

    fn mission_revision_directory(&self, id: &str, rev: &str) -> Result<PathBuf, ServerStoreError> {
        if !valid_id(rev, "revision_", 64) {
            return Err(ServerStoreError::Corrupt);
        }
        Ok(self.mission_directory(id)?.join("revisions").join(rev))
    }

    fn read_mission_pointer(
        &self,
        id: &str,
        control: &ContentStageControl,
    ) -> Result<(Option<String>, Option<String>), ServerStoreError> {
        let path = self.mission_directory(id)?.join("current.json");
        crate::core::server::store::check_directory_chain(
            path.parent().ok_or(ServerStoreError::UnsafePath)?,
        )?;
        if !path.try_exists().map_err(|_| ServerStoreError::Io)? {
            return Ok((None, None));
        }
        let bytes = read_bounded_controlled(&path, 1024, control)?;
        let revision: String =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        if !valid_id(&revision, "revision_", 64) {
            return Err(ServerStoreError::Corrupt);
        }
        Ok((
            Some(revision),
            Some(blake3::hash(&bytes).to_hex().to_string()),
        ))
    }

    pub fn mission_library(
        &self,
        control: &ContentStageControl,
    ) -> Result<MissionLibrarySnapshot, ServerStoreError> {
        self.validate_mission_context()?;
        let root = self.mission_root();
        crate::core::server::store::check_directory_chain(&root)?;
        let mut entries = Vec::new();
        let mut unrecognized_entries = 0;
        if root.try_exists().map_err(|_| ServerStoreError::Io)? {
            for (index, item) in fs::read_dir(&root)
                .map_err(|_| ServerStoreError::Io)?
                .enumerate()
            {
                control.checkpoint()?;
                if index >= 4096 {
                    return Err(ServerStoreError::Invalid(
                        "Mission library exceeds 4096 entries".into(),
                    ));
                }
                let item = item.map_err(|_| ServerStoreError::Io)?;
                let id = item.file_name().to_string_lossy().into_owned();
                if !valid_id(&id, "mission_", 32) {
                    unrecognized_entries += 1;
                    continue;
                }
                let (current, before) = self.read_mission_pointer(&id, control)?;
                if let Some(rev) = current {
                    entries.push(self.mission_revision(&id, &rev, control)?);
                    if self.read_mission_pointer(&id, control)?.1 != before {
                        return Err(ServerStoreError::StaleRevision);
                    }
                } else {
                    unrecognized_entries += 1;
                }
            }
        }
        entries.sort_by(|a, b| a.mission_id.cmp(&b.mission_id));
        self.validate_mission_context()?;
        let fingerprint =
            blake3::hash(&serde_json::to_vec(&entries).map_err(|_| ServerStoreError::Corrupt)?)
                .to_hex()
                .to_string();
        Ok(MissionLibrarySnapshot {
            entries,
            fingerprint,
            unrecognized_entries,
            writes_performed: false,
        })
    }

    pub fn mission_revision(
        &self,
        id: &str,
        rev: &str,
        control: &ContentStageControl,
    ) -> Result<MissionLibraryEntry, ServerStoreError> {
        self.validate_mission_context()?;
        let directory = self.mission_revision_directory(id, rev)?;
        crate::core::server::store::check_directory_chain(&directory)?;
        let bytes =
            read_bounded_controlled(&directory.join("manifest.json"), 4 * 1024 * 1024, control)?;
        let entry: MissionLibraryEntry =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        if entry.schema_version != 1
            || entry.game_space_id != self.context().space_id()
            || entry.mission_id != id
            || entry.revision_id != rev
            || mission_id(&entry.origin)? != id
            || revision(&entry)? != rev
            || !valid_template(&entry.payload.template)
            || entry.payload.payload_bytes > 512 * 1024 * 1024
            || !valid_id(&entry.payload.payload_blake3, "", 64)
        {
            return Err(ServerStoreError::Corrupt);
        }
        let entries = vec![
            StageEntry {
                path: "payload".into(),
                kind: StageEntryKind::Directory,
                bytes: 0,
                blake3: None,
            },
            StageEntry {
                path: format!("payload/{}", entry.mission_id),
                kind: StageEntryKind::Directory,
                bytes: 0,
                blake3: None,
            },
            StageEntry {
                path: format!("payload/{}/mpmissions", entry.mission_id),
                kind: StageEntryKind::Directory,
                bytes: 0,
                blake3: None,
            },
            StageEntry {
                path: format!(
                    "payload/{}/mpmissions/{}.pbo",
                    entry.mission_id, entry.payload.template
                ),
                kind: StageEntryKind::File,
                bytes: entry.payload.payload_bytes,
                blake3: Some(entry.payload.payload_blake3.clone()),
            },
        ];
        verify_subtree(&entries, &directory, "payload", control)?;
        let names = fs::read_dir(&directory)
            .map_err(|_| ServerStoreError::Io)?
            .map(|item| item.map(|i| i.file_name()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ServerStoreError::Io)?;
        if names.len() != 2
            || !names.iter().any(|n| n == "manifest.json")
            || !names.iter().any(|n| n == "payload")
        {
            return Err(ServerStoreError::Corrupt);
        }
        Ok(entry)
    }

    pub fn plan_mission_import(
        &self,
        source: &Path,
        control: &ContentStageControl,
    ) -> Result<MissionImportPlan, ServerStoreError> {
        self.validate_mission_context()?;
        crate::core::server::store::check_directory_chain(
            source.parent().ok_or(ServerStoreError::UnsafePath)?,
        )?;
        let metadata = fs::symlink_metadata(source).map_err(|_| ServerStoreError::Io)?;
        if metadata.is_dir() {
            crate::core::server::store::check_directory_chain(source)?;
        } else {
            crate::core::server::store::check_regular_file(source)?;
        }
        let source = source.canonicalize().map_err(|_| ServerStoreError::Io)?;
        self.plan_mission_source(
            MissionOrigin::Local {
                path: source.clone(),
            },
            source,
            None,
            control,
        )
    }

    pub fn plan_mission_refresh(
        &self,
        id: &str,
        control: &ContentStageControl,
    ) -> Result<MissionImportPlan, ServerStoreError> {
        let (revision, _) = self.read_mission_pointer(id, control)?;
        let entry =
            self.mission_revision(id, &revision.ok_or(ServerStoreError::NotFound)?, control)?;
        match entry.origin {
            MissionOrigin::Local { path } => self.plan_mission_import(&path, control),
            MissionOrigin::Http {
                url,
                template,
                expected_blake3,
            } => self.plan_mission_acquisition(&url, &template, &expected_blake3, control),
        }
    }

    pub fn plan_mission_acquisition(
        &self,
        url: &str,
        template: &str,
        expected_blake3: &str,
        control: &ContentStageControl,
    ) -> Result<MissionImportPlan, ServerStoreError> {
        self.validate_mission_context()?;
        let url = acquisition::validate_url(url)?.to_string();
        let temp = Arc::new(acquisition::download(
            &url,
            template,
            expected_blake3,
            control,
        )?);
        self.plan_mission_source(
            MissionOrigin::Http {
                url,
                template: template.into(),
                expected_blake3: expected_blake3.into(),
            },
            temp.path().join(format!("{template}.pbo")),
            Some(temp),
            control,
        )
    }

    fn plan_mission_source(
        &self,
        origin: MissionOrigin,
        source: PathBuf,
        temporary: Option<Arc<tempfile::TempDir>>,
        control: &ContentStageControl,
    ) -> Result<MissionImportPlan, ServerStoreError> {
        let payload = payload::inspect_payload(&source, control)?;
        let id = mission_id(&origin)?;
        let (_, before) = self.read_mission_pointer(&id, control)?;
        let mut entry = MissionLibraryEntry {
            schema_version: 1,
            game_space_id: self.context().space_id().into(),
            mission_id: id,
            revision_id: String::new(),
            origin,
            payload,
        };
        entry.revision_id = revision(&entry)?;
        let fingerprint = blake3::hash(&encode(&entry)?).to_hex().to_string();
        Ok(MissionImportPlan {
            context: self.context().clone(),
            source,
            before,
            temporary,
            entry,
            proof: fingerprint.clone(),
            fingerprint,
            writes_performed: false,
        })
    }

    pub fn apply_mission_import(
        &self,
        plan: MissionImportPlan,
        control: &ContentStageControl,
    ) -> Result<MissionLibraryEntry, ServerStoreError> {
        let result =
            crate::core::server::owner::with_write(&self.context().database_path(), || {
                Ok((|| {
                    self.validate_mission_context()?;
                    self.validate_mission_destination()?;
                    control.checkpoint()?;
                    let _temporary = &plan.temporary;
                    if &plan.context != self.context()
                        || plan.writes_performed
                        || plan.fingerprint != plan.proof
                        || blake3::hash(&encode(&plan.entry)?).to_hex().as_str() != plan.fingerprint
                        || revision(&plan.entry)? != plan.entry.revision_id
                        || mission_id(&plan.entry.origin)? != plan.entry.mission_id
                        || self
                            .read_mission_pointer(&plan.entry.mission_id, control)?
                            .1
                            != plan.before
                    {
                        return Err(ServerStoreError::StaleRevision);
                    }
                    if payload::inspect_payload(&plan.source, control)? != plan.entry.payload {
                        return Err(ServerStoreError::StaleRevision);
                    }
                    let target = self.mission_revision_directory(
                        &plan.entry.mission_id,
                        &plan.entry.revision_id,
                    )?;
                    let parent = target.parent().ok_or(ServerStoreError::UnsafePath)?;
                    crate::core::server::store::check_directory_chain(parent)?;
                    fs::create_dir_all(parent).map_err(|_| ServerStoreError::Io)?;
                    if !target.try_exists().map_err(|_| ServerStoreError::Io)? {
                        let temp = tempfile::Builder::new()
                            .prefix(".mission-")
                            .tempdir_in(parent)
                            .map_err(|_| ServerStoreError::Io)?;
                        let staged = temp.path().join("entry");
                        fs::create_dir_all(
                            staged.join(format!("payload/{}/mpmissions", plan.entry.mission_id)),
                        )
                        .map_err(|_| ServerStoreError::Io)?;
                        payload::copy_payload(
                            &plan.source,
                            &plan.entry.payload,
                            &staged.join(format!(
                                "payload/{}/mpmissions/{}.pbo",
                                plan.entry.mission_id, plan.entry.payload.template
                            )),
                            control,
                        )?;
                        let mut manifest = fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(staged.join("manifest.json"))
                            .map_err(|_| ServerStoreError::Io)?;
                        manifest
                            .write_all(&encode(&plan.entry)?)
                            .and_then(|()| manifest.sync_all())
                            .map_err(|_| ServerStoreError::Io)?;
                        drop(manifest);
                        control.checkpoint()?;
                        if !crate::core::server::content_staging::publish_no_replace(
                            &staged, &target,
                        )? {
                            self.mission_revision(
                                &plan.entry.mission_id,
                                &plan.entry.revision_id,
                                control,
                            )?;
                        }
                    }
                    let entry = self.mission_revision(
                        &plan.entry.mission_id,
                        &plan.entry.revision_id,
                        control,
                    )?;
                    if entry != plan.entry
                        || self.read_mission_pointer(&entry.mission_id, control)?.1 != plan.before
                    {
                        return Err(ServerStoreError::StaleRevision);
                    }
                    let pointer = self
                        .mission_directory(&entry.mission_id)?
                        .join("current.json");
                    let mut temp = tempfile::NamedTempFile::new_in(
                        pointer.parent().ok_or(ServerStoreError::UnsafePath)?,
                    )
                    .map_err(|_| ServerStoreError::Io)?;
                    temp.write_all(&encode(&entry.revision_id)?)
                        .map_err(|_| ServerStoreError::Io)?;
                    temp.as_file()
                        .sync_all()
                        .map_err(|_| ServerStoreError::Io)?;
                    control.checkpoint()?;
                    temp.persist(&pointer).map_err(|_| ServerStoreError::Io)?;
                    Ok(entry)
                })())
            })
            .map_err(|_| ServerStoreError::Ownership)
            .and_then(|result| result);
        control.finish(&result);
        result
    }

    pub(in crate::core::server) fn selected_library_missions(
        &self,
        id: &ServerInstanceId,
        control: &ContentStageControl,
    ) -> Result<Vec<Arma3MissionSource>, ServerStoreError> {
        let instance = self.show(id)?;
        let mut selected = Vec::new();
        if let ServerDefinition::Arma3(arma) = instance.definition {
            for mission in arma.mission_cycle.iter().filter(|m| m.enabled) {
                if let Some(
                    source @ Arma3MissionSource::Library {
                        mission_id,
                        revision_id,
                    },
                ) = &mission.source
                {
                    let entry = self.mission_revision(mission_id, revision_id, control).map_err(|error|if error==ServerStoreError::Cancelled {error} else {ServerStoreError::Invalid("Selected mission library revision is missing or invalid; import the mission and explicitly rebind the cycle before staging".into())})?;
                    if entry.payload.template != mission.template {
                        return Err(ServerStoreError::Invalid(
                            "Mission library identity differs from the cycle template".into(),
                        ));
                    }
                    if !selected.contains(source) {
                        selected.push(source.clone());
                    }
                }
            }
        }
        Ok(selected)
    }

    pub(super) fn mission_stage_sources(
        &self,
        selections: &[Arma3MissionSource],
    ) -> Result<Vec<StageSource>, ServerStoreError> {
        let mut sources = Vec::new();
        for selection in selections {
            let Arma3MissionSource::Library {
                mission_id,
                revision_id,
            } = selection
            else {
                return Err(ServerStoreError::Corrupt);
            };
            selection
                .validate()
                .map_err(|_| ServerStoreError::Corrupt)?;
            if sources
                .iter()
                .any(|source: &StageSource| source.profile_name == *mission_id)
            {
                return Err(ServerStoreError::Invalid(
                    "A content stage cannot select two revisions of one mission source".into(),
                ));
            }
            sources.push(StageSource {
                profile_name: mission_id.clone(),
                source: self
                    .mission_revision_directory(mission_id, revision_id)?
                    .join("payload")
                    .join(mission_id),
                destination: format!("addons/{mission_id}"),
            });
        }
        Ok(sources)
    }

    pub(in crate::core::server) fn append_mission_stage_sources(
        &self,
        sources: &mut Vec<StageSource>,
        missions: &[Arma3MissionSource],
    ) -> Result<(), ServerStoreError> {
        for source in self.mission_stage_sources(missions)? {
            if sources.iter().any(|existing| {
                existing
                    .destination
                    .eq_ignore_ascii_case(&source.destination)
            }) {
                return Err(ServerStoreError::Invalid(
                    "Mission and addon stage directories collide".into(),
                ));
            }
            sources.push(source);
        }
        Ok(())
    }
}
