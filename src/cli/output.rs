use serde::Serialize;
use serde_json::Value;

use crate::cli::toon;

#[derive(Serialize)]
struct CliEnvelope {
    ok: bool,
    action: String,
    data: Value,
    errors: Vec<String>,
}

pub struct CliOutput {
    pub json: bool,
    pub toon: bool,
    pub quiet: bool,
}

impl CliOutput {
    pub fn success(&self, action: &str, message: &str, data: Value) {
        if self.json {
            let payload = CliEnvelope {
                ok: true,
                action: action.to_string(),
                data,
                errors: Vec::new(),
            };
            if self.toon {
                println!(
                    "{}",
                    envelope_to_toon(&payload).unwrap_or_else(|| {
                        "ok: true\naction: serialization-error\ndata:\nerrors[0]:".to_string()
                    })
                );
                return;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| {
                    "{\"ok\":true,\"action\":\"serialization-error\",\"data\":{},\"errors\":[]}"
                        .to_string()
                })
            );
            return;
        }

        if !self.quiet && !message.trim().is_empty() {
            println!("{}", message);
        }
    }

    pub fn failure(&self, action: &str, message: &str, errors: Vec<String>) {
        if self.json {
            let payload = CliEnvelope {
                ok: false,
                action: action.to_string(),
                data: Value::Null,
                errors,
            };
            if self.toon {
                println!(
                    "{}",
                    envelope_to_toon(&payload).unwrap_or_else(|| {
                        "ok: false\naction: serialization-error\ndata: null\nerrors[1]: Failed to serialize error payload".to_string()
                    })
                );
                return;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| {
                    "{\"ok\":false,\"action\":\"serialization-error\",\"data\":null,\"errors\":[\"Failed to serialize error payload\"]}".to_string()
                })
            );
            return;
        }

        if !action.trim().is_empty() {
            eprintln!("Action: {}", action);
        }
        if !message.trim().is_empty() {
            eprintln!("Error: {}", message);
        }
        for error in errors {
            if error.trim().is_empty() || error.trim() == message.trim() {
                continue;
            }
            eprintln!("Detail: {}", error);
        }
    }
}

fn envelope_to_toon(payload: &CliEnvelope) -> Option<String> {
    let value = serde_json::to_value(payload).ok()?;
    toon::encode(&value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── CliEnvelope serialization ──────────────────────────────────────

    #[test]
    fn cli_envelope_success_serialization() {
        let envelope = CliEnvelope {
            ok: true,
            action: "test-action".to_string(),
            data: serde_json::json!({"key": "value"}),
            errors: Vec::new(),
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["action"], "test-action");
        assert_eq!(parsed["data"]["key"], "value");
        assert!(parsed["errors"].as_array().unwrap().is_empty());
    }

    #[test]
    fn cli_envelope_failure_serialization() {
        let envelope = CliEnvelope {
            ok: false,
            action: "sync".to_string(),
            data: Value::Null,
            errors: vec!["network error".to_string(), "timeout".to_string()],
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["ok"], false);
        assert!(parsed["data"].is_null());
        assert_eq!(parsed["errors"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn cli_envelope_empty_errors_serialize_to_empty_array() {
        let envelope = CliEnvelope {
            ok: true,
            action: "version".to_string(),
            data: serde_json::json!("1.0.0"),
            errors: Vec::new(),
        };
        let json = serde_json::to_string_pretty(&envelope).unwrap();
        assert!(json.contains("\"errors\": []"));
    }

    // ── CliOutput construction ─────────────────────────────────────────

    #[test]
    fn cli_output_json_mode_fields() {
        let output = CliOutput {
            json: true,
            toon: false,
            quiet: false,
        };
        assert!(output.json);
        assert!(!output.quiet);
    }

    #[test]
    fn cli_output_quiet_mode_fields() {
        let output = CliOutput {
            json: false,
            toon: false,
            quiet: true,
        };
        assert!(!output.json);
        assert!(output.quiet);
    }

    #[test]
    fn cli_output_default_mode_fields() {
        let output = CliOutput {
            json: false,
            toon: false,
            quiet: false,
        };
        assert!(!output.json);
        assert!(!output.quiet);
    }

    // ── CliEnvelope round trip ─────────────────────────────────────────

    #[test]
    fn cli_envelope_serde_round_trip_via_value() {
        let envelope = CliEnvelope {
            ok: true,
            action: "repo-sync".to_string(),
            data: serde_json::json!({"synced": 5, "failed": 0}),
            errors: Vec::new(),
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["action"], "repo-sync");
        assert_eq!(parsed["data"]["synced"], 5);
        assert_eq!(parsed["data"]["failed"], 0);
    }

    #[test]
    fn cli_envelope_serializes_to_toon() {
        let envelope = CliEnvelope {
            ok: true,
            action: "agent-gui.addons".to_string(),
            data: serde_json::json!({
                "addons": [
                    { "name": "@ace", "enabled": true },
                    { "name": "@cba_a3", "enabled": false }
                ]
            }),
            errors: Vec::new(),
        };

        let encoded = envelope_to_toon(&envelope).unwrap();
        let decoded = toon::decode(&encoded).unwrap();

        assert_eq!(decoded["ok"], true);
        assert_eq!(decoded["action"], "agent-gui.addons");
        assert_eq!(decoded["data"]["addons"][0]["name"], "@ace");
    }
}
