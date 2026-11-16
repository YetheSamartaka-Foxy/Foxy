use super::*;
use foxy_supervisor::leases::InstallationLocks;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeMaintenanceAction {
    Update,
    Repair,
}

#[derive(Clone, Debug, Serialize)]
pub struct NativeMaintenanceReview {
    pub action: NativeMaintenanceAction,
    pub previous: ServerInstallation,
    pub physical_key: String,
    pub manifest_fingerprint: String,
    pub previous_build_id: Option<u64>,
    pub previous_depots: Vec<(u32, String)>,
    pub update_available: bool,
    pub retained_peer_instances: Vec<ServerInstanceId>,
    pub consumers: super::super::InstallationConsumerInspection,
}

#[derive(Clone, Debug, Serialize)]
pub struct NativeInstallationCheck {
    pub instance_id: ServerInstanceId,
    pub installation: ServerInstallation,
    pub channel: ContentChannel,
    pub installed_build_id: Option<u64>,
    pub review: ContentReview,
    pub update_available: bool,
    pub writes_performed: bool,
    pub database_opened: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct NativeInstallationValidation {
    pub instance_id: ServerInstanceId,
    pub installation_id: super::super::InstallationId,
    pub valid: bool,
    pub files: u64,
    pub bytes: u64,
    pub inventory_blake3: Option<String>,
    pub error: Option<String>,
    pub writes_performed: bool,
    pub database_opened: bool,
}

pub(super) fn blocked() -> ServerStoreError {
    ServerStoreError::Invalid("Installation maintenance is blocked by an active operation, live or unresolved consumers, or pending recovery. Stop consumers through their owners and resolve recovery before retrying.".into())
}

fn manifest(
    installation: &ServerInstallation,
) -> Result<(ManagedManifest, String), ServerStoreError> {
    if installation.ownership != InstallationOwnership::Managed
        || installation.build_evidence != BuildEvidence::NativeDepotManifest
    {
        return Err(ServerStoreError::Invalid(
            "Maintenance requires a recorded Foxy-managed installation".into(),
        ));
    }
    super::super::store::check_directory_chain(&installation.directory)?;
    let path = installation.directory.join(MANIFEST_FILE);
    super::super::store::check_regular_file(&path)?;
    let bytes = super::super::store::read_bounded(&path).map_err(|_| ServerStoreError::Corrupt)?;
    let manifest: ManagedManifest =
        serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
    if manifest.schema_version != 1
        || manifest.game != installation.game
        || manifest.platform != installation.platform
        || dedicated_app(installation.game, manifest.channel).ok() != Some(manifest.app_id)
        || manifest.executable.path != installation.executable
        || manifest.executable.bytes != installation.evidence.bytes
        || manifest.executable.blake3 != installation.evidence.blake3
        || manifest.depots.is_empty()
        || manifest.depots.iter().any(|depot| depot.manifest_gid == 0)
        || manifest
            .depots
            .iter()
            .map(|depot| depot.depot_id)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != manifest.depots.len()
        || manifest.inventory_blake3.len() != 64
        || !manifest
            .inventory_blake3
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ServerStoreError::Corrupt);
    }
    validate_branch(&manifest.branch)?;
    Ok((manifest, blake3::hash(&bytes).to_hex().to_string()))
}

fn changed(manifest: &ManagedManifest, selection: &DepotSelection) -> bool {
    manifest.app_id != selection.app_id
        || steam_platform(manifest.platform) != selection.platform
        || manifest.branch != selection.branch
        || manifest.build_id != selection.build_id
        || manifest
            .depots
            .iter()
            .map(|depot| (depot.depot_id, depot.manifest_gid))
            .collect::<Vec<_>>()
            != selection
                .depots
                .iter()
                .map(|depot| (depot.depot_id, depot.manifest_gid))
                .collect::<Vec<_>>()
}

