use std::fs;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::server::config_import::parser::{self, CfgValue, Node, NodeKind};
use crate::core::server::{
    ContentStageControl, ServerInstanceId, ServerPlatform, ServerStore, ServerStoreError,
    owner::DeploymentExecution,
};

fn activation_error(error: crate::core::server::activation::ActivationError) -> ServerStoreError {
    use crate::core::server::activation::ActivationError;
    match error {
        ActivationError::Store(error) => error,
        ActivationError::OwnerMismatch => ServerStoreError::Ownership,
        ActivationError::Interrupted => ServerStoreError::Cancelled,
        ActivationError::Unavailable | ActivationError::RecoveryRequired => {
            ServerStoreError::Invalid("Native deployment is unavailable for launch.".into())
        }
    }
}

const MARKER: &str = "ownership.json";
const MAX_MARKER: usize = 8 * 1024;
const MAX_INTENT: usize = 8 * 1024;

fn arma_ports(first: u16) -> Result<Vec<u16>, ServerStoreError> {
    if first == 0 {
        return Err(ServerStoreError::Corrupt);
    }
    let last = first.checked_add(4).ok_or_else(|| {
        ServerStoreError::Invalid("Native game/query port range exceeds UDP limits.".into())
    })?;
    Ok((first..=last).collect())
}

pub(super) fn runtime_environment(
    state_root: &Path,
    platform: ServerPlatform,
) -> Result<Vec<(String, String)>, ServerStoreError> {
    let paths: &[(&str, &str)] = if platform == ServerPlatform::LinuxX64 {
        &[
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "home/config"),
            ("XDG_DATA_HOME", "home/data"),
            ("XDG_CACHE_HOME", "home/cache"),
            ("XDG_RUNTIME_DIR", "home/runtime"),
        ]
    } else {
        &[
            ("HOME", "home"),
            ("USERPROFILE", "home"),
            ("APPDATA", "home/config"),
            ("LOCALAPPDATA", "home/data"),
        ]
    };
    paths
        .iter()
        .map(|(name, relative)| Ok(((*name).into(), argument_path(&state_root.join(relative))?)))
        .collect()
}

#[derive(Clone, Debug, Serialize)]
pub struct NativeRuntimeLaunchPlan {
    pub instance_id: ServerInstanceId,
    pub runtime_id: String,
    pub executable: PathBuf,
    pub directory: PathBuf,
    pub arguments: Vec<String>,
    pub udp_ports: Vec<u16>,
    pub environment: Vec<(String, String)>,
    pub profiles: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitoring: Option<crate::core::server::monitoring::NativeMonitoringInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_seed: Option<super::NativeProfileSeed>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub configuration_seeds: Vec<crate::core::server::config_import::NativeConfigurationSeed>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub headless: Vec<crate::core::server::HeadlessLaunchPlan>,
    pub spawned: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchIntent {
    game_port: u16,
    auto_init: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server_config: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    basic_config: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeStateMarker {
    schema_version: u32,
    instance_id: ServerInstanceId,
    game_space_id: String,
    context_fingerprint: String,
}

pub(in crate::core::server) fn argument_path(path: &Path) -> Result<String, ServerStoreError> {
    let value = path
        .to_str()
        .ok_or(ServerStoreError::UnsafePath)?
        .replace('\\', "/");
    let value = value
        .strip_prefix("//?/UNC/")
        .map(|tail| format!("//{tail}"))
        .or_else(|| value.strip_prefix("//?/").map(str::to_owned))
        .unwrap_or(value);
    if value.contains(['"', '\0']) {
        return Err(ServerStoreError::UnsafePath);
    }
    #[cfg(windows)]
    let value = value.replace('/', "\\");
    Ok(value)
}

fn relative_mod(value: &str) -> Result<(), ServerStoreError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains([';', '"', '\0', ':', '\\'])
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(ServerStoreError::UnsafePath);
    }
    Ok(())
}

fn marker_for(store: &ServerStore, id: &ServerInstanceId) -> RuntimeStateMarker {
    RuntimeStateMarker {
        schema_version: 1,
        instance_id: id.clone(),
        game_space_id: store.context().space_id().into(),
        context_fingerprint: store.activation_context_fingerprint(),
    }
}

