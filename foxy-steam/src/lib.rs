pub mod auth;
pub mod content;
pub mod workshop;

pub use steamroom::cdn::CdnServer;
pub use steamroom::client::{LoggedIn, SteamClient};
pub mod store;

use std::fmt;

use serde::Serialize;
use zeroize::Zeroizing;

#[derive(Clone, Default)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn expose_mut(&mut self) -> &mut String {
        &mut self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("[redacted]")
    }
}

#[derive(Clone, Debug)]
pub struct PreparedCredential {
    pub account_name: Secret,
    pub refresh_token: Secret,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthError {
    InvalidInput,
    AuthenticationRequired,
    RejectedCredentials,
    RejectedCode,
    UnsupportedChallenge,
    Network,
    TimedOut,
    Cancelled,
    Storage,
    StorageUncertain,
    Busy,
    InvalidStoredSession,
}

impl fmt::Display for AuthError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::InvalidInput => "Invalid authentication input",
            Self::AuthenticationRequired => "Prepare a native Steam session with attended login",
            Self::RejectedCredentials => "Steam rejected the account credentials",
            Self::RejectedCode => "Steam rejected the Guard code",
            Self::UnsupportedChallenge => "Steam requested an unsupported confirmation method",
            Self::Network => "Steam connection failed; retry when the connection is available",
            Self::TimedOut => "Steam authentication timed out",
            Self::Cancelled => "Steam authentication was cancelled",
            Self::Storage => "Protected Steam session storage is unavailable",
            Self::StorageUncertain => {
                "Steam session storage changed but durability is uncertain; inspect before retrying"
            }
            Self::Busy => "Another authentication operation owns this session store",
            Self::InvalidStoredSession => {
                "The stored Steam session is invalid or belongs to another scope"
            }
        })
    }
}

impl std::error::Error for AuthError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_serialized_error_never_include_credentials() {
        let credential = PreparedCredential {
            account_name: Secret::new("test-account-private".into()),
            refresh_token: Secret::new("test-token-private".into()),
        };
        let diagnostic = format!("{credential:?}");
        assert!(!diagnostic.contains("test-account-private"));
        assert!(!diagnostic.contains("test-token-private"));
        assert_eq!(
            serde_json::to_string(&AuthError::Cancelled).unwrap(),
            "\"cancelled\""
        );
    }
}