pub(super) fn provider_review(
    context: &ServerContext,
    app_id: u32,
    platform: ServerPlatform,
    branch: &str,
    account: Option<&str>,
    control: &ContentStageControl,
) -> Result<ContentReview, ServerStoreError> {
    validate_branch(branch)?;
    if account.is_some_and(|alias| !super::super::steam_auth::valid_account_alias(alias)) {
        return Err(ServerStoreError::Invalid(
            "Invalid Steam session alias".into(),
        ));
    }
    let input = super::super::steam_auth::unattended_input(context, account)
        .map_err(|error| ServerStoreError::Invalid(error.to_string()))?;
    control.start_phase(ContentStagePhase::Preparing, 0, 0);
    let session = Session::new(control)?;
    let cancel = session.cancel_rx();
    let result = session.runtime.block_on(async {
        let logged_in = logon(input, cancel.clone())
            .await
            .map_err(|error| error.to_string())?;
        let result = content::cancellable(
            cancel,
            REVIEW_DEADLINE,
            content::review(
                &logged_in.client,
                app_id,
                branch,
                steam_platform(platform),
                true,
            ),
        )
        .await
        .map_err(|error| error.to_string());
        drop(logged_in);
        result
    });
    session.close();
    control.checkpoint()?;
    result.map_err(ServerStoreError::Invalid)
}

