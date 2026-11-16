use std::path::PathBuf;

use serde::Serialize;

use super::{
    ServerDefinition, ServerDefinitionKind, ServerInstallation, ServerInstanceId, ServerStore,
    ServerStoreError,
};

#[derive(Clone, Debug, Serialize)]
pub struct DeploymentPreflight {
    pub game_space_id: String,
    pub instance_id: ServerInstanceId,
    pub instance_revision: u64,
    pub catalog_revision: u64,
    pub installation: Option<ServerInstallation>,
    pub tested_engine_layout: Option<super::TestedEngineLayout>,
    pub installation_consumers: Option<super::InstallationConsumerInspection>,
    pub installation_recovery: Option<super::InstallationRecoveryInspection>,
    pub configuration: super::ActiveConfigurationEvidence,
    pub runtime_prerequisites: super::RuntimePrerequisites,
    pub paths: Vec<ProposedPath>,
    pub blockers: Vec<PreflightBlocker>,
    pub deployment_ready: bool,
    pub writes_performed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProposedPath {
    pub role: &'static str,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
pub struct PreflightBlocker {
    pub code: &'static str,
    pub message: String,
}

impl ServerStore {
    pub fn preflight(
        &self,
        id: &ServerInstanceId,
    ) -> Result<DeploymentPreflight, ServerStoreError> {
        if !crate::core::game::registry()
            .get(self.context().game_id())
            .is_some_and(|module| module.server_capabilities().deployment_preflight)
        {
            return Err(ServerStoreError::UnsupportedGame);
        }
        let (catalog, before, _) = self.read()?;
        let instance = catalog
            .instances
            .iter()
            .find(|instance| &instance.id == id)
            .ok_or(ServerStoreError::NotFound)?;
        let mut blockers = vec![];
        let mut block = |code, message: &str| {
            blockers.push(PreflightBlocker {
                code,
                message: message.into(),
            })
        };
        let configuration = self.active_configuration_evidence(instance);
        match configuration.status {
            "verified_configuration_only" => {}
            "not_selected" => block(
                "configuration_missing",
                "Select and verify a saved configuration before preparing a runtime deployment.",
            ),
            "invalid" => block(
                "configuration_invalid",
                "The selected configuration archive failed verification. Restore an intact revision before deployment.",
            ),
            "changed_during_inspection" => block(
                "configuration_changed",
                "Configuration selection changed during inspection. Review the preflight again.",
            ),
            _ => block(
                "configuration_unreadable",
                "Configuration history could not be inspected. Check its integrity before deployment.",
            ),
        }
        let runtime_prerequisites = super::RuntimePrerequisites::inspect();
        if !runtime_prerequisites.process.available {
            block(
                "process_containment_unavailable",
                "Owned server process containment is unavailable for this platform or owner token. Start remains blocked.",
            );
        }
        block(
            "installation_leases_not_acquired",
            "Read-only preflight does not acquire installation leases or prove that other consumers have stopped.",
        );
        block(
            "owner_recovery_unavailable",
            "Orphaned consumer cleanup still requires verified recovery. Runtime maintenance remains blocked.",
        );
        let installation = instance
            .installation_id
            .as_ref()
            .and_then(|id| {
                catalog
                    .installations
                    .iter()
                    .find(|installation| &installation.id == id)
            })
            .cloned();
        let installation_recovery = installation
            .as_ref()
            .map(|installation| self.installation_recovery(&installation.id))
            .transpose()?;
        let installation_consumers = installation_recovery
            .as_ref()
            .map(|recovery| recovery.consumers.clone());
        if let Some(recovery) = &installation_recovery {
            if recovery.maintenance_status == "pending" {
                block(
                    "maintenance_recovery_pending",
                    "Unfinished installation maintenance requires verified recovery. Runtime admission remains blocked.",
                );
            }
            if !recovery.inspection_complete {
                block(
                    "maintenance_recovery_unknown",
                    "Installation recovery metadata could not be fully inspected. Runtime admission remains blocked.",
                );
            }
        }
        if let Some(consumers) = &installation_consumers {
            match consumers.orphan_disposition() {
                super::consumers::OrphanDisposition::InspectionFailed => block(
                    "consumer_inspection_incomplete",
                    "Consumer registrations could not be verified; installation maintenance remains blocked.",
                ),
                super::consumers::OrphanDisposition::Retained => block(
                    "consumer_cleanup_required",
                    "Registered consumers still require verified cleanup. A registration does not prove that a server is running.",
                ),
                super::consumers::OrphanDisposition::Eligible => {}
            }
            if consumers.exclusive_maintenance_authorized || consumers.liveness_verified {
                block(
                    "consumer_maintenance_unauthorized",
                    "Inspection cannot authorize maintenance or prove that a consumer is alive.",
                );
            }
        }
        let mut tested_engine_layout = None;
        if let Some(installation) = &installation {
            if let Err(error) = self.check_installation_root(installation) {
                block("installation_root_unsafe", &error.to_string());
            }
            if let Err(error) = installation.verify() {
                block("installation_changed", &error.to_string());
            } else {
                tested_engine_layout = super::engine_layout::observe_tested_layout(installation);
            }
            block(
                "build_unverified",
                "Package completeness and vendor authenticity remain unverified; local build metadata is advisory.",
            );
        } else {
            block(
                "installation_missing",
                "Bind an inspected installation before deployment.",
            );
        }
        let mut roles = vec!["runtime", "profiles", "config", "content", "logs"];
        roles.extend(exclusive_roles(match instance.definition {
            ServerDefinition::Arma3(_) => ServerDefinitionKind::Arma3,
            ServerDefinition::Reforger(_) => ServerDefinitionKind::Reforger,
        }));
        let base = self.context().space_dir();
        let path_list = instance_role_paths(&base, id.as_str(), &roles);
        let paths = roles
            .into_iter()
            .zip(path_list)
            .map(|(role, path)| ProposedPath { role, path })
            .collect::<Vec<_>>();
        for proposed in &paths {
            if super::store::check_directory_chain(&proposed.path).is_err() {
                block(
                    "unsafe_path",
                    "A proposed destination contains a link, reparse point or non-directory.",
                );
            }
            if proposed.path.exists() {
                block(
                    "destination_exists",
                    "A proposed destination already exists; ownership and reuse have not been established.",
                );
            }
            if installation.as_ref().is_some_and(|installation| {
                super::installation::paths_overlap(&installation.directory, &proposed.path)
            }) {
                block(
                    "installation_overlap",
                    "Installation and instance destinations must remain separate.",
                );
            }
        }
        let ports = game_ports(&instance.definition);
        if catalog.instances.iter().any(|other| {
            &other.id != id && !overlapping_ports(&ports, &game_ports(&other.definition)).is_empty()
        }) {
            block(
                "port_collision",
                "Another saved instance in this space uses an overlapping game or query port.",
            );
        }
        let native_verified = tested_engine_layout.is_some()
            && self.capabilities().lifecycle
            && self
                .plan_instance_start(id, &super::ContentStageControl::default())
                .is_ok();
        match &instance.definition {
            ServerDefinition::Arma3(settings) => {
                if settings.game_port > u16::MAX - 4 {
                    block(
                        "port_range_overflow",
                        "The Arma server port range exceeds the available UDP port range.",
                    );
                }
                if !settings.mission_cycle.iter().any(|mission| mission.enabled) {
                    block("mission_missing", "No mission cycle is configured.");
                }
                if tested_engine_layout.is_some() && !native_verified {
                    block(
                        "runtime_layout_not_materialized",
                        "The executable matches a native-tested layout. Instance-owned runtime destinations have not been materialized or admitted.",
                    );
                } else if tested_engine_layout.is_none() {
                    let (code, message) =
                        super::engine_layout::layout_blocker(ServerDefinitionKind::Arma3);
                    block(code, message);
                }
            }
            ServerDefinition::Reforger(settings) => {
                if settings.scenario_id.is_none() {
                    block(
                        "scenario_missing",
                        "No Reforger scenario resource is configured.",
                    );
                }
                if !settings.addons.is_empty() {
                    block(
                        "addon_resolution_unverified",
                        "Reforger addon intent has not been resolved to acquired, verified content.",
                    );
                }
                if tested_engine_layout.is_some() && !native_verified {
                    block(
                        "runtime_layout_not_materialized",
                        "The executable matches a native-tested layout. Instance-owned runtime destinations have not been materialized or admitted.",
                    );
                } else if tested_engine_layout.is_none() {
                    let (code, message) =
                        super::engine_layout::layout_blocker(ServerDefinitionKind::Reforger);
                    block(code, message);
                }
            }
        }
        if !native_verified {
            block(
                "runtime_isolation_unverified",
                "Proposed paths have not been materialized or verified for this instance. Tested executable compatibility does not authorize deployment.",
            );
        }
        block(
            "host_ports_unverified",
            "Port checks cover saved drafts in this space only; other spaces, processes and host port availability remain unverified.",
        );
        if self.capabilities().lifecycle {
            block(
                "foreground_owner_required",
                "Use the exact native Start review and an authenticated foreground owner. This read-only preflight acquires no runtime authority.",
            );
        } else {
            block(
                "deployment_unavailable",
                "Runtime deployment, engine path acceptance and game Start/Stop are not implemented.",
            );
        }
        if tested_engine_layout.is_some()
            && installation
                .as_ref()
                .is_some_and(|installation| installation.verify().is_err())
        {
            tested_engine_layout = None;
            block(
                "installation_changed",
                "The executable changed during preflight. Review the installation again.",
            );
        }
        if self.load()? != catalog {
            block(
                "inputs_changed",
                "Server definitions changed during inspection. Review the preflight again.",
            );
        }
        if self.read()?.1 != before {
            return Err(ServerStoreError::StaleRevision);
        }
        Ok(DeploymentPreflight {
            game_space_id: catalog.game_space_id,
            instance_id: id.clone(),
            instance_revision: instance.revision,
            catalog_revision: catalog.revision,
            installation,
            tested_engine_layout,
            installation_consumers,
            installation_recovery,
            configuration,
            runtime_prerequisites,
            paths,
            blockers,
            deployment_ready: false,
            writes_performed: false,
        })
    }
}

pub(crate) fn instance_role_paths(
    space_dir: &std::path::Path,
    instance_id: &str,
    roles: &[&str],
) -> Vec<std::path::PathBuf> {
    let base = space_dir.join("server/instances").join(instance_id);
    roles.iter().map(|role| base.join(role)).collect()
}

pub(crate) fn overlapping_ports(left: &[u16], right: &[u16]) -> Vec<u16> {
    let mut overlap = left
        .iter()
        .copied()
        .filter(|port| right.contains(port))
        .collect::<Vec<_>>();
    overlap.sort_unstable();
    overlap.dedup();
    overlap
}

pub(crate) fn exclusive_roles(kind: ServerDefinitionKind) -> &'static [&'static str] {
    match kind {
        ServerDefinitionKind::Arma3 => &["keys", "missions", "battleye"],
        ServerDefinitionKind::Reforger => &["addon_temp", "storage"],
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoleCreateError {
    InvalidInstance,
    UnsafePath,
    OverlapsInstallation,
    AlreadyExists,
    Io,
}

/// Adopted installation roots must remain separate from owned runtime directories.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn create_owned_role_directories(
    space_dir: &std::path::Path,
    instance_id: &str,
    roles: &[&str],
    installation_dir: Option<&std::path::Path>,
) -> Result<Vec<std::path::PathBuf>, RoleCreateError> {
    if super::ServerInstanceId::try_from(instance_id.to_string()).is_err() {
        return Err(RoleCreateError::InvalidInstance);
    }
    if roles.is_empty()
        || roles.iter().any(|role| {
            role.is_empty()
                || role
                    .chars()
                    .any(|character| character == '/' || character == '\\' || character == '\0')
                || *role == "."
                || *role == ".."
        })
    {
        return Err(RoleCreateError::UnsafePath);
    }
    let space_meta =
        std::fs::symlink_metadata(space_dir).map_err(|_| RoleCreateError::UnsafePath)?;
    if !space_meta.is_dir() || super::store::check_directory_chain(space_dir).is_err() {
        return Err(RoleCreateError::UnsafePath);
    }
    let paths = instance_role_paths(space_dir, instance_id, roles);
    if paths.iter().any(|path| !path.starts_with(space_dir)) {
        return Err(RoleCreateError::UnsafePath);
    }
    let instance_dir = space_dir.join("server").join("instances").join(instance_id);
    if let Some(installation) = installation_dir
        && std::iter::once(&instance_dir)
            .chain(paths.iter())
            .any(|path| super::installation::paths_overlap(installation, path))
    {
        return Err(RoleCreateError::OverlapsInstallation);
    }
    if instance_dir.exists() || paths.iter().any(|path| path.exists()) {
        return Err(RoleCreateError::AlreadyExists);
    }
    ensure_ordinary_dir(&space_dir.join("server"))?;
    ensure_ordinary_dir(&space_dir.join("server").join("instances"))?;
    std::fs::create_dir(&instance_dir).map_err(|_| RoleCreateError::Io)?;
    let mut created = Vec::new();
    for path in &paths {
        if let Err(error) = std::fs::create_dir(path) {
            for created_path in created.iter().rev() {
                let _ = std::fs::remove_dir(created_path);
            }
            let _ = std::fs::remove_dir(&instance_dir);
            return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
                RoleCreateError::AlreadyExists
            } else {
                RoleCreateError::Io
            });
        }
        created.push(path.clone());
    }
    Ok(paths)
}

