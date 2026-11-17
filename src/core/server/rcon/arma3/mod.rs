//! Typed Arma 3 BattlEye RCon commands and native reply parsing.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

const MAX_WIRE_BYTES: usize = 512;
const MAX_REASON_BYTES: usize = 256;
const MAX_REPLY_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArmaAdminCommand {
    Players,
    Bans,
    Missions,
    Announce {
        message: String,
    },
    Kick {
        player_id: u32,
        expected_guid: String,
        reason: String,
    },
    AddBan {
        guid: String,
        minutes: u32,
        reason: String,
    },
    RemoveBan {
        ban_id: u32,
        expected_guid: String,
    },
    Lock,
    Unlock,
    ChangeMission {
        template: String,
    },
    RestartMission,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ArmaRoster {
    pub complete: bool,
    pub players: Vec<ArmaPlayer>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ArmaPlayer {
    pub player_id: u32,
    pub address: String,
    pub ping: i32,
    pub guid: Option<String>,
    pub verified: bool,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ArmaBanList {
    pub complete: bool,
    pub bans: Vec<ArmaBan>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ArmaBan {
    pub ban_id: u32,
    pub guid: Option<String>,
    pub address: Option<String>,
    pub minutes_left: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ArmaMissionList {
    pub complete: bool,
    pub missions: Vec<String>,
}

impl ArmaAdminCommand {
    pub fn wire_text(&self) -> Result<String, String> {
        let text = match self {
            Self::Players => "players".to_owned(),
            Self::Bans => "bans".to_owned(),
            Self::Missions => "missions".to_owned(),
            Self::Announce { message } => {
                validate_reason(message, "message")?;
                format!("say -1 {message}")
            }
            Self::Kick {
                player_id,
                expected_guid,
                reason,
            } => {
                validate_guid(expected_guid)?;
                validate_reason(reason, "reason")?;
                format!("kick {player_id} {reason}")
            }
            Self::AddBan {
                guid,
                minutes,
                reason,
            } => {
                validate_guid(guid)?;
                validate_reason(reason, "reason")?;
                format!("addBan {guid} {minutes} {reason}")
            }
            Self::RemoveBan {
                ban_id,
                expected_guid,
            } => {
                validate_guid(expected_guid)?;
                format!("removeBan {ban_id}")
            }
            Self::Lock => "#lock".to_owned(),
            Self::Unlock => "#unlock".to_owned(),
            Self::ChangeMission { template } => {
                validate_mission_template(template)?;
                format!("#mission {template}")
            }
            Self::RestartMission => "#restart".to_owned(),
        };
        if text.len() > MAX_WIRE_BYTES || has_forbidden_control(&text) {
            return Err("RCon command is too long or contains control characters".into());
        }
        Ok(text)
    }

    pub fn required_preflight(&self) -> Option<&'static str> {
        match self {
            Self::Kick { .. } => Some("players"),
            Self::RemoveBan { .. } => Some("bans"),
            Self::ChangeMission { .. } => Some("missions"),
            _ => None,
        }
    }

    pub fn validate_preflight(&self, reply: &str) -> Result<(), String> {
        match self {
            Self::Kick {
                player_id,
                expected_guid,
                ..
            } => {
                validate_guid(expected_guid)?;
                let roster = parse_players(reply);
                if !roster.complete {
                    return Err("player roster is incomplete or malformed".into());
                }
                let player = roster
                    .players
                    .iter()
                    .find(|player| player.player_id == *player_id)
                    .ok_or_else(|| "player ID is no longer present".to_owned())?;
                if !player.verified
                    || player
                        .guid
                        .as_deref()
                        .is_none_or(|guid| !guid.eq_ignore_ascii_case(expected_guid))
                {
                    return Err("player ID does not have the expected verified GUID".into());
                }
                Ok(())
            }
            Self::RemoveBan {
                ban_id,
                expected_guid,
            } => {
                validate_guid(expected_guid)?;
                let list = parse_bans(reply);
                if !list.complete {
                    return Err("ban list is incomplete or malformed".into());
                }
                let ban = list
                    .bans
                    .iter()
                    .find(|ban| ban.ban_id == *ban_id)
                    .ok_or_else(|| "ban ID is no longer present".to_owned())?;
                if ban
                    .guid
                    .as_deref()
                    .is_none_or(|guid| !guid.eq_ignore_ascii_case(expected_guid))
                {
                    return Err("ban ID no longer has the expected GUID".into());
                }
                Ok(())
            }
            Self::ChangeMission { template } => {
                validate_mission_template(template)?;
                let list = parse_missions(reply);
                if !list.complete {
                    return Err("mission list is incomplete or malformed".into());
                }
                if !list
                    .missions
                    .iter()
                    .any(|mission| mission.eq_ignore_ascii_case(template))
                {
                    return Err(
                        "mission template is not present in the current mission list".into(),
                    );
                }
                Ok(())
            }
            _ => Err("command does not require a preflight".into()),
        }
    }
}

pub fn parse_players(reply: &str) -> ArmaRoster {
    let mut roster = ArmaRoster {
        complete: false,
        players: Vec::new(),
    };
    if reply.len() > MAX_REPLY_BYTES {
        return roster;
    }
    let lines: Vec<&str> = reply.lines().collect();
    if lines.len() < 4
        || lines[0].trim() != "Players on server:"
        || !lines[1].contains("IP Address")
        || !lines[1].contains("Ping")
        || !lines[1].contains("GUID")
        || !lines[1].contains("Name")
        || !is_separator(lines[2])
    {
        return roster;
    }

    let mut ids = HashSet::new();
    let mut expected_count = None;
    let mut malformed = false;
    for (offset, line) in lines[3..].iter().enumerate() {
        if let Some(count) = parse_player_count(line.trim()) {
            expected_count = Some(count);
            if lines[3 + offset + 1..]
                .iter()
                .any(|trailing| !trailing.trim().is_empty())
            {
                malformed = true;
            }
            break;
        }
        if line.trim().is_empty() {
            malformed = true;
            continue;
        }
        match parse_player_line(line) {
            Some(player) => {
                if !ids.insert(player.player_id) {
                    malformed = true;
                }
                roster.players.push(player);
            }
            None => malformed = true,
        }
    }
    roster.complete =
        !malformed && expected_count.is_some_and(|count| count == roster.players.len());
    roster
}

pub fn parse_bans(reply: &str) -> ArmaBanList {
    let mut result = ArmaBanList {
        complete: false,
        bans: Vec::new(),
    };
    if reply.len() > MAX_REPLY_BYTES {
        return result;
    }
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Section {
        Guid,
        Ip,
    }
    let mut section = None;
    let mut headers = HashSet::new();
    let mut columns = HashSet::new();
    let mut separators = HashSet::new();
    let mut malformed = false;
    let mut ids = HashSet::new();
    for line in reply.lines() {
        let trimmed = line.trim();
        if trimmed == "GUID Bans:" {
            if section.is_some() || !headers.insert("guid") {
                malformed = true;
            }
            section = Some(Section::Guid);
            continue;
        }
        if trimmed == "IP Bans:" {
            if section != Some(Section::Guid)
                || !columns.contains("guid")
                || !separators.contains("guid")
                || !headers.insert("ip")
            {
                malformed = true;
            }
            section = Some(Section::Ip);
            continue;
        }
        if trimmed.starts_with("[#]") {
            let valid = match section {
                Some(Section::Guid) => {
                    trimmed.contains("[GUID]")
                        && trimmed.contains("[Minutes left]")
                        && trimmed.contains("[Reason]")
                }
                Some(Section::Ip) => {
                    trimmed.contains("[IP Address]")
                        && trimmed.contains("[Minutes left]")
                        && trimmed.contains("[Reason]")
                }
                None => false,
            };
            let key = section.map(|item| match item {
                Section::Guid => "guid",
                Section::Ip => "ip",
            });
            if !valid
                || key.is_none_or(|key| {
                    !headers.contains(key) || columns.contains(key) || separators.contains(key)
                })
            {
                malformed = true;
            } else if let Some(key) = key {
                columns.insert(key);
            }
            continue;
        }
        if is_separator(trimmed) {
            match section {
                Some(Section::Guid) if columns.contains("guid") => {
                    separators.insert("guid");
                }
                Some(Section::Ip) if columns.contains("ip") => {
                    separators.insert("ip");
                }
                Some(_) => malformed = true,
                None => malformed = true,
            }
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        let Some(current) = section else {
            malformed = true;
            continue;
        };
        let key = match current {
            Section::Guid => "guid",
            Section::Ip => "ip",
        };
        if !columns.contains(key) || !separators.contains(key) {
            malformed = true;
            continue;
        }
        match parse_ban_line(trimmed, current == Section::Guid) {
            Some(ban) => {
                if !ids.insert(ban.ban_id) {
                    malformed = true;
                }
                result.bans.push(ban);
            }
            None => malformed = true,
        }
    }
    result.complete = !malformed
        && headers.len() == 2
        && columns.len() == 2
        && separators.len() == 2
        && headers.contains("guid")
        && headers.contains("ip");
    result
}

pub fn parse_missions(reply: &str) -> ArmaMissionList {
    let mut result = ArmaMissionList {
        complete: false,
        missions: Vec::new(),
    };
    if reply.len() > MAX_REPLY_BYTES {
        return result;
    }
    let mut lines = reply.lines();
    if lines.next().map(str::trim) != Some("Missions on server:") {
        return result;
    }
    let mut names = HashSet::new();
    let mut malformed = false;
    for line in lines {
        let name = line.trim();
        if name.is_empty() {
            continue;
        }
        let Some(template) = name.strip_suffix(".pbo") else {
            malformed = true;
            continue;
        };
        if validate_mission_template(template).is_err() || !names.insert(name.to_ascii_lowercase())
        {
            malformed = true;
        } else {
            result.missions.push(template.to_owned());
        }
    }
    result.complete = !malformed;
    result
}

fn parse_player_line(line: &str) -> Option<ArmaPlayer> {
    let mut rest = line.trim_start();
    let (id, next) = take_token(rest)?;
    rest = next;
    let (address, next) = take_token(rest)?;
    rest = next;
    let (ping, next) = take_token(rest)?;
    rest = next;
    let (guid_token, name) = take_token(rest)?;
    let player_id = id.parse().ok()?;
    let ping = ping.parse().ok()?;
    if address.len() > 128
        || has_forbidden_control(address)
        || has_forbidden_control(name)
        || name.trim().is_empty()
    {
        return None;
    }
    let (guid_text, verified) = guid_token
        .strip_suffix("(OK)")
        .map_or((guid_token, false), |guid| (guid, true));
    let guid = if matches!(guid_text, "-" | "?" | "N/A") {
        if verified {
            return None;
        }
        None
    } else if valid_guid(guid_text) {
        Some(guid_text.to_owned())
    } else {
        return None;
    };
    Some(ArmaPlayer {
        player_id,
        address: address.to_owned(),
        ping,
        guid,
        verified: verified && guid_text.len() == 32,
        name: name.trim().to_owned(),
    })
}

fn parse_ban_line(line: &str, guid_ban: bool) -> Option<ArmaBan> {
    let mut rest = line;
    let (id, next) = take_token(rest)?;
    rest = next;
    let (identity, next) = take_token(rest)?;
    rest = next;
    let (minutes_left, reason) = take_token(rest)?;
    let ban_id = id.parse().ok()?;
    if has_forbidden_control(minutes_left) || has_forbidden_control(reason) {
        return None;
    }
    let (guid, address) = if guid_ban {
        if !valid_guid(identity) {
            return None;
        }
        (Some(identity.to_owned()), None)
    } else {
        if identity.len() > 128 || has_forbidden_control(identity) {
            return None;
        }
        (None, Some(identity.to_owned()))
    };
    Some(ArmaBan {
        ban_id,
        guid,
        address,
        minutes_left: minutes_left.to_owned(),
        reason: reason.trim().to_owned(),
    })
}

fn take_token(input: &str) -> Option<(&str, &str)> {
    let input = input.trim_start();
    let end = input.find(char::is_whitespace).unwrap_or(input.len());
    (end > 0).then(|| (&input[..end], &input[end..]))
}

fn parse_player_count(line: &str) -> Option<usize> {
    let value = line.strip_prefix('(')?.strip_suffix(" players in total)")?;
    value.parse().ok()
}

fn is_separator(line: &str) -> bool {
    let line = line.trim();
    line.len() >= 8 && line.bytes().all(|byte| byte == b'-')
}

fn validate_reason(value: &str, label: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > MAX_REASON_BYTES || has_forbidden_control(value) {
        return Err(format!(
            "{label} must be non-empty, single-line, and at most {MAX_REASON_BYTES} bytes"
        ));
    }
    Ok(())
}

fn validate_guid(value: &str) -> Result<(), String> {
    if valid_guid(value) {
        Ok(())
    } else {
        Err("GUID must contain exactly 32 hexadecimal characters".into())
    }
}

fn valid_guid(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_mission_template(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || value.starts_with('.')
        || value.ends_with('.')
        || value.to_ascii_lowercase().ends_with(".pbo")
        || !value.contains('.')
        || value
            .rsplit_once('.')
            .is_none_or(|(mission, world)| mission.is_empty() || world.is_empty())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
    {
        return Err("mission template must be a relative world-qualified basename".into());
    }
    Ok(())
}

fn has_forbidden_control(value: &str) -> bool {
    value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests;
