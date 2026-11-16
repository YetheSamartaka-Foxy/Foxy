use super::{
    fields::{Arma3FieldKind as K, Arma3ListKind as L},
    optional_fields,
};
use serde_json::Number;

const TIMESTAMPS: &[(&str, &str)] = &[("none", "None"), ("short", "Short"), ("full", "Full")];
const U32: u64 = u32::MAX as u64;

optional_fields!(Arma3AntiFlood {
    cycle_time: Number => ("cycleTime", K::Decimal { min: f64::MIN_POSITIVE, max: f64::MAX }, "Flood cycle (seconds)", 0),
    cycle_limit: u32 => ("cycleLimit", K::Integer { min: 1, max: U32 }, "Flood messages per cycle", 0),
    cycle_hard_limit: u32 => ("cycleHardLimit", K::Integer { min: 1, max: U32 }, "Flood hard message limit", 0),
    enable_kick: bool => ("enableKick", K::Bool, "Kick for flooding", 0),
});

optional_fields!(Arma3AdvancedOptions {
    log_object_not_found: bool => ("logObjectNotFound", K::Bool, "Log missing objects", 0),
    skip_description_parsing: bool => ("skipDescriptionParsing", K::Bool, "Skip mission description parsing", 0),
    ignore_mission_load_errors: bool => ("ignoreMissionLoadErrors", K::Bool, "Ignore mission load errors", 0),
    queue_size_log_g: u32 => ("queueSizeLogG", K::Integer { min: 0, max: U32 }, "Guaranteed queue diagnostic threshold (bytes)", 0),
});

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum Arma3VoteCommand {
    SideThreshold((String, bool, bool, Number, Number)),
    Threshold((String, bool, bool, Number)),
    Default((String, bool, bool)),
}

