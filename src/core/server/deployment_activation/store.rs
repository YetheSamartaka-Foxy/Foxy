use foxy_supervisor::control::Operation;
use foxy_supervisor::leases::InstallationLocks;
use foxy_supervisor::session::SessionIdentity;

use super::runtime::{fingerprint, hash_id};
use super::{
    DeploymentActivationPlan, DeploymentActivationStatus, DeploymentOperationOutcome,
    DeploymentResolutionReview,
};
use crate::core::server::activation::{
    ActivationError, ActivationRequest, ActivationResolutionPlan, TransactionBoundary,
};
use crate::core::server::{
    ContentStageControl, ServerInstallation, ServerInstanceId, ServerStore, ServerStoreError,
    owner::DeploymentExecution, runtime_payload,
};

pub(super) fn activation_error(error: ActivationError) -> ServerStoreError {
    match error {
        ActivationError::Store(error) => error,
        ActivationError::OwnerMismatch => ServerStoreError::Ownership,
        ActivationError::Interrupted => ServerStoreError::Cancelled,
        ActivationError::Unavailable => ServerStoreError::Invalid(
            "Native deployment admission is unavailable for these inputs.".into(),
        ),
        ActivationError::RecoveryRequired => ServerStoreError::Invalid(
            "A deployment transaction needs reviewed recovery before further activation.".into(),
        ),
    }
}

pub(super) fn execution_identity(execution: &DeploymentExecution, space: &str) -> SessionIdentity {
    let identity = execution.identity();
    SessionIdentity {
        owner_id: identity.owner_id.clone(),
        owner_generation: identity.owner_generation,
        game_space_id: space.into(),
        instance_id: identity.target_instance.as_str().into(),
        operation_id: identity.operation_id.clone(),
        run_generation: identity.run_generation,
    }
}