fn read_marker(
    root: &Path,
    expected: &RuntimeStateMarker,
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    let path = root.join(MARKER);
    let bytes =
        crate::core::server::content_staging::read_bounded_controlled(&path, MAX_MARKER, control)?;
    let actual: RuntimeStateMarker =
        serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
    if &actual != expected {
        return Err(ServerStoreError::Ownership);
    }
    Ok(())
}

fn parse_launch_intent(bytes: &[u8]) -> Result<LaunchIntent, ServerStoreError> {
    let intent: LaunchIntent =
        serde_json::from_slice(bytes).map_err(|_| ServerStoreError::Corrupt)?;
    if intent.game_port == 0 {
        return Err(ServerStoreError::Corrupt);
    }
    crate::core::server::Arma3ConfigurationNames {
        server: intent.server_config.clone(),
        basic: intent.basic_config.clone(),
        profile_name: intent.profile_name.clone(),
    }
    .validate()
    .map_err(|_| ServerStoreError::Corrupt)?;
    Ok(intent)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ArmaMonitoringConfig {
    hostname: Option<String>,
    query_port: Option<u16>,
}

fn arma_monitoring_config(source: &str, game_port: u16) -> ArmaMonitoringConfig {
    let disabled = ArmaMonitoringConfig {
        hostname: None,
        query_port: None,
    };
    let Ok(document) = parser::parse(source) else {
        return disabled;
    };
    if document.unsupported {
        return disabled;
    }
    let hostname = match unique_root_value(&document.nodes, "hostname") {
        Some(CfgValue::String(value)) if !value.trim().is_empty() => Some(value.clone()),
        _ => None,
    };
    let query_port = match matching_root_nodes(&document.nodes, "steamQueryPort").as_slice() {
        [] => game_port.checked_add(1).filter(|port| *port != 0),
        [
            Node {
                kind:
                    NodeKind::Assignment {
                        array: false,
                        append: false,
                        value: CfgValue::Number(value),
                        ..
                    },
                ..
            },
        ] => value
            .as_u64()
            .and_then(|port| u16::try_from(port).ok())
            .filter(|port| *port != 0),
        _ => None,
    };
    ArmaMonitoringConfig {
        hostname,
        query_port,
    }
}

fn battleye_requested(source: &str) -> bool {
    let Ok(document) = parser::parse(source) else {
        return false;
    };
    matching_root_nodes(&document.nodes, "BattlEye")
        .iter()
        .any(|node| {
            !matches!(
                &node.kind,
                NodeKind::Assignment {
                    value: CfgValue::Number(value),
                    ..
                } if value.as_u64() == Some(0)
            )
        })
}

fn matching_root_nodes<'a>(nodes: &'a [Node], name: &str) -> Vec<&'a Node> {
    nodes
        .iter()
        .filter(|node| node.name.eq_ignore_ascii_case(name))
        .collect()
}

fn unique_root_value<'a>(nodes: &'a [Node], name: &str) -> Option<&'a CfgValue> {
    let mut matching = nodes
        .iter()
        .filter(|node| node.name.eq_ignore_ascii_case(name));
    let node = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    match &node.kind {
        NodeKind::Assignment {
            array: false,
            append: false,
            value,
            ..
        } => Some(value),
        _ => None,
    }
}

fn resolve_query_endpoint(hostname: &str, port: u16) -> Option<SocketAddr> {
    (!hostname.trim().is_empty()).then(|| SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port)))
}

pub(in crate::core::server) fn validate_runtime_state(
    store: &ServerStore,
    id: &ServerInstanceId,
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    validate_state_tree(
        &store.record_root(id).join("runtime-state"),
        &marker_for(store, id),
        control,
    )
}

fn state_paths(root: &Path) -> [(&'static str, PathBuf); 6] {
    [
        ("home", root.join("home")),
        ("profiles", root.join("home/profiles")),
        ("config", root.join("home/config")),
        ("data", root.join("home/data")),
        ("cache", root.join("home/cache")),
        ("runtime", root.join("home/runtime")),
    ]
}

fn prepare_state_tree(
    root: &Path,
    marker: &RuntimeStateMarker,
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    control.checkpoint()?;
    crate::core::server::store::check_directory_chain(root)?;
    if root.try_exists().map_err(|_| ServerStoreError::Io)? {
        return validate_state_tree(root, marker, control);
    }
    let parent = root.parent().ok_or(ServerStoreError::UnsafePath)?;
    let temporary = tempfile::Builder::new()
        .prefix(".runtime-state-")
        .tempdir_in(parent)
        .map_err(|_| ServerStoreError::Io)?;
    let bytes = serde_json::to_vec(marker).map_err(|_| ServerStoreError::Corrupt)?;
    if bytes.len() > MAX_MARKER {
        return Err(ServerStoreError::Corrupt);
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary.path().join(MARKER))
        .map_err(|_| ServerStoreError::Io)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| ServerStoreError::Io)?;
    drop(file);
    for (_, path) in state_paths(temporary.path()) {
        control.checkpoint()?;
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        builder.create(&path).map_err(|_| ServerStoreError::Io)?;
    }
    validate_state_tree(temporary.path(), marker, control)?;
    control.checkpoint()?;
    if crate::core::server::content_staging::publish_no_replace(temporary.path(), root)? {
        let _ = temporary.keep();
    }
    validate_state_tree(root, marker, control)
}

