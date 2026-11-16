use super::*;
use crate::core::server::content_staging::{StageEntry, StageEntryKind};
use crate::core::server::{ServerContext, ServerDefinitionKind, ServerInstance};
use std::fs;

struct Fixture {
    _root: tempfile::TempDir,
    store: ServerStore,
    id: ServerInstanceId,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("games.json"),
        serde_json::json!({
            "active_game_space_id":"alpha",
            "last_opened_game_space_id":"alpha",
            "game_spaces":[{"id":"alpha","game_id":"arma3","display_name":"Alpha","created_at":0}]
        })
        .to_string(),
    )
    .unwrap();
    fs::create_dir_all(root.path().join("games/alpha/server/instances")).unwrap();
    fs::create_dir_all(root.path().join("games/alpha")).unwrap();
    let store =
        ServerStore::new(ServerContext::capture(root.path(), Some("alpha")).unwrap()).unwrap();
    let instance = ServerInstance::draft("alpha", ServerDefinitionKind::Arma3, "Fixture").unwrap();
    let id = instance.id.clone();
    let catalog = crate::core::server::ServerCatalog {
        schema_version: crate::core::server::definition::SERVER_SCHEMA_VERSION,
        game_space_id: "alpha".into(),
        game_id: "arma3".into(),
        revision: 1,
        instances: vec![instance],
        installations: Vec::new(),
    };
    fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    fs::write(store.path(), serde_json::to_vec(&catalog).unwrap()).unwrap();
    fs::create_dir_all(store.record_root(&id)).unwrap();
    Fixture {
        _root: root,
        store,
        id,
    }
}

fn file_entry(path: &str, bytes: &[u8]) -> StageEntry {
    StageEntry {
        path: path.into(),
        kind: StageEntryKind::File,
        bytes: bytes.len() as u64,
        blake3: Some(blake3::hash(bytes).to_hex().to_string()),
    }
}

fn stage_name() -> String {
    format!("stage_{}", "a".repeat(64))
}

#[test]
fn receipt_resumes_a_partially_removed_quarantine_subset() {
    let fixture = fixture();
    let quarantine = fixture
        .store
        .record_root(&fixture.id)
        .join(".reconcile-test");
    fs::create_dir_all(&quarantine).unwrap();
    fs::write(quarantine.join("manifest.json"), b"manifest bytes").unwrap();
    fs::write(quarantine.join("payload.bin"), b"payload").unwrap();
    let mut entries = vec![
        file_entry("manifest.json", b"manifest bytes"),
        file_entry("payload.bin", b"payload"),
    ];
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let receipt = super::quarantine::Receipt::new(
        &fixture.store,
        &fixture.id,
        ReconciliationArtifact::ContentStage,
        stage_name(),
        entries,
        None,
    );
    receipt
        .save(&fixture.store, &ContentStageControl::default())
        .unwrap();
    let name = receipt.name().unwrap();
    let loaded = super::quarantine::read(
        &fixture.store,
        &fixture.id,
        ReconciliationArtifact::ContentStage,
        &name,
        &ContentStageControl::default(),
    )
    .unwrap();

    fs::remove_file(quarantine.join("payload.bin")).unwrap();
    let remaining = super::inventory::remaining(
        &quarantine,
        &loaded.entries,
        &ContentStageControl::default(),
    )
    .unwrap();
    assert_eq!(remaining.entries.len(), 1);
    assert_eq!(remaining.entries[0].path, "manifest.json");
    super::quarantine::remove(
        &quarantine,
        &remaining.entries,
        &ContentStageControl::default(),
    )
    .unwrap();
    assert!(!quarantine.exists());
}

