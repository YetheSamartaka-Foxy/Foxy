use super::*;
use crate::core::server::{ServerDefinition, ServerStore};
use crate::core::tasks::db_process_lock;
use std::fs;

fn fixture() -> (tempfile::TempDir, ServerStore) {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("games.json"),
        include_bytes!("../../../../examples/json/appdata/games.json"),
    )
    .unwrap();
    let store =
        ServerStore::new(ServerContext::capture(root.path(), Some("arma3")).unwrap()).unwrap();
    (root, store)
}

#[test]
fn library_refresh_retains_stable_source_identity_and_immutable_revisions() {
    if std::env::var_os("FOXY_MISSION_LIBRARY_CLAIM_TEST").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "core::server::missions::tests::library_refresh_retains_stable_source_identity_and_immutable_revisions",
                "--nocapture",
            ])
            .env("FOXY_MISSION_LIBRARY_CLAIM_TEST", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (root, store) = fixture();
    let source = root.path().join("Example.World");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("mission.sqm"), "briefingName=\"First\";").unwrap();
    let original = fs::read(source.join("mission.sqm")).unwrap();
    let control = ContentStageControl::default();
    let plan = store.plan_mission_import(&source, &control).unwrap();
    assert!(
        !store
            .context()
            .space_dir()
            .join("server/mission-library")
            .exists()
    );
    assert_eq!(
        db_process_lock::acquire(&store.context().database_path()),
        db_process_lock::LockOutcome::Acquired
    );
    let first = store.apply_mission_import(plan, &control).unwrap();
    assert_eq!(fs::read(source.join("mission.sqm")).unwrap(), original);
    let plan = store
        .plan_mission_refresh(&first.mission_id, &control)
        .unwrap();
    fs::write(source.join("mission.sqm"), "briefingName=\"Later\";").unwrap();
    assert_eq!(
        store.apply_mission_import(plan, &control).unwrap_err(),
        ServerStoreError::StaleRevision
    );
    let later = store
        .apply_mission_import(
            store
                .plan_mission_refresh(&first.mission_id, &control)
                .unwrap(),
            &control,
        )
        .unwrap();
    assert_eq!(first.mission_id, later.mission_id);
    assert_ne!(first.revision_id, later.revision_id);
    assert_eq!(
        store
            .mission_revision(&first.mission_id, &first.revision_id, &control)
            .unwrap(),
        first
    );
    assert_eq!(
        store.mission_library(&control).unwrap().entries,
        vec![later]
    );
}

#[test]
fn enabled_cycles_render_in_order_and_autoinit_params_warning_is_explicit() {
    let mut instance = crate::core::server::ServerInstance::draft(
        "arma3",
        crate::core::server::ServerDefinitionKind::Arma3,
        "MainServer",
    )
    .unwrap();
    let ServerDefinition::Arma3(arma) = &mut instance.definition else {
        unreachable!()
    };
    arma.mission_cycle=serde_json::from_str(r#"[{"template":"First.World","difficulty":"regular","params":{"mode":3}},{"template":"Disabled.World","difficulty":"veteran","enabled":false},{"template":"Last.World","difficulty":"custom"}]"#).unwrap();
    let preview = super::super::configuration::render(&instance).unwrap();
    assert!(
        preview.files[0].contents.find("First.World").unwrap()
            < preview.files[0].contents.find("Last.World").unwrap()
    );
    assert!(!preview.files[0].contents.contains("Disabled.World"));
    assert!(preview.files[0].contents.contains("mode = 3;"));
    let ServerDefinition::Arma3(arma) = &mut instance.definition else {
        unreachable!()
    };
    arma.auto_init = true;
    arma.persistent = true;
    assert!(
        super::super::configuration::render(&instance)
            .unwrap()
            .warnings
            .iter()
            .any(|warning| warning.contains("Auto-init with custom mission Params"))
    );
}

#[test]
fn invalid_templates_and_library_refs_cannot_become_paths() {
    let sample: Vec<crate::core::server::Arma3Mission> = serde_json::from_str(include_str!(
        "../../../../examples/json/server_backend/arma3_mission_cycle.json"
    ))
    .unwrap();
    assert!(sample[0].enabled);
    assert!(!sample[1].enabled);
    sample[0].source.as_ref().unwrap().validate().unwrap();
    for bad in [
        "../Example.World",
        "Example/World",
        "Example.World.pbo/evil",
        ".World",
        "Example.",
        "boot.sqf",
    ] {
        assert!(!valid_template(bad));
    }
    assert!(valid_template("Coop_01.World"));
    assert!(
        Arma3MissionSource::Library {
            mission_id: "../outside".into(),
            revision_id: format!("revision_{}", "a".repeat(64))
        }
        .validate()
        .is_err()
    );
}