fn validate_state_tree(
    root: &Path,
    marker: &RuntimeStateMarker,
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    crate::core::server::store::check_directory_chain(root)?;
    read_marker(root, marker, control)?;
    for (_, path) in state_paths(root) {
        control.checkpoint()?;
        crate::core::server::store::check_directory_chain(&path)?;
        let metadata = fs::symlink_metadata(&path).map_err(|_| ServerStoreError::UnsafePath)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(ServerStoreError::UnsafePath);
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.mode() & 0o777 != 0o700 {
                return Err(ServerStoreError::UnsafePath);
            }
        }
    }
    Ok(())
}

impl ServerStore {
    pub fn native_runtime_launch_plan(
        &self,
        id: &ServerInstanceId,
        control: &ContentStageControl,
    ) -> Result<NativeRuntimeLaunchPlan, ServerStoreError> {
        control.checkpoint()?;
        let state = self
            .read_activation_state(id, control)
            .map_err(activation_error)?
            .ok_or(ServerStoreError::NotFound)?
            .0;
        let reference = state
            .deployment
            .runtime
            .as_ref()
            .ok_or(ServerStoreError::UnsupportedGame)?;
        let manifest = self.verify_native_runtime(id, reference, control)?;
        let runtime = self.runtime_directory(id, reference)?;
        if manifest.installation.game == crate::core::server::ServerDefinitionKind::Reforger {
            return self.reforger_runtime_launch_plan(id, reference, &manifest, &runtime, control);
        }
        let engine = runtime.join("engine");
        let executable = engine.join(&manifest.installation.executable);
        crate::core::server::store::check_regular_file(&executable)?;
        #[cfg(windows)]
        let (engine, executable) = (
            PathBuf::from(argument_path(&engine)?),
            PathBuf::from(argument_path(&executable)?),
        );
        let intent_path = runtime.join("configuration/launch-intent.json");
        let intent_bytes = crate::core::server::content_staging::read_bounded_controlled(
            &intent_path,
            MAX_INTENT,
            control,
        )?;
        let intent = parse_launch_intent(&intent_bytes)?;
        let state_root = self.record_root(id).join("runtime-state");
        validate_state_tree(&state_root, &marker_for(self, id), control)?;
        let selected_server_config = intent
            .server_config
            .as_deref()
            .unwrap_or(crate::core::server::ARMA3_DEFAULT_SERVER_CONFIG);
        let mut monitoring_config = ArmaMonitoringConfig {
            hostname: None,
            query_port: None,
        };
        let mut configuration_seeds = Vec::new();
        let mut server_config_source = None;
        for entry in &manifest.realized_entries {
            let Some(name) = entry
                .path
                .strip_prefix("configuration/")
                .filter(|name| name.ends_with(".cfg"))
            else {
                continue;
            };
            let source = runtime.join(&entry.path);
            let bytes = crate::core::server::content_staging::read_bounded_controlled(
                &source,
                1024 * 1024,
                control,
            )?;
            if name == selected_server_config {
                server_config_source = Some(String::from_utf8_lossy(&bytes).into_owned());
                monitoring_config = std::str::from_utf8(&bytes)
                    .map(|source| arma_monitoring_config(source, intent.game_port))
                    .unwrap_or(ArmaMonitoringConfig {
                        hostname: None,
                        query_port: None,
                    });
            }
            let has_references = bytes
                .windows(b"foxy-secret:".len())
                .any(|part| part == b"foxy-secret:");
            let bindings = if name
                == intent
                    .server_config
                    .as_deref()
                    .unwrap_or(crate::core::server::ARMA3_DEFAULT_SERVER_CONFIG)
            {
                manifest.credential_bindings.clone()
            } else {
                Vec::new()
            };
            if has_references || !bindings.is_empty() {
                let destination = state_root
                    .join("home/private-configuration")
                    .join(reference.directory_name())
                    .join(name);
                super::runtime::check_native_path(&destination, manifest.installation.platform)?;
                configuration_seeds.push(
                    crate::core::server::config_import::NativeConfigurationSeed {
                        source,
                        destination,
                        blake3: entry.blake3.clone().ok_or(ServerStoreError::Corrupt)?,
                        bindings,
                    },
                );
            }
        }
        let profiles = state_root.join("home/profiles");
        let name = intent
            .profile_name
            .as_deref()
            .unwrap_or(crate::core::server::ARMA3_DEFAULT_PROFILE);
        let mut profile_seed = None;
        for entry in &manifest.realized_entries {
            let Some(seeded) = entry
                .path
                .strip_prefix("configuration/")
                .and_then(|path| path.strip_suffix(".Arma3Profile"))
            else {
                continue;
            };
            if seeded != name || profile_seed.is_some() {
                return Err(ServerStoreError::Corrupt);
            }
            profile_seed = Some(super::NativeProfileSeed {
                source: runtime.join(&entry.path),
                destination: super::profile::profile_destination(
                    &profiles,
                    manifest.installation.platform,
                    name,
                ),
                profile_name: name.to_owned(),
                blake3: entry.blake3.clone().ok_or(ServerStoreError::Corrupt)?,
            });
        }
        let configuration = runtime.join("configuration");
        let configuration_path = |name: &str| -> PathBuf {
            configuration_seeds
                .iter()
                .find(|seed| seed.source.file_name().is_some_and(|file| file == name))
                .map(|seed| seed.destination.clone())
                .unwrap_or_else(|| configuration.join(name))
        };
        let mut arguments = vec![
            format!("-port={}", intent.game_port),
            format!(
                "-config={}",
                argument_path(&configuration_path(
                    intent
                        .server_config
                        .as_deref()
                        .unwrap_or(crate::core::server::ARMA3_DEFAULT_SERVER_CONFIG)
                ))?
            ),
            format!(
                "-cfg={}",
                argument_path(&configuration_path(
                    intent
                        .basic_config
                        .as_deref()
                        .unwrap_or(crate::core::server::ARMA3_DEFAULT_BASIC_CONFIG)
                ))?
            ),
            format!("-profiles={}", argument_path(&profiles)?),
            format!("-name={name}"),
        ];
        if !manifest.required_mods.is_empty() {
            for item in &manifest.required_mods {
                relative_mod(item)?;
            }
            arguments.push(format!("-mod={}", manifest.required_mods.join(";")));
        }
        if manifest.realized_entries.iter().any(|entry| {
            entry.path == "engine/foxy-keys"
                && entry.kind == super::super::StageEntryKind::Directory
        }) {
            arguments.push("-keysFolder=foxy-keys".into());
        }
        if !manifest.server_mods.is_empty() {
            for item in &manifest.server_mods {
                relative_mod(item)?;
            }
            arguments.push(format!("-serverMod={}", manifest.server_mods.join(";")));
        }
        if intent.auto_init {
            arguments.push("-autoInit".into());
        }
        if server_config_source
            .as_deref()
            .is_some_and(battleye_requested)
            && !manifest
                .payload
                .entries
                .iter()
                .any(|entry| entry.path.eq_ignore_ascii_case("battleye"))
        {
            return Err(ServerStoreError::Invalid("BattlEye is enabled in the committed server configuration, but this managed Arma runtime has no BattlEye files, so the server would not start its mission. Turn BattlEye off in the draft, then commit, prepare and activate again.".into()));
        }
        let environment = runtime_environment(&state_root, manifest.installation.platform)?;
        let headless = if manifest.headless_clients.is_empty() {
            Vec::new()
        } else {
            let source = server_config_source.ok_or(ServerStoreError::Corrupt)?;
            let password_reference = crate::core::server::headless::join_password_reference(
                &source,
                &manifest.credential_bindings,
            )?;
            let platform = manifest.installation.platform;
            let plans = crate::core::server::headless::headless_launch_plans(
                &manifest.headless_clients,
                crate::core::server::headless::HeadlessPlanInputs {
                    executable: &executable,
                    directory: &engine,
                    state_root: &state_root,
                    game_port: intent.game_port,
                    password_reference,
                },
                argument_path,
                |root| runtime_environment(root, platform),
                relative_mod,
            )?;
            for plan in &plans {
                super::runtime::check_native_path(
                    &super::profile::profile_destination(&plan.profiles, platform, &plan.name),
                    platform,
                )?;
                if let Some(path) = &plan.parameter_file {
                    super::runtime::check_native_path(path, platform)?;
                }
            }
            plans
        };
        let mut udp_ports = arma_ports(intent.game_port)?;
        if let Some(query_port) = monitoring_config.query_port
            && !udp_ports.contains(&query_port)
        {
            udp_ports.push(query_port);
            udp_ports.sort_unstable();
        }
        let monitoring_endpoint = monitoring_config.query_port.and_then(|port| {
            monitoring_config
                .hostname
                .as_deref()
                .and_then(|hostname| resolve_query_endpoint(hostname, port))
        });
        let monitoring = Some(crate::core::server::monitoring::NativeMonitoringInput {
            endpoint: monitoring_endpoint,
            expected_name: monitoring_endpoint.and_then(|_| monitoring_config.hostname.clone()),
            executable: executable.clone(),
            log_roots: vec![state_root.join("home/runtime"), profiles.clone()],
        });
        Ok(NativeRuntimeLaunchPlan {
            instance_id: id.clone(),
            runtime_id: reference.runtime_id.clone(),
            executable,
            directory: engine,
            arguments,
            udp_ports,
            environment,
            profiles,
            monitoring,
            profile_seed,
            configuration_seeds,
            headless,
            spawned: false,
        })
    }

