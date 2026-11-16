use std::time::Duration;

use prost::Message;
use steamroom::auth::GuardType;
use steamroom::client::{LoggedIn, PROTOCOL_VERSION, Ready, SteamClient};
use steamroom::connection::{CmServer, Protocol};
use steamroom::transport::tcp::TcpTransport;
use steamroom::transport::websocket::WebSocketTransport;
use steamroom::{generated, messages::EMsg, types::SteamId};
use steamroom_client::login::{CredentialsLoginFlow, LoginBuilder, LoginError};
use tokio::sync::{mpsc, watch};

use crate::{AuthError, PreparedCredential, Secret};

const AUTH_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const NETWORK_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_CODE_ATTEMPTS: usize = 5;
const MAX_ACCOUNT_BYTES: usize = 256;
const MAX_PASSWORD_BYTES: usize = 1024;
const MAX_REFRESH_TOKEN_BYTES: usize = 4096;

#[derive(Debug)]
pub enum LoginInput {
    Password {
        account_name: Secret,
        password: Secret,
    },
    Prepared(PreparedCredential),
    Anonymous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthPhase {
    Connecting,
    EmailCode,
    DeviceCode,
    MobileApproval,
    CodeRejectedEmail,
    CodeRejectedDevice,
    Authenticating,
    Authenticated,
}

pub struct AuthenticatedSession {
    pub client: SteamClient<LoggedIn>,
    pub credential: Option<PreparedCredential>,
}

impl AuthenticatedSession {
    pub fn disconnect(self) {
        drop(self);
    }
}

pub async fn authenticate(
    input: LoginInput,
    phases: watch::Sender<AuthPhase>,
    codes: mpsc::Receiver<Secret>,
    cancel: watch::Receiver<bool>,
) -> Result<AuthenticatedSession, AuthError> {
    let operation = authenticate_inner(input, phases.clone(), codes);
    tokio::pin!(operation);
    let deadline = tokio::time::sleep(AUTH_TIMEOUT);
    tokio::pin!(deadline);
    let mut cancel = cancel;
    if *cancel.borrow() {
        return Err(AuthError::Cancelled);
    }

    let result = loop {
        tokio::select! {
            biased;
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    break Err(AuthError::Cancelled);
                }
            }
            _ = &mut deadline => break Err(AuthError::TimedOut),
            result = &mut operation => break result,
        }
    };

    if result.is_ok() {
        let _ = phases.send(AuthPhase::Authenticated);
    }
    result
}

async fn authenticate_inner(
    input: LoginInput,
    phases: watch::Sender<AuthPhase>,
    mut codes: mpsc::Receiver<Secret>,
) -> Result<AuthenticatedSession, AuthError> {
    let _ = phases.send(AuthPhase::Connecting);

    match input {
        LoginInput::Anonymous => {
            let _ = phases.send(AuthPhase::Authenticating);
            let client = tokio::time::timeout(
                NETWORK_TIMEOUT,
                LoginBuilder::new()
                    .device_name("Foxy")
                    .client_os(steamroom_client::login::ClientOs::new(client_os()))
                    .prefer_protocol(Protocol::WebSocket)
                    .allow_protocol_fallback(true)
                    .anonymous()
                    .login(),
            )
            .await
            .map_err(|_| AuthError::TimedOut)?
            .map_err(map_login_error)?;
            Ok(AuthenticatedSession {
                client,
                credential: None,
            })
        }
        LoginInput::Prepared(credential) => {
            if !valid_account(credential.account_name.expose())
                || !valid_refresh_token(credential.refresh_token.expose())
            {
                return Err(AuthError::InvalidInput);
            }
            let _ = phases.send(AuthPhase::Authenticating);
            let client = native_login(
                credential.account_name.expose(),
                credential.refresh_token.expose(),
                rand::random::<u32>().max(1),
            )
            .await?;
            Ok(AuthenticatedSession {
                client,
                credential: None,
            })
        }
        LoginInput::Password {
            account_name,
            password,
        } => {
            if !valid_account(account_name.expose()) || !valid_password(password.expose()) {
                return Err(AuthError::InvalidInput);
            }
            let flow = tokio::time::timeout(
                NETWORK_TIMEOUT,
                LoginBuilder::new()
                    .device_name("Foxy")
                    .client_os(steamroom_client::login::ClientOs::new(client_os()))
                    .prefer_protocol(Protocol::WebSocket)
                    .allow_protocol_fallback(true)
                    .with_credentials(
                        account_name.expose().to_owned(),
                        password.expose().to_owned(),
                    )
                    .begin(),
            )
            .await
            .map_err(|_| AuthError::TimedOut)?
            .map_err(map_login_error)?;
            let credential = finish_password_flow(flow, &mut codes, &phases).await?;
            let client = native_login(
                credential.account_name.expose(),
                credential.refresh_token.expose(),
                rand::random::<u32>().max(1),
            )
            .await?;
            Ok(AuthenticatedSession {
                client,
                credential: Some(credential),
            })
        }
    }
}

