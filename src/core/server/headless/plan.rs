use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    HEADLESS_LOOPBACK, HeadlessLifecyclePolicy, HeadlessProcessOverrides, MAX_HEADLESS_CLIENTS,
    valid_headless_id,
};
use crate::core::server::ServerStoreError;
use crate::core::server::config_import::parser::{self, CfgValue, NodeKind};

pub(in crate::core::server) const PARAMETER_FILE: &str = "parameters.txt";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeHeadlessClient {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mods: Vec<String>,
    #[serde(default, skip_serializing_if = "HeadlessProcessOverrides::is_empty")]
    pub process: HeadlessProcessOverrides,
    #[serde(default, skip_serializing_if = "HeadlessLifecyclePolicy::is_default")]
    pub lifecycle: HeadlessLifecyclePolicy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HeadlessLaunchPlan {
    pub id: String,
    pub name: String,
    pub executable: PathBuf,
    pub directory: PathBuf,
    pub arguments: Vec<String>,
    pub environment: Vec<(String, String)>,
    pub state_root: PathBuf,
    pub profiles: PathBuf,
    pub join_password: bool,
    #[serde(skip_serializing_if = "HeadlessLifecyclePolicy::is_default")]
    pub lifecycle: HeadlessLifecyclePolicy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameter_file: Option<PathBuf>,
    #[serde(skip)]
    pub(in crate::core::server) password_reference: Option<String>,
}

pub(in crate::core::server) struct HeadlessPlanInputs<'a> {
    pub executable: &'a Path,
    pub directory: &'a Path,
    pub state_root: &'a Path,
    pub game_port: u16,
    pub password_reference: Option<String>,
}

pub(in crate::core::server) fn headless_launch_plans(
    clients: &[NativeHeadlessClient],
    inputs: HeadlessPlanInputs<'_>,
    argument_path: impl Fn(&Path) -> Result<String, ServerStoreError>,
    environment: impl Fn(&Path) -> Result<Vec<(String, String)>, ServerStoreError>,
    relative_mod: impl Fn(&str) -> Result<(), ServerStoreError>,
) -> Result<Vec<HeadlessLaunchPlan>, ServerStoreError> {
    if clients.len() > MAX_HEADLESS_CLIENTS || inputs.game_port == 0 {
        return Err(ServerStoreError::Corrupt);
    }
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    let mut plans = Vec::with_capacity(clients.len());
    for client in clients {
        if !valid_headless_id(&client.id)
            || !ids.insert(client.id.as_str())
            || !crate::core::server::profile_name_valid(&client.name)
            || !names.insert(client.name.to_ascii_lowercase())
        {
            return Err(ServerStoreError::Corrupt);
        }
        client
            .process
            .validate()
            .map_err(|_| ServerStoreError::Corrupt)?;
        client
            .lifecycle
            .validate()
            .map_err(|_| ServerStoreError::Corrupt)?;
        let state_root = inputs.state_root.join("headless").join(&client.id);
        let profiles = state_root.join("home/profiles");
        let mut arguments = vec![
            "-client".to_owned(),
            format!("-connect={HEADLESS_LOOPBACK}"),
            format!("-port={}", inputs.game_port),
            format!("-profiles={}", argument_path(&profiles)?),
            format!("-name={}", client.name),
        ];
        if !client.mods.is_empty() {
            for item in &client.mods {
                relative_mod(item)?;
            }
            arguments.push(format!("-mod={}", client.mods.join(";")));
        }
        arguments.extend(client.process.arguments());
        let parameter_file = inputs
            .password_reference
            .as_ref()
            .map(|_| state_root.join("private").join(PARAMETER_FILE));
        if let Some(path) = &parameter_file {
            arguments.push(format!("-par={}", argument_path(path)?));
        }
        plans.push(HeadlessLaunchPlan {
            id: client.id.clone(),
            name: client.name.clone(),
            executable: inputs.executable.to_path_buf(),
            directory: inputs.directory.to_path_buf(),
            arguments,
            environment: environment(&state_root)?,
            profiles,
            join_password: parameter_file.is_some(),
            lifecycle: client.lifecycle.clone(),
            parameter_file,
            password_reference: inputs.password_reference.clone(),
            state_root,
        });
    }
    Ok(plans)
}

pub(in crate::core::server) fn join_password_reference(
    source: &str,
    bindings: &[(String, String)],
) -> Result<Option<String>, ServerStoreError> {
    let unreadable = || {
        ServerStoreError::Invalid(
            "Headless clients need a server configuration whose join password Foxy can read; this committed configuration uses unsupported syntax.".into(),
        )
    };
    let document = parser::parse(source).map_err(|_| unreadable())?;
    if document.unsupported {
        return Err(unreadable());
    }
    let mut matching = document
        .nodes
        .iter()
        .filter(|node| node.name.eq_ignore_ascii_case("password"));
    let Some(node) = matching.next() else {
        return Ok(None);
    };
    if matching.next().is_some() {
        return Err(unreadable());
    }
    let NodeKind::Assignment {
        value: CfgValue::String(value),
        array: false,
        append: false,
        ..
    } = &node.kind
    else {
        return Err(unreadable());
    };
    if value.is_empty() {
        return Ok(None);
    }
    if let Some(reference) = value.strip_prefix("foxy-secret:") {
        return Ok(Some(reference.to_owned()));
    }
    if value == "<redacted secret reference>" {
        return bindings
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("password"))
            .map(|(_, reference)| Some(reference.clone()))
            .ok_or(ServerStoreError::Corrupt);
    }
    Err(ServerStoreError::Corrupt)
}
