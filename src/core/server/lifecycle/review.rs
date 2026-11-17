use foxy_supervisor::leases::InstallationLocks;

use super::InstanceStartPlan;
use crate::core::server::{
    ContentStageControl, ServerInstanceId, ServerPlatform, ServerStore, ServerStoreError,
};

impl ServerStore {
    pub fn plan_instance_start(
        &self,
        id: &ServerInstanceId,
        control: &ContentStageControl,
    ) -> Result<InstanceStartPlan, ServerStoreError> {
        if !self.capabilities().lifecycle {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let installation = self.native_installation(id)?;
        if installation.platform != ServerPlatform::current()? {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let locks = InstallationLocks::capture(&installation.directory)
            .map_err(|_| ServerStoreError::Ownership)?;
        if locks
            .inspect_maintenance()
            .map_err(|_| ServerStoreError::Ownership)?
            .pending
            .is_some()
        {
            return Err(ServerStoreError::Invalid(
                "Resolve the reviewed deployment recovery before starting this instance.".into(),
            ));
        }
        let (state, state_fingerprint) =
            self.settled_activation_state(id, control)
                .map_err(|error| match error {
                    super::super::activation::ActivationError::Store(error) => error,
                    _ => ServerStoreError::Invalid(
                        "A settled native deployment is required before Start.".into(),
                    ),
                })?;
        if locks
            .coordination_key()
            .map_err(|_| ServerStoreError::Ownership)?
            != state.installation_key
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let launch = self.native_runtime_launch_plan(id, control)?;
        if state
            .deployment
            .runtime
            .as_ref()
            .map(|value| &value.runtime_id)
            != Some(&launch.runtime_id)
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let bytes = serde_json::to_vec(&(
            &state_fingerprint,
            &installation,
            &launch,
            super::OWNER_EXIT_POLICY,
        ))
        .map_err(|_| ServerStoreError::Corrupt)?;
        control.checkpoint()?;
        Ok(InstanceStartPlan {
            instance_id: id.clone(),
            activation_generation: state.generation,
            runtime_id: launch.runtime_id.clone(),
            fingerprint: blake3::hash(&bytes).to_hex().to_string(),
            owner_exit_policy: super::OWNER_EXIT_POLICY,
            launch,
        })
    }
}

impl InstanceStartPlan {
    pub(crate) fn require_review(
        &self,
        expected_activation_generation: u64,
        fingerprint: &str,
    ) -> Result<(), ServerStoreError> {
        if expected_activation_generation == 0
            || self.activation_generation != expected_activation_generation
            || self.fingerprint != fingerprint
        {
            return Err(ServerStoreError::StaleRevision);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_requires_both_exact_activation_and_review() {
        let id = ServerInstanceId::try_from(format!("server_{}", "1".repeat(32))).unwrap();
        let plan = InstanceStartPlan {
            instance_id: id.clone(),
            activation_generation: 3,
            runtime_id: format!("runtime_{}", "a".repeat(64)),
            fingerprint: "b".repeat(64),
            owner_exit_policy: super::super::OWNER_EXIT_POLICY,
            launch: crate::core::server::deployment_activation::NativeRuntimeLaunchPlan {
                reforger_scenario: None,
                monitoring: None,
                instance_id: id,
                runtime_id: format!("runtime_{}", "a".repeat(64)),
                executable: "R:/Mods/Server/arma3server_x64.exe".into(),
                directory: "R:/Mods/Server".into(),
                arguments: vec![],
                udp_ports: vec![2302, 2303, 2304, 2305, 2306],
                environment: vec![],
                profiles: "R:/Mods/Profiles".into(),
                profile_seed: None,
                configuration_seeds: Vec::new(),
                headless: Vec::new(),
                rcon: None,
                battleye: None,
                spawned: false,
            },
        };
        assert!(plan.require_review(3, &"b".repeat(64)).is_ok());
        assert_eq!(
            plan.require_review(0, &plan.fingerprint),
            Err(ServerStoreError::StaleRevision)
        );
        assert_eq!(
            plan.require_review(2, &plan.fingerprint),
            Err(ServerStoreError::StaleRevision)
        );
        assert_eq!(
            plan.require_review(3, &"c".repeat(64)),
            Err(ServerStoreError::StaleRevision)
        );
    }
}