async fn finish_password_flow(
    flow: CredentialsLoginFlow,
    codes: &mut mpsc::Receiver<Secret>,
    phases: &watch::Sender<AuthPhase>,
) -> Result<PreparedCredential, AuthError> {
    let approved = match flow {
        CredentialsLoginFlow::Approved(approved) => approved,
        CredentialsLoginFlow::NeedsMobileConfirm(challenge) => {
            let _ = phases.send(AuthPhase::MobileApproval);
            tokio::time::timeout(NETWORK_TIMEOUT, challenge.wait_for_confirmation())
                .await
                .map_err(|_| AuthError::TimedOut)?
                .map_err(map_login_error)?
        }
        CredentialsLoginFlow::NeedsGuardCode(mut challenge) => {
            let email = challenge.allowed_kinds().contains(&GuardType::EmailCode);
            let device = challenge.allowed_kinds().contains(&GuardType::DeviceCode);
            if !email && !device {
                return Err(AuthError::UnsupportedChallenge);
            }
            let initial_phase = if email {
                AuthPhase::EmailCode
            } else {
                AuthPhase::DeviceCode
            };
            let mut rejected = false;
            let mut approved = None;
            for _ in 0..MAX_CODE_ATTEMPTS {
                let _ = phases.send(if rejected {
                    if email {
                        AuthPhase::CodeRejectedEmail
                    } else {
                        AuthPhase::CodeRejectedDevice
                    }
                } else {
                    initial_phase
                });
                let code = codes.recv().await.ok_or(AuthError::Cancelled)?;
                let code = code.expose().trim();
                if code.is_empty()
                    || code.len() > 32
                    || !code.bytes().all(|b| b.is_ascii_alphanumeric())
                {
                    rejected = true;
                    continue;
                }
                let kind = if email {
                    GuardType::EmailCode
                } else {
                    GuardType::DeviceCode
                };
                let _ = phases.send(AuthPhase::Authenticating);
                match tokio::time::timeout(NETWORK_TIMEOUT, challenge.submit_code(code, kind))
                    .await
                    .map_err(|_| AuthError::TimedOut)?
                {
                    Ok(result) => {
                        approved = Some(result);
                        break;
                    }
                    Err((returned, error)) if is_rejected_guard_code(&error) => {
                        challenge = returned;
                        rejected = true;
                    }
                    Err((_returned, error)) => return Err(map_login_error(error)),
                }
            }
            approved.ok_or(AuthError::RejectedCode)?
        }
        _ => return Err(AuthError::UnsupportedChallenge),
    };

    let tokens = approved.tokens();
    let account_name = tokens
        .account_name
        .as_deref()
        .ok_or(AuthError::AuthenticationRequired)?;
    if !valid_account(account_name) || !valid_refresh_token(&tokens.refresh_token) {
        return Err(AuthError::AuthenticationRequired);
    }
    let credential = PreparedCredential {
        account_name: Secret::new(account_name.to_owned()),
        refresh_token: Secret::new(tokens.refresh_token.clone()),
    };
    let _ = phases.send(AuthPhase::Authenticating);
    drop(approved);
    Ok(credential)
}

