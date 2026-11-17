use crate::core::server::rcon::arma3::ArmaAdminCommand;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ArmaAdminAction {
    #[default]
    Players,
    Bans,
    Missions,
    Announce,
    Kick,
    AddBan,
    RemoveBan,
    Lock,
    Unlock,
    ChangeMission,
    RestartMission,
}

#[derive(Clone, Debug)]
pub(crate) struct ArmaAdminConfirmation {
    pub instance_id: crate::core::server::ServerInstanceId,
    pub command: ArmaAdminCommand,
    pub wire_text: String,
    pub run_generation: u64,
    pub owner_id: String,
    pub owner_generation: u64,
}

#[derive(Default)]
pub(crate) struct ArmaAdminViewState {
    pub action: ArmaAdminAction,
    pub message: String,
    pub reason: String,
    pub guid: String,
    pub target: String,
    pub mission: String,
    pub minutes: String,
    pub confirmation: Option<ArmaAdminConfirmation>,
    pub focus_cancel: bool,
    pub action_error: Option<String>,
    pub last_submitted: Option<String>,
}
