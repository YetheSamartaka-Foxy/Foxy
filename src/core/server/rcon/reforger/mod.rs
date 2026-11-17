//! Typed Reforger BattlEye RCon commands and conservative native transcript parsing.

use serde::{Deserialize, Serialize};

pub(crate) mod transcript;

const MAX_WIRE_BYTES: usize = 512;
const MAX_REASON_BYTES: usize = 256;
const MAX_IDENTITY_BYTES: usize = 64;
const MAX_REPLY_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReforgerAdminCommand {
    Players,
    Bans {
        page: u32,
    },
    Kick {
        player_id: u32,
        expected_identity: String,
    },
    Ban {
        identity: String,
        duration_seconds: u32,
        reason: String,
    },
    RemoveBan {
        identity: String,
    },
    ReloadScenario,
    Logout,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReforgerRoster {
    pub complete: bool,
    pub players: Vec<ReforgerPlayer>,
    pub raw: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReforgerPlayer {
    pub player_id: u32,
    pub identity: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReforgerBanList {
    pub complete: bool,
    pub bans: Vec<ReforgerBan>,
    pub raw: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReforgerBan {
    pub ban_id: u32,
    pub identity: String,
    pub duration: String,
}

impl ReforgerAdminCommand {
    pub fn wire_text(&self) -> Result<String, String> {
        let text = match self {
            Self::Players => "#players".to_owned(),
            Self::Bans { page } => {
                if *page == 0 {
                    return Err("ban list page must be at least 1".into());
                }
                format!("#ban list {page}")
            }
            Self::Kick {
                player_id,
                expected_identity,
            } => {
                validate_identity(expected_identity)?;
                format!("#kick {player_id}")
            }
            Self::Ban {
                identity,
                duration_seconds,
                reason,
            } => {
                validate_identity(identity)?;
                validate_reason(reason)?;
                format!("#ban create {identity} {duration_seconds} {reason}")
            }
            Self::RemoveBan { identity } => {
                validate_identity(identity)?;
                format!("#ban remove {identity}")
            }
            Self::ReloadScenario => "#restart".to_owned(),
            Self::Logout => "@logout".to_owned(),
        };
        if text.len() > MAX_WIRE_BYTES || has_control(&text) {
            return Err("RCon command is too long or contains control characters".into());
        }
        Ok(text)
    }

    pub fn requires_admin(&self) -> bool {
        !matches!(self, Self::Players | Self::Logout)
    }

    pub fn required_preflight(&self) -> Option<&'static str> {
        matches!(self, Self::Kick { .. }).then_some("#players")
    }

    pub fn validate_preflight(&self, transcript: &str) -> Result<(), String> {
        let Self::Kick {
            player_id,
            expected_identity,
        } = self
        else {
            return Err("command does not require a preflight".into());
        };
        validate_identity(expected_identity)?;
        let roster = parse_players(transcript);
        let mut matches = roster
            .players
            .iter()
            .filter(|player| player.player_id == *player_id);
        let player = matches
            .next()
            .ok_or_else(|| "player ID is absent from the observed roster".to_owned())?;
        if matches.next().is_some()
            || player.identity.as_deref() != Some(expected_identity.as_str())
        {
            return Err("player ID does not have the expected identity".into());
        }
        Ok(())
    }
}

pub fn parse_players(transcript: &str) -> ReforgerRoster {
    let mut result = ReforgerRoster {
        complete: false,
        players: Vec::new(),
        raw: transcript.to_owned(),
    };
    if transcript.len() > MAX_REPLY_BYTES {
        return result;
    }
    let mut saw_header = false;
    for line in transcript.lines() {
        if line.contains("Players on server:") {
            saw_header = true;
            continue;
        }
        if !saw_header {
            continue;
        }
        let Some((id, identity, name)) = parse_semicolon_row(line) else {
            continue;
        };
        let Some(id) = id
            .strip_prefix("Player#")
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        let identity = nonempty_field(identity);
        let name = nonempty_field(name);
        if identity
            .as_ref()
            .is_some_and(|value| value.len() > MAX_IDENTITY_BYTES || has_control(value))
            || name
                .as_ref()
                .is_some_and(|value| value.len() > 256 || has_control(value))
        {
            continue;
        }
        result.players.push(ReforgerPlayer {
            player_id: id,
            identity,
            name,
        });
    }
    result
}

pub fn parse_bans(transcript: &str, page: u32) -> ReforgerBanList {
    let mut result = ReforgerBanList {
        complete: false,
        bans: Vec::new(),
        raw: transcript.to_owned(),
    };
    if transcript.len() > MAX_REPLY_BYTES || page == 0 {
        return result;
    }
    if transcript
        .lines()
        .any(|line| line.trim() == "Server has no bans to list.")
    {
        result.complete = true;
        return result;
    }
    let mut saw_header = false;
    for line in transcript.lines() {
        if line.contains("BanID") && line.contains("Player UID") && line.contains("Duration") {
            saw_header = true;
            continue;
        }
        if !saw_header {
            continue;
        }
        let Some((id, identity, duration)) = parse_semicolon_row(line) else {
            continue;
        };
        let Some(ban_id) = id.parse::<u32>().ok() else {
            continue;
        };
        let Some(identity) = nonempty_field(identity) else {
            continue;
        };
        let Some(duration) = nonempty_field(duration) else {
            continue;
        };
        if identity.len() > MAX_IDENTITY_BYTES
            || duration.len() > 64
            || has_control(&identity)
            || has_control(&duration)
        {
            continue;
        }
        result.bans.push(ReforgerBan {
            ban_id,
            identity,
            duration,
        });
    }
    result
}

pub fn response_rejected(transcript: &str) -> bool {
    transcript.lines().any(|line| {
        let line = line.trim();
        line == "Failed to create ban."
            || line
                .strip_prefix("missing permission '")
                .and_then(|name| name.strip_suffix('\''))
                .is_some_and(|name| {
                    !name.is_empty()
                        && name
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b' ')
                })
            || line
                .strip_prefix("unknown command '")
                .is_some_and(|command| {
                    command
                        .strip_suffix('\'')
                        .is_some_and(|name| !name.is_empty())
                })
            || line
                .strip_prefix("error 'ban create ")
                .is_some_and(|command| command.ends_with('\''))
    })
}

fn parse_semicolon_row(line: &str) -> Option<(&str, &str, &str)> {
    let mut fields = line.split(';');
    let first = fields.next()?.trim();
    let second = fields.next()?.trim();
    let third = fields.next()?.trim();
    if fields.next().is_some() {
        return None;
    }
    let first = first.strip_prefix('[')?.strip_suffix(']')?.trim();
    let second = second.strip_prefix('[')?.strip_suffix(']')?.trim();
    let third = third.strip_prefix('[')?.strip_suffix(']')?.trim();
    Some((first, second, third))
}

fn nonempty_field(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn validate_identity(value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    let base64_identity = bytes.len() == 44
        && bytes[43] == b'='
        && bytes[..43]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'));
    if base64_identity {
        Ok(())
    } else {
        Err("identity must use the observed 44-character Base64 identity form".into())
    }
}

fn validate_reason(value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > MAX_REASON_BYTES || has_control(value) {
        Err(format!(
            "reason must be non-empty, single-line, and at most {MAX_REASON_BYTES} bytes"
        ))
    } else {
        Ok(())
    }
}

fn has_control(value: &str) -> bool {
    value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests;
