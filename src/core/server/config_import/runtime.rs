use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::parser::{self, CfgValue, Node, NodeKind};
use crate::core::server::{ContentStageControl, ServerInstanceId, ServerStore, ServerStoreError};

const MAX_CONFIG_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct NativeConfigurationSeed {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub blake3: String,
    pub bindings: Vec<(String, String)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reforger_session: Option<crate::core::server::ReforgerSessionLaunch>,
}

pub(in crate::core::server) fn private_configuration_name(
    runtime: &str,
    session: Option<&crate::core::server::ReforgerSessionLaunch>,
) -> String {
    match session
        .filter(|session| session.intent != crate::core::server::ReforgerSessionIntent::Configured)
    {
        Some(session) => format!(
            "{runtime}_session_{}_{}",
            u8::from(session.load),
            u8::from(session.keep)
        ),
        None => runtime.to_owned(),
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RuntimeStateMarker {
    schema_version: u32,
    instance_id: ServerInstanceId,
    game_space_id: String,
    context_fingerprint: String,
}

impl ServerStore {
    pub(in crate::core::server) fn prepare_secret_configuration(
        &self,
        id: &ServerInstanceId,
        seeds: &[NativeConfigurationSeed],
        control: &ContentStageControl,
    ) -> Result<(), ServerStoreError> {
        control.checkpoint()?;
        self.validate_record_context()?;
        if seeds.is_empty() || seeds.len() > 3 {
            return Err(ServerStoreError::Invalid(
                "Invalid runtime configuration set".into(),
            ));
        }
        let record_root = self.record_root(id);
        let state_root = record_root.join("runtime-state");
        validate_state_marker(self, id, &state_root, control)?;
        let mut prepared = Vec::new();
        let mut runtime_name = None;
        let mut names: Vec<String> = Vec::new();
        for seed in seeds {
            control.checkpoint()?;
            let (runtime, filename) = validate_source(self, id, &record_root, &seed.source)?;
            let runtime = private_configuration_name(runtime, seed.reforger_session.as_ref());
            if runtime_name
                .as_deref()
                .is_some_and(|old| old != runtime.as_str())
                || names.iter().any(|name| name == filename)
            {
                return Err(ServerStoreError::UnsafePath);
            }
            runtime_name = Some(runtime.clone());
            names.push(filename.to_owned());
            let expected_destination = state_root
                .join("home/private-configuration")
                .join(&runtime)
                .join(filename);
            if seed.destination != expected_destination {
                return Err(ServerStoreError::UnsafePath);
            }
            let expected_hash = parse_hash(&seed.blake3)?;
            crate::core::server::store::check_regular_file(&seed.source)?;
            let source = crate::core::server::content_staging::read_bounded_controlled(
                &seed.source,
                MAX_CONFIG_BYTES,
                control,
            )?;
            if blake3::hash(&source) != expected_hash {
                return Err(ServerStoreError::StaleRevision);
            }
            let source_text =
                std::str::from_utf8(&source).map_err(|_| ServerStoreError::Corrupt)?;
            let rendered = if filename == "server.json" {
                if !seed.bindings.is_empty() {
                    return Err(ServerStoreError::Corrupt);
                }
                if let Some(session) = &seed.reforger_session {
                    let current = self.reforger_sessions(id, control)?;
                    if current.fingerprint != session.inventory_fingerprint {
                        return Err(ServerStoreError::StaleRevision);
                    }
                }
                let rendered = render_json_secrets(self, id, source_text)?;
                render_reforger_session(&rendered, seed.reforger_session.as_ref())?
            } else {
                if seed.reforger_session.is_some() {
                    return Err(ServerStoreError::Corrupt);
                }
                render_secret_values(self, id, source_text, &seed.bindings, control)?
            };
            prepared.push((filename.to_owned(), rendered));
        }
        let runtime = runtime_name.ok_or(ServerStoreError::Corrupt)?;
        publish_private_config(self, id, &state_root, &runtime, &prepared, control)
    }
}

fn validate_source<'a>(
    store: &ServerStore,
    id: &ServerInstanceId,
    record_root: &Path,
    source: &'a Path,
) -> Result<(&'a str, &'a str), ServerStoreError> {
    let relative = source
        .strip_prefix(record_root)
        .map_err(|_| ServerStoreError::UnsafePath)?;
    let parts: Vec<_> = relative.components().collect();
    if parts.len() != 4
        || parts[0].as_os_str() != "runtimes"
        || parts[2].as_os_str() != "configuration"
    {
        return Err(ServerStoreError::UnsafePath);
    }
    let runtime = parts[1]
        .as_os_str()
        .to_str()
        .ok_or(ServerStoreError::UnsafePath)?;
    let filename = parts[3]
        .as_os_str()
        .to_str()
        .ok_or(ServerStoreError::UnsafePath)?;
    if !runtime.strip_prefix("runtime_").is_some_and(is_hex32)
        || !portable_config_name(filename)
        || store.record_root(id) != record_root
    {
        return Err(ServerStoreError::UnsafePath);
    }
    crate::core::server::store::check_directory_chain(
        source.parent().ok_or(ServerStoreError::UnsafePath)?,
    )?;
    Ok((runtime, filename))
}

fn render_secret_values(
    store: &ServerStore,
    id: &ServerInstanceId,
    source: &str,
    bindings: &[(String, String)],
    control: &ContentStageControl,
) -> Result<Vec<u8>, ServerStoreError> {
    let document = parser::parse(source).map_err(|_| ServerStoreError::Corrupt)?;
    if document.unsupported || has_opaque(&document.nodes) {
        return Err(ServerStoreError::Corrupt);
    }
    let mut assignments = Vec::new();
    collect_assignments(&document.nodes, &mut assignments);
    let mut replacements = Vec::new();
    for node in assignments
        .into_iter()
        .filter(|node| secret_name(&node.name))
    {
        control.checkpoint()?;
        let NodeKind::Assignment {
            value: CfgValue::String(value),
            value_span,
            append: false,
            array: false,
        } = &node.kind
        else {
            return Err(ServerStoreError::Corrupt);
        };
        let reference = if let Some(reference) = value.strip_prefix("foxy-secret:") {
            reference
        } else if value == "<redacted secret reference>" {
            let key = node.name.to_ascii_lowercase();
            let Some((_, reference)) = bindings
                .iter()
                .find(|(name, _)| name.to_ascii_lowercase() == key)
            else {
                return Err(ServerStoreError::Corrupt);
            };
            reference.as_str()
        } else if value.is_empty() {
            continue;
        } else {
            return Err(ServerStoreError::Corrupt);
        };
        let secret = store.resolve_config_secret(id, reference)?;
        replacements.push((
            value_span.clone(),
            crate::core::server::arma3_config::quoted(&secret),
        ));
    }
    parser::replace_spans(source, &replacements)
        .map(String::into_bytes)
        .map_err(|_| ServerStoreError::Corrupt)
}

fn render_json_secrets(
    store: &ServerStore,
    id: &ServerInstanceId,
    source: &str,
) -> Result<Vec<u8>, ServerStoreError> {
    replace_rcon_password(source, |reference| {
        store.resolve_config_secret(id, reference)
    })
}

fn replace_rcon_password(
    source: &str,
    resolve: impl FnOnce(&str) -> Result<String, ServerStoreError>,
) -> Result<Vec<u8>, ServerStoreError> {
    let mut value: serde_json::Value =
        serde_json::from_str(source).map_err(|_| ServerStoreError::Corrupt)?;
    if value.get("rcon").is_none() {
        return serde_json::to_vec_pretty(&value).map_err(|_| ServerStoreError::Corrupt);
    }
    let password = value
        .get_mut("rcon")
        .and_then(|rcon| rcon.get_mut("password"))
        .ok_or(ServerStoreError::Corrupt)?;
    let reference = password
        .as_str()
        .and_then(|text| text.strip_prefix("foxy-secret:"))
        .filter(|reference| crate::core::server::rcon::valid_secret_reference(reference))
        .ok_or(ServerStoreError::Corrupt)?
        .to_owned();
    let secret = zeroize::Zeroizing::new(resolve(&reference)?);
    *password = serde_json::Value::String(secret.to_string());
    let bytes = serde_json::to_vec_pretty(&value).map_err(|_| ServerStoreError::Corrupt)?;
    if let Some(serde_json::Value::String(text)) = value
        .get_mut("rcon")
        .and_then(|rcon| rcon.get_mut("password"))
    {
        zeroize::Zeroize::zeroize(text);
    }
    Ok(bytes)
}

fn render_reforger_session(
    bytes: &[u8],
    session: Option<&crate::core::server::ReforgerSessionLaunch>,
) -> Result<Vec<u8>, ServerStoreError> {
    let Some(session) = session else {
        return Ok(bytes.to_vec());
    };
    if session.intent == crate::core::server::ReforgerSessionIntent::Configured {
        return Ok(bytes.to_vec());
    }
    let mut config: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| ServerStoreError::Corrupt)?;
    let game = config
        .get_mut("game")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or(ServerStoreError::Corrupt)?;
    let properties = game
        .entry("gameProperties")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or(ServerStoreError::Corrupt)?;
    let persistence = properties
        .entry("persistence")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or(ServerStoreError::Corrupt)?;
    persistence.insert("loadSessionSave".into(), session.load.into());
    persistence.insert("keepSessionSave".into(), session.keep.into());
    serde_json::to_vec_pretty(&config).map_err(|_| ServerStoreError::Corrupt)
}

