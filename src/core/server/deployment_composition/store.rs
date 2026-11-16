use super::super::{
    ContentStageControl, ContentStagePhase, RuntimePrerequisites, ServerContext, ServerDefinition,
    ServerStore, ServerStoreError,
};
use super::{
    CompositionConfiguration, CompositionContent, CompositionIssue, DeploymentCompositionPlan,
    DeploymentCompositionRequest, artifacts,
};

impl ServerStore {
    pub fn plan_deployment_composition(
        &self,
        request: DeploymentCompositionRequest,
        control: &ContentStageControl,
    ) -> Result<DeploymentCompositionPlan, ServerStoreError> {
        let result = self.inspect_composition(request, control).map_err(|error| {
            if error == ServerStoreError::Cancelled {
                ServerStoreError::ReviewCancelled
            } else {
                error
            }
        });
        control.finish(&result);
        result
    }

    pub(in crate::core::server) fn inspect_composition(
        &self,
        request: DeploymentCompositionRequest,
        control: &ContentStageControl,
    ) -> Result<DeploymentCompositionPlan, ServerStoreError> {
        self.validate_composition_context()?;
        control.start_phase(ContentStagePhase::Preparing, 0, 0);
        control.checkpoint()?;
        let (catalog, catalog_hash, _) = self.read()?;
        let instance = catalog
            .instances
            .iter()
            .find(|instance| instance.id == request.instance_id)
            .ok_or(ServerStoreError::NotFound)?;
        if request
            .expected_revision
            .is_some_and(|revision| revision != instance.revision)
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let (selection, selection_hash) =
            self.read_configuration_state_controlled(&instance.id, control)?;
        if request
            .expected_configuration_generation
            .is_some_and(|generation| generation != selection.generation)
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let bindings = self.content_bindings(&instance.id)?;
        let sources_hash = self.content_sources()?.fingerprint;
        let mut blockers = Vec::new();
        let mut configuration = CompositionConfiguration {
            status: "not_selected",
            generation: selection.generation,
            selection_fingerprint: selection_hash.map(|hash| hash.to_hex().to_string()),
            revision_id: selection.active.clone(),
            manifest_fingerprint: None,
            integrity_verified: false,
            matches_current_draft: None,
            preview: None,
        };
        let mut verified_configuration = None;
        if let Some(revision) = &selection.active {
            match self.read_verified_configuration_controlled(&instance.id, revision, control) {
                Ok((archived_instance, preview, manifest_hash)) => {
                    configuration.status = "verified_configuration_only";
                    configuration.integrity_verified = true;
                    configuration.matches_current_draft = Some(
                        super::super::configuration::render(instance)
                            .is_ok_and(|current| current == preview),
                    );
                    configuration.manifest_fingerprint = Some(manifest_hash);
                    configuration.preview = Some(preview);
                    verified_configuration = Some(archived_instance);
                }
                Err(error) => {
                    propagate_interruption(&error)?;
                    configuration.status = "invalid";
                    block(&mut blockers, "configuration_invalid", &error.to_string());
                }
            }
        } else {
            block(
                &mut blockers,
                "configuration_missing",
                "Select a saved configuration archive before reviewing a deployment composition.",
            );
        }
        if configuration.matches_current_draft == Some(false) {
            block(
                &mut blockers,
                "configuration_intent_changed",
                "The selected configuration is intact but differs from the current saved draft.",
            );
        }
        let mut content = CompositionContent {
            status: "not_selected",
            requested_stage_id: request.stage_id.clone(),
            integrity_verified: false,
            stage: None,
            reviewed_bindings: None,
            matches_saved_bindings: None,
            current_bindings: bindings.clone(),
            current_sources_fingerprint: sources_hash.clone(),
        };
        let mut verified_manifest = None;
        if let Some(stage_id) = &request.stage_id {
            match self.inspect_verified_content_stage(&instance.id, stage_id, control) {
                Ok((manifest, report)) => {
                    let matches = bindings.document.as_ref() == Some(&manifest.bindings)
                        && bindings.fingerprint.as_deref() == Some(&manifest.bindings_fingerprint);
                    content.status = "verified_content_only";
                    content.integrity_verified = true;
                    content.matches_saved_bindings = Some(matches);
                    content.reviewed_bindings = Some(manifest.bindings.clone());
                    content.stage = Some(report);
                    if !matches {
                        block(
                            &mut blockers,
                            "content_intent_changed",
                            "The content archive is intact but its reviewed bindings differ from the current saved selection.",
                        );
                    }
                    if verified_configuration.as_ref().is_some_and(|archived| {
                        archived.revision != manifest.bindings.instance_revision
                    }) {
                        block(
                            &mut blockers,
                            "configuration_content_revision_mismatch",
                            "Configuration and content were reviewed against different instance revisions.",
                        );
                    }
                    verified_manifest = Some(manifest);
                }
                Err(error @ ServerStoreError::Invalid(_)) => return Err(error),
                Err(error) => {
                    propagate_interruption(&error)?;
                    content.status = if error == ServerStoreError::NotFound {
                        "missing"
                    } else {
                        "invalid"
                    };
                    block(&mut blockers, "content_unverified", &error.to_string());
                }
            }
        } else {
            block(
                &mut blockers,
                "content_missing",
                "Choose a retained content stage explicitly before reviewing a deployment composition.",
            );
        }
        if !bindings.review_current {
            block(
                &mut blockers,
                "bindings_review_required",
                "Current saved content bindings are absent or require review. Retained content integrity does not establish current intent.",
            );
        }
        let artifacts = match (&verified_configuration, &verified_manifest) {
            (Some(archived), Some(manifest)) => {
                artifacts::inspect(&archived.definition, &manifest.entries)
            }
            _ => {
                block(
                    &mut blockers,
                    "artifact_evidence_incomplete",
                    "Key and mission candidates require both a verified configuration and a verified content archive.",
                );
                artifacts::ArtifactEvidence::default()
            }
        };
        for issue in &artifacts.issues {
            if issue.code == "flattened_key_collision" && verified_configuration.as_ref().is_some_and(|archived| {
                matches!(&archived.definition, ServerDefinition::Arma3(arma) if arma.key_trust.as_ref().is_some_and(|trust|trust.instance_id==archived.id && trust.validate().is_ok()))
            }) { continue; }
            block(&mut blockers, issue.code, issue.message);
        }
        if artifacts.key_files > 0 {
            block(
                &mut blockers,
                "key_trust_unverified",
                "Key inventory does not prove signatures. Only exact reviewed public-key trust is used by native activation; the engine verifies client signatures.",
            );
        }
        let definition = verified_configuration
            .as_ref()
            .map_or(&instance.definition, |archived| &archived.definition);
        match definition {
            ServerDefinition::Arma3(settings)
                if !settings.mission_cycle.iter().any(|mission| mission.enabled) =>
            {
                block(
                    &mut blockers,
                    "mission_missing",
                    "No mission cycle is configured.",
                );
            }
            ServerDefinition::Reforger(settings) => {
                if settings.scenario_id.is_none() {
                    block(
                        &mut blockers,
                        "scenario_missing",
                        "No Reforger scenario resource is configured.",
                    );
                }
                block(
                    &mut blockers,
                    "reforger_resources_unverified",
                    "Local file integrity does not resolve Reforger addon versions, dependencies or scenario-provider provenance.",
                );
            }
            _ => {}
        }
        let installation = instance
            .installation_id
            .as_ref()
            .and_then(|id| catalog.installations.iter().find(|item| &item.id == id));
        let installation_consumers =
            installation.map(super::super::InstallationConsumerInspection::inspect);
        if installation.is_none() {
            block(
                &mut blockers,
                "installation_missing",
                "Bind an inspected installation before deployment.",
            );
        }
        if let Some(consumers) = &installation_consumers {
            match consumers.orphan_disposition() {
                super::super::consumers::OrphanDisposition::InspectionFailed => block(
                    &mut blockers,
                    "consumer_inspection_incomplete",
                    "Consumer registrations could not be verified; installation maintenance remains blocked.",
                ),
                super::super::consumers::OrphanDisposition::Retained => block(
                    &mut blockers,
                    "consumer_cleanup_required",
                    "Registered consumers still require verified cleanup. A registration does not prove that a server is running.",
                ),
                super::super::consumers::OrphanDisposition::Eligible => {}
            }
        }
        let (layout_code, layout_message) =
            super::super::engine_layout::layout_blocker(definition.kind());
        block(&mut blockers, layout_code, layout_message);
        block(
            &mut blockers,
            "installation_not_verified",
            "This review does not recheck the adopted executable, engine package, version or channel.",
        );
        block(
            &mut blockers,
            "installation_leases_not_acquired",
            "Read-only preflight does not acquire installation leases or prove that other consumers have stopped.",
        );
        block(
            &mut blockers,
            "owner_recovery_unavailable",
            "Orphaned consumer cleanup still requires verified recovery. Runtime maintenance remains blocked.",
        );
        block(
            &mut blockers,
            "runtime_activation_unavailable",
            "Deployment composition is a read-only review and grants no runtime activation authority.",
        );
        control.checkpoint()?;
        if let Some(revision) = &selection.active
            && configuration.integrity_verified
        {
            let (_, preview, manifest_hash) =
                self.read_verified_configuration_controlled(&instance.id, revision, control)?;
            if configuration.preview.as_ref() != Some(&preview)
                || configuration.manifest_fingerprint.as_ref() != Some(&manifest_hash)
            {
                return Err(ServerStoreError::StaleRevision);
            }
        }
        let after_bindings = self.content_bindings(&instance.id)?;
        if self.read()?.1 != catalog_hash
            || self.read_configuration_state_controlled(&instance.id, control)?
                != (selection, selection_hash)
            || after_bindings.document != bindings.document
            || after_bindings.fingerprint != bindings.fingerprint
            || after_bindings.review_current != bindings.review_current
            || self.content_sources()?.fingerprint != sources_hash
        {
            return Err(ServerStoreError::StaleRevision);
        }
        self.validate_composition_context()?;
        control.checkpoint()?;
        let mut plan = DeploymentCompositionPlan {
            schema_version: 1,
            game_space_id: self.context().space_id().into(),
            game_id: self.context().game_id().into(),
            instance_id: instance.id.clone(),
            instance_revision: instance.revision,
            catalog_revision: catalog.revision,
            catalog_fingerprint: catalog_hash.map(|hash| hash.to_hex().to_string()),
            configuration,
            content,
            artifacts,
            installation_id: instance.installation_id.clone(),
            installation_verified: false,
            installation_consumers,
            runtime_prerequisites: RuntimePrerequisites::inspect(),
            blockers,
            fingerprint: String::new(),
            deployment_ready: false,
            writes_performed: false,
            active_deployment_changed: false,
            downloads_started: false,
            installation_leases_acquired: false,
            process_started: false,
        };
        let bytes = serde_json::to_vec(&plan).map_err(|_| ServerStoreError::Corrupt)?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err(ServerStoreError::Invalid(
                "Deployment composition report exceeds 32 MiB".into(),
            ));
        }
        plan.fingerprint = blake3::hash(&bytes).to_hex().to_string();
        control.checkpoint()?;
        Ok(plan)
    }

    fn validate_composition_context(&self) -> Result<(), ServerStoreError> {
        if !self.capabilities().deployment_composition {
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
}

fn block(blockers: &mut Vec<CompositionIssue>, code: &'static str, message: &str) {
    blockers.push(CompositionIssue {
        code,
        message: message.into(),
    });
}

fn propagate_interruption(error: &ServerStoreError) -> Result<(), ServerStoreError> {
    if matches!(
        error,
        ServerStoreError::Cancelled
            | ServerStoreError::ReviewCancelled
            | ServerStoreError::StaleRevision
    ) {
        Err(error.clone())
    } else {
        Ok(())
    }
}