optional_fields!(Arma3ServerSettings {
    password_ref: String => ("password", K::SecretRef, "Join password reference", 0),
    password_admin_ref: String => ("passwordAdmin", K::SecretRef, "Admin password reference", 0),
    server_command_password_ref: String => ("serverCommandPassword", K::SecretRef, "Server command password reference", 0),
    admins: Vec<String> => ("admins", K::Strings(L::Uids), "Administrator Steam IDs", 0),
    motd: Vec<String> => ("motd", K::Strings(L::Motd), "Welcome messages", 0),
    motd_interval: Number => ("motdInterval", K::Decimal { min: 0.0, max: f64::MAX }, "Welcome message interval (seconds)", 0),
    kick_duplicate: bool => ("kickDuplicate", K::Bool, "Reject duplicate game IDs", 0),
    required_build: u32 => ("requiredBuild", K::Integer { min: 0, max: U32 }, "Minimum client build", 0),
    allowed_file_patching: u8 => ("allowedFilePatching", K::Integer { min: 0, max: 2 }, "File patching: 0 none, 1 headless clients, 2 all clients", 150),
    file_patching_exceptions: Vec<String> => ("filePatchingExceptions", K::Strings(L::Uids), "File patching Steam ID exceptions", 0),
    allowed_load_file_extensions: Vec<String> => ("allowedLoadFileExtensions", K::Strings(L::Extensions), "Allowed loadFile extensions", 0),
    allowed_preprocess_file_extensions: Vec<String> => ("allowedPreprocessFileExtensions", K::Strings(L::Extensions), "Allowed preprocessFile extensions", 0),
    allowed_html_load_extensions: Vec<String> => ("allowedHTMLLoadExtensions", K::Strings(L::Extensions), "Allowed HTML extensions", 0),
    allowed_html_load_uris: Vec<String> => ("allowedHTMLLoadURIs", K::Strings(L::Urls), "Allowed HTML URLs", 0),
    headless_clients: Vec<String> => ("headlessClients", K::Strings(L::Ipv4), "Allowed headless client IPv4 addresses", 0),
    local_client: Vec<String> => ("localClient", K::Strings(L::Ipv4), "Trusted unlimited-bandwidth IPv4 clients", 0),
    vote_threshold: Number => ("voteThreshold", K::Decimal { min: 0.0, max: 1.0 }, "Required vote fraction", 0),
    vote_mission_players: u32 => ("voteMissionPlayers", K::Integer { min: 0, max: U32 }, "Players required for mission voting", 0),
    allowed_vote_cmds: Vec<Arma3VoteCommand> => ("allowedVoteCmds", K::VoteCommands, "Player vote commands", 0),
    allowed_voted_admin_cmds: Vec<(String, bool, bool)> => ("allowedVotedAdminCmds", K::VotedAdminCommands, "Voted administrator commands", 0),
    loopback: bool => ("loopback", K::Bool, "LAN-only server", 0),
    upnp: bool => ("upnp", K::Bool, "Let the engine request UPnP mapping", 0),
    steam_protocol_max_data_size: u32 => ("steamProtocolMaxDataSize", K::Integer { min: 1, max: 65507 }, "Steam query maximum packet (bytes)", 0),
    disconnect_timeout: u8 => ("disconnectTimeout", K::Integer { min: 5, max: 90 }, "Disconnect timeout (seconds)", 156),
    max_desync: i32 => ("maxDesync", K::Integer { min: -1, max: i32::MAX as u64 }, "Maximum desync (-1 disables)", 0),
    max_ping: i32 => ("maxPing", K::Integer { min: -1, max: i32::MAX as u64 }, "Maximum ping (milliseconds, -1 disables)", 0),
    max_packet_loss: i32 => ("maxPacketLoss", K::Integer { min: -1, max: i32::MAX as u64 }, "Maximum packet loss (-1 disables)", 0),
    kick_clients_on_slow_network: [bool; 4] => ("kickClientsOnSlowNetwork", K::SlowNetwork, "Kick instead of log for ping, loss, desync, timeout", 0),
    kick_timeout: Vec<(u8, i32)> => ("kickTimeout", K::KickTimeouts, "Rejoin delay after a kick", 0),
    lobby_idle_timeout: u32 => ("lobbyIdleTimeout", K::Integer { min: 0, max: U32 }, "Lobby idle timeout (seconds)", 0),
    role_timeout: [u32; 2] => ("roleTimeOut", K::Pair, "Role selection timeout (ready / not ready seconds)", 0),
    briefing_timeout: [u32; 2] => ("briefingTimeOut", K::Pair, "Briefing timeout (ready / not ready seconds)", 0),
    voting_timeout: [u32; 2] => ("votingTimeOut", K::Pair, "Voting timeout (ready / not ready seconds)", 0),
    debriefing_timeout: [u32; 2] => ("debriefingTimeOut", K::Pair, "Debriefing timeout (ready / not ready seconds)", 0),
    arma_units_timeout: u32 => ("armaUnitsTimeout", K::Integer { min: 0, max: U32 }, "Arma Units timeout (seconds)", 0),
    skip_lobby: bool => ("skipLobby", K::Bool, "Skip lobby unless overridden by mission", 0),
    forced_difficulty: super::super::Arma3Difficulty => ("forcedDifficulty", K::Difficulty, "Forced difficulty (mission cycle takes precedence)", 0),
    drawing_in_map: bool => ("drawingInMap", K::Bool, "Allow map drawing", 0),
    force_rotor_lib_simulation: u8 => ("forceRotorLibSimulation", K::Integer { min: 0, max: 2 }, "Flight model: 0 player choice, 1 advanced, 2 standard", 0),
    allow_profile_glasses: bool => ("allowProfileGlasses", K::Bool, "Allow profile glasses", 0),
    zeus_composition_script_level: u8 => ("zeusCompositionScriptLevel", K::Integer { min: 0, max: 2 }, "Zeus composition scripts: 0 none, 1 attributes, 2 all", 0),
    override_haze_quality: i8 => ("overrideHazeQuality", K::Integer { min: -1, max: 2 }, "Haze quality override: -1 none, 0 to 2 quality", 0),
    disable_von: bool => ("disableVoN", K::Bool, "Enable voice communication", 0),
    von_codec: u8 => ("vonCodec", K::Integer { min: 0, max: 1 }, "Voice codec: 0 SPEEX, 1 OPUS", 158),
    von_codec_quality: u8 => ("vonCodecQuality", K::Integer { min: 1, max: 30 }, "Voice codec quality", 164),
    disable_channels: Vec<(u8, bool, bool)> => ("disableChannels", K::Channels, "Channel restrictions", 0),
    log_file: String => ("logFile", K::Text, "Engine console log filename", 0),
    timestamp_format: String => ("timeStampFormat", K::Choice(TIMESTAMPS), "Report timestamp format", 0),
    timestamp_format_console: String => ("timeStampFormatConsole", K::Choice(TIMESTAMPS), "Console timestamp format", 222),
    enable_player_diag: bool => ("enablePlayerDiag", K::Bool, "Log player network diagnostics", 0),
    call_ext_report_limit: u32 => ("callExtReportLimit", K::Integer { min: 0, max: U32 }, "Extension warning duration (milliseconds)", 0),
    statistics_enabled: bool => ("statisticsEnabled", K::Bool, "Enable engine analytics", 0),
    idle_fps_limit: u8 => ("idleFPSLimit", K::Integer { min: 5, max: 60 }, "Idle server FPS limit", 222),
    mission_whitelist: Vec<String> => ("missionWhitelist", K::Strings(L::Missions), "Mission whitelist", 0),
    auto_select_mission: bool => ("autoSelectMission", K::Bool, "Select next mission automatically", 0),
    random_mission_order: bool => ("randomMissionOrder", K::Bool, "Random mission order", 0),
    missions_to_server_restart: u32 => ("missionsToServerRestart", K::Integer { min: 0, max: U32 }, "Mission ends before engine restart", 0),
    missions_to_shutdown: u32 => ("missionsToShutdown", K::Integer { min: 0, max: U32 }, "Mission ends before shutdown", 0),
    mission_http_download_base_url: String => ("missionHTTPDownloadBaseURL", K::Text, "Mission download base URL", 220),
    double_id_detected: String => ("doubleIdDetected", K::Text, "Duplicate ID script hook", 0),
    on_user_connected: String => ("onUserConnected", K::Text, "User connected script hook", 0),
    on_user_disconnected: String => ("onUserDisconnected", K::Text, "User disconnected script hook", 0),
    on_user_kicked: String => ("onUserKicked", K::Text, "User kicked script hook", 0),
    on_hacked_data: String => ("onHackedData", K::Text, "Hacked data script hook", 0),
    on_unsigned_data: String => ("onUnsignedData", K::Text, "Unsigned data script hook", 0),
    anti_flood: Box<Arma3AntiFlood> => ("AntiFlood", K::Section(Arma3AntiFlood::FIELDS), "Flood protection", 0),
    advanced_options: Box<Arma3AdvancedOptions> => ("AdvancedOptions", K::Section(Arma3AdvancedOptions::FIELDS), "Advanced engine options", 0),
});

