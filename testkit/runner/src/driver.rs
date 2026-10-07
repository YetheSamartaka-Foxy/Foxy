use crate::launch::{self, Environment};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::Path, time::Duration};

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub command: String,
    pub view: String,
    pub elapsed_ms: u128,
    pub data: Value,
    pub errors: Vec<DriverError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DriverError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Logs {
    pub generation: u64,
    pub entries: Vec<LogEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub level: String,
    pub message: String,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// Save the raw envelope when `FOXY_TESTKIT_DRIVER_CAPTURE` names a directory,
/// so `tests/driver-corpus/` can be refreshed from real runs rather than from
/// the kit's own idea of the protocol.
fn capture(command: &str, envelope: &Value) {
    let Some(directory) = std::env::var_os("FOXY_TESTKIT_DRIVER_CAPTURE") else {
        return;
    };
    let directory = Path::new(&directory);
    if std::fs::create_dir_all(directory).is_err() {
        return;
    }
    let name = format!(
        "{}.json",
        command
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>()
    );
    if let Ok(text) = serde_json::to_string_pretty(envelope) {
        let _ = std::fs::write(directory.join(name), text);
    }
}

pub fn call(
    exe: &Path,
    config: &Path,
    args: &[String],
    env: &Environment,
    timeout: Duration,
) -> Result<Response> {
    let arguments: Vec<_> = std::iter::once("agent-gui".to_owned())
        .chain(args.iter().cloned())
        .collect();
    let envelope = launch::foxy(exe, config, &arguments, env, timeout)?;
    let response = decode(&envelope["data"])?;
    capture(&response.command, &envelope["data"]);
    ensure!(response.ok, "Driver command failed: {:?}", response.errors);
    Ok(response)
}

pub fn decode(payload: &Value) -> Result<Response> {
    let response: Response = serde_json::from_value(payload.clone())
        .context("Foxy agent-gui response no longer matches the driver contract")?;
    if response.command == "logs" {
        let _: Logs = serde_json::from_value(response.data.clone())
            .context("Driver log payload no longer matches the contract")?;
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_incomplete_driver_response() {
        assert!(serde_json::from_value::<Response>(serde_json::json!({"data":{}})).is_err());
    }

    // The kit mirrors the driver types rather than importing them, so a Foxy
    // change that breaks the wire contract must fail here instead of compiling.
    #[test]
    fn every_captured_driver_response_still_deserializes() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/driver-corpus");
        let entries: Vec<_> = std::fs::read_dir(&directory)
            .expect("driver corpus directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect();
        assert!(
            !entries.is_empty(),
            "driver corpus is empty; capture responses with FOXY_TESTKIT_DRIVER_CAPTURE"
        );
        for path in entries {
            let payload: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            decode(&payload).unwrap_or_else(|error| panic!("{}: {error:#}", path.display()));
        }
    }
}