impl ServerStore {
    fn managed_for(&self, id: &ServerInstanceId) -> Result<ServerInstallation, ServerStoreError> {
        if !self.capabilities().native_installation {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let instance = self.show(id)?;
        let installation = self
            .load()?
            .installations
            .into_iter()
            .find(|installation| Some(&installation.id) == instance.installation_id.as_ref())
            .ok_or(ServerStoreError::InstallationNotFound)?;
        self.check_installation_root(&installation)?;
        manifest(&installation)?;
        Ok(installation)
    }

    pub fn check_native_installation(
        &self,
        id: &ServerInstanceId,
        account: Option<&str>,
        control: &ContentStageControl,
    ) -> Result<NativeInstallationCheck, ServerStoreError> {
        let installation = self.managed_for(id)?;
        let (installed, fingerprint) = manifest(&installation)?;
        let review = provider_review(
            self.context(),
            installed.app_id,
            installation.platform,
            &installed.branch,
            account,
            control,
        )?;
        if manifest(&installation)?.1 != fingerprint || self.managed_for(id)? != installation {
            return Err(ServerStoreError::StaleRevision);
        }
        control.finish(&Ok(()));
        Ok(NativeInstallationCheck {
            channel: installed.channel,
            instance_id: id.clone(),
            update_available: changed(&installed, &review.selection),
            installed_build_id: installed.build_id,
            installation,
            review,
            writes_performed: false,
            database_opened: false,
        })
    }

    pub fn validate_native_installation(
        &self,
        id: &ServerInstanceId,
        control: &ContentStageControl,
    ) -> Result<NativeInstallationValidation, ServerStoreError> {
        let installation = self.managed_for(id)?;
        let (expected, fingerprint) = manifest(&installation)?;
        let locks = InstallationLocks::capture(&installation.directory)
            .map_err(|_| ServerStoreError::UnsafePath)?;
        control.start_phase(ContentStagePhase::Verifying, expected.files, expected.bytes);
        let files = content::inventory_tree(&installation.directory, MANIFEST_FILE, &|| {
            control.checkpoint().is_err()
        });
        control.checkpoint()?;
        locks
            .revalidate()
            .map_err(|_| ServerStoreError::StaleRevision)?;
        if manifest(&installation)?.1 != fingerprint || self.managed_for(id)? != installation {
            return Err(ServerStoreError::StaleRevision);
        }
        let mut result = NativeInstallationValidation {
            instance_id: id.clone(),
            installation_id: installation.id.clone(),
            valid: false,
            files: 0,
            bytes: 0,
            inventory_blake3: None,
            error: None,
            writes_performed: false,
            database_opened: false,
        };
        match files {
            Ok(files) => {
                result.files = files.len() as u64;
                result.bytes = files.iter().map(|file| file.bytes).sum();
                let hash = inventory_hash(&files);
                result.valid = hash == expected.inventory_blake3
                    && result.files == expected.files
                    && result.bytes == expected.bytes
                    && installation.verify().is_ok();
                result.inventory_blake3 = Some(hash);
                if !result.valid {
                    result.error = Some("Managed installation differs from its complete verified inventory; repair into a new folder to preserve existing files".into());
                }
            }
            Err(error) => result.error = Some(error.to_string()),
        }
        control.finish(&if result.valid {
            Ok(())
        } else {
            Err(ServerStoreError::Corrupt)
        });
        Ok(result)
    }

    pub fn plan_native_maintenance(
        &self,
        mut request: NativeInstallRequest,
        action: NativeMaintenanceAction,
        control: &ContentStageControl,
    ) -> Result<NativeInstallPlan, ServerStoreError> {
        let previous = self.managed_for(&request.instance_id)?;
        let (installed, fingerprint) = manifest(&previous)?;
        if request
            .channel
            .is_some_and(|channel| channel != installed.channel)
        {
            return Err(ServerStoreError::Invalid("Update and repair retain the installed channel. Use Install into a separate folder to select another channel.".into()));
        }
        request.channel = Some(installed.channel);
        let locks = InstallationLocks::capture(&previous.directory)
            .map_err(|_| ServerStoreError::UnsafePath)?;
        let physical_key = locks
            .coordination_key()
            .map_err(|_| ServerStoreError::UnsafePath)?;
        let consumers = self.installation_consumers(&previous.id)?;
        let inventory = locks.inspect_maintenance().map_err(|_| blocked())?;
        let mut plan = self.plan_native_install(request, control)?;
        if manifest(&previous)?.1 != fingerprint || self.managed_for(&plan.instance_id)? != previous
        {
            return Err(ServerStoreError::StaleRevision);
        }
        if !consumers.inspection_complete
            || consumers.cleanup_required != Some(false)
            || inventory.pending.is_some()
        {
            plan.blockers.push(blocked().to_string());
        }
        let catalog = self.load()?;
        let retained_peer_instances = catalog
            .instances
            .iter()
            .filter(|instance| instance.id != plan.instance_id)
            .filter(|instance| {
                catalog
                    .installations
                    .iter()
                    .find(|installation| {
                        Some(&installation.id) == instance.installation_id.as_ref()
                    })
                    .is_some_and(|installation| {
                        InstallationLocks::capture(&installation.directory)
                            .and_then(|other| other.coordination_key())
                            .is_ok_and(|key| key == physical_key)
                    })
            })
            .map(|instance| instance.id.clone())
            .collect();
        plan.maintenance = Some(NativeMaintenanceReview {
            action,
            previous,
            physical_key,
            manifest_fingerprint: fingerprint,
            previous_build_id: installed.build_id,
            previous_depots: installed
                .depots
                .iter()
                .map(|depot| (depot.depot_id, depot.manifest_gid.to_string()))
                .collect(),
            update_available: changed(&installed, &plan.review.selection),
            retained_peer_instances,
            consumers,
        });
        plan.fingerprint = plan_fingerprint(&plan);
        Ok(plan)
    }

    pub(super) fn previous_installation(
        &self,
        plan: &NativeInstallPlan,
    ) -> Result<Option<ServerInstallation>, ServerStoreError> {
        let Some(id) = &plan.previous_installation else {
            return Ok(None);
        };
        self.load()?
            .installations
            .into_iter()
            .find(|installation| installation.id.as_str() == id)
            .map(Some)
            .ok_or(ServerStoreError::InstallationNotFound)
    }

    pub(super) fn revalidate_maintenance(
        &self,
        plan: &NativeInstallPlan,
        locks: Option<&InstallationLocks>,
    ) -> Result<(), ServerStoreError> {
        let context =
            ServerContext::capture(self.context().data_root(), Some(self.context().space_id()))
                .map_err(ServerStoreError::Invalid)?;
        if &context != self.context() {
            return Err(ServerStoreError::StaleRevision);
        }
        if let Some(locks) = locks {
            locks
                .revalidate()
                .map_err(|_| ServerStoreError::StaleRevision)?;
        }
        if locks
            .map(|locks| {
                locks
                    .coordination_key()
                    .map_err(|_| ServerStoreError::StaleRevision)
            })
            .transpose()?
            != plan.previous_physical_key
        {
            return Err(ServerStoreError::StaleRevision);
        }
        if let Some(review) = &plan.maintenance
            && (self.previous_installation(plan)?.as_ref() != Some(&review.previous)
                || locks
                    .ok_or(ServerStoreError::StaleRevision)?
                    .coordination_key()
                    .map_err(|_| ServerStoreError::StaleRevision)?
                    != review.physical_key
                || manifest(&review.previous)?.1 != review.manifest_fingerprint)
        {
            return Err(ServerStoreError::StaleRevision);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
