mod helper;
mod transaction;
pub(crate) mod worker;

use std::path::PathBuf;

use serde::Serialize;

use super::{
    AddonSupply, ContentStageControl, ContentStagePhase, ContentStageReport, ServerContext,
    ServerInstanceId, ServerStore, ServerStoreError,
};

#[derive(Clone, Debug, Serialize)]
pub struct ContentAcquisitionProfile {
    pub name: String,
    pub repository_url: String,
    pub repository_local_path: PathBuf,
    pub addons: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ContentAcquisitionPlan {
    #[serde(skip)]
    context: ServerContext,
    pub instance_id: ServerInstanceId,
    pub bindings_fingerprint: String,
    pub seed_stage: Option<String>,
    pub profiles: Vec<ContentAcquisitionProfile>,
    pub blockers: Vec<String>,
    pub fingerprint: String,
    pub writes_performed: bool,
    pub downloads_started: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentAcquisitionStatus {
    Complete,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
pub struct ContentAcquisitionAddon {
    pub name: String,
    pub checksum: String,
    pub verified: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ContentAcquisitionItem {
    pub profile: String,
    pub status: ContentAcquisitionStatus,
    pub addons: Vec<ContentAcquisitionAddon>,
    pub planned_transfer_bytes: u64,
    pub full_download_bytes: u64,
    pub patch_files_planned: u64,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ContentAcquisitionOutcome {
    pub instance_id: ServerInstanceId,
    pub status: ContentAcquisitionStatus,
    pub stage: Option<ContentStageReport>,
    pub items: Vec<ContentAcquisitionItem>,
    pub message: String,
    pub downloads_started: Option<bool>,
    pub writes_performed: bool,
    pub scratch_database_opened: Option<bool>,
    pub client_database_opened: bool,
}

impl ServerStore {
    pub fn plan_content_acquisition(
        &self,
        id: &ServerInstanceId,
        seed_stage: Option<&str>,
        control: &ContentStageControl,
    ) -> Result<ContentAcquisitionPlan, ServerStoreError> {
        control.defer_terminal();
        let result = self.plan_content_acquisition_inner(id, seed_stage, control);
        control.finish_operation(&result);
        result
    }

    fn plan_content_acquisition_inner(
        &self,
        id: &ServerInstanceId,
        seed_stage: Option<&str>,
        control: &ContentStageControl,
    ) -> Result<ContentAcquisitionPlan, ServerStoreError> {
        (|| {
            control.checkpoint()?;
            control.start_phase(ContentStagePhase::Preparing, 0, 0);
            self.validate_record_context()?;
            if !self.capabilities().content_acquisition {
                return Err(ServerStoreError::UnsupportedGame);
            }
            let bindings = self.content_bindings(id)?;
            if !bindings.review_current {
                return Err(ServerStoreError::StaleRevision);
            }
            let document = bindings.document.ok_or_else(|| {
                ServerStoreError::Invalid(
                    "Save reviewed content bindings before acquiring repositories".into(),
                )
            })?;
            let bindings_fingerprint = bindings.fingerprint.ok_or(ServerStoreError::Corrupt)?;
            let mut profiles = Vec::new();
            let mut blockers = Vec::new();
            for item in &document.bindings {
                if item.profile.supply == AddonSupply::Download {
                    super::validate_acquisition_url(&item.profile.remote_url)?;
                    profiles.push(ContentAcquisitionProfile {
                        name: item.profile.name.clone(),
                        repository_url: item.profile.remote_url.clone(),
                        repository_local_path: item.profile.local_path.clone(),
                        addons: item.profile.addons.clone(),
                    });
                }
            }
            if profiles.is_empty() {
                blockers.push("No download profiles are included in the saved bindings.".into());
            }
            let support = foxy_supervisor::process::support();
            if !support.available {
                blockers.push("Contained repository acquisition is unavailable on this host. Use an ordinary supported Windows or Linux user session.".into());
            }
            self.check_deployment_destination(&self.acquisition_root(id), Some(&document))?;
            if let Some(seed) = seed_stage {
                self.verify_content_stage(id, seed, control)?;
            }
            let mut plan = ContentAcquisitionPlan {
                context: self.context().clone(),
                instance_id: id.clone(),
                bindings_fingerprint,
                seed_stage: seed_stage.map(str::to_owned),
                profiles,
                blockers,
                fingerprint: String::new(),
                writes_performed: false,
                downloads_started: false,
            };
            plan.fingerprint = blake3::hash(
                &serde_json::to_vec(&(self.activation_context_fingerprint(), &plan))
                    .map_err(|_| ServerStoreError::Corrupt)?,
            )
            .to_hex()
            .to_string();
            let current = self.content_bindings(id)?;
            if !current.review_current
                || current.fingerprint.as_deref() != Some(plan.bindings_fingerprint.as_str())
            {
                return Err(ServerStoreError::StaleRevision);
            }
            control.checkpoint()?;
            Ok(plan)
        })()
    }

    pub(in crate::core::server) fn acquisition_root(&self, id: &ServerInstanceId) -> PathBuf {
        self.context()
            .space_dir()
            .join("server/instances")
            .join(id.as_str())
            .join("acquisition")
    }

    pub fn acquire_content(
        &self,
        plan: &ContentAcquisitionPlan,
        control: &ContentStageControl,
    ) -> Result<ContentAcquisitionOutcome, ServerStoreError> {
        control.defer_terminal();
        let result = self.acquire_content_inner(plan, control);
        let terminal = match &result {
            Ok(outcome) if outcome.status == ContentAcquisitionStatus::Cancelled => {
                Err(ServerStoreError::Cancelled)
            }
            Ok(outcome) if outcome.status == ContentAcquisitionStatus::Failed => {
                Err(ServerStoreError::Io)
            }
            Ok(_) => Ok(()),
            Err(error) => Err(error.clone()),
        };
        control.finish_operation(&terminal);
        result
    }
}