fn collect_assignments<'a>(nodes: &'a [Node], found: &mut Vec<&'a Node>) {
    for node in nodes {
        match &node.kind {
            NodeKind::Class { children, .. } => collect_assignments(children, found),
            NodeKind::Assignment { .. } => found.push(node),
        }
    }
}

fn has_opaque(nodes: &[Node]) -> bool {
    nodes.iter().any(|node| match &node.kind {
        NodeKind::Class { children, .. } => has_opaque(children),
        NodeKind::Assignment { value, .. } => has_opaque_value(value),
    })
}

fn has_opaque_value(value: &CfgValue) -> bool {
    match value {
        CfgValue::Opaque(_) => true,
        CfgValue::Array(values) => values.iter().any(has_opaque_value),
        _ => false,
    }
}

fn secret_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("password") || name.contains("secret") || name.contains("token")
}

pub(super) fn validate_state_marker(
    store: &ServerStore,
    id: &ServerInstanceId,
    root: &Path,
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    crate::core::server::store::check_directory_chain(root)?;
    let marker_path = root.join("ownership.json");
    crate::core::server::store::check_regular_file(&marker_path)?;
    let bytes =
        crate::core::server::content_staging::read_bounded_controlled(&marker_path, 8192, control)?;
    let marker: RuntimeStateMarker =
        serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
    if marker
        != (RuntimeStateMarker {
            schema_version: 1,
            instance_id: id.clone(),
            game_space_id: store.context().space_id().to_owned(),
            context_fingerprint: store.activation_context_fingerprint(),
        })
    {
        return Err(ServerStoreError::Ownership);
    }
    Ok(())
}