    pub(in crate::core::server) fn prepare_native_runtime_state(
        &self,
        id: &ServerInstanceId,
        execution: &DeploymentExecution,
        control: &ContentStageControl,
    ) -> Result<(), ServerStoreError> {
        control.checkpoint()?;
        execution
            .validate(self.context())
            .map_err(|_| ServerStoreError::Ownership)?;
        if execution.identity().target_instance != *id {
            return Err(ServerStoreError::Ownership);
        }
        let root = self.record_root(id).join("runtime-state");
        self.check_deployment_destination(&root, None)?;
        prepare_state_tree(&root, &marker_for(self, id), control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arma_ports_reserve_the_full_range_and_reject_overflow() {
        assert_eq!(
            arma_ports(2302).unwrap(),
            vec![2302, 2303, 2304, 2305, 2306]
        );
        assert_eq!(
            arma_ports(65531).unwrap(),
            vec![65531, 65532, 65533, 65534, 65535]
        );
        assert!(arma_ports(0).is_err());
        assert!(arma_ports(65532).is_err());
    }

    #[test]
    fn launch_intent_is_strict_and_port_is_bounded() {
        assert_eq!(
            parse_launch_intent(br#"{"game_port":2302,"auto_init":true}"#).unwrap(),
            LaunchIntent {
                game_port: 2302,
                auto_init: true,
                server_config: None,
                basic_config: None,
                profile_name: None,
            }
        );
        assert!(
            parse_launch_intent(br#"{"game_port":2302,"auto_init":true,"profiles":"other"}"#)
                .is_err()
        );
        assert!(parse_launch_intent(br#"{"game_port":0,"auto_init":false}"#).is_err());
        let named = parse_launch_intent(
            br#"{"game_port":2302,"auto_init":false,"server_config":"dedicated.cfg","basic_config":"network.cfg","profile_name":"Managed"}"#,
        )
        .unwrap();
        assert_eq!(named.server_config.as_deref(), Some("dedicated.cfg"));
        assert_eq!(named.profile_name.as_deref(), Some("Managed"));
        for bad in [
            br#"{"game_port":2302,"auto_init":false,"server_config":"../server.cfg"}"#.as_slice(),
            br#"{"game_port":2302,"auto_init":false,"basic_config":"server.cfg"}"#,
            br#"{"game_port":2302,"auto_init":false,"profile_name":"a/b"}"#,
        ] {
            assert!(parse_launch_intent(bad).is_err());
        }
    }

    #[test]
    fn arma_monitoring_uses_only_literal_root_fields_and_bounded_port_defaults() {
        assert_eq!(
            arma_monitoring_config("hostname = \"Fixture\"; steamQueryPort = 27018;", 2302),
            ArmaMonitoringConfig {
                hostname: Some("Fixture".into()),
                query_port: Some(27018),
            }
        );
        assert_eq!(
            arma_monitoring_config("hostname = \"Fixture\";", 2302),
            ArmaMonitoringConfig {
                hostname: Some("Fixture".into()),
                query_port: Some(2303),
            }
        );
        for source in [
            "hostname = \"Fixture\"; steamQueryPort = 0;",
            "hostname = \"Fixture\"; steamQueryPort = QUERY_PORT;",
            "hostname = SERVER_NAME; steamQueryPort = 27018;",
            "hostname = \"Fixture\"; hostname = \"Other\"; steamQueryPort = 27018;",
        ] {
            let parsed = arma_monitoring_config(source, 2302);
            assert!(parsed.query_port.is_none() || parsed.hostname.is_none());
        }
    }

    #[test]
    fn only_an_explicit_nonzero_battleye_setting_requests_battleye() {
        assert!(battleye_requested("BattlEye = 1;"));
        assert!(battleye_requested("battleye = 2;"));
        assert!(!battleye_requested("BattlEye = 0;"));
        assert!(!battleye_requested("hostname = \"A\";"));
        assert!(battleye_requested("BattlEye = 1; BattlEye = 0;"));
    }

    #[test]
    fn mod_paths_reject_delimiters_and_traversal() {
        assert!(relative_mod("foxy-content/RepoA").is_ok());
        for bad in [
            "../outside",
            "mods; -profiles=elsewhere",
            "/absolute",
            "mods\\..\\outside",
        ] {
            assert!(relative_mod(bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn argument_paths_strip_windows_verbatim_prefix_and_reject_quotes() {
        assert_eq!(
            argument_path(Path::new(r"\\?\R:\Mods\runtime\engine")).unwrap(),
            if cfg!(windows) {
                r"R:\Mods\runtime\engine"
            } else {
                "R:/Mods/runtime/engine"
            }
        );
        assert!(argument_path(Path::new("bad\"path")).is_err());
    }

    #[test]
    fn state_marker_must_match_instance_space_and_context() {
        let directory = tempfile::tempdir().unwrap();
        let marker = RuntimeStateMarker {
            schema_version: 1,
            instance_id: ServerInstanceId::try_from(
                "server_00000000000000000000000000000001".to_owned(),
            )
            .unwrap(),
            game_space_id: "arma3".into(),
            context_fingerprint: "a".repeat(64),
        };
        fs::write(
            directory.path().join(MARKER),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        let control = ContentStageControl::default();
        assert!(read_marker(directory.path(), &marker, &control).is_ok());
        let mut foreign = marker;
        foreign.game_space_id = "other".into();
        assert_eq!(
            read_marker(directory.path(), &foreign, &control),
            Err(ServerStoreError::Ownership)
        );
    }

    #[test]
    fn state_publication_is_reusable_and_preserves_profile_data() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("runtime-state");
        let marker = RuntimeStateMarker {
            schema_version: 1,
            instance_id: ServerInstanceId::try_from(
                "server_00000000000000000000000000000001".to_owned(),
            )
            .unwrap(),
            game_space_id: "arma3".into(),
            context_fingerprint: "a".repeat(64),
        };
        let control = ContentStageControl::default();
        prepare_state_tree(&root, &marker, &control).unwrap();
        fs::write(root.join("home/profiles/saved.bin"), b"retained").unwrap();
        prepare_state_tree(&root, &marker, &control).unwrap();
        assert_eq!(
            fs::read(root.join("home/profiles/saved.bin")).unwrap(),
            b"retained"
        );
        let mut foreign = marker;
        foreign.context_fingerprint = "b".repeat(64);
        assert_eq!(
            prepare_state_tree(&root, &foreign, &control),
            Err(ServerStoreError::Ownership)
        );
        assert_eq!(
            fs::read(root.join("home/profiles/saved.bin")).unwrap(),
            b"retained"
        );
    }

    #[test]
    fn cancelled_state_preparation_creates_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("runtime-state");
        let marker = RuntimeStateMarker {
            schema_version: 1,
            instance_id: ServerInstanceId::try_from(
                "server_00000000000000000000000000000001".to_owned(),
            )
            .unwrap(),
            game_space_id: "arma3".into(),
            context_fingerprint: "a".repeat(64),
        };
        let control = ContentStageControl::default();
        control.cancel();
        assert_eq!(
            prepare_state_tree(&root, &marker, &control),
            Err(ServerStoreError::Cancelled)
        );
        assert!(!root.exists());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}