async fn native_login(
    account_name: &str,
    refresh_token: &str,
    login_id: u32,
) -> Result<SteamClient<LoggedIn>, AuthError> {
    let servers = match tokio::time::timeout(NETWORK_TIMEOUT, CmServer::fetch()).await {
        Ok(Ok(servers)) if !servers.is_empty() => servers,
        Ok(Err(_)) | Err(_) => CmServer::defaults(),
        Ok(Ok(_)) => CmServer::defaults(),
    };
    let mut ordered = Vec::new();
    ordered.extend(
        servers
            .iter()
            .filter(|server| server.protocol == Protocol::WebSocket)
            .take(2)
            .cloned(),
    );
    ordered.extend(
        servers
            .iter()
            .filter(|server| server.protocol == Protocol::Tcp)
            .take(2)
            .cloned(),
    );

    if ordered.is_empty() {
        return Err(AuthError::Network);
    }

    let mut last_error = AuthError::Network;
    for server in ordered {
        let client = match tokio::time::timeout(NETWORK_TIMEOUT, connect_ready(&server)).await {
            Ok(Ok(client)) => client,
            Ok(Err(_)) => continue,
            Err(_) => {
                last_error = AuthError::TimedOut;
                continue;
            }
        };
        let body = encode_logon(login_id, account_name, refresh_token);
        let mut message = steamroom::client::msg::ClientMsg::with_body(EMsg::CLIENT_LOGON, &body);
        message.header.steamid = Some(SteamId::from_parts(1, 1, 1, 0).raw());
        message.header.client_sessionid = Some(0);
        match tokio::time::timeout(NETWORK_TIMEOUT, client.login(message)).await {
            Ok(Ok((client, _response))) => return Ok(client),
            Ok(Err(error)) => last_error = map_native_login_error(error),
            Err(_) => last_error = AuthError::TimedOut,
        }
        if matches!(last_error, AuthError::RejectedCredentials) {
            return Err(last_error);
        }
    }
    Err(last_error)
}

async fn connect_ready(server: &CmServer) -> Result<SteamClient<Ready>, steamroom::Error> {
    match server.protocol {
        Protocol::WebSocket => {
            let transport = WebSocketTransport::connect(server).await?;
            let (client, _receiver) = SteamClient::connect_ws(transport).await?;
            client.prepare().await
        }
        Protocol::Tcp => {
            let transport = TcpTransport::connect(server).await?;
            let (client, _receiver) = SteamClient::connect(transport).await?;
            client.encrypt().await?.prepare().await
        }
    }
}

fn encode_logon(login_id: u32, account_name: &str, refresh_token: &str) -> Vec<u8> {
    let message = generated::CMsgClientLogon {
        protocol_version: Some(PROTOCOL_VERSION),
        deprecated_obfustucated_private_ip: Some(login_id),
        obfuscated_private_ip: Some(generated::CMsgIpAddress {
            ip: Some(generated::c_msg_ip_address::Ip::V4(login_id)),
        }),
        client_os_type: Some(client_os() as u32),
        account_name: Some(account_name.to_owned()),
        access_token: Some(refresh_token.to_owned()),
        ..Default::default()
    };
    message.encode_to_vec()
}

fn client_os() -> i32 {
    if cfg!(target_os = "linux") { -203 } else { 20 }
}

fn map_native_login_error(error: steamroom::Error) -> AuthError {
    match error {
        steamroom::Error::Connection(steamroom::error::ConnectionError::LogonFailed(
            steamroom::enums::EResultError::InvalidPassword,
        )) => AuthError::RejectedCredentials,
        steamroom::Error::Connection(steamroom::error::ConnectionError::LogonFailed(
            steamroom::enums::EResultError::NoConnection
            | steamroom::enums::EResultError::Timeout
            | steamroom::enums::EResultError::ServiceUnavailable
            | steamroom::enums::EResultError::Fail
            | steamroom::enums::EResultError::Busy,
        )) => AuthError::Network,
        steamroom::Error::Connection(steamroom::error::ConnectionError::LogonFailed(_)) => {
            AuthError::RejectedCredentials
        }
        steamroom::Error::Connection(steamroom::error::ConnectionError::Io(ref io_error))
            if io_error.kind() == std::io::ErrorKind::TimedOut =>
        {
            AuthError::TimedOut
        }
        _ => AuthError::Network,
    }
}

fn map_login_error(error: LoginError) -> AuthError {
    match error {
        LoginError::InvalidPassword => AuthError::RejectedCredentials,
        LoginError::InvalidGuardCode => AuthError::RejectedCode,
        LoginError::LogonFailed(steamroom::enums::EResultError::InvalidPassword) => {
            AuthError::RejectedCredentials
        }
        LoginError::LogonFailed(_) => AuthError::RejectedCredentials,
        LoginError::Transport(steamroom::Error::Connection(
            steamroom::error::ConnectionError::ServiceMethodFailed(
                steamroom::enums::EResultError::InvalidPassword,
            ),
        )) => AuthError::RejectedCredentials,
        LoginError::NoCmServers | LoginError::Transport(_) => AuthError::Network,
        LoginError::MissingField(_) => AuthError::AuthenticationRequired,
        _ => AuthError::Network,
    }
}