impl ServerStore {
    fn bound_native_installation(
        &self,
        id: &ServerInstanceId,
    ) -> Result<ServerInstallation, ServerStoreError> {
        if !self.capabilities().deployment_activation {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let instance = self.show(id)?;
        let installation_id = instance.installation_id.ok_or_else(|| {
            ServerStoreError::Invalid(
                "Bind a reviewed native installation before activation.".into(),
            )
        })?;
        let installation = self
            .load()?
            .installations
            .into_iter()
            .find(|installation| installation.id == installation_id)
            .ok_or(ServerStoreError::Corrupt)?;
        Ok(installation)
    }

    pub(in crate::core::server) fn native_installation(
        &self,
        id: &ServerInstanceId,
    ) -> Result<ServerInstallation, ServerStoreError> {
        let installation = self.bound_native_installation(id)?;
        self.check_installation_root(&installation)?;
        installation.verify()?;
        if self.capabilities().definition_kind != Some(installation.game)
            || super::super::engine_layout::observe_tested_layout(&installation).is_none()
        {
            return Err(ServerStoreError::Invalid(
                "The bound executable does not match a native-tested game layout.".into(),
            ));
        }
        Ok(installation)
    }

    pub fn plan_deployment_activation(
        &self,
        id: &ServerInstanceId,
        preparation_id: &str,
        expected_selection_generation: u64,
        expected_activation_generation: u64,
        control: &ContentStageControl,
    ) -> Result<DeploymentActivationPlan, ServerStoreError> {
        control.checkpoint()?;
        self.validate_record_context()?;
        if !hash_id(preparation_id, "prepared_") {
            return Err(ServerStoreError::Invalid(
                "Invalid native preparation identity.".into(),
            ));
        }
        let catalog_fingerprint = self
            .read()?
            .1
            .ok_or(ServerStoreError::Corrupt)?
            .to_hex()
            .to_string();
        let (selection, selection_fingerprint) = self.read_record_state(id, control)?;
        let previous = self
            .read_activation_state(id, control)
            .map_err(activation_error)?;
        if selection.generation != expected_selection_generation
            || previous.as_ref().map_or(0, |(state, _)| state.generation)
                != expected_activation_generation
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let installation = self.native_installation(id)?;
        let locks = InstallationLocks::capture(&installation.directory)
            .map_err(|_| ServerStoreError::Ownership)?;
        if locks
            .inspect_maintenance()
            .map_err(|_| ServerStoreError::Ownership)?
            .pending
            .is_some()
        {
            return Err(ServerStoreError::Invalid(
                "Installation maintenance has unfinished intent. Review recovery first.".into(),
            ));
        }
        if !locks
            .inspect_consumers()
            .map_err(|_| ServerStoreError::Ownership)?
            .is_empty()
        {
            return Err(ServerStoreError::Invalid(
                "Registered installation consumers need witnessed cleanup before activation."
                    .into(),
            ));
        }
        let preparation = self.verify_deployment_preparation(id, preparation_id, control)?;
        if selection.selected.as_deref() != Some(preparation.manifest.record_id.as_str()) {
            return Err(ServerStoreError::StaleRevision);
        }
        let payload = runtime_payload::inventory(
            installation.game,
            &installation.directory,
            installation.platform,
            control,
        )?;
        let manifest =
            self.native_runtime_manifest(&preparation, installation.clone(), payload, control)?;
        let manifest_fingerprint = fingerprint(&manifest)?;
        let runtime = super::NativeRuntimeReference {
            runtime_id: format!("runtime_{manifest_fingerprint}"),
            manifest_fingerprint,
        };
        let runtime_directory = self.runtime_directory(id, &runtime)?;
        self.validate_native_runtime_paths(&manifest, &runtime_directory)?;
        self.check_deployment_destination(
            &runtime_directory,
            Some(&preparation.manifest.record.content_bindings),
        )?;
        if let Some((state, _)) = &previous {
            self.verify_activation_reference(id, &state.deployment, control)
                .map_err(activation_error)?;
        }
        let mut plan = DeploymentActivationPlan {
            context: self.context().clone(),
            instance_id: id.clone(),
            preparation_id: preparation_id.into(),
            expected_selection_generation,
            expected_activation_generation,
            catalog_fingerprint,
            selection_fingerprint: selection_fingerprint.ok_or(ServerStoreError::Corrupt)?,
            installation,
            native_runtime: manifest,
            runtime,
            runtime_directory,
            fingerprint: String::new(),
            writes_performed: false,
            process_started: false,
        };
        plan.fingerprint = fingerprint(&plan)?;
        if self
            .read()?
            .1
            .map(|hash| hash.to_hex().to_string())
            .as_deref()
            != Some(plan.catalog_fingerprint.as_str())
            || self.read_record_state(id, control)?.1.as_deref()
                != Some(plan.selection_fingerprint.as_str())
        {
            return Err(ServerStoreError::StaleRevision);
        }
        plan.installation.verify()?;
        control.checkpoint()?;
        Ok(plan)
    }

    pub fn deployment_activation_status(
        &self,
        id: &ServerInstanceId,
        control: &ContentStageControl,
    ) -> Result<DeploymentActivationStatus, ServerStoreError> {
        self.show(id)?;
        let installation = self.bound_native_installation(id)?;
        let locks = InstallationLocks::capture(&installation.directory)
            .map_err(|_| ServerStoreError::Ownership)?;
        let maintenance = locks
            .inspect_maintenance()
            .map_err(|_| ServerStoreError::Ownership)?;
        let state = self
            .read_activation_state(id, control)
            .map_err(activation_error)?;
        let runtime = state
            .as_ref()
            .and_then(|(state, _)| state.deployment.runtime.clone());
        if let Some(runtime) = &runtime {
            self.verify_native_runtime(id, runtime, control)?;
        }
        Ok(DeploymentActivationStatus {
            instance_id: id.clone(),
            activation_generation: state.as_ref().map_or(0, |(state, _)| state.generation),
            runtime_directory: runtime
                .as_ref()
                .map(|runtime| self.runtime_directory(id, runtime))
                .transpose()?,
            preparation_id: state
                .as_ref()
                .map(|(state, _)| state.deployment.preparation_id.clone()),
            integrity_verified: runtime.is_some(),
            runtime,
            recovery_required: maintenance.pending.is_some(),
            writes_performed: false,
            process_started: false,
        })
    }

    pub fn plan_deployment_resolution(
        &self,
        id: &ServerInstanceId,
        control: &ContentStageControl,
    ) -> Result<DeploymentResolutionReview, ServerStoreError> {
        let installation = self.native_installation(id)?;
        let locks = InstallationLocks::capture(&installation.directory)
            .map_err(|_| ServerStoreError::Ownership)?;
        let resolver = SessionIdentity {
            owner_id: "review_only".into(),
            owner_generation: 1,
            game_space_id: self.context().space_id().into(),
            instance_id: id.as_str().into(),
            operation_id: "review_only".into(),
            run_generation: 1,
        };
        let plan = self
            .plan_activation_resolution(id, &locks, resolver, control)
            .map_err(activation_error)?;
        self.resolution_review(&plan)
    }

    fn resolution_review(
        &self,
        plan: &ActivationResolutionPlan,
    ) -> Result<DeploymentResolutionReview, ServerStoreError> {
        let fingerprint = fingerprint(&(
            &plan.record.intent,
            &plan.record.journal,
            &plan.record.active,
            &plan.record.active_fingerprint,
            plan.record.outcome,
            &plan.journal_fingerprint,
            &plan.catalog_fingerprint,
            &plan.selection_fingerprint,
            &plan.receipt,
        ))?;
        Ok(DeploymentResolutionReview {
            instance_id: plan.record.journal.instance_id.clone(),
            fingerprint,
            expected_outcome: format!("{:?}", plan.record.outcome).to_ascii_lowercase(),
            activation_generation: plan
                .record
                .active
                .as_ref()
                .map_or(0, |state| state.generation),
            runtime_id: plan
                .record
                .active
                .as_ref()
                .and_then(|state| state.deployment.runtime.as_ref())
                .map(|runtime| runtime.runtime_id.clone()),
            journal_present: plan.journal_fingerprint.is_some(),
            writes_performed: false,
            process_started: false,
        })
    }

    pub(crate) fn execute_owner_deployment(
        &self,
        operation: Operation,
        execution: &DeploymentExecution,
        control: &ContentStageControl,
    ) -> Result<DeploymentOperationOutcome, ServerStoreError> {
        execution
            .validate(self.context())
            .map_err(|_| ServerStoreError::Ownership)?;
        let identity = execution_identity(execution, self.context().space_id());
        let id = execution.identity().target_instance.clone();
        self.require_no_server_restore(&id)?;
        match operation {
            Operation::ActivateDeployment {
                instance_id,
                preparation_id,
                expected_selection_generation,
                expected_activation_generation,
                review_fingerprint,
            } => {
                if instance_id != id.as_str() {
                    return Err(ServerStoreError::Ownership);
                }
                let plan = self.plan_deployment_activation(
                    &id,
                    &preparation_id,
                    expected_selection_generation,
                    expected_activation_generation,
                    control,
                )?;
                if plan.context != *self.context() || plan.fingerprint != review_fingerprint {
                    return Err(ServerStoreError::StaleRevision);
                }
                let locks = InstallationLocks::capture(&plan.installation.directory)
                    .map_err(|_| ServerStoreError::Ownership)?;
                let activation = self
                    .plan_activation(
                        ActivationRequest {
                            instance_id: id.clone(),
                            preparation_id: preparation_id.clone(),
                            expected_selection_generation,
                            expected_activation_generation,
                            owner: identity.clone(),
                            installation_key: locks
                                .coordination_key()
                                .map_err(|_| ServerStoreError::Ownership)?,
                            runtime: Some(plan.runtime.clone()),
                        },
                        control,
                    )
                    .map_err(activation_error)?;
                let admission = self
                    .admit_native_activation(
                        &activation,
                        &locks,
                        execution,
                        &plan.installation,
                        control,
                    )
                    .map_err(activation_error)?;
                let preparation =
                    self.verify_deployment_preparation(&id, &preparation_id, control)?;
                let committed = self
                    .execute_activation(activation, &admission, control, |boundary| {
                        if boundary == TransactionBoundary::AfterIntent {
                            let runtime = self.materialize_native_runtime(
                                &plan.native_runtime,
                                &preparation.directory,
                                control,
                            )?;
                            if runtime != plan.runtime {
                                return Err(ServerStoreError::StaleRevision.into());
                            }
                            self.prepare_native_runtime_state(&id, execution, control)?;
                            execution
                                .validate(self.context())
                                .map_err(|_| ServerStoreError::Ownership)?;
                            plan.installation.verify()?;
                        }
                        Ok(())
                    })
                    .map_err(activation_error)?;
                drop(admission);
                let resolution = self
                    .plan_activation_resolution(&id, &locks, identity, control)
                    .map_err(activation_error)?;
                let admission = self
                    .admit_native_resolution(
                        &resolution,
                        &locks,
                        execution,
                        &plan.installation,
                        control,
                    )
                    .map_err(activation_error)?;
                self.apply_activation_resolution(resolution, &locks, &admission, control)
                    .map_err(activation_error)?;
                Ok(DeploymentOperationOutcome {
                    instance_id: id,
                    status: "committed".into(),
                    activation_generation: committed.active.as_ref().map(|state| state.generation),
                    runtime_id: Some(plan.runtime.runtime_id),
                    runtime_verified: true,
                    journal_pending: false,
                    intent_pending: false,
                    writes_performed: true,
                    process_started: false,
                })
            }
            Operation::ResolveDeployment {
                instance_id,
                review_fingerprint,
            } => {
                if instance_id != id.as_str() {
                    return Err(ServerStoreError::Ownership);
                }
                let installation = self.native_installation(&id)?;
                let locks = InstallationLocks::capture(&installation.directory)
                    .map_err(|_| ServerStoreError::Ownership)?;
                let plan = self
                    .plan_activation_resolution(&id, &locks, identity, control)
                    .map_err(activation_error)?;
                let review = self.resolution_review(&plan)?;
                if review.fingerprint != review_fingerprint {
                    return Err(ServerStoreError::StaleRevision);
                }
                let admission = self
                    .admit_native_resolution(&plan, &locks, execution, &installation, control)
                    .map_err(activation_error)?;
                self.apply_activation_resolution(plan, &locks, &admission, control)
                    .map_err(activation_error)?;
                let runtime_verified = review.runtime_id.is_some();
                Ok(DeploymentOperationOutcome {
                    instance_id: id,
                    status: review.expected_outcome,
                    activation_generation: Some(review.activation_generation),
                    runtime_id: review.runtime_id,
                    runtime_verified,
                    journal_pending: false,
                    intent_pending: false,
                    writes_performed: true,
                    process_started: false,
                })
            }
            _ => Err(ServerStoreError::Invalid(
                "The owner operation is not a deployment effect.".into(),
            )),
        }
    }
}
