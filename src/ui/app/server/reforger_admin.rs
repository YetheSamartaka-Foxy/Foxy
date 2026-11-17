use crate::core::server::ServerInstanceId;
use crate::core::server::rcon::reforger::ReforgerAdminCommand;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ReforgerAdminAction {
    #[default]
    Players,
    Bans,
    Kick,
    Ban,
    RemoveBan,
    ReloadScenario,
    Logout,
}

#[derive(Clone, Debug)]
pub(crate) struct ReforgerAdminConfirmation {
    pub instance_id: ServerInstanceId,
    pub command: ReforgerAdminCommand,
    pub wire_text: String,
    pub run_generation: u64,
    pub owner_id: String,
    pub owner_generation: u64,
}

#[derive(Default)]
pub(crate) struct ReforgerAdminViewState {
    pub action: ReforgerAdminAction,
    pub page: String,
    pub player_id: String,
    pub identity: String,
    pub duration_seconds: String,
    pub reason: String,
    pub confirmation: Option<ReforgerAdminConfirmation>,
    pub focus_cancel: bool,
    pub action_error: Option<String>,
    pub last_submitted: Option<String>,
}
