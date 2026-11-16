use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn invoke(root: &Path, args: &[&str], expected_code: i32) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .arg("--config-dir")
        .arg(root)
        .arg("--json")
        .args(args);
    command.env("FOXY_CONFIG_DIR", root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(expected_code),
        "args={args:?}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["ok"], expected_code == 0);
    result
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("games.json"),
        include_str!("../examples/json/appdata/games.json"),
    )
    .unwrap();
    root
}

fn inventory(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut result = BTreeMap::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let name = PathBuf::from(path.file_name().unwrap());
        if path.is_dir() {
            result.insert(name.clone(), None);
            for (nested, bytes) in inventory(&path) {
                result.insert(name.join(nested), bytes);
            }
        } else {
            result.insert(name, Some(fs::read(path).unwrap()));
        }
    }
    result
}

fn arma_instance(root: &Path) -> String {
    let added = invoke(
        root,
        &[
            "server",
            "instance",
            "add",
            "--game-space",
            "arma3",
            "--name",
            "HeadlessHost",
        ],
        0,
    );
    added["data"]["instance"]["id"].as_str().unwrap().to_owned()
}

fn clients(result: &Value) -> Vec<Value> {
    result["data"]["instance"]["definition"]["settings"]["headless_clients"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn headless_definitions_round_trip_and_render_loopback_admission() {
    let root = fixture();
    let id = arma_instance(root.path());
    let space = ["--game-space", "arma3"];
    let added = invoke(
        root.path(),
        &[
            "server",
            "headless",
            "add",
            space[0],
            space[1],
            &id,
            "--name",
            "HC1",
            "--expected-revision",
            "1",
            "--include",
            "RepoAlpha",
            "--limit-fps",
            "60",
        ],
        0,
    );
    let first = clients(&added);
    assert_eq!(first.len(), 1);
    let first_id = first[0]["id"].as_str().unwrap().to_owned();
    assert!(first_id.starts_with("hc_") && first_id.len() == 19);
    assert_eq!(first[0]["name"], "HC1");
    assert_eq!(first[0]["addons"]["include"][0], "RepoAlpha");
    assert_eq!(first[0]["process"]["limit_fps"], 60);
    assert!(first[0].get("enabled").is_none());

    invoke(
        root.path(),
        &[
            "server",
            "headless",
            "add",
            space[0],
            space[1],
            &id,
            "--name",
            "hc1",
            "--expected-revision",
            "2",
        ],
        2,
    );
    invoke(
        root.path(),
        &[
            "server",
            "headless",
            "add",
            space[0],
            space[1],
            &id,
            "--name",
            "HC2",
            "--expected-revision",
            "1",
        ],
        2,
    );
    invoke(
        root.path(),
        &[
            "server",
            "headless",
            "add",
            space[0],
            space[1],
            &id,
            "--name",
            "HC2",
            "--expected-revision",
            "2",
            "--ex-threads",
            "2",
        ],
        2,
    );
    invoke(
        root.path(),
        &[
            "server",
            "headless",
            "add",
            space[0],
            space[1],
            &id,
            "--name",
            "HC0",
            "--order",
            "0",
            "--expected-revision",
            "2",
            "--disabled",
        ],
        0,
    );
    let listed = invoke(
        root.path(),
        &["server", "headless", "list", space[0], space[1], &id],
        0,
    );
    let names: Vec<_> = listed["data"]["headless_clients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|client| client["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names.len(), 2);
    assert_eq!(listed["data"]["server_admission"], "derived_loopback");
    assert_eq!(listed["data"]["writes_performed"], false);

    let rendered = invoke(
        root.path(),
        &["server", "instance", "render", space[0], space[1], &id],
        0,
    );
    let preview = &rendered["data"]["configuration_preview"];
    let server = preview["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["relative_path"] == "server.cfg")
        .unwrap();
    assert!(
        server["contents"]
            .as_str()
            .unwrap()
            .contains("headlessClients[] = {\"127.0.0.1\"};")
    );
    let warnings = preview["warnings"].to_string();
    assert!(warnings.contains("1 of 2 headless clients are enabled"));
    assert!(warnings.contains("Mission slots and AI offload are not verified"));

    let replaced = invoke(
        root.path(),
        &[
            "server",
            "headless",
            "set",
            space[0],
            space[1],
            &id,
            &first_id,
            "--name",
            "HCMain",
            "--order",
            "3",
            "--no-inherit-required",
            "--expected-revision",
            "3",
        ],
        0,
    );
    let main = clients(&replaced)
        .into_iter()
        .find(|client| client["id"] == first_id.as_str())
        .unwrap();
    assert_eq!(main["name"], "HCMain");
    assert_eq!(main["order"], 3);
    assert_eq!(main["addons"]["inherit_required"], false);
    assert!(main["addons"].get("include").is_none());
    assert!(main.get("process").is_none());

    invoke(
        root.path(),
        &[
            "server",
            "headless",
            "remove",
            space[0],
            space[1],
            &id,
            &first_id,
            "--expected-revision",
            "4",
        ],
        2,
    );
    invoke(
        root.path(),
        &[
            "--yes",
            "server",
            "headless",
            "remove",
            space[0],
            space[1],
            &id,
            "hc_0000000000000009",
            "--expected-revision",
            "4",
        ],
        3,
    );
    let before = inventory(root.path());
    invoke(
        root.path(),
        &[
            "--dry-run",
            "server",
            "headless",
            "remove",
            space[0],
            space[1],
            &id,
            &first_id,
            "--expected-revision",
            "4",
        ],
        0,
    );
    assert_eq!(inventory(root.path()), before);
    let removed = invoke(
        root.path(),
        &[
            "--yes",
            "server",
            "headless",
            "remove",
            space[0],
            space[1],
            &id,
            &first_id,
            "--expected-revision",
            "4",
        ],
        0,
    );
    let remaining = clients(&removed);
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0]["name"], "HC0");
}

#[test]
fn headless_commands_are_absent_for_reforger_and_start_needs_review_and_an_owner() {
    let root = fixture();
    invoke(
        root.path(),
        &[
            "server",
            "headless",
            "list",
            "--game-space",
            "reforger",
            "server_00000000000000000000000000000002",
        ],
        2,
    );
    let id = arma_instance(root.path());
    let space = ["--game-space", "arma3"];
    invoke(
        root.path(),
        &[
            "server",
            "headless",
            "start",
            space[0],
            space[1],
            &id,
            "--expected-run-generation",
            "1",
        ],
        2,
    );
    invoke(
        root.path(),
        &[
            "--yes",
            "server",
            "headless",
            "start",
            space[0],
            space[1],
            &id,
            "--expected-run-generation",
            "0",
        ],
        2,
    );
    let before = inventory(root.path());
    let preview = invoke(
        root.path(),
        &[
            "--dry-run",
            "server",
            "headless",
            "start",
            space[0],
            space[1],
            &id,
            "--expected-run-generation",
            "1",
        ],
        0,
    );
    assert_eq!(preview["data"]["process_started"], false);
    assert_eq!(preview["data"]["owner_contacted"], false);
    assert_eq!(inventory(root.path()), before);
    let refused = invoke(
        root.path(),
        &[
            "--yes",
            "server",
            "headless",
            "start",
            space[0],
            space[1],
            &id,
            "--expected-run-generation",
            "1",
        ],
        4,
    );
    assert!(
        refused["errors"]
            .to_string()
            .contains("No foreground owner")
    );
}

#[test]
fn lifecycle_policy_round_trips_and_owner_actions_refuse_before_any_change() {
    let root = fixture();
    let id = arma_instance(root.path());
    let space = ["--game-space", "arma3"];
    let added = invoke(
        root.path(),
        &[
            "server",
            "headless",
            "add",
            space[0],
            space[1],
            &id,
            "--name",
            "HC1",
            "--expected-revision",
            "1",
            "--manual-start",
            "--start-delay",
            "30",
            "--join-timeout",
            "300",
            "--max-restarts",
            "0",
        ],
        0,
    );
    let client = clients(&added)[0].clone();
    assert_eq!(
        client["lifecycle"],
        serde_json::json!({
            "auto_start": false,
            "start_delay_seconds": 30,
            "connect_timeout_seconds": 300,
            "max_restarts": 0
        })
    );
    let client_id = client["id"].as_str().unwrap().to_owned();
    let defaulted = invoke(
        root.path(),
        &[
            "server",
            "headless",
            "add",
            space[0],
            space[1],
            &id,
            "--name",
            "HC2",
            "--expected-revision",
            "2",
        ],
        0,
    );
    assert!(clients(&defaulted)[1].get("lifecycle").is_none());
    let defaulted_id = clients(&defaulted)[1]["id"].as_str().unwrap().to_owned();
    let listed = invoke(
        root.path(),
        &["server", "headless", "list", space[0], space[1], &id],
        0,
    );
    let listed_clients = listed["data"]["headless_clients"].as_array().unwrap();
    let policy_client = listed_clients
        .iter()
        .find(|item| item["id"] == client_id)
        .unwrap();
    let defaulted_client = listed_clients
        .iter()
        .find(|item| item["id"] == defaulted_id)
        .unwrap();
    assert_eq!(policy_client["lifecycle"]["auto_start"], false);
    assert!(defaulted_client.get("lifecycle").is_none());

    let before = inventory(root.path());
    let preview = invoke(
        root.path(),
        &[
            "--dry-run",
            "server",
            "headless",
            "stop",
            space[0],
            space[1],
            &id,
            "--expected-run-generation",
            "3",
            "--client",
            &client_id,
            "--force",
        ],
        0,
    );
    assert_eq!(preview["data"]["operation"], "stop_headless_clients");
    assert_eq!(preview["data"]["headless_id"], client_id.as_str());
    assert_eq!(preview["data"]["force"], true);
    assert_eq!(preview["data"]["owner_contacted"], false);
    let restart_preview = invoke(
        root.path(),
        &[
            "--dry-run",
            "server",
            "headless",
            "restart",
            space[0],
            space[1],
            &id,
            "--expected-run-generation",
            "3",
        ],
        0,
    );
    assert_eq!(
        restart_preview["data"]["operation"],
        "restart_headless_clients"
    );
    assert!(restart_preview["data"].get("headless_id").is_none());
    assert_eq!(inventory(root.path()), before);

    for args in [
        vec![
            "server",
            "headless",
            "stop",
            space[0],
            space[1],
            &id,
            "--expected-run-generation",
            "3",
        ],
        vec![
            "--yes",
            "server",
            "headless",
            "restart",
            space[0],
            space[1],
            &id,
            "--expected-run-generation",
            "3",
            "--client",
            "hc_BAD",
        ],
        vec![
            "--yes",
            "server",
            "headless",
            "start",
            space[0],
            space[1],
            &id,
            "--expected-run-generation",
            "0",
            "--client",
            &client_id,
        ],
    ] {
        invoke(root.path(), &args, 2);
    }
    for args in [
        vec![
            "--yes",
            "server",
            "headless",
            "stop",
            space[0],
            space[1],
            &id,
            "--expected-run-generation",
            "3",
            "--client",
            &client_id,
        ],
        vec!["server", "headless", "status", space[0], space[1], &id],
    ] {
        let refused = invoke(root.path(), &args, 4);
        assert!(
            refused["errors"]
                .to_string()
                .contains("No foreground owner")
        );
    }
    assert_eq!(inventory(root.path()), before);
}