fn is_rejected_guard_code(error: &LoginError) -> bool {
    matches!(error, LoginError::InvalidGuardCode)
        // steamroom 0.3 leaves EResult 65 and 71 as Unknown.
        || matches!(
            error,
            LoginError::Transport(steamroom::Error::Connection(
                steamroom::error::ConnectionError::ServiceMethodFailed(
                    steamroom::enums::EResultError::TwoFactorCodeMismatch
                        | steamroom::enums::EResultError::Unknown(65 | 71)
                )
            ))
        )
}

fn valid_account(account: &str) -> bool {
    !account.trim().is_empty()
        && account.len() <= MAX_ACCOUNT_BYTES
        && !account.chars().any(char::is_control)
}

fn valid_password(password: &str) -> bool {
    !password.is_empty()
        && password.len() <= MAX_PASSWORD_BYTES
        && !password.chars().any(char::is_control)
}

fn valid_refresh_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_REFRESH_TOKEN_BYTES
        && !token.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_and_expired_guard_results_are_retryable_but_transport_errors_are_not() {
        use steamroom::enums::EResultError;
        use steamroom::error::ConnectionError;

        for code in [
            EResultError::TwoFactorCodeMismatch,
            EResultError::Unknown(65),
            EResultError::Unknown(71),
        ] {
            assert!(is_rejected_guard_code(&LoginError::Transport(
                steamroom::Error::Connection(ConnectionError::ServiceMethodFailed(code))
            )));
        }
        assert!(!is_rejected_guard_code(&LoginError::Transport(
            steamroom::Error::Connection(ConnectionError::DnsResolutionFailed)
        )));
    }

    #[test]
    fn input_bounds_preserve_unicode_and_password_punctuation() {
        assert!(valid_account("steam-user-é"));
        assert!(valid_password("pāss word!#$"));
        assert!(!valid_account("bad\naccount"));
        assert!(!valid_password("bad\tpassword"));
        assert!(!valid_account(&"a".repeat(MAX_ACCOUNT_BYTES + 1)));
        assert!(!valid_password(&"x".repeat(MAX_PASSWORD_BYTES + 1)));
        assert!(!valid_refresh_token("bad\ntoken"));
        assert!(!valid_refresh_token(
            &"t".repeat(MAX_REFRESH_TOKEN_BYTES + 1)
        ));
    }

    #[test]
    fn native_logon_encodes_unique_identity_and_keeps_tokens_out_of_debug_output() {
        let message = encode_logon(0x1234_5678, "synthetic-account", "synthetic-token");
        let decoded = generated::CMsgClientLogon::decode(message.as_slice()).unwrap();
        assert_eq!(decoded.protocol_version, Some(PROTOCOL_VERSION));
        assert_eq!(
            decoded.deprecated_obfustucated_private_ip,
            Some(0x1234_5678)
        );
        assert_eq!(
            decoded.obfuscated_private_ip.unwrap().ip,
            Some(generated::c_msg_ip_address::Ip::V4(0x1234_5678))
        );
        assert_eq!(decoded.account_name.as_deref(), Some("synthetic-account"));
        assert_eq!(decoded.access_token.as_deref(), Some("synthetic-token"));
        let input = LoginInput::Prepared(PreparedCredential {
            account_name: Secret::new("synthetic-account".into()),
            refresh_token: Secret::new("synthetic-token".into()),
        });
        assert!(!format!("{input:?}").contains("synthetic-token"));
    }

    #[test]
    fn login_input_secrets_remain_redacted() {
        let input = LoginInput::Password {
            account_name: Secret::new("private-account".into()),
            password: Secret::new("private-password".into()),
        };
        assert!(!format!("{input:?}").contains("private"));
        assert_eq!(
            serde_json::to_string(&AuthPhase::MobileApproval).unwrap(),
            "\"mobile_approval\""
        );
    }

    #[tokio::test]
    async fn already_cancelled_authentication_does_not_start_login() {
        let (phase_tx, phase_rx) = watch::channel(AuthPhase::Connecting);
        let (code_tx, code_rx) = mpsc::channel(1);
        let (cancel_tx, cancel_rx) = watch::channel(true);
        drop(code_tx);
        let result = authenticate(LoginInput::Anonymous, phase_tx, code_rx, cancel_rx).await;
        assert_eq!(result.err(), Some(AuthError::Cancelled));
        assert_eq!(*phase_rx.borrow(), AuthPhase::Connecting);
        drop(cancel_tx);
    }
}
