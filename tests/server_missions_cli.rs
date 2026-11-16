use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
};

fn invoke(root: &Path, args: &[&str], expected: i32) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .arg("--config-dir")
        .arg(root)
        .arg("--json")
        .args(args)
        .args(["--game-space", "arma3"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(expected),
        "{args:?}: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], expected == 0);
    value
}

fn fixture(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    fs::create_dir(&root).unwrap();
    fs::write(
        root.join("games.json"),
        include_bytes!("../examples/json/appdata/games.json"),
    )
    .unwrap();
    root
}

fn inventory(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(base: &Path, path: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(base, &path, result);
            } else {
                result.insert(
                    path.strip_prefix(base).unwrap().into(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

fn add_instance(root: &Path) -> String {
    invoke(
        root,
        &["server", "instance", "add", "--name", "MainRepo"],
        0,
    )["data"]["instance"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn mission_folder(parent: &Path, name: &str, content: &[u8]) -> PathBuf {
    let folder = parent.join(name);
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("mission.sqm"), content).unwrap();
    folder
}

fn write_pbo(path: &Path, mission: &[u8]) {
    let mut file = fs::File::create(path).unwrap();
    file.write_all(b"\0sreV").unwrap();
    file.write_all(&[0; 16]).unwrap();
    file.write_all(&[0]).unwrap();
    file.write_all(b"mission.sqm\0").unwrap();
    file.write_all(&0u32.to_le_bytes()).unwrap();
    file.write_all(&(mission.len() as u32).to_le_bytes())
        .unwrap();
    file.write_all(&[0; 8]).unwrap();
    file.write_all(&(mission.len() as u32).to_le_bytes())
        .unwrap();
    file.write_all(&[0; 21]).unwrap();
    file.write_all(mission).unwrap();
}

fn library_payload(root: &Path, entry: &Value) -> PathBuf {
    root.join("games/arma3/server/mission-library")
        .join(entry["mission_id"].as_str().unwrap())
        .join("revisions")
        .join(entry["revision_id"].as_str().unwrap())
        .join("payload")
        .join(entry["mission_id"].as_str().unwrap())
        .join("mpmissions")
        .join(format!(
            "{}.pbo",
            entry["payload"]["template"].as_str().unwrap()
        ))
}

fn import_args<'a>(source: &'a Path, extra: &'a [&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "server",
        "mission",
        "import",
        "--source",
        source.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    args
}

#[test]
fn hash_verified_http_acquisition_persists_the_exact_reviewed_pbo() {
    use sha1::{Digest, Sha1};
    use std::io::Read;
    use std::time::{Duration, Instant};
    let scratch = tempfile::tempdir().unwrap();
    let root = fixture(scratch.path(), "http-library");
    let data = b"briefingName=\"HTTP fixture\";";
    let mut bytes = b"\0sreV".to_vec();
    bytes.extend_from_slice(&[0; 16]);
    bytes.push(0);
    bytes.extend_from_slice(b"mission.sqm\0");
    bytes.extend_from_slice(&[0; 16]);
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&[0; 21]);
    bytes.extend_from_slice(data);
    let checksum = Sha1::digest(&bytes);
    bytes.push(0);
    bytes.extend_from_slice(&checksum);
    let digest = blake3::hash(&bytes).to_hex().to_string();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/Foo.World.pbo", listener.local_addr().unwrap());
    let body = bytes.clone();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) => return Err(error),
            }
        };
        // Windows sockets inherit the listener's non-blocking mode on accept.
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut request = Vec::new();
        let mut buf = [0; 1024];
        while !request.windows(4).any(|part| part == b"\r\n\r\n") {
            let count = stream.read(&mut buf)?;
            if count == 0 || request.len() + count > 8192 {
                return Err(std::io::Error::other("Invalid fixture HTTP request"));
            }
            request.extend_from_slice(&buf[..count]);
        }
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )?;
        stream.write_all(&body)
    });
    let acquired = std::panic::catch_unwind(|| {
        invoke(
            &root,
            &[
                "--yes",
                "server",
                "mission",
                "acquire",
                "--url",
                &url,
                "--template",
                "Foo.World",
                "--expected-blake3",
                &digest,
                "--apply",
            ],
            0,
        )
    });
    server.join().unwrap().unwrap();
    let acquired = acquired.unwrap();
    assert_eq!(acquired["data"]["downloads_started"], true);
    assert_eq!(acquired["data"]["writes_performed"], true);
    assert_eq!(acquired["data"]["entry"]["origin"]["kind"], "http");
    assert_eq!(
        fs::read(library_payload(&root, &acquired["data"]["entry"])).unwrap(),
        bytes
    );
    assert!(!root.join("games/arma3/database.db").exists());
}

