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

const SPACE: [&str; 2] = ["--game-space", "arma3"];

fn instance(root: &Path) -> String {
    let added = invoke(
        root,
        &[
            "server",
            "instance",
            "add",
            SPACE[0],
            SPACE[1],
            "--name",
            "MaintainedHost",
        ],
        0,
    );
    added["data"]["instance"]["id"].as_str().unwrap().to_owned()
}

fn maintenance(root: &Path, extra: &[&str], code: i32) -> Value {
    let mut args = vec!["server", "maintenance"];
    args.extend_from_slice(extra);
    args.extend_from_slice(&SPACE);
    invoke(root, &args, code)
}

#[test]
fn jobs_round_trip_with_policy_revisions_and_explicit_confirmation() {
    let root = fixture();
    let id = instance(root.path());
    let empty = maintenance(root.path(), &["list", &id], 0);
    assert_eq!(empty["data"]["overview"]["catalog_revision"], 0);
    assert_eq!(empty["data"]["overview"]["owner_running"], false);

    let before = inventory(root.path());
    let preview = invoke(
        root.path(),
        &[
            "--dry-run",
            "server",
            "maintenance",
            "add",
            &id,
            "--expected-revision",
            "0",
            "--name",
            "Nightly",
            "--action",
            "restart",
            "--weekly",
            "daily",
            "--time",
            "04:30",
            "--utc",
            SPACE[0],
            SPACE[1],
        ],
        0,
    );
    assert_eq!(preview["data"]["dry_run"], true);
    assert_eq!(inventory(root.path()), before, "a dry run writes nothing");

    let added = maintenance(
        root.path(),
        &[
            "add",
            &id,
            "--expected-revision",
            "0",
            "--name",
            "Nightly",
            "--action",
            "restart",
            "--weekly",
            "daily",
            "--time",
            "04:30",
            "--utc",
            "--require-empty",
        ],
        0,
    );
    let job = added["data"]["job_id"].as_str().unwrap().to_owned();
    let saved = &added["data"]["catalog"]["jobs"][0];
    assert_eq!(saved["revision"], 1);
    assert_eq!(saved["definition"]["clock"], "utc");
    assert_eq!(
        saved["definition"]["schedule"]["Recurring"]["minutes_of_day"],
        270
    );
    assert_eq!(
        saved["definition"]["policy"]["player_policy"],
        "require_empty"
    );

    maintenance(
        root.path(),
        &[
            "add",
            &id,
            "--expected-revision",
            "0",
            "--name",
            "Stale",
            "--action",
            "restart",
            "--weekly",
            "mon",
            "--time",
            "01:00",
        ],
        2,
    );
    maintenance(
        root.path(),
        &[
            "add",
            &id,
            "--expected-revision",
            "1",
            "--name",
            "Weekly backup",
            "--action",
            "backup",
            "--weekly",
            "sun",
            "--time",
            "03:00",
        ],
        0,
    );
    let same = maintenance(
        root.path(),
        &[
            "set",
            &id,
            &job,
            "--expected-revision",
            "2",
            "--name",
            "Nightly",
            "--action",
            "restart",
            "--weekly",
            "daily",
            "--time",
            "04:30",
            "--utc",
            "--require-empty",
        ],
        0,
    );
    assert_eq!(same["data"]["catalog"]["jobs"][0]["revision"], 1);
    let changed = maintenance(
        root.path(),
        &[
            "set",
            &id,
            &job,
            "--expected-revision",
            "3",
            "--name",
            "Nightly",
            "--action",
            "restart",
            "--weekly",
            "daily",
            "--time",
            "05:00",
            "--utc",
        ],
        0,
    );
    assert_eq!(changed["data"]["catalog"]["jobs"][0]["revision"], 2);

    let listed = maintenance(root.path(), &["list", &id], 0);
    let statuses = listed["data"]["overview"]["jobs"].as_array().unwrap();
    assert_eq!(statuses.len(), 2);
    assert!(
        statuses[0]["blocker"]
            .as_str()
            .unwrap()
            .contains("inactive")
    );
    assert!(statuses[0]["next_run_unix_ms"].is_u64());
    assert!(
        statuses[1]["blocker"]
            .as_str()
            .unwrap()
            .contains("not available")
    );

    maintenance(
        root.path(),
        &["remove", &id, &job, "--expected-revision", "4"],
        2,
    );
    let run_preview = invoke(
        root.path(),
        &[
            "--dry-run",
            "server",
            "maintenance",
            "run",
            &id,
            &job,
            "--expected-policy-revision",
            "2",
            SPACE[0],
            SPACE[1],
        ],
        0,
    );
    assert_eq!(run_preview["data"]["operation"], "run_maintenance_job");
    assert_eq!(run_preview["data"]["owner_contacted"], false);
    let refused = invoke(
        root.path(),
        &[
            "--yes",
            "server",
            "maintenance",
            "run",
            &id,
            &job,
            "--expected-policy-revision",
            "2",
            SPACE[0],
            SPACE[1],
        ],
        4,
    );
    assert!(
        refused["errors"]
            .to_string()
            .contains("No foreground owner")
    );
    invoke(
        root.path(),
        &[
            "--yes",
            "server",
            "maintenance",
            "cancel",
            &id,
            "occ_not_valid",
            SPACE[0],
            SPACE[1],
        ],
        2,
    );
    let history = maintenance(root.path(), &["history", &id], 0);
    assert_eq!(history["data"]["occurrences"].as_array().unwrap().len(), 0);

    invoke(
        root.path(),
        &[
            "--yes",
            "server",
            "maintenance",
            "remove",
            &id,
            &job,
            "--expected-revision",
            "4",
            SPACE[0],
            SPACE[1],
        ],
        0,
    );
    let after = maintenance(root.path(), &["list", &id], 0);
    assert_eq!(
        after["data"]["overview"]["jobs"].as_array().unwrap().len(),
        1
    );
}

#[test]
fn schedules_and_policies_are_validated_before_any_write() {
    let root = fixture();
    let id = instance(root.path());
    let before = inventory(root.path());
    for extra in [
        vec!["--weekly", "someday", "--time", "04:00"],
        vec!["--weekly", "mon", "--time", "25:00"],
        vec!["--at", "tomorrow"],
        vec![],
    ] {
        let mut args = vec![
            "add",
            id.as_str(),
            "--expected-revision",
            "0",
            "--name",
            "Bad",
            "--action",
            "restart",
        ];
        args.extend(extra);
        maintenance(root.path(), &args, 2);
    }
    assert_eq!(inventory(root.path()), before);
    invoke(
        root.path(),
        &[
            "server",
            "maintenance",
            "list",
            "--game-space",
            "twwh3",
            "server_00000000000000000000000000000001",
        ],
        2,
    );
}