#[test]
fn receipt_rejects_same_size_changes_and_added_entries() {
    let fixture = fixture();
    let quarantine = fixture
        .store
        .record_root(&fixture.id)
        .join(".reconcile-test");
    fs::create_dir_all(&quarantine).unwrap();
    fs::write(quarantine.join("manifest.json"), b"manifest").unwrap();
    fs::write(quarantine.join("payload.bin"), b"first").unwrap();
    let mut entries = vec![
        file_entry("manifest.json", b"manifest"),
        file_entry("payload.bin", b"first"),
    ];
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let control = ContentStageControl::default();
    assert!(super::inventory::remaining(&quarantine, &entries, &control).is_ok());

    fs::write(quarantine.join("payload.bin"), b"other").unwrap();
    assert!(super::inventory::remaining(&quarantine, &entries, &control).is_err());
    fs::write(quarantine.join("payload.bin"), b"first").unwrap();
    fs::write(quarantine.join("operator-note"), b"extra").unwrap();
    assert!(super::inventory::remaining(&quarantine, &entries, &control).is_err());
}

#[test]
fn receipt_scope_and_cancellation_preserve_unfinished_entries() {
    let fixture = fixture();
    let quarantine = fixture
        .store
        .record_root(&fixture.id)
        .join(".reconcile-test");
    fs::create_dir_all(&quarantine).unwrap();
    fs::write(quarantine.join("payload.bin"), b"payload").unwrap();
    let entries = vec![file_entry("payload.bin", b"payload")];
    let receipt = super::quarantine::Receipt::new(
        &fixture.store,
        &fixture.id,
        ReconciliationArtifact::ContentStage,
        stage_name(),
        entries.clone(),
        None,
    );
    receipt
        .save(&fixture.store, &ContentStageControl::default())
        .unwrap();
    let name = receipt.name().unwrap();
    assert!(
        super::quarantine::read(
            &fixture.store,
            &fixture.id,
            ReconciliationArtifact::Preparation,
            &name,
            &ContentStageControl::default(),
        )
        .is_err()
    );
    let other_id = ServerInstanceId::new();
    assert!(
        super::quarantine::read(
            &fixture.store,
            &other_id,
            ReconciliationArtifact::ContentStage,
            &name,
            &ContentStageControl::default(),
        )
        .is_err()
    );
    assert!(
        super::quarantine::read(
            &fixture.store,
            &fixture.id,
            ReconciliationArtifact::ContentStage,
            &format!(".reconcile-{}", "f".repeat(64)),
            &ContentStageControl::default(),
        )
        .is_err()
    );

    let control = ContentStageControl::default();
    control.cancel();
    assert_eq!(
        super::quarantine::remove(&quarantine, &entries, &control),
        Err(ServerStoreError::Cancelled)
    );
    assert_eq!(
        fs::read(quarantine.join("payload.bin")).unwrap(),
        b"payload"
    );
}

#[test]
fn reconciliation_examples_match_strict_documents() {
    for (json, space, id) in [
        (
            include_str!(
                "../../../../examples/json/appdata/games/arma3/server/instances/server_00000000000000000000000000000001/deployments/pins.json"
            ),
            "arma3",
            "server_00000000000000000000000000000001",
        ),
        (
            include_str!(
                "../../../../examples/json/appdata/games/reforger/server/instances/server_00000000000000000000000000000002/deployments/pins.json"
            ),
            "reforger",
            "server_00000000000000000000000000000002",
        ),
    ] {
        let document: PinDocument = serde_json::from_str(json).unwrap();
        assert_eq!(document.schema_version, 1);
        assert_eq!(document.game_space_id, space);
        assert_eq!(document.instance_id.as_str(), id);
        assert_eq!(document.generation, 1);
        assert!(document.records.is_empty());
        let mut value: serde_json::Value = serde_json::from_str(json).unwrap();
        value["unexpected"] = true.into();
        assert!(serde_json::from_value::<PinDocument>(value).is_err());
    }

    let example =
        include_str!("../../../../examples/json/server_backend/reconciliation_receipt.json");
    let receipt: super::quarantine::Receipt = serde_json::from_str(example).unwrap();
    assert_eq!(receipt.kind, ReconciliationArtifact::ContentStage);
    assert_eq!(receipt.original, format!("stage_{}", "b".repeat(64)));
    let mut value: serde_json::Value = serde_json::from_str(example).unwrap();
    value["unexpected"] = true.into();
    assert!(serde_json::from_value::<super::quarantine::Receipt>(value).is_err());
}
