use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::{mpsc, watch};

use crate::core::server::context::ServerContext;
use foxy_steam::auth::{self, AuthPhase, LoginInput};
use foxy_steam::store::SessionStore;
use foxy_steam::{AuthError, PreparedCredential, Secret};

const CODE_QUEUE_CAPACITY: usize = 4;

#[derive(Clone, Default)]
pub(crate) struct SteamAuthControl {
    inner: Arc<ControlInner>,
}

struct ControlInner {
    phase_tx: watch::Sender<AuthPhase>,
    phase_rx: watch::Receiver<AuthPhase>,
    cancel_tx: watch::Sender<bool>,
    cancel_rx: watch::Receiver<bool>,
    code_tx: mpsc::Sender<Secret>,
    code_rx: Mutex<Option<mpsc::Receiver<Secret>>>,
}

impl Default for ControlInner {
    fn default() -> Self {
        let (phase_tx, phase_rx) = watch::channel(AuthPhase::Connecting);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (code_tx, code_rx) = mpsc::channel(CODE_QUEUE_CAPACITY);
        Self {
            phase_tx,
            phase_rx,
            cancel_tx,
            cancel_rx,
            code_tx,
            code_rx: Mutex::new(Some(code_rx)),
        }
    }
}

impl SteamAuthControl {
    pub(crate) fn phase(&self) -> AuthPhase {
        *self.inner.phase_rx.borrow()
    }

    pub(crate) fn submit_code(&self, code: Secret) -> Result<(), AuthError> {
        if self.is_cancelled() {
            return Err(AuthError::Cancelled);
        }
        if !matches!(
            self.phase(),
            AuthPhase::EmailCode
                | AuthPhase::DeviceCode
                | AuthPhase::CodeRejectedEmail
                | AuthPhase::CodeRejectedDevice
        ) {
            return Err(AuthError::InvalidInput);
        }
        self.inner
            .code_tx
            .try_send(code)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => AuthError::Busy,
                mpsc::error::TrySendError::Closed(_) => AuthError::Cancelled,
            })
    }

    pub(crate) fn cancel(&self) {
        self.inner.cancel_tx.send_replace(true);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        *self.inner.cancel_rx.borrow()
    }

    fn take_code_receiver(&self) -> Result<mpsc::Receiver<Secret>, AuthError> {
        self.inner
            .code_rx
            .lock()
            .map_err(|_| AuthError::Busy)?
            .take()
            .ok_or(AuthError::Busy)
    }
}