fn publish_private_config(
    store: &ServerStore,
    id: &ServerInstanceId,
    state_root: &Path,
    runtime: &str,
    files: &[(String, Vec<u8>)],
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    let base = state_root.join("home/private-configuration");
    let destination = base.join(runtime);
    crate::core::server::store::check_directory_chain(&base)?;
    create_protected_directory(&base)?;
    check_private_dir(&base)?;
    if destination.exists() {
        return verify_existing(store, id, state_root, runtime, files, control);
    }
    let mut builder = tempfile::Builder::new();
    builder.prefix(".private-config-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let temp = builder
        .tempdir_in(&base)
        .map_err(|_| ServerStoreError::Io)?;
    let payload = temp.path().join("payload");
    create_protected_directory(&payload)?;
    let owner = PrivateConfigurationMarker {
        schema_version: 1,
        game_space_id: store.context().space_id().to_owned(),
        instance_id: id.clone(),
        context_fingerprint: store.activation_context_fingerprint(),
        runtime_id: runtime.to_owned(),
    };
    let owner_bytes = serde_json::to_vec(&owner).map_err(|_| ServerStoreError::Corrupt)?;
    write_private_file(&payload.join("ownership.json"), &owner_bytes)?;
    for (name, bytes) in files {
        control.checkpoint()?;
        let path = payload.join(name);
        write_private_file(&path, bytes)?;
    }
    control.checkpoint()?;
    match crate::core::server::content_staging::publish_no_replace(&payload, &destination) {
        Ok(true) => verify_existing(store, id, state_root, runtime, files, control),
        Ok(false) if destination.exists() => {
            verify_existing(store, id, state_root, runtime, files, control)
        }
        Ok(false) => Err(ServerStoreError::Ownership),
        Err(error) => Err(error),
    }
}

fn verify_existing(
    store: &ServerStore,
    id: &ServerInstanceId,
    state_root: &Path,
    runtime: &str,
    files: &[(String, Vec<u8>)],
    control: &ContentStageControl,
) -> Result<(), ServerStoreError> {
    let directory = state_root.join("home/private-configuration").join(runtime);
    crate::core::server::store::check_directory_chain(&directory)?;
    foxy_supervisor::control::handoff::ProtectedDirectory::open(
        directory.parent().ok_or(ServerStoreError::UnsafePath)?,
    )
    .map_err(|_| ServerStoreError::UnsafePath)?;
    foxy_supervisor::control::handoff::ProtectedDirectory::open(&directory)
        .map_err(|_| ServerStoreError::UnsafePath)?;
    check_private_dir(directory.parent().ok_or(ServerStoreError::UnsafePath)?)?;
    check_private_dir(&directory)?;
    let marker_bytes = crate::core::server::content_staging::read_bounded_controlled(
        &directory.join("ownership.json"),
        8192,
        control,
    )?;
    check_private_file(&directory.join("ownership.json"))?;
    let marker: PrivateConfigurationMarker =
        serde_json::from_slice(&marker_bytes).map_err(|_| ServerStoreError::Corrupt)?;
    if marker != private_marker(store, id, runtime) {
        return Err(ServerStoreError::Ownership);
    }
    let expected_names: std::collections::HashSet<_> = files
        .iter()
        .map(|(name, _)| name.as_str())
        .chain(std::iter::once("ownership.json"))
        .collect();
    let actual_names: std::collections::HashSet<_> = fs::read_dir(&directory)
        .map_err(|_| ServerStoreError::Io)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_, _>>()
        .map_err(|_| ServerStoreError::Io)?;
    if actual_names.len() != expected_names.len()
        || actual_names
            .iter()
            .any(|name| !expected_names.contains(name.as_str()))
    {
        return Err(ServerStoreError::Ownership);
    }
    for (name, expected) in files {
        control.checkpoint()?;
        let path = directory.join(name);
        crate::core::server::store::check_regular_file(&path)?;
        check_private_file(&path)?;
        let actual = crate::core::server::content_staging::read_bounded_controlled(
            &path,
            MAX_CONFIG_BYTES,
            control,
        )?;
        if actual != *expected {
            return Err(ServerStoreError::Ownership);
        }
    }
    if !directory.is_dir() || store.record_root(id).join("runtime-state") != state_root {
        return Err(ServerStoreError::UnsafePath);
    }
    Ok(())
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PrivateConfigurationMarker {
    schema_version: u32,
    game_space_id: String,
    instance_id: ServerInstanceId,
    context_fingerprint: String,
    runtime_id: String,
}

fn private_marker(
    store: &ServerStore,
    id: &ServerInstanceId,
    runtime: &str,
) -> PrivateConfigurationMarker {
    PrivateConfigurationMarker {
        schema_version: 1,
        game_space_id: store.context().space_id().to_owned(),
        instance_id: id.clone(),
        context_fingerprint: store.activation_context_fingerprint(),
        runtime_id: runtime.to_owned(),
    }
}

pub(super) fn check_private_dir(path: &Path) -> Result<(), ServerStoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ServerStoreError::UnsafePath)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ServerStoreError::UnsafePath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o700 {
            return Err(ServerStoreError::UnsafePath);
        }
    }
    Ok(())
}

