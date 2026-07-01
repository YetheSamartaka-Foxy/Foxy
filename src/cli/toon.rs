use serde_json::Value;
use toon_format::{DecodeOptions, EncodeOptions, ToonError};

pub fn encode(value: &Value) -> Result<String, ToonError> {
    toon_format::encode(value, &encode_options())
}

pub fn decode(input: &str) -> Result<Value, ToonError> {
    toon_format::decode(input, &decode_options())
}

fn encode_options() -> EncodeOptions {
    EncodeOptions::default()
}

fn decode_options() -> DecodeOptions {
    DecodeOptions::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_round_trip(value: Value) {
        let encoded = encode(&value).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, value);
    }

    #[test]
    fn round_trips_snapshot_shape() {
        assert_round_trip(serde_json::json!({
            "view": "repository-settings",
            "fps": 59.75,
            "nodes": [
                {
                    "id": "footer.settings",
                    "role": "button",
                    "text": "Settings",
                    "enabled": true,
                    "rect": { "x": 12.5, "y": -4.25, "w": 100.5, "h": 28.25 }
                }
            ],
            "pointer": null
        }));
    }

    #[test]
    fn round_trips_uniform_arrays_and_u64_numbers() {
        assert_round_trip(serde_json::json!({
            "tab": "external-addons",
            "total": 3,
            "returned": 3,
            "addons": [
                { "name": "@ace", "enabled": true, "kind": "external", "size_bytes": 1234567890123_u64, "source": "github" },
                { "name": "@cba_a3", "enabled": true, "kind": "external", "size_bytes": 7890_u64, "source": "github" },
                { "name": "@tfar", "enabled": false, "kind": "external", "size_bytes": 4567_u64, "source": "steam" }
            ]
        }));
    }

    #[test]
    fn integer_like_coordinates_decode_as_usable_numbers() {
        let value = serde_json::json!({ "rect": { "x": 100.0, "y": 28.0 } });
        let encoded = encode(&value).unwrap();
        let decoded = decode(&encoded).unwrap();

        assert_eq!(decoded["rect"]["x"].as_f64(), value["rect"]["x"].as_f64());
        assert_eq!(decoded["rect"]["y"].as_f64(), value["rect"]["y"].as_f64());
    }

    #[test]
    fn round_trips_logs_with_delimiters_newlines_and_unicode() {
        assert_round_trip(serde_json::json!({
            "generation": 42,
            "entries": [
                {
                    "level": "warn",
                    "source": "ui",
                    "message": "comma, quote \" and newline\nDaja: Prilis zlutoucky kun"
                },
                {
                    "level": "info",
                    "source": "i18n",
                    "message": "Čeština bez otazniku"
                }
            ]
        }));
    }

    #[test]
    fn round_trips_empty_values() {
        assert_round_trip(serde_json::json!({
            "empty_array": [],
            "empty_object": {},
            "null_value": null
        }));
    }
}
