mod evidence;
mod plan;
mod runtime;
mod schedule;
mod status;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub(in crate::core::server) use evidence::JoinEvidence;
pub use plan::{HeadlessLaunchPlan, NativeHeadlessClient};
pub(in crate::core::server) use plan::{
    HeadlessPlanInputs, headless_launch_plans, join_password_reference,
};
pub(in crate::core::server) use runtime::{HeadlessReadiness, prepare_headless_state};
pub(in crate::core::server) use schedule::{ClientSchedule, Due, ExitDecision};
pub(in crate::core::server) use status::SharedHeadlessStatus;
pub use status::{
    HeadlessClientState, HeadlessClientStatus, HeadlessConnection, HeadlessExit,
    HeadlessGroupStatus, HeadlessReason,
};

pub const MAX_HEADLESS_CLIENTS: usize = 32;
pub const HEADLESS_LOOPBACK: &str = "127.0.0.1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arma3HeadlessClient {
    pub id: String,
    pub name: String,
    pub order: u32,
    #[serde(default = "enabled", skip_serializing_if = "is_enabled")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "HeadlessAddonSelection::is_default")]
    pub addons: HeadlessAddonSelection,
    #[serde(default, skip_serializing_if = "HeadlessProcessOverrides::is_empty")]
    pub process: HeadlessProcessOverrides,
    #[serde(default, skip_serializing_if = "HeadlessLifecyclePolicy::is_default")]
    pub lifecycle: HeadlessLifecyclePolicy,
}

pub const HEADLESS_RESTART_WINDOW_SECONDS: u64 = 600;
pub const HEADLESS_DEFAULT_CONNECT_TIMEOUT_SECONDS: u16 = 120;
pub const HEADLESS_DEFAULT_MAX_RESTARTS: u8 = 3;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeadlessLifecyclePolicy {
    #[serde(default = "enabled", skip_serializing_if = "is_enabled")]
    pub auto_start: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub start_delay_seconds: u16,
    #[serde(
        default = "default_connect_timeout",
        skip_serializing_if = "is_default_connect_timeout"
    )]
    pub connect_timeout_seconds: u16,
    #[serde(
        default = "default_max_restarts",
        skip_serializing_if = "is_default_max_restarts"
    )]
    pub max_restarts: u8,
}

impl Default for HeadlessLifecyclePolicy {
    fn default() -> Self {
        Self {
            auto_start: true,
            start_delay_seconds: 0,
            connect_timeout_seconds: HEADLESS_DEFAULT_CONNECT_TIMEOUT_SECONDS,
            max_restarts: HEADLESS_DEFAULT_MAX_RESTARTS,
        }
    }
}

impl HeadlessLifecyclePolicy {
    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.start_delay_seconds > 600 {
            return Err("Headless client start delay must be 0-600 seconds".into());
        }
        if !(15..=900).contains(&self.connect_timeout_seconds) {
            return Err("Headless client join timeout must be 15-900 seconds".into());
        }
        if self.max_restarts > 10 {
            return Err("Headless client automatic restarts must be 0-10".into());
        }
        Ok(())
    }

    pub(crate) fn restart_delay(previous_restarts: usize) -> std::time::Duration {
        let exponent = u32::try_from(previous_restarts.min(4)).unwrap_or(4);
        std::time::Duration::from_secs((5u64 << exponent).min(60))
    }
}

fn default_connect_timeout() -> u16 {
    HEADLESS_DEFAULT_CONNECT_TIMEOUT_SECONDS
}

fn is_default_connect_timeout(value: &u16) -> bool {
    *value == HEADLESS_DEFAULT_CONNECT_TIMEOUT_SECONDS
}

fn default_max_restarts() -> u8 {
    HEADLESS_DEFAULT_MAX_RESTARTS
}

fn is_default_max_restarts(value: &u8) -> bool {
    *value == HEADLESS_DEFAULT_MAX_RESTARTS
}