pub(crate) enum SteamAuthOperation {
    Inspect,
    Password {
        account_name: Secret,
        password: Secret,
    },
    Resume,
    Anonymous,
    Forget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SteamAuthStatus {
    Inspected,
    Authenticated,
    Forgotten,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SteamAuthOutcome {
    pub alias: String,
    pub phase: AuthPhase,
    pub status: SteamAuthStatus,
    pub authenticated: bool,
    pub disconnected: bool,
    pub prepared: Option<bool>,
    pub credentials_saved: Option<bool>,
    pub writes_performed: Option<bool>,
    pub database_opened: bool,
    pub error: Option<AuthError>,
}

pub(crate) fn run_steam_auth(
    context: &ServerContext,
    alias: &str,
    operation: SteamAuthOperation,
    control: &SteamAuthControl,
) -> SteamAuthOutcome {
    let safe_alias = if valid_alias(alias) {
        alias.to_owned()
    } else {
        String::new()
    };
    let mut outcome = SteamAuthOutcome {
        alias: safe_alias,
        phase: control.phase(),
        status: SteamAuthStatus::Failed,
        authenticated: false,
        disconnected: false,
        prepared: None,
        credentials_saved: Some(false),
        writes_performed: Some(false),
        database_opened: false,
        error: None,
    };

    if !valid_alias(alias) {
        outcome.error = Some(AuthError::InvalidInput);
        return outcome;
    }

    if control.is_cancelled() {
        outcome.error = Some(AuthError::Cancelled);
        return outcome;
    }
    let is_resume = matches!(&operation, SteamAuthOperation::Resume);
    let code_receiver = match control.take_code_receiver() {
        Ok(receiver) => receiver,
        Err(error) => {
            outcome.error = Some(error);
            return outcome;
        }
    };
    let store = match session_store(context) {
        Ok(store) => store,
        Err(error) => {
            outcome.error = Some(error);
            return outcome;
        }
    };

    if matches!(&operation, SteamAuthOperation::Inspect) {
        return match store.inspect(alias) {
            Ok(prepared) => SteamAuthOutcome {
                prepared: Some(prepared),
                status: SteamAuthStatus::Inspected,
                ..outcome
            },
            Err(error) => {
                outcome.error = Some(error);
                outcome
            }
        };
    }

    if is_resume {
        match store.inspect(alias) {
            Ok(true) => (),
            Ok(false) => {
                outcome.prepared = Some(false);
                outcome.error = Some(AuthError::AuthenticationRequired);
                return outcome;
            }
            Err(error) => {
                outcome.error = Some(error);
                return outcome;
            }
        }
    }
    outcome.writes_performed = None;
    let guard = match store.lock() {
        Ok(guard) => guard,
        Err(error) => {
            outcome.error = Some(error);
            return outcome;
        }
    };
    let existing = guard.load(alias);
    outcome.prepared = credential_presence(&existing);

    if matches!(&operation, SteamAuthOperation::Forget) {
        return match guard.forget(alias) {
            Ok(()) => {
                outcome.status = SteamAuthStatus::Forgotten;
                outcome.prepared = Some(false);
                if existing.is_ok() {
                    outcome.writes_performed = Some(true);
                }
                outcome
            }
            Err(error) => {
                if error == AuthError::StorageUncertain {
                    outcome.credentials_saved = None;
                    outcome.writes_performed = Some(true);
                    outcome.prepared = credential_presence(&guard.load(alias));
                }
                outcome.error = Some(error);
                outcome
            }
        };
    }

    let input = match operation {
        SteamAuthOperation::Password {
            account_name,
            password,
        } => LoginInput::Password {
            account_name,
            password,
        },
        SteamAuthOperation::Resume => match existing {
            Ok(credential) => LoginInput::Prepared(credential),
            Err(error) => {
                outcome.error = Some(error);
                return outcome;
            }
        },
        SteamAuthOperation::Anonymous => LoginInput::Anonymous,
        SteamAuthOperation::Inspect | SteamAuthOperation::Forget => unreachable!(),
    };

    let (input, save_new_credential) = match input {
        LoginInput::Prepared(credential) => (LoginInput::Prepared(credential), false),
        LoginInput::Password {
            account_name,
            password,
        } => (
            LoginInput::Password {
                account_name,
                password,
            },
            true,
        ),
        LoginInput::Anonymous => (LoginInput::Anonymous, false),
    };

    if tokio::runtime::Handle::try_current().is_ok() {
        outcome.error = Some(AuthError::Busy);
        return outcome;
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            outcome.error = Some(AuthError::Network);
            return outcome;
        }
    };
    let result = runtime.block_on(auth::authenticate(
        input,
        control.inner.phase_tx.clone(),
        code_receiver,
        control.inner.cancel_rx.clone(),
    ));

    match result {
        Ok(session) => {
            let credential = session.credential.clone();
            session.disconnect();
            outcome.authenticated = true;
            outcome.disconnected = true;
            if save_new_credential {
                if control.is_cancelled() {
                    outcome.error = Some(AuthError::Cancelled);
                    outcome.phase = control.phase();
                    return outcome;
                }
                match (recapture_context(context), credential) {
                    (Ok(()), Some(credential)) => match guard.save(alias, &credential) {
                        Ok(()) => {
                            outcome.credentials_saved = Some(true);
                            outcome.writes_performed = Some(true);
                            outcome.prepared = Some(true);
                            outcome.status = SteamAuthStatus::Authenticated;
                        }
                        Err(error) => {
                            if error == AuthError::StorageUncertain {
                                outcome.credentials_saved = None;
                                outcome.writes_performed = Some(true);
                                outcome.prepared = credential_presence(&guard.load(alias));
                            }
                            outcome.error = Some(error);
                        }
                    },
                    (Err(error), _) => outcome.error = Some(error),
                    (_, None) => outcome.error = Some(AuthError::AuthenticationRequired),
                }
            } else {
                outcome.status = SteamAuthStatus::Authenticated;
            }
        }
        Err(error) => {
            outcome.disconnected = true;
            outcome.error = Some(
                if matches!(error, AuthError::RejectedCredentials) && is_resume {
                    AuthError::AuthenticationRequired
                } else {
                    error
                },
            );
        }
    }
    outcome.phase = control.phase();
    outcome
}

pub(crate) fn unattended_input(
    context: &ServerContext,
    account: Option<&str>,
) -> Result<LoginInput, AuthError> {
    let Some(alias) = account else {
        return Ok(LoginInput::Anonymous);
    };
    if !valid_alias(alias) {
        return Err(AuthError::InvalidInput);
    }
    session_store(context)?
        .load(alias)
        .map(LoginInput::Prepared)
}

pub(crate) fn valid_account_alias(alias: &str) -> bool {
    valid_alias(alias)
}

/// Scope and location derive from the canonical config root so another spelling of the same
/// folder (separators, `\\?\` prefix, letter case) still finds the prepared session.
fn session_store(context: &ServerContext) -> Result<SessionStore, AuthError> {
    let root = context
        .data_root()
        .canonicalize()
        .map_err(|_| AuthError::Storage)?;
    let mut identity = root.to_string_lossy().into_owned();
    identity.push('\0');
    identity.push_str(context.space_id());
    identity.push('\0');
    identity.push_str(context.game_id());
    let scope = format!("scope_{}", blake3::hash(identity.as_bytes()).to_hex());
    SessionStore::new(
        root.join("games")
            .join(context.space_id())
            .join("server/steam-auth"),
        scope,
    )
}

fn recapture_context(context: &ServerContext) -> Result<(), AuthError> {
    let current = ServerContext::capture(context.data_root(), Some(context.space_id()))
        .map_err(|_| AuthError::InvalidInput)?;
    if current == *context {
        Ok(())
    } else {
        Err(AuthError::InvalidInput)
    }
}

fn credential_presence(credential: &Result<PreparedCredential, AuthError>) -> Option<bool> {
    match credential {
        Ok(_) | Err(AuthError::InvalidStoredSession) => Some(true),
        Err(AuthError::AuthenticationRequired) => Some(false),
        Err(_) => None,
    }
}

fn valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias.len() <= 64
        && alias
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    fn context(root: &Path) -> ServerContext {
        std::fs::write(
            root.join("games.json"),
            serde_json::to_vec(&json!({
                "active_game_space_id": "alpha",
                "game_spaces": [{
                    "id": "alpha",
                    "game_id": "arma3",
                    "display_name": "Alpha",
                    "created_at": 0
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        ServerContext::capture(root, None).unwrap()
    }

    #[test]
    fn inspect_missing_alias_is_write_free() {
        let root = tempfile::tempdir().unwrap();
        let context = context(root.path());
        let control = SteamAuthControl::default();
        let outcome = run_steam_auth(&context, "operator", SteamAuthOperation::Inspect, &control);
        assert_eq!(outcome.status, SteamAuthStatus::Inspected);
        assert_eq!(outcome.prepared, Some(false));
        assert_eq!(outcome.writes_performed, Some(false));
        assert!(!context.space_dir().exists());
    }

    #[test]
    fn invalid_alias_fails_without_store_creation() {
        let root = tempfile::tempdir().unwrap();
        let context = context(root.path());
        let control = SteamAuthControl::default();
        let outcome = run_steam_auth(
            &context,
            "../private",
            SteamAuthOperation::Inspect,
            &control,
        );
        assert_eq!(outcome.alias, "");
        assert_eq!(outcome.error, Some(AuthError::InvalidInput));
        assert_eq!(outcome.writes_performed, Some(false));
        assert!(!context.space_dir().exists());
    }

    #[test]
    fn prepared_sessions_survive_another_spelling_of_the_config_root() {
        let root = tempfile::tempdir().unwrap();
        let plain = context(root.path());
        let canonical = ServerContext::capture(&root.path().canonicalize().unwrap(), None).unwrap();
        let credential = PreparedCredential {
            account_name: Secret::new("synthetic-account".into()),
            refresh_token: Secret::new("synthetic-token".into()),
        };
        session_store(&plain)
            .unwrap()
            .lock()
            .unwrap()
            .save("operator", &credential)
            .unwrap();
        let outcome = run_steam_auth(
            &canonical,
            "operator",
            SteamAuthOperation::Inspect,
            &SteamAuthControl::default(),
        );
        assert_eq!(outcome.prepared, Some(true), "{outcome:?}");
        assert_eq!(
            session_store(&canonical)
                .unwrap()
                .load("operator")
                .unwrap()
                .refresh_token
                .expose(),
            "synthetic-token"
        );
    }

    #[test]
    fn cancellation_preserves_an_existing_prepared_credential() {
        let root = tempfile::tempdir().unwrap();
        let context = context(root.path());
        let store = session_store(&context).unwrap();
        let credential = PreparedCredential {
            account_name: Secret::new("synthetic-account".into()),
            refresh_token: Secret::new("synthetic-token".into()),
        };
        store.lock().unwrap().save("operator", &credential).unwrap();

        let control = SteamAuthControl::default();
        control.cancel();
        let outcome = run_steam_auth(
            &context,
            "operator",
            SteamAuthOperation::Password {
                account_name: Secret::new("synthetic-account".into()),
                password: Secret::new("synthetic-password".into()),
            },
            &control,
        );

        assert_eq!(outcome.error, Some(AuthError::Cancelled));
        assert_eq!(outcome.prepared, None);
        assert_eq!(
            store.load("operator").unwrap().refresh_token.expose(),
            "synthetic-token"
        );
    }
}
