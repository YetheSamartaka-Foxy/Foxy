use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReforgerScenarioEnd {
    Shutdown { delay_seconds: u16 },
    Reload { delay_seconds: u16 },
    Disabled,
}

impl Default for ReforgerScenarioEnd {
    fn default() -> Self {
        Self::Shutdown { delay_seconds: 0 }
    }
}

impl ReforgerScenarioEnd {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Shutdown { delay_seconds } | Self::Reload { delay_seconds }
                if *delay_seconds > 3600 =>
            {
                Err("Scenario-end delay must be 0-3600 seconds.".into())
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn arguments(&self) -> Vec<String> {
        match self {
            Self::Shutdown { delay_seconds } => vec![
                "-autoreload".into(),
                delay_seconds.to_string(),
                "-autoshutdown".into(),
            ],
            Self::Reload { delay_seconds } => vec!["-autoreload".into(), delay_seconds.to_string()],
            Self::Disabled => vec![
                "-autoreload".into(),
                "disabled".into(),
                "-autoshutdown".into(),
            ],
        }
    }

    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_end_modes_keep_shutdown_distinct_from_same_process_reload() {
        assert_eq!(
            ReforgerScenarioEnd::default().arguments(),
            ["-autoreload", "0", "-autoshutdown"]
        );
        assert_eq!(
            ReforgerScenarioEnd::Reload { delay_seconds: 5 }.arguments(),
            ["-autoreload", "5"]
        );
        assert_eq!(
            ReforgerScenarioEnd::Disabled.arguments(),
            ["-autoreload", "disabled", "-autoshutdown"]
        );
        assert!(
            ReforgerScenarioEnd::Reload {
                delay_seconds: 3601
            }
            .validate()
            .is_err()
        );
        assert!(
            serde_json::from_str::<ReforgerScenarioEnd>(
                r#"{"mode":"reload","delay_seconds":5,"restart_process":true}"#
            )
            .is_err()
        );
    }
}