pub(super) fn create_protected_directory(path: &Path) -> Result<(), ServerStoreError> {
    foxy_supervisor::control::handoff::ProtectedDirectory::create(path)
        .map(drop)
        .map_err(|_| ServerStoreError::UnsafePath)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), ServerStoreError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).map_err(|_| ServerStoreError::Io)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| ServerStoreError::Io)
}

fn check_private_file(path: &Path) -> Result<(), ServerStoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ServerStoreError::UnsafePath)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(ServerStoreError::UnsafePath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o777 != 0o600
            || metadata.nlink() != 1
        {
            return Err(ServerStoreError::UnsafePath);
        }
    }
    Ok(())
}

fn parse_hash(value: &str) -> Result<blake3::Hash, ServerStoreError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(ServerStoreError::Invalid(
            "Invalid configuration hash".into(),
        ));
    }
    blake3::Hash::from_hex(value)
        .map_err(|_| ServerStoreError::Invalid("Invalid configuration hash".into()))
}

fn is_hex32(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn portable_config_name(value: &str) -> bool {
    if value == "server.json" {
        return true;
    }
    value.len() <= 128
        && value.ends_with(".cfg")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        && value != ".cfg"
        && !value.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reforger_rcon_password_references_resolve_only_their_own_value() {
        let reference = format!("secret_{}", "d".repeat(32));
        let source = format!(
            r#"{{"bindPort":2001,"rcon":{{"address":"127.0.0.1","port":19999,"password":"foxy-secret:{reference}","permission":"monitor"}},"game":{{"name":"A"}}}}"#
        );
        let rendered = replace_rcon_password(&source, |asked| {
            assert_eq!(asked, reference);
            Ok("synthetic-pass".into())
        })
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&rendered).unwrap();
        assert_eq!(value["rcon"]["password"], "synthetic-pass");
        assert_eq!(value["rcon"]["port"], 19999);
        assert_eq!(value["game"]["name"], "A");
        for bad in [
            r#"{"rcon":{"password":"plain"}}"#.to_string(),
            r#"{"rcon":{"port":1}}"#.to_string(),
            r#"{"rcon":{"password":"foxy-secret:secret_x"}}"#.to_string(),
            "not json".to_string(),
        ] {
            assert!(
                replace_rcon_password(&bad, |_| Ok("x".into())).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn explicit_session_intent_overrides_load_defaults_without_rewriting_source() {
        use crate::core::server::{
            ReforgerProviderCoverage, ReforgerSessionIntent, ReforgerSessionLaunch,
        };
        let source = br#"{"game":{"scenarioId":"{1111111111111111}Missions/Main.conf","gameProperties":{"persistence":{"loadSessionSave":true,"hiveId":7,"saveRetention":10},"custom":42}},"operatorField":"preserved"}"#;
        let mut session = ReforgerSessionLaunch {
            intent: ReforgerSessionIntent::New,
            selected_save_id: None,
            playthrough: None,
            savepoint: None,
            load: false,
            keep: true,
            fingerprint: "a".repeat(64),
            inventory_fingerprint: "b".repeat(64),
            provider: ReforgerProviderCoverage {
                local_default: true,
                blockers: vec![],
            },
        };
        let value: serde_json::Value =
            serde_json::from_slice(&render_reforger_session(source, Some(&session)).unwrap())
                .unwrap();
        assert_eq!(
            value["game"]["gameProperties"]["persistence"]["loadSessionSave"],
            false
        );
        assert_eq!(
            value["game"]["gameProperties"]["persistence"]["keepSessionSave"],
            true
        );
        assert_eq!(value["game"]["gameProperties"]["persistence"]["hiveId"], 7);
        assert_eq!(value["game"]["gameProperties"]["custom"], 42);
        assert_eq!(value["operatorField"], "preserved");
        assert_eq!(
            private_configuration_name("runtime_a", Some(&session)),
            "runtime_a_session_0_1"
        );
        session.intent = ReforgerSessionIntent::Configured;
        assert_eq!(
            private_configuration_name("runtime_a", Some(&session)),
            "runtime_a"
        );
        assert_eq!(
            render_reforger_session(source, Some(&session)).unwrap(),
            source
        );
        session.intent = ReforgerSessionIntent::Latest;
        session.load = true;
        assert_eq!(
            private_configuration_name("runtime_a", Some(&session)),
            "runtime_a_session_1_1"
        );
        let disabled = br#"{"game":{"gameProperties":{"persistence":{"loadSessionSave":false}}}}"#;
        let value: serde_json::Value =
            serde_json::from_slice(&render_reforger_session(disabled, Some(&session)).unwrap())
                .unwrap();
        assert_eq!(
            value["game"]["gameProperties"]["persistence"]["loadSessionSave"],
            true
        );
        let no_rcon =
            replace_rcon_password(r#"{"game":{}}"#, |_| panic!("No RCon reference exists"))
                .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&no_rcon).unwrap(),
            serde_json::json!({"game":{}})
        );
    }

    #[test]
    fn protected_configuration_uses_private_home_beneath_a_legacy_runtime_root() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("games.json"),
            include_bytes!("../../../../examples/json/appdata/games.json"),
        )
        .unwrap();
        let store = ServerStore::new(
            crate::core::server::ServerContext::capture(root.path(), Some("arma3")).unwrap(),
        )
        .unwrap();
        let id = ServerInstanceId::new();
        let state = store.record_root(&id).join("runtime-state");
        fs::create_dir_all(&state).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(state.join("home"))
                .unwrap();
        }
        #[cfg(not(unix))]
        create_protected_directory(&state.join("home")).unwrap();
        let runtime = format!("runtime_{}", "a".repeat(32));
        let files = vec![("server.cfg".into(), b"hostname=\"Fixture\";".to_vec())];
        let control = ContentStageControl::default();
        publish_private_config(&store, &id, &state, &runtime, &files, &control).unwrap();
        let group = state.join("home/private-configuration").join(&runtime);
        assert_eq!(fs::read(group.join("server.cfg")).unwrap(), files[0].1);
        publish_private_config(&store, &id, &state, &runtime, &files, &control).unwrap();
        write_private_file(&group.join("unknown.cfg"), b"retained").unwrap();
        assert!(publish_private_config(&store, &id, &state, &runtime, &files, &control).is_err());
        assert_eq!(fs::read(group.join("unknown.cfg")).unwrap(), b"retained");
    }
    use crate::core::server::ServerContext;
    use serde_json::json;

    fn store(root: &Path) -> (ServerStore, ServerInstanceId) {
        fs::write(
            root.join("games.json"),
            serde_json::to_vec(&json!({
                "active_game_space_id": "alpha",
                "last_opened_game_space_id": "alpha",
                "game_spaces": [{"id":"alpha", "game_id":"arma3", "display_name":"Alpha", "created_at":0}]
            })).unwrap(),
        ).unwrap();
        let store = ServerStore::new(ServerContext::capture(root, Some("alpha")).unwrap()).unwrap();
        let id = ServerInstanceId::new();
        (store, id)
    }

    #[test]
    fn source_and_name_scope_are_strict() {
        assert!(portable_config_name("server.cfg"));
        assert!(!portable_config_name("../server.cfg"));
        assert!(!portable_config_name("server.Arma3Profile"));
        assert!(is_hex32(&"a".repeat(32)));
        assert!(!is_hex32(&"A".repeat(32)));
    }

    #[test]
    fn source_scope_accepts_the_verified_runtime_configuration_layout() {
        let temp = tempfile::tempdir().unwrap();
        let (store, id) = store(temp.path());
        let record = store.record_root(&id);
        let runtime = format!("runtime_{}", "a".repeat(32));
        let config_dir = record.join("runtimes").join(&runtime).join("configuration");
        fs::create_dir_all(&config_dir).unwrap();
        let source = config_dir.join("server.cfg");
        fs::write(&source, b"password=\"\";").unwrap();
        assert_eq!(
            validate_source(&store, &id, &record, &source).unwrap(),
            (runtime.as_str(), "server.cfg")
        );
        assert!(
            validate_source(
                &store,
                &id,
                &record,
                &record.join("runtimes").join(&runtime).join("server.cfg")
            )
            .is_err()
        );
    }

    #[test]
    fn parser_replaces_only_known_secret_assignment_values() {
        let text = "password = \"foxy-secret:secret_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\";";
        let doc = parser::parse(text).unwrap();
        let mut found = Vec::new();
        collect_assignments(&doc.nodes, &mut found);
        let NodeKind::Assignment { value_span, .. } = &found[0].kind else {
            panic!()
        };
        assert_eq!(
            parser::replace_spans(text, &[(value_span.clone(), "\"replacement\"".into())]).unwrap(),
            "password = \"replacement\";"
        );
        assert!(parser::parse("password = tokenValue;").is_ok());
        assert!(portable_config_name("a.cfg"));
        let opaque = parser::parse("password = call(secret); ").unwrap();
        assert!(has_opaque(&opaque.nodes));
    }

    #[test]
    fn source_digest_is_a_strict_lowercase_blake3_hash() {
        assert!(parse_hash(blake3::hash(b"synthetic").to_hex().as_ref()).is_ok());
        assert!(parse_hash(&"A".repeat(64)).is_err());
        assert!(parse_hash("../not-a-hash").is_err());
    }
}