fn ensure_ordinary_dir(path: &std::path::Path) -> Result<(), RoleCreateError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            if super::store::check_directory_chain(path).is_err() {
                Err(RoleCreateError::UnsafePath)
            } else {
                Ok(())
            }
        }
        Ok(_) => Err(RoleCreateError::UnsafePath),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(path).map_err(|_| RoleCreateError::Io)
        }
        Err(_) => Err(RoleCreateError::Io),
    }
}

fn game_ports(definition: &ServerDefinition) -> Vec<u16> {
    match definition {
        ServerDefinition::Arma3(settings) => (0..=4)
            .filter_map(|offset| settings.game_port.checked_add(offset))
            .collect(),
        ServerDefinition::Reforger(settings) => std::iter::once(settings.bind_port)
            .chain(settings.a2s_port)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::ServerDefinitionKind;

    #[test]
    fn two_instance_role_paths_stay_disjoint_and_outside_an_adopted_root() {
        let space = std::path::Path::new("games/alpha");
        let adopted = std::path::Path::new("R:/Games/Arma3Server");
        let roles = ["runtime", "profiles", "config", "content", "logs"];
        let first = instance_role_paths(space, "instance-a", &roles);
        let second = instance_role_paths(space, "instance-b", &roles);
        assert_eq!(first.len(), roles.len());
        assert!(first.iter().all(|path| !second.contains(path)));
        assert!(
            first
                .iter()
                .chain(second.iter())
                .all(|path| !path.starts_with(adopted))
        );
        assert!(first.iter().all(|path| path.starts_with(space)));
    }

    #[test]
    fn overlapping_ports_are_the_shared_numbers_only() {
        let arma = ServerDefinition::draft(ServerDefinitionKind::Arma3, "MainServer");
        let mut other = ServerDefinition::draft(ServerDefinitionKind::Arma3, "SecondServer");
        assert!(!overlapping_ports(&game_ports(&arma), &game_ports(&other)).is_empty());
        let ServerDefinition::Arma3(settings) = &mut other else {
            unreachable!()
        };
        settings.game_port = 2402;
        assert!(overlapping_ports(&game_ports(&arma), &game_ports(&other)).is_empty());
        assert_eq!(overlapping_ports(&[2302, 2302, 9], &[9, 1]), vec![9]);
    }

    #[test]
    fn arma_and_reforger_exclusive_roles_do_not_intersect() {
        let arma = exclusive_roles(ServerDefinitionKind::Arma3);
        let reforger = exclusive_roles(ServerDefinitionKind::Reforger);
        assert!(arma.iter().all(|role| !reforger.contains(role)));
        assert!(arma.contains(&"missions"));
        assert!(reforger.contains(&"storage"));
    }

    #[test]
    fn port_ranges_are_game_specific_and_never_wrap() {
        let mut arma = ServerDefinition::draft(ServerDefinitionKind::Arma3, "MainServer");
        assert_eq!(game_ports(&arma), vec![2302, 2303, 2304, 2305, 2306]);
        let ServerDefinition::Arma3(settings) = &mut arma else {
            unreachable!()
        };
        settings.game_port = u16::MAX;
        assert_eq!(game_ports(&arma), vec![u16::MAX]);
        assert_eq!(
            game_ports(&ServerDefinition::draft(
                ServerDefinitionKind::Reforger,
                "MainServer"
            )),
            vec![2001]
        );
    }

    const FIRST_INSTANCE: &str = "server_0000000000000000000000000000000a";
    const SECOND_INSTANCE: &str = "server_0000000000000000000000000000000b";

    #[test]
    fn owned_role_directories_are_created_only_under_the_space() {
        let root = tempfile::tempdir().unwrap();
        let space = root.path().join("games").join("alpha");
        let adopted = root.path().join("adopted");
        std::fs::create_dir_all(&space).unwrap();
        std::fs::create_dir_all(&adopted).unwrap();
        let roles = ["runtime", "profiles", "config", "content", "logs"];
        let first =
            create_owned_role_directories(&space, FIRST_INSTANCE, &roles, Some(&adopted)).unwrap();
        let second =
            create_owned_role_directories(&space, SECOND_INSTANCE, &roles, Some(&adopted)).unwrap();
        assert_eq!(first.len(), roles.len());
        assert!(
            first
                .iter()
                .all(|path| path.is_dir() && path.starts_with(&space))
        );
        assert!(first.iter().all(|path| !second.contains(path)));
        assert!(
            first
                .iter()
                .chain(second.iter())
                .all(|path| { !path.starts_with(&adopted) && !adopted.starts_with(path) })
        );
        assert_eq!(
            create_owned_role_directories(&space, "instance-a", &roles, Some(&adopted)),
            Err(RoleCreateError::InvalidInstance)
        );
        assert_eq!(
            create_owned_role_directories(&space, FIRST_INSTANCE, &roles, Some(&adopted)),
            Err(RoleCreateError::AlreadyExists)
        );
        assert_eq!(
            create_owned_role_directories(&space, SECOND_INSTANCE, &roles, Some(&space)),
            Err(RoleCreateError::OverlapsInstallation)
        );
    }
}
