use std::time::{Duration, Instant};

use crate::core::server::deployment_activation::NativeRconPlan;
use crate::core::server::rcon::records::{
    self, RconCommandReason, RconCommandRecord, RconCommandState,
};
use crate::core::server::rcon::{
    self, CommandOutcome, MAX_COMMAND_BYTES, RconPassword, Retransmit, Session,
};
use crate::core::server::{ContentStageControl, ServerInstanceId, ServerStore, ServerStoreError};
use foxy_supervisor::control::Scope;

#[allow(clippy::too_many_arguments)]
pub(in crate::core::server) fn command_once(
    store: &ServerStore,
    id: &ServerInstanceId,
    scope: &Scope,
    run_generation: u64,
    plan: &NativeRconPlan,
    identity: &str,
    text: &str,
    timeout: Duration,
    control: &ContentStageControl,
) -> Result<RconCommandRecord, ServerStoreError> {
    if scope.validate().is_err()
        || scope.instance_id != "space_catalog"
        || scope.context_key
            != crate::core::server::owner::context_key(store.context())
                .map_err(|_| ServerStoreError::Ownership)?
        || scope.space_id != store.context().space_id()
        || run_generation == 0
        || identity.is_empty()
        || identity.len() > 128
        || !identity
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        || !foxy_supervisor::control::valid_rcon_text(text)
        || text.len() > MAX_COMMAND_BYTES
        || rcon::carries_secret(text)
    {
        return Err(ServerStoreError::Invalid(
            "Invalid or secret-bearing owned RCon command".into(),
        ));
    }

    let command_id = owned_command_id(scope, identity, run_generation);
    let mut record = RconCommandRecord::new(
        command_id.clone(),
        id,
        &scope.owner_id,
        scope.generation,
        run_generation,
        text,
    );
    let root = store.rcon_command_root(id);
    if !records::claim(&root, &record).map_err(|_| ServerStoreError::Io)? {
        let existing = records::read(&root, &command_id)
            .map_err(|_| ServerStoreError::RconOutcomeUnknown)?
            .ok_or(ServerStoreError::RconOutcomeUnknown)?;
        if existing.instance_id != id.as_str()
            || existing.owner_id != scope.owner_id
            || existing.owner_generation != scope.generation
            || existing.run_generation != run_generation
            || existing.text != text
        {
            return Err(ServerStoreError::RconOutcomeUnknown);
        }
        return Ok(existing);
    }

    let deadline = Instant::now() + timeout.min(Duration::from_secs(60));
    if control.snapshot().cancel_requested {
        return finish(&root, &mut record, RconCommandState::NotSent, None);
    }

    let secret = match store.resolve_config_secret(id, &plan.password_reference) {
        Ok(secret) => secret,
        Err(_) => {
            return finish(
                &root,
                &mut record,
                RconCommandState::NotSent,
                Some(RconCommandReason::NotConnected),
            );
        }
    };
    let password = match RconPassword::new(secret) {
        Ok(password) => password,
        Err(_) => {
            return finish(
                &root,
                &mut record,
                RconCommandState::NotSent,
                Some(RconCommandReason::NotConnected),
            );
        }
    };
    if control.snapshot().cancel_requested {
        drop(password);
        return finish(&root, &mut record, RconCommandState::NotSent, None);
    }
    let login_budget = deadline.saturating_duration_since(Instant::now());
    if login_budget.is_zero() {
        drop(password);
        return finish(&root, &mut record, RconCommandState::NotSent, None);
    }
    let mut session = match Session::login(plan.endpoint, &password, login_budget) {
        Ok(session) => session,
        Err(_) => {
            drop(password);
            return finish(
                &root,
                &mut record,
                RconCommandState::NotSent,
                Some(RconCommandReason::NotConnected),
            );
        }
    };
    drop(password);

    let command_budget = deadline.saturating_duration_since(Instant::now());
    if control.snapshot().cancel_requested || command_budget.is_zero() {
        return finish(&root, &mut record, RconCommandState::NotSent, None);
    }
    record.state = RconCommandState::Sending;
    record.sent_unix_ms = Some(rcon::unix_ms());
    record.session_generation = Some(1);
    records::update(&root, &record).map_err(|_| ServerStoreError::Io)?;

    if control.snapshot().cancel_requested {
        return finish(&root, &mut record, RconCommandState::NotSent, None);
    }
    let outcome = if plan.reforger.is_some() {
        let (outcome, messages) =
            rcon::reforger::transcript::command(&mut session, text, command_budget);
        record.messages = messages;
        outcome
    } else {
        session.command(text.as_bytes(), command_budget, Retransmit::Never)
    };
    let result = match outcome {
        CommandOutcome::Replied(response) => {
            record.set_response(rcon::response_text(&response));
            finish(&root, &mut record, RconCommandState::Replied, None)
        }
        CommandOutcome::Unknown(reason) => finish(
            &root,
            &mut record,
            RconCommandState::Unknown,
            Some(reason.into()),
        ),
        CommandOutcome::NotSent => finish(
            &root,
            &mut record,
            RconCommandState::NotSent,
            Some(RconCommandReason::SendFailed),
        ),
    };
    result.map_err(|_| ServerStoreError::RconOutcomeUnknown)
}

