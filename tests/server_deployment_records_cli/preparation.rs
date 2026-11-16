use super::*;

fn prep_plan(root: &Path, game: &str, id: &str, generation: &str) -> Value {
    invoke(
        root,
        game,
        &[
            "server",
            "deploy",
            "runtime",
            "plan",
            id,
            "--expected-deployment-generation",
            generation,
        ],
        0,
    )
}

#[test]
fn preparation_plan_build_list_verify_contract_for_both_games() {
    let root = fixture();
    for game in ["arma3", "reforger"] {
        let prepared = prepare(root.path(), game);
        let before = inventory(root.path());
        let plan = prep_plan(root.path(), game, &prepared.id, "1");
        let p = &plan["data"]["plan"];
        assert_eq!(p["manifest"]["record_id"], prepared.record);
        assert!(
            p["preparation_id"]
                .as_str()
                .unwrap()
                .starts_with("prepared_")
        );
        assert_eq!(plan["data"]["writes_performed"], false);
        assert_eq!(inventory(root.path()), before);
        invoke(
            root.path(),
            game,
            &[
                "--dry-run",
                "server",
                "deploy",
                "runtime",
                "build",
                &prepared.id,
                "--expected-deployment-generation",
                "1",
                "--expected-plan-fingerprint",
                p["fingerprint"].as_str().unwrap(),
            ],
            0,
        );
        assert_eq!(inventory(root.path()), before);
        let built = invoke(
            root.path(),
            game,
            &[
                "server",
                "deploy",
                "runtime",
                "build",
                &prepared.id,
                "--expected-deployment-generation",
                "1",
                "--expected-plan-fingerprint",
                p["fingerprint"].as_str().unwrap(),
            ],
            0,
        );
        let report = &built["data"]["report"];
        assert_eq!(report["integrity_verified"], true);
        assert_eq!(report["deployment_ready"], false);
        assert_eq!(report["active_deployment_changed"], false);
        assert_eq!(report["process_started"], false);
        let preparation_id = report["preparation_id"].as_str().unwrap();
        let verified = invoke(
            root.path(),
            game,
            &[
                "server",
                "deploy",
                "runtime",
                "verify",
                &prepared.id,
                preparation_id,
            ],
            0,
        );
        assert_eq!(verified["data"]["report"]["integrity_verified"], true);
        let listed = invoke(
            root.path(),
            game,
            &["server", "deploy", "runtime", "list", &prepared.id],
            0,
        );
        assert_eq!(
            listed["data"]["preparations"]["preparation_ids"][0],
            preparation_id
        );
        assert_eq!(listed["data"]["preparations"]["writes_performed"], false);
        assert!(
            !root
                .path()
                .join(format!("games/{game}/database.db"))
                .exists()
        );
    }
}

#[test]
fn preparation_cli_rejects_stale_generation_fingerprint_and_corrupt_copy() {
    let root = fixture();
    let prepared = prepare(root.path(), "arma3");
    invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "deploy",
            "runtime",
            "plan",
            &prepared.id,
            "--expected-deployment-generation",
            "0",
        ],
        2,
    );
    let plan = prep_plan(root.path(), "arma3", &prepared.id, "1");
    invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "deploy",
            "runtime",
            "build",
            &prepared.id,
            "--expected-deployment-generation",
            "1",
            "--expected-plan-fingerprint",
            "stale",
        ],
        2,
    );
    let fp = plan["data"]["plan"]["fingerprint"].as_str().unwrap();
    let state_path = root.path().join(format!(
        "games/arma3/server/instances/{}/deployments/state.json",
        prepared.id
    ));
    let state_bytes = fs::read(&state_path).unwrap();
    let mut changed_state = state_bytes.clone();
    changed_state.push(b'\n');
    fs::write(&state_path, &changed_state).unwrap();
    invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "deploy",
            "runtime",
            "build",
            &prepared.id,
            "--expected-deployment-generation",
            "1",
            "--expected-plan-fingerprint",
            fp,
        ],
        2,
    );
    assert_eq!(fs::read(&state_path).unwrap(), changed_state);
    fs::write(&state_path, state_bytes).unwrap();
    let built = invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "deploy",
            "runtime",
            "build",
            &prepared.id,
            "--expected-deployment-generation",
            "1",
            "--expected-plan-fingerprint",
            fp,
        ],
        0,
    );
    let preparation_id = built["data"]["report"]["preparation_id"].as_str().unwrap();
    let dir = root.path().join(format!(
        "games/arma3/server/instances/{}/deployments/prepared/{preparation_id}",
        prepared.id
    ));
    fs::write(dir.join("operator-data"), b"preserve").unwrap();
    invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "deploy",
            "runtime",
            "verify",
            &prepared.id,
            preparation_id,
        ],
        4,
    );
    assert_eq!(fs::read(dir.join("operator-data")).unwrap(), b"preserve");
    let mut future: Value =
        serde_json::from_slice(&fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    future["layout_version"] = 99.into();
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec(&future).unwrap(),
    )
    .unwrap();
    invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "deploy",
            "runtime",
            "verify",
            &prepared.id,
            preparation_id,
        ],
        4,
    );
    let missing = format!("prepared_{}", "a".repeat(64));
    invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "deploy",
            "runtime",
            "verify",
            &prepared.id,
            &missing,
        ],
        3,
    );
    invoke(
        root.path(),
        "arma3",
        &["server", "deploy", "runtime", "verify", &prepared.id, "bad"],
        2,
    );
}

#[test]
fn preparation_cli_read_only_under_busy_claim_and_build_requires_claim() {
    let root = fixture();
    let prepared = prepare(root.path(), "arma3");
    let initial_plan = prep_plan(root.path(), "arma3", &prepared.id, "1");
    let initial_fingerprint = initial_plan["data"]["plan"]["fingerprint"]
        .as_str()
        .unwrap();
    let initial = invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "deploy",
            "runtime",
            "build",
            &prepared.id,
            "--expected-deployment-generation",
            "1",
            "--expected-plan-fingerprint",
            initial_fingerprint,
        ],
        0,
    );
    let existing_preparation = initial["data"]["report"]["preparation_id"]
        .as_str()
        .unwrap();
    let before = inventory(root.path());
    let lock: File = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join("games/arma3/database.lock"))
        .unwrap();
    lock.lock().unwrap();
    let plan = prep_plan(root.path(), "arma3", &prepared.id, "1");
    let fingerprint = plan["data"]["plan"]["fingerprint"].as_str().unwrap();
    invoke(
        root.path(),
        "arma3",
        &[
            "--dry-run",
            "server",
            "deploy",
            "runtime",
            "build",
            &prepared.id,
            "--expected-deployment-generation",
            "1",
            "--expected-plan-fingerprint",
            fingerprint,
        ],
        0,
    );
    invoke(
        root.path(),
        "arma3",
        &["server", "deploy", "runtime", "list", &prepared.id],
        0,
    );
    invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "deploy",
            "runtime",
            "verify",
            &prepared.id,
            existing_preparation,
        ],
        0,
    );
    let prep_id = format!("prepared_{}", "a".repeat(64));
    invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "deploy",
            "runtime",
            "verify",
            &prepared.id,
            &prep_id,
        ],
        3,
    );
    invoke(
        root.path(),
        "arma3",
        &[
            "server",
            "deploy",
            "runtime",
            "build",
            &prepared.id,
            "--expected-deployment-generation",
            "1",
            "--expected-plan-fingerprint",
            fingerprint,
        ],
        6,
    );
    drop(lock);
    assert_eq!(inventory(root.path()), before);
}
