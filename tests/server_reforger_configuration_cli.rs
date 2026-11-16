use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;

const ID: &str = "server_00000000000000000000000000000002";

#[test]
fn reforger_import_export_and_scenario_listing_preserve_intent() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("games.json"),
        include_str!("../examples/json/appdata/games.json"),
    )
    .unwrap();
    let directory = root.path().join("games/reforger/server");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("servers.json"),
        include_str!("../examples/json/appdata/games/reforger/server/servers.json"),
    )
    .unwrap();
    fs::write(root.path().join("games/reforger/database.lock"), b"").unwrap();
    let source = root.path().join("server.json");
    fs::write(
        &source,
        include_str!("../examples/json/server_backend/reforger_server.json"),
    )
    .unwrap();
    let before = fs::read(directory.join("servers.json")).unwrap();
    let reviewed = invoke(
        root.path(),
        &[
            "server",
            "configuration",
            "import-reforger",
            ID,
            "--expected-revision",
            "1",
            "--file",
            source.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(reviewed["data"]["writes_performed"], false);
    assert_eq!(fs::read(directory.join("servers.json")).unwrap(), before);
    invoke(
        root.path(),
        &[
            "server",
            "configuration",
            "import-reforger",
            ID,
            "--expected-revision",
            "1",
            "--file",
            source.to_str().unwrap(),
            "--apply",
        ],
        2,
    );
    assert_eq!(fs::read(directory.join("servers.json")).unwrap(), before);
    let applied = invoke(
        root.path(),
        &[
            "server",
            "configuration",
            "import-reforger",
            ID,
            "--expected-revision",
            "1",
            "--file",
            source.to_str().unwrap(),
            "--apply",
            "--yes",
        ],
        0,
    );
    assert_eq!(applied["data"]["writes_performed"], true);
    let saved: Value =
        serde_json::from_slice(&fs::read(directory.join("servers.json")).unwrap()).unwrap();
    let settings = &saved["instances"][0]["definition"]["settings"];
    assert_eq!(saved["instances"][0]["revision"], 2);
    assert_eq!(
        settings["scenario_id"],
        "{2BBBE828037C6F4B}Missions/22_GM_Arland.conf"
    );
    assert_eq!(settings["cross_platform"], false);
    assert_eq!(
        settings["preserved"]["root"]["publicAddress"],
        "203.0.113.10"
    );
    assert_eq!(
        settings["preserved"]["game_properties"]["missionHeader"]["kept"],
        true
    );
    assert!(!saved.to_string().contains("password"));
    let exported = root.path().join("exported.json");
    let export = invoke(
        root.path(),
        &[
            "server",
            "configuration",
            "export-reforger",
            ID,
            "--file",
            exported.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(export["data"]["writes_performed"], true);
    let rendered = fs::read_to_string(&exported).unwrap();
    assert!(rendered.contains("missionHeader"));
    assert!(rendered.contains("203.0.113.10"));
    assert!(!rendered.to_ascii_lowercase().contains("password"));
    let log = root.path().join("listing.log");
    fs::write(
        &log,
        "SCRIPT : Official scenarios (1 entries)\nSCRIPT : --------------------------------------------------\nSCRIPT : {2BBBE828037C6F4B}Missions/22_GM_Arland.conf (Game Master)\n",
    )
    .unwrap();
    let stored = invoke(
        root.path(),
        &[
            "server",
            "configuration",
            "scenario",
            "import-log",
            ID,
            "--file",
            log.to_str().unwrap(),
            "--yes",
        ],
        0,
    );
    assert_eq!(stored["data"]["writes_performed"], true);
    assert_eq!(
        stored["data"]["selected_scenario"],
        "{2BBBE828037C6F4B}Missions/22_GM_Arland.conf"
    );
    let catalog = directory.join(format!("instances/{ID}/scenario-discovery.json"));
    let listing_bytes = fs::read(&catalog).unwrap();
    fs::write(
        &log,
        "SCRIPT : Official scenarios (2 entries)\nSCRIPT : one row\n",
    )
    .unwrap();
    invoke(
        root.path(),
        &[
            "server",
            "configuration",
            "scenario",
            "import-log",
            ID,
            "--file",
            log.to_str().unwrap(),
            "--yes",
        ],
        2,
    );
    assert_eq!(fs::read(&catalog).unwrap(), listing_bytes);
    let listed = invoke(
        root.path(),
        &["server", "configuration", "scenario", "list", ID],
        0,
    );
    assert_eq!(
        listed["data"]["note"],
        "The selected scenario is in the stored listing."
    );
    let after_listing: Value =
        serde_json::from_slice(&fs::read(directory.join("servers.json")).unwrap()).unwrap();
    assert_eq!(after_listing["instances"][0]["revision"], 2);
    assert_eq!(
        after_listing["instances"][0]["definition"]["settings"]["scenario_id"],
        "{2BBBE828037C6F4B}Missions/22_GM_Arland.conf"
    );
    let parsed = invoke(
        root.path(),
        &[
            "server",
            "configuration",
            "scenario",
            "parse-log",
            "--file",
            root.path().join("listing.log").to_str().unwrap(),
        ],
        2,
    );
    assert_eq!(parsed["ok"], false);
}

fn invoke(root: &Path, args: &[&str], code: i32) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .arg("--config-dir")
        .arg(root)
        .arg("--json")
        .args(args)
        .args(["--game-space", "reforger"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(code),
        "args={args:?}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