fn owned_command_id(scope: &Scope, identity: &str, run_generation: u64) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"foxy owned rcon command v1\0");
    hasher.update(&run_generation.to_le_bytes());
    hasher.update(b"\0");
    hasher.update(identity.as_bytes());
    let digest = hasher.finalize();
    let mut request_bytes = [0; 8];
    request_bytes.copy_from_slice(&digest.as_bytes()[..8]);
    let request_id = u64::from_le_bytes(request_bytes);
    records::command_id(&scope.owner_id, scope.generation, "owned", request_id)
}

fn finish(
    root: &std::path::Path,
    record: &mut RconCommandRecord,
    state: RconCommandState,
    reason: Option<RconCommandReason>,
) -> Result<RconCommandRecord, ServerStoreError> {
    record.finish(state, reason);
    if state == RconCommandState::NotSent {
        record.sent_unix_ms = None;
    }
    records::update(root, record).map_err(|_| ServerStoreError::Io)?;
    Ok(record.clone())
}

#[cfg(test)]
mod tests {
    use crate::core::server::ServerContext;
    use crate::core::server::rcon::tests::{accepting, echo};
    use std::fs;

    use super::*;

    fn fixture() -> (tempfile::TempDir, ServerStore, ServerInstanceId, String) {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("games.json"),
            include_bytes!("../../../../examples/json/appdata/games.json"),
        )
        .unwrap();
        let space_dir = root.path().join("games/arma3/server");
        fs::create_dir_all(&space_dir).unwrap();
        let catalog_path = space_dir.join("servers.json");
        let mut catalog: serde_json::Value = serde_json::from_slice(include_bytes!(
            "../../../../examples/json/appdata/games/arma3/server/servers.json"
        ))
        .unwrap();
        let reference = format!("secret_{}", "a".repeat(32));
        catalog["instances"][0]["definition"]["settings"]["rcon"] =
            serde_json::json!({"port": 2320, "password_ref": reference});
        fs::write(&catalog_path, serde_json::to_vec_pretty(&catalog).unwrap()).unwrap();
        let store =
            ServerStore::new(ServerContext::capture(root.path(), Some("arma3")).unwrap()).unwrap();
        let id = store.load().unwrap().instances[0].id.clone();
        fs::create_dir_all(store.record_root(&id)).unwrap();
        store
            .store_config_secret(&id, &reference, "synthetic-pass")
            .unwrap();
        (root, store, id, reference)
    }

    fn scope(store: &ServerStore) -> Scope {
        Scope {
            context_key: crate::core::server::owner::context_key(store.context()).unwrap(),
            space_id: "arma3".into(),
            instance_id: "space_catalog".into(),
            owner_id: "owner_test".into(),
            generation: 7,
        }
    }

    fn plan(address: std::net::SocketAddr, reference: &str) -> NativeRconPlan {
        NativeRconPlan {
            endpoint: address,
            password_reference: reference.into(),
            reforger: None,
        }
    }

    #[test]
    fn repeated_owned_command_identity_returns_unknown_without_resending() {
        let (_root, store, id, reference) = fixture();
        let server = accepting(|_socket, _peer, _kind, _payload| {});
        let target = plan(server.address, &reference);
        let scope = scope(&store);
        let control = ContentStageControl::default();
        let first = command_once(
            &store,
            &id,
            &scope,
            3,
            &target,
            "shutdown",
            "#shutdown",
            Duration::from_millis(200),
            &control,
        )
        .unwrap();
        assert_eq!(first.state, RconCommandState::Unknown);
        let second = command_once(
            &store,
            &id,
            &scope,
            3,
            &target,
            "shutdown",
            "#shutdown",
            Duration::from_secs(1),
            &control,
        )
        .unwrap();
        assert_eq!(second, first);
        let sent: Vec<_> = server
            .received()
            .into_iter()
            .filter(|(kind, payload)| *kind == 0x01 && payload.get(1..) == Some(&b"#shutdown"[..]))
            .collect();
        assert_eq!(sent.len(), 1);
    }

    #[test]
    fn cancelled_owned_command_is_persisted_not_sent_before_login() {
        let (_root, store, id, reference) = fixture();
        let server = accepting(echo);
        let target = plan(server.address, &reference);
        let scope = scope(&store);
        let control = ContentStageControl::default();
        control.cancel();
        let record = command_once(
            &store,
            &id,
            &scope,
            3,
            &target,
            "shutdown",
            "#shutdown",
            Duration::from_secs(1),
            &control,
        )
        .unwrap();
        assert_eq!(record.state, RconCommandState::NotSent);
        assert_eq!(record.reason, None);
        assert!(!server.received().iter().any(|(kind, _)| *kind == 0x01));
        assert!(!server.received().iter().any(|(kind, _)| *kind == 0x00));
    }

    #[test]
    fn an_unreadable_existing_effect_is_unknown_and_never_replaced_or_sent() {
        let (_root, store, id, reference) = fixture();
        let server = accepting(echo);
        let target = plan(server.address, &reference);
        let scope = scope(&store);
        let root = store.rcon_command_root(&id);
        fs::create_dir_all(&root).unwrap();
        let path = root.join(format!("{}.json", owned_command_id(&scope, "shutdown", 3)));
        fs::write(&path, b"{malformed").unwrap();
        assert_eq!(
            command_once(
                &store,
                &id,
                &scope,
                3,
                &target,
                "shutdown",
                "#shutdown",
                Duration::from_secs(1),
                &ContentStageControl::default()
            ),
            Err(ServerStoreError::RconOutcomeUnknown)
        );
        assert_eq!(fs::read(&path).unwrap(), b"{malformed");
        assert!(server.received().is_empty());
    }
}
