use std::io::ErrorKind;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppMode {
    #[default]
    Client,
    Server,
}

impl AppMode {
    pub fn effective(self, launch_override: Option<Self>) -> Self {
        launch_override.unwrap_or(self)
    }
}

pub fn read_saved_app_mode(path: &Path) -> Result<AppMode, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(AppMode::default()),
        Err(error) => return Err(format!("Cannot read application mode: {}", error.kind())),
    };
    decode_app_mode(&bytes)
}

fn decode_app_mode(bytes: &[u8]) -> Result<AppMode, String> {
    #[derive(Deserialize)]
    struct ModeSettings {
        #[serde(default)]
        app_mode: AppMode,
    }

    serde_json::from_slice::<ModeSettings>(bytes)
        .map(|settings| settings.app_mode)
        .map_err(|_| "Cannot read application mode: invalid app_settings.json".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_mode_preserves_the_saved_preference_under_session_overrides() {
        for saved in [AppMode::Client, AppMode::Server] {
            assert_eq!(saved.effective(None), saved);
            for session in [AppMode::Client, AppMode::Server] {
                assert_eq!(saved.effective(Some(session)), session);
                assert_eq!(
                    serde_json::to_value(saved).unwrap(),
                    if saved == AppMode::Client {
                        "client"
                    } else {
                        "server"
                    }
                );
            }
        }
    }

    #[test]
    fn bootstrap_reads_only_the_app_mode_and_defaults_old_settings_to_client() {
        assert_eq!(decode_app_mode(br#"{}"#).unwrap(), AppMode::Client);
        assert_eq!(
            decode_app_mode(br#"{"app_mode":"server","locale":"en"}"#).unwrap(),
            AppMode::Server
        );
        assert_eq!(
            decode_app_mode(br#"{"app_mode":"client","app_update_mode":"Server"}"#).unwrap(),
            AppMode::Client
        );
        for invalid in [br#"{"app_mode":"other"}"#.as_slice(), b"{", b"null"] {
            assert!(decode_app_mode(invalid).is_err());
        }
    }

    #[test]
    fn bootstrap_does_not_create_or_rewrite_files() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing").join("app_settings.json");
        assert_eq!(read_saved_app_mode(&missing).unwrap(), AppMode::Client);
        assert!(!missing.parent().unwrap().exists());

        let path = root.path().join("app_settings.json");
        let original = br#"{"app_mode":"server","other":42}"#;
        std::fs::write(&path, original).unwrap();
        assert_eq!(read_saved_app_mode(&path).unwrap(), AppMode::Server);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::write(&path, b"{").unwrap();
        assert!(read_saved_app_mode(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{");
    }
}