#[test]
fn folder_and_pbo_import_refresh_preserve_sources_and_old_revisions() {
    let scratch = tempfile::tempdir().unwrap();
    let root = fixture(scratch.path(), "missions");
    let instance_id = add_instance(&root);

    let folder = mission_folder(
        scratch.path(),
        "Foo.World",
        b"class Mission { briefingName=\"First fixture\"; };",
    );
    let original_source = inventory(&folder);
    let before_review = inventory(&root);
    let folder_review = invoke(&root, &import_args(&folder, &[]), 0);
    assert_eq!(
        folder_review["data"]["plan"]["entry"]["payload"]["template"],
        "Foo.World"
    );
    assert_eq!(folder_review["data"]["writes_performed"], false);
    assert_eq!(folder_review["data"]["downloads_started"], false);
    assert_eq!(inventory(&root), before_review);
    assert_eq!(inventory(&folder), original_source);

    let mut apply_args = vec![
        "server",
        "mission",
        "import",
        "--source",
        folder.to_str().unwrap(),
        "--apply",
    ];
    invoke(&root, &apply_args, 2);
    assert_eq!(inventory(&root), before_review);
    apply_args.insert(0, "--yes");
    let imported = invoke(&root, &apply_args, 0);
    assert_eq!(imported["data"]["writes_performed"], true);
    assert_eq!(imported["data"]["downloads_started"], false);
    assert_eq!(inventory(&folder), original_source);
    let first_entry = imported["data"]["entry"].clone();
    let first_path = library_payload(&root, &first_entry);
    let first_bytes = fs::read(&first_path).unwrap();

    let second_content = b"class Mission { briefingName=\"Second fixture\"; };";
    fs::write(folder.join("mission.sqm"), second_content).unwrap();
    let updated_source = inventory(&folder);
    let before_refresh = inventory(&root);
    let mission_id = first_entry["mission_id"].as_str().unwrap();
    let refresh_review = invoke(&root, &["server", "mission", "refresh", mission_id], 0);
    let refreshed_entry = &refresh_review["data"]["plan"]["entry"];
    assert_eq!(refreshed_entry["mission_id"], mission_id);
    assert_ne!(refreshed_entry["revision_id"], first_entry["revision_id"]);
    assert_ne!(
        refreshed_entry["payload"]["payload_blake3"],
        first_entry["payload"]["payload_blake3"]
    );
    assert_eq!(refresh_review["data"]["writes_performed"], false);
    assert_eq!(inventory(&root), before_refresh);
    assert_eq!(inventory(&folder), updated_source);

    let refreshed = invoke(
        &root,
        &[
            "--yes", "server", "mission", "refresh", mission_id, "--apply",
        ],
        0,
    );
    let second_entry = &refreshed["data"]["entry"];
    assert_eq!(second_entry["mission_id"], mission_id);
    assert_ne!(second_entry["revision_id"], first_entry["revision_id"]);
    assert_eq!(fs::read(&first_path).unwrap(), first_bytes);
    let second_path = library_payload(&root, second_entry);
    let second_pbo = fs::read(&second_path).unwrap();
    assert_ne!(second_pbo, first_bytes);
    assert!(
        second_pbo
            .windows(b"Second fixture".len())
            .any(|window| window == b"Second fixture")
    );
    assert_eq!(
        second_entry["payload"]["files"][0]["blake3"],
        blake3::hash(second_content).to_hex().as_str()
    );
    assert_eq!(inventory(&folder), updated_source);

    let pbo = scratch.path().join("Bar.World.pbo");
    write_pbo(&pbo, b"class Mission { briefingName=\"PBO fixture\"; };");
    let pbo_bytes = fs::read(&pbo).unwrap();
    let before_pbo_review = inventory(&root);
    let pbo_review = invoke(
        &root,
        &[
            "--dry-run",
            "server",
            "mission",
            "import",
            "--source",
            pbo.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(pbo_review["data"]["dry_run"], true);
    assert_eq!(pbo_review["data"]["writes_performed"], false);
    assert_eq!(inventory(&root), before_pbo_review);
    assert_eq!(fs::read(&pbo).unwrap(), pbo_bytes);

    let pbo_saved = invoke(
        &root,
        &[
            "--yes",
            "server",
            "mission",
            "import",
            "--source",
            pbo.to_str().unwrap(),
            "--apply",
        ],
        0,
    );
    assert_eq!(pbo_saved["data"]["writes_performed"], true);
    assert_eq!(
        fs::read(library_payload(&root, &pbo_saved["data"]["entry"])).unwrap(),
        pbo_bytes
    );
    assert_eq!(fs::read(&pbo).unwrap(), pbo_bytes);

    let cycle_read = invoke(
        &root,
        &[
            "server",
            "mission",
            "cycle",
            &instance_id,
            "--expected-revision",
            "1",
        ],
        0,
    );
    assert_eq!(cycle_read["data"]["revision"], 1);
    assert_eq!(
        cycle_read["data"]["mission_cycle"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn cycle_replace_is_revision_guarded_and_respects_operator_server_file_ownership() {
    let scratch = tempfile::tempdir().unwrap();
    let root = fixture(scratch.path(), "cycle");
    let instance_id = add_instance(&root);
    let cycle_file = scratch.path().join("cycle.json");
    let cycle = json!([
        {
            "enabled": false,
            "template": "Disabled.World",
            "difficulty": "veteran",
            "params": {"mode": 3}
        },
        {
            "enabled": true,
            "template": "Foo.World",
            "difficulty": "custom",
            "params": {"missionParam": 9}
        }
    ]);
    fs::write(&cycle_file, serde_json::to_vec_pretty(&cycle).unwrap()).unwrap();
    let source_before = fs::read(&cycle_file).unwrap();
    let before_review = inventory(&root);
    let reviewed = invoke(
        &root,
        &[
            "server",
            "mission",
            "cycle",
            &instance_id,
            "--expected-revision",
            "1",
            "--file",
            cycle_file.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(reviewed["data"]["writes_performed"], false);
    assert_eq!(reviewed["data"]["instance"]["revision"], 2);
    let reviewed_cycle = reviewed["data"]["instance"]["definition"]["settings"]["mission_cycle"]
        .as_array()
        .unwrap();
    assert_eq!(reviewed_cycle.len(), 2);
    assert_eq!(reviewed_cycle[0]["template"], "Disabled.World");
    assert_eq!(reviewed_cycle[0]["enabled"], false);
    assert_eq!(reviewed_cycle[0]["difficulty"], "veteran");
    assert_eq!(reviewed_cycle[0]["params"]["mode"], 3);
    assert_eq!(reviewed_cycle[1]["template"], "Foo.World");
    assert_eq!(reviewed_cycle[1]["difficulty"], "custom");
    assert_eq!(reviewed_cycle[1]["params"]["missionParam"], 9);
    assert_eq!(inventory(&root), before_review);
    assert_eq!(fs::read(&cycle_file).unwrap(), source_before);

    let mut unconfirmed = vec![
        "server",
        "mission",
        "cycle",
        &instance_id,
        "--expected-revision",
        "1",
        "--file",
        cycle_file.to_str().unwrap(),
        "--apply",
    ];
    invoke(&root, &unconfirmed, 2);
    assert_eq!(inventory(&root), before_review);
    unconfirmed.insert(0, "--yes");
    let applied = invoke(&root, &unconfirmed, 0);
    assert_eq!(applied["data"]["writes_performed"], true);
    assert_eq!(applied["data"]["instance"]["revision"], 2);
    assert_eq!(fs::read(&cycle_file).unwrap(), source_before);

    let after_apply = inventory(&root);
    invoke(&root, &unconfirmed, 2);
    assert_eq!(inventory(&root), after_apply);

    let server_config = scratch.path().join("operator-server.cfg");
    fs::write(
        &server_config,
        "hostname=\"OperatorHost\"; class Missions { class One { template=\"Foo.World\"; difficulty=\"Regular\"; }; };",
    )
    .unwrap();
    let imported = invoke(
        &root,
        &[
            "--yes",
            "server",
            "configuration",
            "import",
            &instance_id,
            "--expected-revision",
            "2",
            "--server",
            server_config.to_str().unwrap(),
            "--apply",
        ],
        0,
    );
    assert_eq!(imported["data"]["instance"]["revision"], 3);
    assert_eq!(
        imported["data"]["instance"]["configuration_import"]["files"][0]["ownership"],
        "operator"
    );
    let before_denied_edit = inventory(&root);
    invoke(
        &root,
        &[
            "--yes",
            "server",
            "mission",
            "cycle",
            &instance_id,
            "--expected-revision",
            "3",
            "--file",
            cycle_file.to_str().unwrap(),
            "--apply",
        ],
        2,
    );
    assert_eq!(inventory(&root), before_denied_edit);
    assert_eq!(fs::read(&cycle_file).unwrap(), source_before);
}

#[test]
fn selected_library_revision_is_retained_in_content_stage_at_mpmissions_path() {
    let scratch = tempfile::tempdir().unwrap();
    let root = fixture(scratch.path(), "stage");
    let instance_id = add_instance(&root);
    let folder = mission_folder(
        scratch.path(),
        "Foo.World",
        b"class Mission { briefingName=\"Stage fixture\"; };",
    );
    let imported = invoke(
        &root,
        &[
            "--yes",
            "server",
            "mission",
            "import",
            "--source",
            folder.to_str().unwrap(),
            "--apply",
        ],
        0,
    );
    let entry = &imported["data"]["entry"];
    let mission_id = entry["mission_id"].as_str().unwrap();
    let revision_id = entry["revision_id"].as_str().unwrap();
    let cycle_path = scratch.path().join("library-cycle.json");
    fs::write(
        &cycle_path,
        serde_json::to_vec(&json!([{
            "enabled": true,
            "source": {"kind":"library", "mission_id":mission_id, "revision_id":revision_id},
            "template": "Foo.World",
            "difficulty": "regular",
            "params": {}
        }]))
        .unwrap(),
    )
    .unwrap();
    invoke(
        &root,
        &[
            "--yes",
            "server",
            "mission",
            "cycle",
            &instance_id,
            "--expected-revision",
            "1",
            "--file",
            cycle_path.to_str().unwrap(),
            "--apply",
        ],
        0,
    );

    let bindings = scratch.path().join("bindings.json");
    fs::write(&bindings, b"[]").unwrap();
    let binding_plan = invoke(
        &root,
        &[
            "server",
            "content",
            "bindings",
            "plan",
            &instance_id,
            "--bindings",
            bindings.to_str().unwrap(),
        ],
        0,
    );
    let binding_fingerprint = binding_plan["data"]["plan"]["fingerprint"]
        .as_str()
        .unwrap();
    invoke(
        &root,
        &[
            "server",
            "content",
            "bindings",
            "save",
            &instance_id,
            "--bindings",
            bindings.to_str().unwrap(),
            "--expected-plan-fingerprint",
            binding_fingerprint,
        ],
        0,
    );

    let stage_plan = invoke(
        &root,
        &["server", "content", "stage", "plan", &instance_id],
        0,
    );
    let plan = &stage_plan["data"]["plan"];
    assert_eq!(plan["manifest"]["missions"][0]["kind"], "library");
    assert_eq!(plan["manifest"]["missions"][0]["mission_id"], mission_id);
    assert_eq!(plan["manifest"]["missions"][0]["revision_id"], revision_id);
    let mission_stage_path = format!("addons/{mission_id}/mpmissions/Foo.World.pbo");
    assert!(
        plan["manifest"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == mission_stage_path)
    );

    let stage_id = plan["stage_id"].as_str().unwrap();
    let stage_fingerprint = plan["fingerprint"].as_str().unwrap();
    let applied = invoke(
        &root,
        &[
            "server",
            "content",
            "stage",
            "apply",
            &instance_id,
            "--expected-plan-fingerprint",
            stage_fingerprint,
        ],
        0,
    );
    assert_eq!(applied["data"]["stage"]["stage_id"], stage_id);
    let retained = root
        .join("games/arma3/server/instances")
        .join(&instance_id)
        .join("content-stages")
        .join(stage_id);
    let manifest: Value =
        serde_json::from_slice(&fs::read(retained.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["missions"][0]["revision_id"], revision_id);
    assert_eq!(
        fs::read(retained.join(mission_stage_path)).unwrap(),
        fs::read(library_payload(&root, entry)).unwrap()
    );
}

#[test]
fn acquisition_dry_run_and_credential_rejection_do_not_contact_or_change_local_state() {
    let scratch = tempfile::tempdir().unwrap();
    let root = fixture(scratch.path(), "acquire");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let url = format!("http://{address}/mission.pbo");
    let digest = blake3::hash(b"network access must not happen")
        .to_hex()
        .to_string();
    let args = [
        "--dry-run",
        "server",
        "mission",
        "acquire",
        "--url",
        url.as_str(),
        "--template",
        "Foo.World",
        "--expected-blake3",
        digest.as_str(),
    ];
    let before = inventory(&root);
    let preview = invoke(&root, &args, 0);
    assert_eq!(preview["data"]["url_valid"], true);
    assert_eq!(preview["data"]["downloads_started"], false);
    assert_eq!(preview["data"]["temporary_download"], false);
    assert_eq!(preview["data"]["writes_performed"], false);
    assert_eq!(preview["data"]["ownership_claimed"], false);
    assert_eq!(inventory(&root), before);
    assert!(!root.join("games/arma3/database.db").exists());
    assert!(!root.join("games/arma3/server").exists());
    assert!(matches!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    ));

    let credential_url = format!("http://user@{address}/mission.pbo");
    invoke(
        &root,
        &[
            "server",
            "mission",
            "acquire",
            "--url",
            credential_url.as_str(),
            "--template",
            "Foo.World",
            "--expected-blake3",
            digest.as_str(),
        ],
        2,
    );
    assert_eq!(inventory(&root), before);
    assert!(!root.join("games/arma3/database.db").exists());
    assert!(!root.join("games/arma3/server").exists());
    assert!(matches!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    ));
}