fn is_zero(value: &u16) -> bool {
    *value == 0
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeadlessAddonSelection {
    #[serde(default = "enabled", skip_serializing_if = "is_enabled")]
    pub inherit_required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

impl Default for HeadlessAddonSelection {
    fn default() -> Self {
        Self {
            inherit_required: true,
            include: Vec::new(),
            exclude: Vec::new(),
        }
    }
}

impl HeadlessAddonSelection {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeadlessProcessOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_count: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ex_threads: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_mem: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_fps: Option<u16>,
}

impl HeadlessProcessOverrides {
    pub(crate) fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self
            .cpu_count
            .is_some_and(|value| !(1..=256).contains(&value))
        {
            return Err("Headless client CPU count must be 1-256".into());
        }
        if self
            .ex_threads
            .is_some_and(|value| ![0, 1, 3, 5, 7].contains(&value))
        {
            return Err("Headless client extra threads must be 0, 1, 3, 5 or 7".into());
        }
        if self
            .max_mem
            .is_some_and(|value| !(256..=262_144).contains(&value))
        {
            return Err("Headless client memory limit must be 256-262144 MB".into());
        }
        if self
            .limit_fps
            .is_some_and(|value| !(5..=1000).contains(&value))
        {
            return Err("Headless client FPS limit must be 5-1000".into());
        }
        Ok(())
    }

    pub(crate) fn arguments(&self) -> Vec<String> {
        let mut arguments = Vec::new();
        if let Some(value) = self.cpu_count {
            arguments.push(format!("-cpuCount={value}"));
        }
        if let Some(value) = self.ex_threads {
            arguments.push(format!("-exThreads={value}"));
        }
        if let Some(value) = self.max_mem {
            arguments.push(format!("-maxMem={value}"));
        }
        if let Some(value) = self.limit_fps {
            arguments.push(format!("-limitFPS={value}"));
        }
        arguments
    }
}

fn enabled() -> bool {
    true
}

fn is_enabled(value: &bool) -> bool {
    *value
}

pub fn new_headless_id() -> String {
    format!("hc_{:016x}", rand::random::<u64>())
}

pub(crate) fn valid_headless_id(value: &str) -> bool {
    value.strip_prefix("hc_").is_some_and(|suffix| {
        suffix.len() == 16
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn valid_source_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn unique_sources(values: &[String], label: &str) -> Result<HashSet<String>, String> {
    let mut seen = HashSet::new();
    for value in values {
        if !valid_source_id(value) {
            return Err(format!("Invalid headless client {label} source id"));
        }
        if !seen.insert(value.to_ascii_lowercase()) {
            return Err(format!("Duplicate headless client {label} source id"));
        }
    }
    Ok(seen)
}

pub fn validate_headless_clients(
    clients: &[Arma3HeadlessClient],
    admitted: Option<&[String]>,
) -> Result<(), String> {
    if clients.len() > MAX_HEADLESS_CLIENTS {
        return Err(format!(
            "Arma 3 accepts at most {MAX_HEADLESS_CLIENTS} headless client definitions"
        ));
    }
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    for client in clients {
        if !valid_headless_id(&client.id) || !ids.insert(client.id.as_str()) {
            return Err("Invalid or duplicate headless client id".into());
        }
        if !super::profile_name_valid(&client.name)
            || !names.insert(client.name.to_ascii_lowercase())
        {
            return Err("Headless client names must be unique profile names of 1-24 letters, digits, underscores or hyphens".into());
        }
        let include = unique_sources(&client.addons.include, "include")?;
        let exclude = unique_sources(&client.addons.exclude, "exclude")?;
        if !client.addons.inherit_required && !exclude.is_empty() {
            return Err(
                "Headless client exclusions apply only to inherited required addons".into(),
            );
        }
        if include.intersection(&exclude).next().is_some() {
            return Err("A headless client cannot both include and exclude one source".into());
        }
        client.process.validate()?;
        client.lifecycle.validate()?;
    }
    if clients.iter().any(|client| client.enabled)
        && let Some(admitted) = admitted
        && !admitted.iter().any(|address| address == HEADLESS_LOOPBACK)
    {
        return Err(format!(
            "Local headless clients connect from {HEADLESS_LOOPBACK}; add it to headlessClients[] or leave that list unset"
        ));
    }
    Ok(())
}

pub fn ordered_headless_clients(clients: &[Arma3HeadlessClient]) -> Vec<&Arma3HeadlessClient> {
    let mut ordered: Vec<_> = clients.iter().collect();
    ordered.sort_by(|left, right| (left.order, &left.id).cmp(&(right.order, &right.id)));
    ordered
}

pub(crate) fn selects(selection: &HeadlessAddonSelection, source_id: &str, required: bool) -> bool {
    let named = |values: &[String]| {
        values
            .iter()
            .any(|value| value.eq_ignore_ascii_case(source_id))
    };
    named(&selection.include)
        || (required && selection.inherit_required && !named(&selection.exclude))
}

pub(crate) fn consumer_instance_id(instance_id: &str, client_id: &str) -> String {
    format!("{instance_id}-{client_id}")
}

pub(crate) fn consumer_belongs_to(consumer_instance: &str, instance_id: &str) -> bool {
    consumer_instance == instance_id
        || consumer_instance
            .strip_prefix(instance_id)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(valid_headless_id)
}
