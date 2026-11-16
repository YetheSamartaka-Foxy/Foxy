use super::*;
use foxy_supervisor::recovery::{CleanupBinding, ConsumerRecord};

fn row(
    group: &str,
    context: &str,
    owner: &str,
    generation: u64,
    space: &str,
    present: bool,
    state: &'static str,
) -> ConsumerCleanupInspection {
    let consumer = ConsumerRecord {
        schema_version: 2,
        installation_key: "synthetic-installation".into(),
        session: foxy_supervisor::session::SessionIdentity {
            owner_id: owner.into(),
            owner_generation: generation,
            game_space_id: space.into(),
            instance_id: "server_00000000000000000000000000000001".into(),
            operation_id: "op-test".into(),
            run_generation: 1,
        },
        cleanup: Some(CleanupBinding {
            receipt_id: "00000000000000000000000000000001".into(),
            group_id: group.into(),
            context_key: context.into(),
        }),
    };
    ConsumerCleanupInspection {
        consumer,
        consumer_fingerprint: "a".repeat(64),
        registration_present: present,
        receipt_fingerprint: Some("b".repeat(64)),
        receipt_state: state,
        observation: None,
    }
}

fn fingerprint(
    catalog_fingerprint: &str,
    id: &ServerInstanceId,
    installation: &InstallationId,
    scope: &foxy_supervisor::control::Scope,
    entries: &[ConsumerRecoveryEntry],
    raw: &[RawFingerprint],
) -> String {
    plan_fingerprint(FingerprintInput {
        catalog_fingerprint,
        instance: id,
        installation,
        space: "alpha",
        game: "arma3",
        owner: Some(scope),
        inspection_complete: true,
        entries,
        raw,
    })
    .unwrap()
}

#[test]
fn eligibility_requires_exact_group_space_and_verified_old_owner_registration() {
    let id =
        ServerInstanceId::try_from("server_00000000000000000000000000000001".to_string()).unwrap();
    let scope = foxy_supervisor::control::Scope {
        context_key: "c".repeat(64),
        space_id: "alpha".into(),
        instance_id: "space_catalog".into(),
        owner_id: "owner-current".into(),
        generation: 9,
    };
    let rows = [
        row(
            id.as_str(),
            &scope.context_key,
            "owner-old",
            2,
            "alpha",
            true,
            "verified_exited",
        ),
        row(
            id.as_str(),
            &scope.context_key,
            "owner-current",
            9,
            "alpha",
            true,
            "verified_exited",
        ),
        row(
            "server_00000000000000000000000000000002",
            &scope.context_key,
            "owner-old",
            2,
            "alpha",
            true,
            "verified_exited",
        ),
        row(
            id.as_str(),
            &scope.context_key,
            "owner-old",
            2,
            "beta",
            true,
            "verified_exited",
        ),
        row(
            id.as_str(),
            &scope.context_key,
            "owner-old",
            2,
            "alpha",
            true,
            "pending",
        ),
        row(
            id.as_str(),
            &scope.context_key,
            "owner-old",
            2,
            "alpha",
            false,
            "verified_history",
        ),
        row(
            id.as_str(),
            &"d".repeat(64),
            "owner-old",
            2,
            "alpha",
            true,
            "verified_exited",
        ),
    ];
    let (entries, _) = classify_rows(&rows, &id, "alpha", Some(&scope), true);
    assert_eq!(entries.iter().filter(|entry| entry.eligible).count(), 1);
    assert!(entries[0].eligible);
    assert!(!entries[1].eligible);
    assert!(!entries[2].eligible);
    assert!(!entries[3].eligible);
    assert!(!entries[4].eligible);
    assert!(!entries[5].eligible);
    assert_eq!(entries[5].reason, "receipt history without a registration");
    assert!(!entries[6].eligible);
    assert_eq!(entries[6].reason, "foreign configuration root");
    assert!(
        classify_rows(&rows, &id, "alpha", Some(&scope), false)
            .0
            .iter()
            .all(|entry| !entry.eligible)
    );
    assert!(
        classify_rows(&rows, &id, "alpha", None, true)
            .0
            .iter()
            .all(|entry| !entry.eligible)
    );
    assert!(
        classify_rows(&rows, &id, "alpha", None, true)
            .0
            .iter()
            .all(|entry| entry.reason == "owner is not authenticated")
    );
    let installation =
        InstallationId::try_from("install_00000000000000000000000000000001".to_string()).unwrap();
    let (entries, mut raw) = classify_rows(&rows, &id, "alpha", Some(&scope), true);
    let first = fingerprint("catalog-a", &id, &installation, &scope, &entries, &raw);
    raw[0].receipt = Some("c".repeat(64));
    let changed = fingerprint("catalog-a", &id, &installation, &scope, &entries, &raw);
    assert_ne!(first, changed);
    let changed_catalog = fingerprint("catalog-b", &id, &installation, &scope, &entries, &raw);
    assert_ne!(changed, changed_catalog);
}

#[test]
fn one_pending_member_preserves_the_whole_selected_group() {
    let id =
        ServerInstanceId::try_from("server_00000000000000000000000000000001".to_string()).unwrap();
    let scope = foxy_supervisor::control::Scope {
        context_key: "c".repeat(64),
        space_id: "alpha".into(),
        instance_id: "space_catalog".into(),
        owner_id: "current".into(),
        generation: 9,
    };
    let mut rows = vec![
        row(
            id.as_str(),
            &scope.context_key,
            "old",
            2,
            "alpha",
            true,
            "verified_exited",
        ),
        row(
            id.as_str(),
            &scope.context_key,
            "old",
            2,
            "alpha",
            true,
            "pending",
        ),
    ];
    let (mut entries, _) = classify_rows(&rows, &id, "alpha", Some(&scope), true);
    assert!(entries[0].eligible);
    assert!(restrict_unverified_group(
        &rows,
        &id,
        "alpha",
        Some(&scope),
        &mut entries
    ));
    assert!(entries.iter().all(|entry| !entry.eligible));
    rows[1].consumer.cleanup.as_mut().unwrap().context_key = "d".repeat(64);
    let (mut entries, _) = classify_rows(&rows, &id, "alpha", Some(&scope), true);
    assert!(!restrict_unverified_group(
        &rows,
        &id,
        "alpha",
        Some(&scope),
        &mut entries
    ));
    assert!(entries[0].eligible);
}
