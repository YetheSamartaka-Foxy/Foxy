use serde::Serialize;

use super::ServerDefinitionKind;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ServerCapabilities {
    pub definition_kind: Option<ServerDefinitionKind>,
    pub draft_definitions: bool,
    pub installation_adoption: bool,
    pub deployment_preflight: bool,
    pub deployment_composition: bool,
    pub deployment_records: bool,
    pub deployment_preparation: bool,
    pub deployment_activation: bool,
    pub native_installation: bool,
    pub native_installation_channels: bool,
    pub workshop_acquisition: bool,
    pub configuration_preview: bool,
    pub configuration_snapshots: bool,
    pub configuration_rollback: bool,
    pub configuration_import: bool,
    pub instance_packs: bool,
    pub mission_library: bool,
    pub key_trust: bool,
    pub content_planning: bool,
    pub content_bindings: bool,
    pub content_staging: bool,
    pub content_reconciliation: bool,
    pub content_acquisition: bool,
    pub steam_authentication: bool,
    pub installation: bool,
    pub deployment: bool,
    pub lifecycle: bool,
    pub durable_owner: bool,
    pub consumer_recovery: bool,
    pub headless_clients: bool,
}

impl ServerCapabilities {
    pub fn drafts(kind: ServerDefinitionKind) -> Self {
        let native = cfg!(all(
            target_arch = "x86_64",
            any(windows, target_os = "linux")
        ));
        let native_arma = kind == ServerDefinitionKind::Arma3 && native;
        Self {
            definition_kind: Some(kind),
            draft_definitions: true,
            installation_adoption: true,
            deployment_preflight: true,
            deployment_composition: cfg!(any(windows, target_os = "linux")),
            deployment_records: cfg!(any(windows, target_os = "linux")),
            deployment_preparation: cfg!(any(windows, target_os = "linux")),
            deployment_activation: native,
            native_installation: native,
            native_installation_channels: kind == ServerDefinitionKind::Reforger && native,
            workshop_acquisition: native_arma,
            headless_clients: native_arma,
            lifecycle: native,
            durable_owner: native,
            consumer_recovery: native,
            configuration_preview: true,
            configuration_snapshots: true,
            configuration_rollback: true,
            configuration_import: kind == ServerDefinitionKind::Arma3,
            instance_packs: true,
            mission_library: kind == ServerDefinitionKind::Arma3,
            key_trust: kind == ServerDefinitionKind::Arma3,
            content_planning: true,
            content_bindings: true,
            content_staging: cfg!(any(windows, target_os = "linux")),
            content_acquisition: cfg!(all(
                target_arch = "x86_64",
                any(windows, target_os = "linux")
            )),
            content_reconciliation: cfg!(any(windows, target_os = "linux"))
                && matches!(
                    kind,
                    ServerDefinitionKind::Arma3 | ServerDefinitionKind::Reforger
                ),
            steam_authentication: cfg!(any(windows, target_os = "linux")),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_implemented_draft_operations_are_advertised() {
        for id in ["arma3", "reforger", "generic", "twwh3"] {
            let module = crate::core::game::registry().get(id).unwrap();
            let caps = module.server_capabilities();
            assert_eq!(caps.draft_definitions, caps.definition_kind.is_some());
            assert_eq!(caps.installation_adoption, caps.draft_definitions);
            assert_eq!(caps.deployment_preflight, caps.draft_definitions);
            assert_eq!(caps.deployment_composition, caps.content_staging);
            assert_eq!(caps.deployment_records, caps.content_staging);
            assert_eq!(caps.deployment_preparation, caps.content_staging);
            assert_eq!(
                caps.deployment_activation,
                caps.definition_kind.is_some()
                    && cfg!(all(
                        target_arch = "x86_64",
                        any(windows, target_os = "linux")
                    ))
            );
            assert_eq!(caps.configuration_preview, caps.draft_definitions);
            assert_eq!(caps.configuration_snapshots, caps.draft_definitions);
            assert_eq!(caps.configuration_rollback, caps.draft_definitions);
            assert_eq!(caps.content_planning, caps.draft_definitions);
            assert_eq!(caps.content_bindings, caps.draft_definitions);
            assert_eq!(
                caps.content_staging,
                caps.draft_definitions && cfg!(any(windows, target_os = "linux"))
            );
            assert!(!caps.installation && !caps.deployment);
            assert_eq!(caps.lifecycle, caps.deployment_activation);
            assert_eq!(caps.consumer_recovery, caps.lifecycle);
            assert_eq!(
                caps.native_installation,
                caps.definition_kind.is_some() && caps.lifecycle
            );
            assert_eq!(
                caps.workshop_acquisition,
                caps.definition_kind == Some(super::ServerDefinitionKind::Arma3) && caps.lifecycle
            );
            assert_eq!(caps.headless_clients, caps.workshop_acquisition);
            assert_eq!(
                caps.native_installation_channels,
                caps.definition_kind == Some(super::ServerDefinitionKind::Reforger)
                    && caps.native_installation
            );
        }
        assert!(
            crate::core::game::registry()
                .get("arma3")
                .unwrap()
                .server_capabilities()
                .draft_definitions
        );
        assert!(
            crate::core::game::registry()
                .get("reforger")
                .unwrap()
                .server_capabilities()
                .draft_definitions
        );
        assert!(
            !crate::core::game::registry()
                .get("generic")
                .unwrap()
                .server_capabilities()
                .draft_definitions
        );
        assert!(
            !crate::core::game::registry()
                .get("twwh3")
                .unwrap()
                .server_capabilities()
                .draft_definitions
        );
    }
}