impl Arma3ServerSettings {
    pub fn validate_relationships(&self) -> Result<(), String> {
        self.validate()?;
        if self.missions_to_server_restart.unwrap_or(0) > 0
            && self.missions_to_shutdown.unwrap_or(0) > 0
        {
            return Err(
                "Engine restart and shutdown mission counters are mutually exclusive".into(),
            );
        }
        if let Some(flood) = &self.anti_flood
            && let (Some(limit), Some(hard)) = (flood.cycle_limit, flood.cycle_hard_limit)
            && limit > hard
        {
            return Err("Flood cycle limit cannot exceed the hard limit".into());
        }
        if let Some(name) = &self.log_file {
            portable_name(name, None)?;
        }
        if let Some(url) = &self.mission_http_download_base_url
            && !super::fields::http_url(url)
        {
            return Err("Mission download URL must be HTTP(S) without embedded credentials".into());
        }
        Ok(())
    }

    pub fn has_credentials(&self) -> bool {
        self.password_ref.is_some()
            || self.password_admin_ref.is_some()
            || self.server_command_password_ref.is_some()
    }
}

pub const ARMA3_DEFAULT_SERVER_CONFIG: &str = "server.cfg";
pub const ARMA3_DEFAULT_BASIC_CONFIG: &str = "basic.cfg";
pub const ARMA3_DEFAULT_PROFILE: &str = "FoxyServer";

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arma3ConfigurationNames {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_name: Option<String>,
}

