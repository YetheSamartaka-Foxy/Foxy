//! Reclaim only unreferenced owned stages, preparations and runtimes under an exact reviewed plan.
//! Keep records, configuration archives, mutable state and content sources.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

mod inventory;
mod quarantine;
#[cfg(test)]
mod receipt_tests;

use super::{ContentStageControl, ServerContext, ServerInstanceId, ServerStore, ServerStoreError};

const PINS_FILE: &str = "pins.json";

type Reasons = BTreeMap<(ReconciliationArtifact, String), BTreeSet<String>>;
const QUARANTINE_PREFIX: &str = ".reconcile-";
const MAX_DOCUMENT: usize = 256 * 1024;
const MAX_ARTIFACTS: usize = 4096;
const MAX_PINS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PinDocument {
    schema_version: u32,
    game_space_id: String,
    instance_id: ServerInstanceId,
    generation: u64,
    records: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeploymentPins {
    pub instance_id: ServerInstanceId,
    pub generation: u64,
    pub records: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationArtifact {
    ContentStage,
    Preparation,
    Runtime,
    Leftover,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RetainedArtifact {
    pub kind: ReconciliationArtifact,
    pub id: String,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReconciliationCandidate {
    pub kind: ReconciliationArtifact,
    pub id: String,
    pub bytes: u64,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReconciliationPlan {
    #[serde(skip)]
    context: ServerContext,
    pub instance_id: ServerInstanceId,
    pub pins: Vec<String>,
    pub selected_record: Option<String>,
    pub active_record: Option<String>,
    pub retained: Vec<RetainedArtifact>,
    pub candidates: Vec<ReconciliationCandidate>,
    pub unrecognized_entries: Vec<String>,
    pub blockers: Vec<String>,
    pub reclaimable_bytes: u64,
    pub fingerprint: String,
    pub writes_performed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReconciliationOutcome {
    pub instance_id: ServerInstanceId,
    pub removed: Vec<ReconciliationCandidate>,
    pub reclaimed_bytes: u64,
    pub sources_changed: bool,
    pub records_changed: bool,
    pub writes_performed: bool,
}

#[derive(Deserialize)]
struct RecordLink {
    content_stage_id: String,
}

#[derive(Deserialize)]
struct PreparationLink {
    record_id: String,
}

#[derive(Deserialize)]
struct RuntimeLink {
    preparation_id: String,
}

fn hex_id(value: &str, prefix: &str, len: usize) -> bool {
    value.strip_prefix(prefix).is_some_and(|hex| {
        hex.len() == len
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn quarantine_name(value: &str) -> bool {
    hex_id(value, QUARANTINE_PREFIX, 64)
}

fn owned_name(kind: ReconciliationArtifact, name: &str) -> bool {
    match kind {
        ReconciliationArtifact::ContentStage => hex_id(name, "stage_", 64),
        ReconciliationArtifact::Preparation => hex_id(name, "prepared_", 64),
        ReconciliationArtifact::Runtime => hex_id(name, "runtime_", 32),
        ReconciliationArtifact::Leftover => false,
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    control: &ContentStageControl,
) -> Result<T, ServerStoreError> {
    let bytes = super::content_staging::read_bounded_controlled(path, 48 * 1024 * 1024, control)?;
    serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)
}

fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

struct Inventory {
    directory: PathBuf,
    kind: ReconciliationArtifact,
    names: Vec<String>,
    unrecognized: Vec<String>,
    leftovers: Vec<String>,
}

fn inventory(
    directory: &Path,
    kind: ReconciliationArtifact,
    valid: impl Fn(&str) -> bool,
) -> Result<Inventory, ServerStoreError> {
    let mut result = Inventory {
        directory: directory.to_path_buf(),
        kind,
        names: Vec::new(),
        unrecognized: Vec::new(),
        leftovers: Vec::new(),
    };
    if !directory.try_exists().map_err(|_| ServerStoreError::Io)? {
        return Ok(result);
    }
    super::store::check_directory_chain(directory)?;
    for entry in fs::read_dir(directory).map_err(|_| ServerStoreError::Io)? {
        let entry = entry.map_err(|_| ServerStoreError::Io)?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            result.unrecognized.push("<non-UTF-8 entry>".into());
            continue;
        };
        let metadata = fs::symlink_metadata(entry.path()).map_err(|_| ServerStoreError::Io)?;
        let directory_entry =
            metadata.is_dir() && !metadata.file_type().is_symlink() && !is_reparse_point(&metadata);
        if directory_entry && valid(&name) {
            result.names.push(name);
        } else if directory_entry && quarantine_name(&name) {
            result.leftovers.push(name);
        } else {
            result.unrecognized.push(name);
        }
        if result.names.len() + result.leftovers.len() + result.unrecognized.len() > MAX_ARTIFACTS {
            return Err(ServerStoreError::Invalid(
                "Too many deployment artifacts to reconcile safely".into(),
            ));
        }
    }
    result.names.sort();
    result.leftovers.sort();
    result.unrecognized.sort();
    Ok(result)
}

impl ServerStore {
    fn pins_path(&self, id: &ServerInstanceId) -> PathBuf {
        self.record_root(id).join(PINS_FILE)
    }

    fn read_pins(
        &self,
        id: &ServerInstanceId,
        control: &ContentStageControl,
    ) -> Result<PinDocument, ServerStoreError> {
        let path = self.pins_path(id);
        if !path.try_exists().map_err(|_| ServerStoreError::Io)? {
            return Ok(PinDocument {
                schema_version: 1,
                game_space_id: self.context().space_id().into(),
                instance_id: id.clone(),
                generation: 0,
                records: Vec::new(),
            });
        }
        super::store::check_regular_file(&path)?;
        let bytes = super::content_staging::read_bounded_controlled(&path, MAX_DOCUMENT, control)?;
        let document: PinDocument =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        if document.schema_version != 1
            || document.game_space_id != self.context().space_id()
            || &document.instance_id != id
            || document.generation == 0
            || document.records.len() > MAX_PINS
            || document
                .records
                .iter()
                .any(|record| !hex_id(record, "deployment_", 64))
            || document.records.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(ServerStoreError::Corrupt);
        }
        Ok(document)
    }

    pub fn deployment_pins(
        &self,
        id: &ServerInstanceId,
    ) -> Result<DeploymentPins, ServerStoreError> {
        self.validate_record_context()?;
        self.show(id)?;
        let document = self.read_pins(id, &ContentStageControl::default())?;
        Ok(DeploymentPins {
            instance_id: id.clone(),
            generation: document.generation,
            records: document.records,
        })
    }

    pub fn set_deployment_pin(
        &self,
        id: &ServerInstanceId,
        record: &str,
        pinned: bool,
        expected_generation: u64,
    ) -> Result<DeploymentPins, ServerStoreError> {
        let _pin = super::owner::pin_write(&self.context().database_path())
            .map_err(|_| ServerStoreError::Ownership)?;
        self.inspect_deployment_pin(id, record, pinned, expected_generation)?;
        let control = ContentStageControl::default();
        let mut document = self.read_pins(id, &control)?;
        let mut records: BTreeSet<String> = document.records.iter().cloned().collect();
        if pinned {
            records.insert(record.to_owned());
        } else {
            records.remove(record);
        }
        if records.len() > MAX_PINS {
            return Err(ServerStoreError::Invalid(
                "Too many pinned deployment records".into(),
            ));
        }
        document.records = records.into_iter().collect();
        document.generation = document
            .generation
            .checked_add(1)
            .ok_or(ServerStoreError::Corrupt)?;
        let root = self.record_root(id);
        super::store::check_directory_chain(&root)?;
        fs::create_dir_all(&root).map_err(|_| ServerStoreError::Io)?;
        let bytes = serde_json::to_vec_pretty(&document).map_err(|_| ServerStoreError::Corrupt)?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".pins-")
            .tempfile_in(&root)
            .map_err(|_| ServerStoreError::Io)?;
        temporary
            .write_all(&bytes)
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|_| ServerStoreError::Io)?;
        temporary
            .persist(self.pins_path(id))
            .map_err(|_| ServerStoreError::Io)?;
        Ok(DeploymentPins {
            instance_id: id.clone(),
            generation: document.generation,
            records: document.records,
        })
    }

    pub fn inspect_deployment_pin(
        &self,
        id: &ServerInstanceId,
        record: &str,
        pinned: bool,
        expected_generation: u64,
    ) -> Result<DeploymentPins, ServerStoreError> {
        let current = self.deployment_pins(id)?;
        if current.generation != expected_generation {
            return Err(ServerStoreError::StaleRevision);
        }
        if !hex_id(record, "deployment_", 64) {
            return Err(ServerStoreError::Invalid(
                "Invalid deployment record ID".into(),
            ));
        }
        if pinned {
            let control = ContentStageControl::default();
            if !self
                .read_record_state(id, &control)?
                .0
                .records
                .iter()
                .any(|known| known == record)
            {
                return Err(ServerStoreError::NotFound);
            }
            self.read_record(id, record, &control)?;
            if current.records.len() >= MAX_PINS
                && !current.records.iter().any(|known| known == record)
            {
                return Err(ServerStoreError::Invalid(
                    "Too many pinned deployment records".into(),
                ));
            }
        }
        Ok(current)
    }

    pub fn plan_reconciliation(
        &self,
        id: &ServerInstanceId,
        control: &ContentStageControl,
    ) -> Result<ReconciliationPlan, ServerStoreError> {
        self.validate_record_context()?;
        self.show(id)?;
        let pins = self.read_pins(id, control)?;
        let mut blockers = Vec::new();
        let mut reasons: Reasons = BTreeMap::new();
        let retain = |reasons: &mut Reasons, kind, id: &str, reason: String| {
            reasons
                .entry((kind, id.to_owned()))
                .or_default()
                .insert(reason);
        };

        let catalog_fingerprint = self.read()?.1;
        let (state, record_state_fingerprint) = self.read_record_state(id, control)?;
        let indexed = state.records;
        let selected = state.selected;
        let mut retained_records: BTreeSet<String> = pins.records.iter().cloned().collect();
        retained_records.extend(selected.iter().cloned());

        let journal = self.record_root(id).join("activation/journal.json");
        if journal.try_exists().map_err(|_| ServerStoreError::Io)? {
            blockers.push(
                "An activation is pending recovery. Resolve it before reconciling deployment artifacts."
                    .into(),
            );
        }
        let active_snapshot = self.read_activation_state(id, control);
        let active_fingerprint = active_snapshot
            .as_ref()
            .ok()
            .and_then(|value| value.as_ref().map(|(_, fingerprint)| fingerprint.clone()));
        let active = match active_snapshot {
            Ok(value) => value.map(|(state, _)| state),
            Err(_) => {
                blockers.push("The active deployment pointer could not be verified.".into());
                None
            }
        };
        let active_record = active
            .as_ref()
            .map(|state| state.deployment.record_id.clone());
        if let Some(state) = &active {
            retained_records.insert(state.deployment.record_id.clone());
            retain(
                &mut reasons,
                ReconciliationArtifact::Preparation,
                &state.deployment.preparation_id,
                "active deployment".into(),
            );
            if let Some(runtime) = &state.deployment.runtime {
                retain(
                    &mut reasons,
                    ReconciliationArtifact::Runtime,
                    &runtime.directory_name(),
                    "active runtime".into(),
                );
            }
        }

        let mut stages = inventory(
            &self.stage_root(id),
            ReconciliationArtifact::ContentStage,
            |name| hex_id(name, "stage_", 64),
        )?;
        let mut preparations = inventory(
            &self.record_root(id).join("prepared"),
            ReconciliationArtifact::Preparation,
            |name| hex_id(name, "prepared_", 64),
        )?;
        let mut runtimes = inventory(
            &self.native_runtime_root(id),
            ReconciliationArtifact::Runtime,
            |name| hex_id(name, "runtime_", 32),
        )?;

        let mut verified = BTreeMap::new();
        for listing in [&mut stages, &mut preparations, &mut runtimes] {
            let mut owned = Vec::new();
            for name in &listing.names {
                control.checkpoint()?;
                match inventory::inspect(
                    self,
                    id,
                    listing.kind,
                    name,
                    &listing.directory.join(name),
                    control,
                ) {
                    Ok(evidence) => {
                        verified.insert((listing.kind, name.clone()), evidence);
                        owned.push(name.clone());
                    }
                    Err(ServerStoreError::Cancelled) => return Err(ServerStoreError::Cancelled),
                    Err(_) => {
                        listing.unrecognized.push(name.clone());
                        if listing.kind == ReconciliationArtifact::Runtime {
                            blockers.push(format!("Runtime {name} could not be verified; its installation consumer protection is unknown."));
                        }
                    }
                }
            }
            listing.names = owned;
        }

        for record in &indexed {
            let link: RecordLink =
                match self
                    .read_record(id, record, control)
                    .map(|manifest| RecordLink {
                        content_stage_id: manifest.content_stage_id,
                    }) {
                    Ok(link) => link,
                    Err(_) => {
                        blockers.push(format!("Deployment record {record} could not be read."));
                        continue;
                    }
                };
            let reason = if retained_records.contains(record) {
                format!("stage of retained record {record}")
            } else {
                format!("stage of indexed record {record}")
            };
            retain(
                &mut reasons,
                ReconciliationArtifact::ContentStage,
                &link.content_stage_id,
                reason,
            );
        }
        let mut retained_preparations = BTreeSet::new();
        for name in &preparations.names {
            match read_json::<PreparationLink>(
                &preparations.directory.join(name).join("manifest.json"),
                control,
            ) {
                Ok(link) if retained_records.contains(&link.record_id) => {
                    retain(
                        &mut reasons,
                        ReconciliationArtifact::Preparation,
                        name,
                        format!("prepared from retained record {}", link.record_id),
                    );
                    retained_preparations.insert(name.clone());
                }
                Ok(_) => {}
                Err(_) => blockers.push(format!("Preparation {name} could not be read.")),
            }
        }
        if let Some(state) = &active {
            retained_preparations.insert(state.deployment.preparation_id.clone());
        }
        for name in &runtimes.names {
            match read_json::<RuntimeLink>(
                &runtimes.directory.join(name).join("manifest.json"),
                control,
            ) {
                Ok(link) if retained_preparations.contains(&link.preparation_id) => retain(
                    &mut reasons,
                    ReconciliationArtifact::Runtime,
                    name,
                    format!("built from retained preparation {}", link.preparation_id),
                ),
                Ok(_) => {}
                Err(_) => blockers.push(format!("Runtime {name} could not be read.")),
            }
        }
        let newest_unreferenced_stage = stages
            .names
            .iter()
            .filter(|name| {
                !reasons.contains_key(&(ReconciliationArtifact::ContentStage, (*name).clone()))
            })
            .filter_map(|name| {
                fs::metadata(stages.directory.join(name).join("manifest.json"))
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .map(|time| (time, name.clone()))
            })
            .max();
        if let Some((_, name)) = newest_unreferenced_stage {
            retain(
                &mut reasons,
                ReconciliationArtifact::ContentStage,
                &name,
                "newest stage not yet used by a record".into(),
            );
        }

        let mut candidates = Vec::new();
        let mut unrecognized_entries = Vec::new();
        for listing in [&stages, &preparations, &runtimes] {
            for name in &listing.names {
                if reasons.contains_key(&(listing.kind, name.clone())) {
                    continue;
                }
                candidates.push(ReconciliationCandidate {
                    kind: listing.kind,
                    id: name.clone(),
                    bytes: verified[&(listing.kind, name.clone())].bytes,
                    fingerprint: verified[&(listing.kind, name.clone())].fingerprint.clone(),
                });
            }
            for name in &listing.leftovers {
                let evidence =
                    quarantine::read(self, id, listing.kind, name, control).and_then(|receipt| {
                        inventory::remaining(
                            &listing.directory.join(name),
                            &receipt.entries,
                            control,
                        )
                    });
                match evidence {
                    Ok(evidence) => candidates.push(ReconciliationCandidate {
                        kind: ReconciliationArtifact::Leftover,
                        id: format!("{}/{name}", artifact_folder(listing.kind)),
                        bytes: evidence.bytes,
                        fingerprint: evidence.fingerprint,
                    }),
                    Err(ServerStoreError::Cancelled) => return Err(ServerStoreError::Cancelled),
                    Err(_) => unrecognized_entries
                        .push(format!("{}/{name}", artifact_folder(listing.kind))),
                }
            }
            unrecognized_entries.extend(
                listing
                    .unrecognized
                    .iter()
                    .map(|name| format!("{}/{name}", artifact_folder(listing.kind))),
            );
        }
        let retained: Vec<RetainedArtifact> = reasons
            .into_iter()
            .map(|((kind, id), reasons)| RetainedArtifact {
                kind,
                id,
                reasons: reasons.into_iter().collect(),
            })
            .collect();
        for retained in &retained {
            if !verified.contains_key(&(retained.kind, retained.id.clone())) {
                blockers.push(format!(
                    "Retained artifact {} could not be verified.",
                    retained.id
                ));
            }
        }
        let reclaimable_bytes = candidates.iter().map(|candidate| candidate.bytes).sum();
        if self.read_record_state(id, control)?.1 != record_state_fingerprint
            || self.read_pins(id, control)? != pins
            || self.read()?.1 != catalog_fingerprint
            || self
                .read_activation_state(id, control)
                .ok()
                .flatten()
                .map(|(_, fingerprint)| fingerprint)
                != active_fingerprint
        {
            return Err(ServerStoreError::StaleRevision);
        }
        unrecognized_entries.sort();
        let fingerprint = blake3::hash(
            &serde_json::to_vec(&(
                1u32,
                self.context().space_id(),
                self.context().data_root(),
                self.context().game_id(),
                id,
                &pins,
                &record_state_fingerprint,
                &active_fingerprint,
                &selected,
                &active_record,
                &retained,
                &candidates,
                &blockers,
                &unrecognized_entries,
                &catalog_fingerprint.map(|hash| hash.to_hex().to_string()),
            ))
            .map_err(|_| ServerStoreError::Corrupt)?,
        )
        .to_hex()
        .to_string();
        Ok(ReconciliationPlan {
            context: self.context().clone(),
            instance_id: id.clone(),
            pins: pins.records,
            selected_record: selected,
            active_record,
            retained,
            candidates,
            unrecognized_entries,
            blockers,
            reclaimable_bytes,
            fingerprint,
            writes_performed: false,
        })
    }

    pub fn apply_reconciliation(
        &self,
        plan: &ReconciliationPlan,
        control: &ContentStageControl,
    ) -> Result<ReconciliationOutcome, ServerStoreError> {
        let _pin = super::owner::pin_write(&self.context().database_path())
            .map_err(|_| ServerStoreError::Ownership)?;
        if &plan.context != self.context() {
            return Err(ServerStoreError::StaleRevision);
        }
        let current = self.plan_reconciliation(&plan.instance_id, control)?;
        if current.fingerprint != plan.fingerprint {
            return Err(ServerStoreError::StaleRevision);
        }
        if !current.blockers.is_empty() {
            return Err(ServerStoreError::Invalid(current.blockers.join(" ")));
        }
        let catalog = self.load()?;
        let instance = self.show(&plan.instance_id)?;
        let mut installation_paths: BTreeSet<PathBuf> = catalog
            .installations
            .iter()
            .filter(|installation| Some(&installation.id) == instance.installation_id.as_ref())
            .map(|installation| installation.directory.clone())
            .collect();
        let runtimes = inventory(
            &self.native_runtime_root(&plan.instance_id),
            ReconciliationArtifact::Runtime,
            |name| owned_name(ReconciliationArtifact::Runtime, name),
        )?;
        for name in runtimes.names {
            let manifest: super::deployment_activation::NativeRuntimeManifest = read_json(
                &runtimes.directory.join(name).join("manifest.json"),
                control,
            )?;
            installation_paths.insert(manifest.installation.directory);
        }
        for candidate in &current.candidates {
            if candidate.kind == ReconciliationArtifact::Leftover {
                let (folder, name) = candidate
                    .id
                    .split_once('/')
                    .ok_or(ServerStoreError::UnsafePath)?;
                let receipt =
                    quarantine::read(self, &plan.instance_id, folder_kind(folder)?, name, control)?;
                installation_paths.extend(receipt.installation);
            }
        }
        let mut lock_map = BTreeMap::new();
        for path in installation_paths {
            let lock = foxy_supervisor::leases::InstallationLocks::capture(&path)
                .map_err(|_| ServerStoreError::Ownership)?;
            lock_map
                .entry(
                    lock.coordination_key()
                        .map_err(|_| ServerStoreError::Ownership)?,
                )
                .or_insert(lock);
        }
        let locks: Vec<_> = lock_map.into_values().collect();
        let _admissions = locks
            .iter()
            .map(|lock| lock.try_admit())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| {
                ServerStoreError::Invalid(
                    "Installation admission is busy or requires recovery.".into(),
                )
            })?;
        for lock in &locks {
            let consumers = lock.inspect_consumers().map_err(|_| {
                ServerStoreError::Invalid(
                    "Installation consumer registrations could not be verified.".into(),
                )
            })?;
            if consumers.iter().any(|consumer| {
                super::headless::consumer_belongs_to(
                    &consumer.session.instance_id,
                    plan.instance_id.as_str(),
                ) && consumer.session.game_space_id == self.context().space_id()
            }) {
                return Err(ServerStoreError::Invalid("This instance still has a running or unresolved engine consumer. Stop it and resolve recovery before reconciling.".into()));
            }
        }
        let mut removed = Vec::new();
        for candidate in &current.candidates {
            control.checkpoint()?;
            let path = self.artifact_path(&plan.instance_id, candidate)?;
            let parent = path.parent().ok_or(ServerStoreError::UnsafePath)?;
            super::store::check_directory_chain(&path)?;
            self.check_reconciliation_destination(&path)?;
            let quarantine = if candidate.kind == ReconciliationArtifact::Leftover {
                let (folder, name) = candidate
                    .id
                    .split_once('/')
                    .ok_or(ServerStoreError::UnsafePath)?;
                let kind = folder_kind(folder)?;
                let receipt = quarantine::read(self, &plan.instance_id, kind, name, control)?;
                let evidence = inventory::remaining(&path, &receipt.entries, control)?;
                if evidence.fingerprint != candidate.fingerprint {
                    return Err(ServerStoreError::StaleRevision);
                }
                path.clone()
            } else {
                let evidence = inventory::inspect(
                    self,
                    &plan.instance_id,
                    candidate.kind,
                    &candidate.id,
                    &path,
                    control,
                )?;
                if evidence.fingerprint != candidate.fingerprint {
                    return Err(ServerStoreError::StaleRevision);
                }
                let receipt = quarantine::Receipt::new(
                    self,
                    &plan.instance_id,
                    candidate.kind,
                    candidate.id.clone(),
                    evidence.entries,
                    evidence.installation,
                );
                receipt.save(self, control)?;
                let target = parent.join(receipt.name()?);
                if !target.starts_with(parent)
                    || target.try_exists().map_err(|_| ServerStoreError::Io)?
                {
                    return Err(ServerStoreError::UnsafePath);
                }
                self.check_reconciliation_destination(&target)?;
                fs::rename(&path, &target).map_err(|_| ServerStoreError::Io)?;
                target
            };
            let kind = if candidate.kind == ReconciliationArtifact::Leftover {
                folder_kind(
                    candidate
                        .id
                        .split_once('/')
                        .ok_or(ServerStoreError::UnsafePath)?
                        .0,
                )?
            } else {
                candidate.kind
            };
            let name = quarantine
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or(ServerStoreError::UnsafePath)?;
            let receipt = quarantine::read(self, &plan.instance_id, kind, name, control)?;
            let evidence = inventory::remaining(&quarantine, &receipt.entries, control)?;
            quarantine::remove(&quarantine, &evidence.entries, control)?;
            removed.push(candidate.clone());
        }
        Ok(ReconciliationOutcome {
            instance_id: plan.instance_id.clone(),
            writes_performed: !removed.is_empty(),
            reclaimed_bytes: removed.iter().map(|candidate| candidate.bytes).sum(),
            removed,
            sources_changed: false,
            records_changed: false,
        })
    }

    fn check_reconciliation_destination(&self, path: &Path) -> Result<(), ServerStoreError> {
        self.check_deployment_destination(path, None)?;
        let physical = super::content_staging::physical_directory(path)?;
        let sources = self.content_sources()?;
        for source in sources
            .sources
            .folders
            .iter()
            .map(|folder| &folder.path)
            .chain(
                sources
                    .sources
                    .profiles
                    .iter()
                    .map(|profile| &profile.local_path),
            )
        {
            let source = super::content_staging::physical_directory(source)?;
            if super::installation::paths_overlap(&physical, &source) {
                return Err(ServerStoreError::UnsafePath);
            }
        }
        Ok(())
    }

    fn artifact_path(
        &self,
        id: &ServerInstanceId,
        candidate: &ReconciliationCandidate,
    ) -> Result<PathBuf, ServerStoreError> {
        let path = match candidate.kind {
            ReconciliationArtifact::ContentStage if hex_id(&candidate.id, "stage_", 64) => {
                self.stage_root(id).join(&candidate.id)
            }
            ReconciliationArtifact::Preparation if hex_id(&candidate.id, "prepared_", 64) => {
                self.record_root(id).join("prepared").join(&candidate.id)
            }
            ReconciliationArtifact::Runtime if hex_id(&candidate.id, "runtime_", 32) => {
                self.native_runtime_root(id).join(&candidate.id)
            }
            ReconciliationArtifact::Leftover => {
                let (folder, name) = candidate
                    .id
                    .split_once('/')
                    .ok_or(ServerStoreError::UnsafePath)?;
                if !quarantine_name(name) {
                    return Err(ServerStoreError::UnsafePath);
                }
                match folder {
                    "content-stages" => self.stage_root(id).join(name),
                    "prepared" => self.record_root(id).join("prepared").join(name),
                    "runtimes" => self.native_runtime_root(id).join(name),
                    _ => return Err(ServerStoreError::UnsafePath),
                }
            }
            _ => return Err(ServerStoreError::UnsafePath),
        };
        let metadata = fs::symlink_metadata(&path).map_err(|_| ServerStoreError::Io)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
            return Err(ServerStoreError::UnsafePath);
        }
        Ok(path)
    }
}

fn artifact_folder(kind: ReconciliationArtifact) -> &'static str {
    match kind {
        ReconciliationArtifact::ContentStage => "content-stages",
        ReconciliationArtifact::Preparation => "prepared",
        ReconciliationArtifact::Runtime | ReconciliationArtifact::Leftover => "runtimes",
    }
}

fn folder_kind(folder: &str) -> Result<ReconciliationArtifact, ServerStoreError> {
    match folder {
        "content-stages" => Ok(ReconciliationArtifact::ContentStage),
        "prepared" => Ok(ReconciliationArtifact::Preparation),
        "runtimes" => Ok(ReconciliationArtifact::Runtime),
        _ => Err(ServerStoreError::UnsafePath),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_names_and_quarantine_names_are_strict() {
        assert!(hex_id(&format!("stage_{}", "a".repeat(64)), "stage_", 64));
        assert!(!hex_id(&format!("stage_{}", "A".repeat(64)), "stage_", 64));
        assert!(!hex_id(&format!("stage_{}", "a".repeat(63)), "stage_", 64));
        assert!(hex_id(
            &format!("runtime_{}", "0".repeat(32)),
            "runtime_",
            32
        ));
        assert!(quarantine_name(&format!(".reconcile-{}", "a".repeat(64))));
        assert!(!quarantine_name(".reconcile-runtime_ab-0123456789abcdef"));
        assert!(!quarantine_name(".reconcile-"));
        assert!(!quarantine_name(".reconcile-../x"));
        assert!(!quarantine_name("stage_x"));
    }

    #[test]
    fn inventory_separates_owned_leftover_and_unknown_entries() {
        let temp = tempfile::tempdir().unwrap();
        let stage = format!("stage_{}", "b".repeat(64));
        fs::create_dir(temp.path().join(&stage)).unwrap();
        let leftover = format!(".reconcile-{}", "c".repeat(64));
        fs::create_dir(temp.path().join(&leftover)).unwrap();
        fs::create_dir(temp.path().join(".stage-build-x")).unwrap();
        fs::write(temp.path().join("notes.txt"), b"operator").unwrap();
        let listing = inventory(temp.path(), ReconciliationArtifact::ContentStage, |name| {
            hex_id(name, "stage_", 64)
        })
        .unwrap();
        assert_eq!(listing.names, vec![stage]);
        assert_eq!(listing.leftovers, vec![leftover]);
        assert_eq!(
            listing.unrecognized,
            vec![".stage-build-x".to_owned(), "notes.txt".to_owned()]
        );
    }
}
