//! Shared RCon transport. Game adapters own command semantics, permissions and settings.

pub mod arma3;
pub(in crate::core::server) mod owned;
pub(crate) mod records;
pub mod reforger;
mod session;
mod settings;
#[cfg(test)]
pub(crate) mod tests;
mod wire;

pub use records::{RconCommandRecord, RconCommandState};
pub(crate) use session::{
    CommandOutcome, KEEPALIVE_INTERVAL, LoginFailure, MAX_COMMAND_BYTES, RconPassword, Retransmit,
    ServerText, Session, SessionCounters, UnknownReason, response_text, unix_ms,
};
pub use settings::{Arma3RconSettings, ReforgerRconPermission, ReforgerRconSettings};
pub(crate) use settings::{
    carries_secret, connect_endpoint, new_secret_reference, valid_secret_reference,
};
mod credentials;
pub(crate) use credentials::clear_password_reference;
pub use credentials::{RconDraftSettings, RconSettingsInput, draft_settings};
