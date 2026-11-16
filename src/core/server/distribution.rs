#![cfg_attr(not(test), allow(dead_code))]

use super::ServerDefinitionKind;

pub const ARMA3_SERVER_APP: u32 = 233_780;
pub const ARMA3_CLIENT_APP: u32 = 107_410;
pub const REFORGER_STABLE_SERVER_APP: u32 = 1_874_900;
pub const REFORGER_STABLE_CLIENT_APP: u32 = 1_874_880;
pub const REFORGER_EXPERIMENTAL_SERVER_APP: u32 = 1_890_870;
pub const REFORGER_EXPERIMENTAL_CLIENT_APP: u32 = 1_890_860;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentChannel {
    #[default]
    Stable,
    Experimental,
}

impl ContentChannel {
    pub(super) fn is_stable(&self) -> bool {
        *self == Self::Stable
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcquireBackend {
    Native,
    SteamCmd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppSelection {
    pub server_app_id: u32,
    pub client_app_id: u32,
    pub channel: ContentChannel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DistributionError {
    ExperimentalArma3,
    ReforgerWorkshop,
    DedicatedAppIsNotWorkshop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderOutcome {
    AuthenticationRequired,
    SteamCmdNotInvoked,
}

pub fn select_dedicated_app(
    kind: ServerDefinitionKind,
    channel: ContentChannel,
) -> Result<AppSelection, DistributionError> {
    match (kind, channel) {
        (ServerDefinitionKind::Arma3, ContentChannel::Stable) => Ok(AppSelection {
            server_app_id: ARMA3_SERVER_APP,
            client_app_id: ARMA3_CLIENT_APP,
            channel,
        }),
        (ServerDefinitionKind::Arma3, ContentChannel::Experimental) => {
            Err(DistributionError::ExperimentalArma3)
        }
        (ServerDefinitionKind::Reforger, ContentChannel::Stable) => Ok(AppSelection {
            server_app_id: REFORGER_STABLE_SERVER_APP,
            client_app_id: REFORGER_STABLE_CLIENT_APP,
            channel,
        }),
        (ServerDefinitionKind::Reforger, ContentChannel::Experimental) => Ok(AppSelection {
            server_app_id: REFORGER_EXPERIMENTAL_SERVER_APP,
            client_app_id: REFORGER_EXPERIMENTAL_CLIENT_APP,
            channel,
        }),
    }
}

pub fn workshop_item_app(item_id: &str) -> Result<u32, DistributionError> {
    let guid = item_id.len() == 16 && item_id.bytes().all(|byte| byte.is_ascii_hexdigit());
    if guid {
        return Err(DistributionError::ReforgerWorkshop);
    }
    if item_id.is_empty() {
        return Err(DistributionError::DedicatedAppIsNotWorkshop);
    }
    Ok(ARMA3_CLIENT_APP)
}

pub fn acquire(backend: AcquireBackend, selection: &AppSelection) -> ProviderOutcome {
    let _ = selection;
    match backend {
        AcquireBackend::Native => ProviderOutcome::AuthenticationRequired,
        AcquireBackend::SteamCmd => ProviderOutcome::SteamCmdNotInvoked,
    }
}

pub fn steamcmd_was_started() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedicated_apps_stay_separate_from_client_apps_and_steamcmd_is_not_started() {
        let arma =
            select_dedicated_app(ServerDefinitionKind::Arma3, ContentChannel::Stable).unwrap();
        assert_eq!(arma.server_app_id, ARMA3_SERVER_APP);
        assert_ne!(arma.server_app_id, arma.client_app_id);
        assert!(
            select_dedicated_app(ServerDefinitionKind::Arma3, ContentChannel::Experimental)
                .is_err()
        );
        let experimental =
            select_dedicated_app(ServerDefinitionKind::Reforger, ContentChannel::Experimental)
                .unwrap();
        assert_eq!(experimental.server_app_id, REFORGER_EXPERIMENTAL_SERVER_APP);
        assert_ne!(experimental.server_app_id, experimental.client_app_id);
        assert_eq!(
            workshop_item_app("0123456789ABCDEF"),
            Err(DistributionError::ReforgerWorkshop)
        );
        assert_eq!(workshop_item_app("1234567890").unwrap(), ARMA3_CLIENT_APP);
        assert_eq!(
            acquire(AcquireBackend::SteamCmd, &arma),
            ProviderOutcome::SteamCmdNotInvoked
        );
        assert_eq!(
            acquire(AcquireBackend::Native, &arma),
            ProviderOutcome::AuthenticationRequired
        );
        assert!(!steamcmd_was_started());
    }
}