impl Arma3ConfigurationNames {
    pub fn is_empty(&self) -> bool {
        self.server.is_none() && self.basic.is_none() && self.profile_name.is_none()
    }

    pub fn server_file(&self) -> &str {
        self.server
            .as_deref()
            .unwrap_or(ARMA3_DEFAULT_SERVER_CONFIG)
    }

    pub fn basic_file(&self) -> &str {
        self.basic.as_deref().unwrap_or(ARMA3_DEFAULT_BASIC_CONFIG)
    }

    pub fn profile_name(&self) -> &str {
        self.profile_name
            .as_deref()
            .unwrap_or(ARMA3_DEFAULT_PROFILE)
    }

    pub fn validate(&self) -> Result<(), String> {
        portable_name(self.server_file(), Some("cfg"))?;
        portable_name(self.basic_file(), Some("cfg"))?;
        if self.server_file().eq_ignore_ascii_case(self.basic_file()) {
            return Err("Server and basic configuration filenames must differ".into());
        }
        if !profile_name_valid(self.profile_name()) {
            return Err(
                "Profile name must use 1-24 ASCII letters, digits, underscores or hyphens".into(),
            );
        }
        Ok(())
    }
}

pub(crate) fn profile_name_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 24
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        && portable_name(name, None).is_ok()
}

fn portable_name(name: &str, extension: Option<&str>) -> Result<(), String> {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9')
            })
        });
    if name.is_empty()
        || name.len() > 64
        || name.starts_with('.')
        || name.ends_with('.')
        || name.contains("..")
        || reserved
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        || extension.is_some_and(|ext| !name.to_ascii_lowercase().ends_with(&format!(".{ext}")))
    {
        return Err("Configuration and log filenames must be portable single filenames".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames_and_profile_names_are_portable_and_distinct() {
        let mut names = Arma3ConfigurationNames::default();
        assert!(names.validate().is_ok());
        assert_eq!(names.server_file(), "server.cfg");
        assert_eq!(names.profile_name(), "FoxyServer");
        names.server = Some("Basic.CFG".into());
        names.basic = Some("basic.cfg".into());
        assert!(names.validate().is_err());
        for bad in [
            "../server.cfg",
            "server.txt",
            "CON.cfg",
            "dir/server.cfg",
            ".cfg",
        ] {
            names = Arma3ConfigurationNames {
                server: Some(bad.into()),
                ..Default::default()
            };
            assert!(names.validate().is_err(), "{bad}");
        }
        assert!(profile_name_valid("A_1-z"));
        assert!(profile_name_valid("123456789012345678901234"));
        assert!(!profile_name_valid("1234567890123456789012345"));
        assert!(!profile_name_valid("LPT1"));
        assert!(!profile_name_valid("name.with.dot"));
    }

    #[test]
    fn relationship_checks_cover_counters_flood_limits_and_urls() {
        let restart_and_shutdown = Arma3ServerSettings {
            missions_to_server_restart: Some(2),
            missions_to_shutdown: Some(3),
            ..Default::default()
        };
        assert!(restart_and_shutdown.validate_relationships().is_err());
        let flood = Arma3ServerSettings {
            anti_flood: Some(Box::new(Arma3AntiFlood {
                cycle_limit: Some(20),
                cycle_hard_limit: Some(10),
                ..Default::default()
            })),
            ..Default::default()
        };
        assert!(flood.validate_relationships().is_err());
        let url = Arma3ServerSettings {
            mission_http_download_base_url: Some("ftp://repo.example.invalid/".into()),
            ..Default::default()
        };
        assert!(url.validate_relationships().is_err());
        let log = Arma3ServerSettings {
            log_file: Some("../server.log".into()),
            ..Default::default()
        };
        assert!(log.validate_relationships().is_err());
    }
}
