use std::net::Ipv4Addr;
use std::time::Instant;

use zeroize::Zeroizing;

use crate::core::server::owner::RconSessionStatus;
use crate::core::server::rcon::{RconCommandRecord, RconDraftSettings, RconSettingsInput};
use crate::core::server::{ReforgerRconPermission, ServerInstanceId};

pub(crate) const RECENT_COMMANDS: usize = 10;

#[derive(Clone, Debug)]
pub(crate) struct RconSnapshot {
    pub instance_id: ServerInstanceId,
    pub draft_revision: u64,
    pub reforger: bool,
    pub draft: Option<RconDraftSettings>,
    pub password_available: bool,
    pub run_generation: Option<u64>,
    pub session: Option<RconSessionStatus>,
    pub session_note: Option<String>,
    pub recent: Vec<RconCommandRecord>,
    pub unreadable: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RconNotice {
    SettingsSaved,
    Disabled,
    PasswordStored,
    TestReplied { round_trip_ms: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RconConfirmation {
    Disable { revision: u64 },
    Send { text: String, run_generation: u64 },
}

pub(crate) struct RconPasswordEntry {
    pub base_revision: u64,
    pub value: Zeroizing<String>,
    pub error: Option<String>,
}

#[derive(Default)]
pub(crate) struct RconViewState {
    pub snapshot: Option<RconSnapshot>,
    pub form: Option<RconForm>,
    pub password: Option<RconPasswordEntry>,
    pub command_text: String,
    pub confirmation: Option<RconConfirmation>,
    pub focus_confirmation: bool,
    pub focus_form: bool,
    pub focus_password: bool,
    pub notice: Option<RconNotice>,
    pub last_command: Option<RconCommandRecord>,
    pub last_refresh: Option<Instant>,
    pub load_error: Option<String>,
    pub action_error: Option<String>,
}

impl RconViewState {
    pub fn for_instance(&self, id: &ServerInstanceId) -> Option<&RconSnapshot> {
        self.snapshot
            .as_ref()
            .filter(|snapshot| &snapshot.instance_id == id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RconForm {
    /// The draft revision the form was opened against.
    pub base_revision: u64,
    pub reforger: bool,
    pub port: u16,
    pub address: String,
    pub permission: ReforgerRconPermission,
    /// Zero leaves the engine default.
    pub max_clients: u8,
    pub error: Option<String>,
}

impl RconForm {
    pub fn open(snapshot: &RconSnapshot) -> Self {
        let draft = snapshot.draft.as_ref();
        Self {
            base_revision: snapshot.draft_revision,
            reforger: snapshot.reforger,
            port: draft
                .map(|settings| settings.port)
                .unwrap_or(if snapshot.reforger { 19999 } else { 2310 }),
            address: draft
                .map(|settings| settings.address.to_string())
                .unwrap_or_else(|| Ipv4Addr::LOCALHOST.to_string()),
            permission: draft
                .and_then(|settings| settings.permission)
                .unwrap_or(ReforgerRconPermission::Monitor),
            max_clients: draft.and_then(|settings| settings.max_clients).unwrap_or(0),
            error: None,
        }
    }

    pub fn build(&self) -> Result<RconSettingsInput, &'static str> {
        let address: Ipv4Addr = self
            .address
            .trim()
            .parse()
            .map_err(|_| "Enter an IPv4 address such as 127.0.0.1.")?;
        if self.port == 0 {
            return Err("Enter an RCon port from 1 to 65535.");
        }
        if self.max_clients > 16 {
            return Err("Reforger allows 1 to 16 RCon clients.");
        }
        Ok(RconSettingsInput {
            port: self.port,
            address,
            permission: self.reforger.then_some(self.permission),
            max_clients: (self.reforger && self.max_clients > 0).then_some(self.max_clients),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(reforger: bool) -> RconSnapshot {
        RconSnapshot {
            instance_id: ServerInstanceId::try_from(format!("server_{}", "1".repeat(32))).unwrap(),
            draft_revision: 4,
            reforger,
            draft: None,
            password_available: false,
            run_generation: None,
            session: None,
            session_note: None,
            recent: Vec::new(),
            unreadable: 0,
        }
    }

    #[test]
    fn a_new_form_defaults_to_loopback_and_the_game_default_port() {
        let arma = RconForm::open(&snapshot(false));
        assert_eq!((arma.port, arma.address.as_str()), (2310, "127.0.0.1"));
        assert_eq!(arma.base_revision, 4);
        let input = arma.build().unwrap();
        assert_eq!((input.permission, input.max_clients), (None, None));
        let mut reforger = RconForm::open(&snapshot(true));
        assert_eq!(reforger.port, 19999);
        reforger.max_clients = 4;
        reforger.permission = ReforgerRconPermission::Admin;
        let input = reforger.build().unwrap();
        assert_eq!(input.permission, Some(ReforgerRconPermission::Admin));
        assert_eq!(input.max_clients, Some(4));
    }

    #[test]
    fn invalid_addresses_ports_and_client_limits_are_refused() {
        let mut form = RconForm::open(&snapshot(true));
        form.address = "localhost".into();
        assert!(form.build().is_err());
        form.address = "127.0.0.1".into();
        form.port = 0;
        assert!(form.build().is_err());
        form.port = 19999;
        form.max_clients = 17;
        assert!(form.build().is_err());
    }
}
