use std::net::{Ipv4Addr, SocketAddr};

use serde::{Deserialize, Serialize};

/// Arma BattlEye RCon. The password lives in the protected store under `password_ref`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arma3RconSettings {
    /// BattlEye `RConPort`; must stay outside the game port bundle.
    pub port: u16,
    /// BattlEye `RConIP`. Loopback keeps RCon local to this computer.
    #[serde(default = "loopback", skip_serializing_if = "is_loopback")]
    pub address: Ipv4Addr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_ref: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReforgerRconPermission {
    Admin,
    Monitor,
}

impl ReforgerRconPermission {
    pub fn native(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Monitor => "monitor",
        }
    }
}

/// Reforger native `rcon` block. The permission applies to every RCon connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReforgerRconSettings {
    #[serde(default = "loopback")]
    pub address: Ipv4Addr,
    pub port: u16,
    pub permission: ReforgerRconPermission,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_clients: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_ref: Option<String>,
}

fn loopback() -> Ipv4Addr {
    Ipv4Addr::LOCALHOST
}

fn is_loopback(address: &Ipv4Addr) -> bool {
    *address == Ipv4Addr::LOCALHOST
}

pub(crate) fn valid_secret_reference(value: &str) -> bool {
    value.strip_prefix("secret_").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

pub(crate) fn new_secret_reference() -> String {
    format!("secret_{:032x}", rand::random::<u128>())
}

/// Where Foxy connects: the bind address, or loopback when the server binds every interface.
pub(crate) fn connect_endpoint(address: Ipv4Addr, port: u16) -> SocketAddr {
    let host = if address.is_unspecified() {
        Ipv4Addr::LOCALHOST
    } else {
        address
    };
    SocketAddr::from((host, port))
}

fn check_reference(reference: Option<&str>) -> Result<(), String> {
    if reference.is_some_and(|value| !valid_secret_reference(value)) {
        return Err("Invalid RCon password reference".into());
    }
    Ok(())
}

impl Arma3RconSettings {
    pub(crate) fn validate(&self, game_port: u16, battleye: bool) -> Result<(), String> {
        if self.port == 0 {
            return Err("The RCon port must be nonzero".into());
        }
        if (game_port..=game_port.saturating_add(4)).contains(&self.port) {
            return Err(format!(
                "RCon port {} overlaps the game port range {}-{}",
                self.port,
                game_port,
                game_port.saturating_add(4)
            ));
        }
        if !battleye {
            return Err(
                "Arma RCon is provided by BattlEye; enable BattlEye or remove the RCon settings"
                    .into(),
            );
        }
        check_reference(self.password_ref.as_deref())
    }
}

impl ReforgerRconSettings {
    pub(crate) fn validate(&self, bind_port: u16, a2s_port: Option<u16>) -> Result<(), String> {
        if self.port == 0 || self.port == bind_port || Some(self.port) == a2s_port {
            return Err(
                "The RCon port must be nonzero and differ from the game and query ports".into(),
            );
        }
        if self
            .max_clients
            .is_some_and(|clients| !(1..=16).contains(&clients))
        {
            return Err("Reforger RCon accepts 1 to 16 clients".into());
        }
        check_reference(self.password_ref.as_deref())
    }
}

/// Commands that set or present a password would persist it in argv, records and history.
pub(crate) fn carries_secret(text: &str) -> bool {
    text.split_whitespace().next().is_some_and(|word| {
        let word = word.to_ascii_lowercase();
        word.contains("password") || word == "#login"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_that_carry_passwords_are_recognized() {
        for text in [
            "RConPassword next",
            "rconpassword",
            "#login secret",
            "  Password x",
        ] {
            assert!(carries_secret(text), "{text}");
        }
        for text in [
            "players",
            "#players",
            "say -1 password reset tonight",
            "@logout",
        ] {
            assert!(!carries_secret(text), "{text}");
        }
    }
}
