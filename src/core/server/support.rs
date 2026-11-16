use super::{ServerCapabilities, ServerContext, ServerStore};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct RuntimePrerequisites {
    pub process: foxy_supervisor::process::ProcessSupport,
    pub installation_lease_protocol: &'static str,
    pub installation_lease_scope: &'static str,
    pub installation_leases: &'static str,
    pub foreground_session: &'static str,
    pub crash_consumer_reconciliation: &'static str,
    pub owner_handoff: &'static str,
    pub runtime_deployment: &'static str,
    pub engine_layout: &'static str,
    pub launch_available: bool,
}

impl RuntimePrerequisites {
    pub fn inspect() -> Self {
        Self {
            process: foxy_supervisor::process::support(),
            installation_lease_protocol: "physical_directory_admission_then_content_v1",
            installation_lease_scope: "same_user_local_filesystem_only",
            installation_leases: "not_acquired",
            foreground_session: "primitive_only",
            crash_consumer_reconciliation: "persistent_registration_barrier",
            owner_handoff: "not_implemented",
            runtime_deployment: "not_implemented",
            engine_layout: "unsupported",
            launch_available: false,
        }
    }
}

#[derive(Serialize)]
pub struct ServerAvailability {
    pub inspect_drafts: &'static str,
    pub edit_drafts: &'static str,
    pub installation_adoption: &'static str,
    pub installation_consumers: &'static str,
    pub deployment_preflight: &'static str,
    pub deployment_composition: &'static str,
    pub deployment_records: &'static str,
    pub deployment_preparation: &'static str,
    pub configuration_preview: &'static str,
    pub configuration_snapshots: &'static str,
    pub configuration_rollback: &'static str,
    pub content_planning: &'static str,
    pub content_bindings: &'static str,
    pub content_staging: &'static str,
    pub content_acquisition: &'static str,
    pub steam_authentication: &'static str,
    pub installation: &'static str,
    pub deployment: &'static str,
    pub lifecycle: &'static str,
}

#[derive(Serialize)]
pub struct ServerSupportReport {
    pub capabilities: ServerCapabilities,
    pub availability: ServerAvailability,
    pub storage_error: Option<String>,
    pub runtime_prerequisites: RuntimePrerequisites,
}

pub fn support_report(context: &ServerContext) -> ServerSupportReport {
    let capabilities = crate::core::game::registry()
        .get(context.game_id())
        .map(|module| module.server_capabilities())
        .unwrap_or_default();
    let available = capabilities.draft_definitions;
    let storage_error = available
        .then(|| {
            ServerStore::new(context.clone())
                .and_then(|store| store.load())
                .err()
                .map(|error| error.to_string())
        })
        .flatten();
    ServerSupportReport {
        capabilities,
        availability: availability(capabilities, storage_error.is_some()),
        storage_error,
        runtime_prerequisites: RuntimePrerequisites::inspect(),
    }
}

fn availability(caps: ServerCapabilities, blocked: bool) -> ServerAvailability {
    let operation = |supported: bool, writes: bool| {
        if !supported {
            "unsupported"
        } else if blocked {
            "blocked_by_store"
        } else if writes {
            "requires_exclusive_owner"
        } else {
            "available"
        }
    };
    let future = if caps.draft_definitions {
        "not_implemented"
    } else {
        "unsupported"
    };
    ServerAvailability {
        inspect_drafts: operation(caps.draft_definitions, false),
        edit_drafts: operation(caps.draft_definitions, true),
        installation_adoption: operation(caps.installation_adoption, true),
        installation_consumers: operation(caps.installation_adoption, false),
        deployment_preflight: operation(caps.deployment_preflight, false),
        deployment_composition: operation(caps.deployment_composition, false),
        deployment_records: operation(caps.deployment_records, true),
        deployment_preparation: operation(caps.deployment_preparation, true),
        configuration_preview: operation(caps.configuration_preview, false),
        configuration_snapshots: operation(caps.configuration_snapshots, true),
        configuration_rollback: operation(caps.configuration_rollback, true),
        content_planning: operation(caps.content_planning, false),
        content_bindings: operation(caps.content_bindings, true),
        content_staging: operation(caps.content_staging, true),
        content_acquisition: operation(caps.content_acquisition, true),
        steam_authentication: if caps.steam_authentication {
            "requires_private_session_store"
        } else {
            "unsupported"
        },
        installation: future,
        deployment: future,
        lifecycle: if caps.lifecycle {
            operation(true, true)
        } else {
            future
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_primitives_do_not_advertise_game_launch_or_recovery() {
        let report = RuntimePrerequisites::inspect();
        assert!(!report.launch_available);
        assert_eq!(report.installation_leases, "not_acquired");
        assert_eq!(
            report.crash_consumer_reconciliation,
            "persistent_registration_barrier"
        );
        assert_eq!(report.owner_handoff, "not_implemented");
        assert_eq!(report.runtime_deployment, "not_implemented");
        assert_eq!(report.engine_layout, "unsupported");
    }

    #[test]
    fn availability_respects_each_operation_capability_and_store_failure() {
        let mut caps = ServerCapabilities::drafts(super::super::ServerDefinitionKind::Arma3);
        caps.installation_adoption = false;
        caps.configuration_snapshots = false;
        let report = availability(caps, false);
        assert_eq!(report.installation_adoption, "unsupported");
        assert_eq!(report.configuration_snapshots, "unsupported");
        assert_eq!(report.configuration_preview, "available");
        assert_eq!(report.content_planning, "available");
        assert_eq!(report.content_bindings, "requires_exclusive_owner");
        assert_eq!(report.configuration_rollback, "requires_exclusive_owner");
        let blocked = availability(caps, true);
        assert_eq!(blocked.installation_adoption, "unsupported");
        assert_eq!(blocked.configuration_preview, "blocked_by_store");
        assert_eq!(blocked.content_planning, "blocked_by_store");
        assert_eq!(blocked.content_bindings, "blocked_by_store");
        assert_eq!(blocked.edit_drafts, "blocked_by_store");
        assert_eq!(
            availability(ServerCapabilities::default(), false).lifecycle,
            "unsupported"
        );
    }
}
