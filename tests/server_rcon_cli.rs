use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::net::UdpSocket;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

const PASSWORD: &str = "synthetic-rcon-1";

fn run(root: &Path, args: &[&str], stdin: Option<&str>) -> (i32, Value, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .arg("--config-dir")
        .arg(root)
        .arg("--json")
        .arg("--quiet")
        .args(args)
        .env("FOXY_CONFIG_DIR", root)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn().unwrap();
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let value: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|_| panic!("args={args:?}\nstdout={stdout}\nstderr={stderr}"));
    (output.status.code().unwrap_or(-1), value, stdout + &stderr)
}

fn ok(root: &Path, args: &[&str], stdin: Option<&str>) -> Value {
    let (code, value, text) = run(root, args, stdin);
    assert_eq!(code, 0, "args={args:?}\n{text}");
    value
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

fn add(root: &Path, space: &str, name: &str) -> String {
    let added = ok(
        root,
        &[
            "server",
            "instance",
            "add",
            "--game-space",
            space,
            "--name",
            name,
        ],
        None,
    );
    added["data"]["instance"]["id"].as_str().unwrap().to_owned()
}

fn files_containing(root: &Path, needle: &[u8], found: &mut Vec<String>) {
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files_containing(&path, needle, found);
        } else if fs::read(&path)
            .unwrap()
            .windows(needle.len())
            .any(|window| window == needle)
        {
            found.push(path.to_string_lossy().replace('\\', "/"));
        }
    }
}

fn config_inventory(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn visit(root: &Path, directory: &Path, inventory: &mut BTreeMap<String, Option<Vec<u8>>>) {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if relative == "logs" || relative.starts_with("logs/") {
                continue;
            }
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                inventory.insert(relative, None);
                visit(root, &path, inventory);
            } else if kind.is_file() {
                inventory.insert(relative, Some(fs::read(path).unwrap()));
            }
        }
    }

    let mut inventory = BTreeMap::new();
    visit(root, root, &mut inventory);
    inventory
}

fn run_admin(
    root: &Path,
    space: &str,
    dry_run: bool,
    yes: bool,
    command: &[&str],
) -> (i32, Value, String) {
    let mut args = Vec::new();
    if dry_run {
        args.push("--dry-run");
    }
    if yes {
        args.push("--yes");
    }
    args.extend(["server", "admin"]);
    args.extend_from_slice(command);
    args.extend(["--game-space", space]);
    run(root, &args, None)
}

#[test]
fn arma_admin_dry_runs_and_rejections_are_owner_free_and_write_free() {
    let root = fixture();
    let id = add(root.path(), "arma3", "AdminHost");
    let reforger_id = add(root.path(), "reforger", "AdminReforger");
    let before = config_inventory(root.path());
    assert!(!before.keys().any(|path| path.ends_with("database.db")));

    let (code, players, text) = run_admin(root.path(), "arma3", true, false, &["players", &id]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(players["action"], "server.admin.players");
    assert_eq!(players["data"]["wire_text"], "players");
    assert_eq!(
        players["data"]["command"],
        serde_json::json!({"kind": "players"})
    );
    assert_eq!(players["data"]["owner_contacted"], false);
    assert_eq!(players["data"]["database_opened"], false);
    assert_eq!(players["data"]["writes_performed"], false);

    let (code, announce, text) = run_admin(
        root.path(),
        "arma3",
        true,
        false,
        &[
            "announce",
            &id,
            "--message",
            "Restart in five minutes",
            "--expected-run-generation",
            "41",
        ],
    );
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        announce["data"]["wire_text"],
        "say -1 Restart in five minutes"
    );
    assert_eq!(
        announce["data"]["command"],
        serde_json::json!({"kind": "announce", "message": "Restart in five minutes"})
    );
    assert_eq!(announce["data"]["expected_run_generation"], 41);
    assert_eq!(announce["data"]["owner_contacted"], false);

    let (code, _, text) = run_admin(
        root.path(),
        "arma3",
        false,
        false,
        &[
            "announce",
            &id,
            "--message",
            "Maintenance",
            "--expected-run-generation",
            "41",
        ],
    );
    assert_eq!(
        code, 2,
        "live side effects require --yes before owner attach: {text}"
    );
    assert!(text.contains("requires --yes"), "{text}");

    let (code, _, text) = run_admin(
        root.path(),
        "arma3",
        false,
        true,
        &["announce", &id, "--message", "Maintenance"],
    );
    assert_eq!(
        code, 2,
        "live side effects require an explicit generation: {text}"
    );
    assert!(text.contains("--expected-run-generation"), "{text}");

    let (code, _, text) = run_admin(
        root.path(),
        "arma3",
        false,
        true,
        &[
            "announce",
            &id,
            "--message",
            "Maintenance",
            "--expected-run-generation",
            "0",
        ],
    );
    assert_eq!(code, 2, "zero cannot identify a current run: {text}");
    assert!(text.contains("greater than zero"), "{text}");

    for command in [
        vec![
            "kick",
            &id,
            "--player-id",
            "2",
            "--expected-guid",
            "bad",
            "--reason",
            "test",
            "--expected-run-generation",
            "41",
        ],
        vec![
            "kick",
            &id,
            "--player-id",
            "2",
            "--expected-guid",
            "0123456789abcdef0123456789abcdef",
            "--reason",
            "line\r\n#shutdown",
            "--expected-run-generation",
            "41",
        ],
        vec![
            "announce",
            &id,
            "--message",
            "line\r\n#shutdown",
            "--expected-run-generation",
            "41",
        ],
        vec![
            "mission",
            &id,
            "--template",
            "../mission.Altis",
            "--expected-run-generation",
            "41",
        ],
    ] {
        let (code, _, text) = run_admin(root.path(), "arma3", true, true, &command);
        assert_eq!(code, 2, "invalid typed command must fail offline: {text}");
    }

    let (code, _, text) = run_admin(
        root.path(),
        "reforger",
        true,
        false,
        &["players", &reforger_id],
    );
    assert_eq!(
        code, 2,
        "Arma administration is not a Reforger capability: {text}"
    );
    assert!(text.contains("unavailable"), "{text}");

    let after = config_inventory(root.path());
    assert_eq!(
        after, before,
        "admin previews and validation must not write configuration"
    );
    assert!(!after.keys().any(|path| path.ends_with("database.db")));
}

#[test]
fn rcon_settings_and_passwords_are_protected_draft_changes() {
    let root = fixture();
    let id = add(root.path(), "arma3", "RconHost");
    let space = ["--game-space", "arma3"];
    let configure = |port: &str, revision: &str| {
        run(
            root.path(),
            &[
                "server",
                "rcon",
                "configure",
                &id,
                "--expected-revision",
                revision,
                "--port",
                port,
                space[0],
                space[1],
            ],
            None,
        )
    };
    assert_eq!(
        configure("2306", "1").0,
        2,
        "the game port bundle is refused"
    );
    let (code, saved, _) = configure("2320", "1");
    assert_eq!(code, 0);
    assert_eq!(saved["data"]["revision"], 2);
    assert_eq!(saved["data"]["rcon"]["password_stored"], false);
    let (code, _, _) = run(
        root.path(),
        &[
            "server",
            "rcon",
            "configure",
            &id,
            "--expected-revision",
            "2",
            "--port",
            "2321",
            "--permission",
            "admin",
            space[0],
            space[1],
        ],
        None,
    );
    assert_eq!(code, 2, "Arma has no permission setting");
    let (code, _, _) = run(
        root.path(),
        &[
            "server",
            "rcon",
            "set-password",
            &id,
            "--expected-revision",
            "2",
            space[0],
            space[1],
        ],
        Some("no\n"),
    );
    assert_eq!(code, 2, "a two-character password is refused");
    let (code, stored, text) = run(
        root.path(),
        &[
            "server",
            "rcon",
            "set-password",
            &id,
            "--expected-revision",
            "2",
            space[0],
            space[1],
        ],
        Some(&format!("{PASSWORD}\n")),
    );
    assert_eq!(code, 0, "{text}");
    assert!(!text.contains(PASSWORD));
    assert_eq!(stored["data"]["revision"], 3);
    let status = ok(
        root.path(),
        &["server", "rcon", "status", &id, space[0], space[1]],
        None,
    );
    assert_eq!(status["data"]["draft"]["port"], 2320);
    assert_eq!(status["data"]["password_available"], true);
    assert_eq!(status["data"]["owner_contacted"], false);
    let preview = ok(
        root.path(),
        &["server", "configuration", "plan", &id, space[0], space[1]],
        None,
    );
    let preview_text = preview.to_string();
    assert!(!preview_text.contains(PASSWORD));
    let intent = preview["data"]["preview"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["relative_path"] == "launch-intent.json")
        .unwrap();
    let intent: Value = serde_json::from_str(intent["contents"].as_str().unwrap()).unwrap();
    assert_eq!(intent["rcon"]["port"], 2320);
    assert!(
        intent["rcon"]["password_ref"]
            .as_str()
            .unwrap()
            .starts_with("secret_")
    );
    let mut found = Vec::new();
    files_containing(root.path(), PASSWORD.as_bytes(), &mut found);
    assert!(
        found
            .iter()
            .all(|path| path.contains("/server/credentials/")),
        "{found:?}"
    );
    if cfg!(windows) {
        assert!(
            found.is_empty(),
            "Windows stores the password encrypted: {found:?}"
        );
    }
    let history = ok(
        root.path(),
        &["server", "rcon", "history", &id, space[0], space[1]],
        None,
    );
    assert_eq!(
        history["data"]["history"]["records"],
        Value::Array(Vec::new())
    );
    let (code, _, text) = run(
        root.path(),
        &[
            "--yes",
            "server",
            "rcon",
            "command",
            &id,
            "--text",
            "RConPassword next-pass",
            space[0],
            space[1],
        ],
        None,
    );
    assert_eq!(
        code, 2,
        "a command carrying a password is refused before any owner: {text}"
    );
    let cloned = ok(
        root.path(),
        &[
            "server",
            "instance",
            "clone",
            &id,
            "--name",
            "RconCopy",
            "--expected-revision",
            "3",
            space[0],
            space[1],
        ],
        None,
    );
    let copy = cloned["data"]["instance"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let copy_status = ok(
        root.path(),
        &["server", "rcon", "status", &copy, space[0], space[1]],
        None,
    );
    assert_eq!(copy_status["data"]["draft"]["port"], 2320);
    assert_eq!(copy_status["data"]["draft"]["password_stored"], false);
    assert_eq!(copy_status["data"]["password_available"], false);
    let (code, _, _) = run(
        root.path(),
        &[
            "server",
            "rcon",
            "disable",
            &id,
            "--expected-revision",
            "3",
            space[0],
            space[1],
        ],
        None,
    );
    assert_eq!(code, 2, "disable needs --yes");
    ok(
        root.path(),
        &[
            "--yes",
            "server",
            "rcon",
            "disable",
            &id,
            "--expected-revision",
            "3",
            space[0],
            space[1],
        ],
        None,
    );
    let status = ok(
        root.path(),
        &["server", "rcon", "status", &id, space[0], space[1]],
        None,
    );
    assert_eq!(status["data"]["draft"], Value::Null);
    assert_eq!(status["data"]["password_available"], false);
}

#[test]
fn reforger_rcon_renders_a_protected_reference_and_exports_no_password() {
    let root = fixture();
    let id = add(root.path(), "reforger", "RconReforger");
    let space = ["--game-space", "reforger"];
    ok(
        root.path(),
        &[
            "server",
            "rcon",
            "configure",
            &id,
            "--expected-revision",
            "1",
            "--port",
            "19999",
            "--permission",
            "admin",
            "--max-clients",
            "4",
            space[0],
            space[1],
        ],
        None,
    );
    ok(
        root.path(),
        &[
            "server",
            "rcon",
            "set-password",
            &id,
            "--expected-revision",
            "2",
            space[0],
            space[1],
        ],
        Some(&format!("{PASSWORD}\n")),
    );
    let preview = ok(
        root.path(),
        &["server", "configuration", "plan", &id, space[0], space[1]],
        None,
    );
    let file = preview["data"]["preview"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["relative_path"] == "server.json")
        .unwrap();
    let rendered: Value = serde_json::from_str(file["contents"].as_str().unwrap()).unwrap();
    assert_eq!(rendered["rcon"]["port"], 19999);
    assert_eq!(rendered["rcon"]["permission"], "admin");
    assert_eq!(rendered["rcon"]["maxClients"], 4);
    assert_eq!(rendered["rcon"]["address"], "127.0.0.1");
    assert!(
        rendered["rcon"]["password"]
            .as_str()
            .unwrap()
            .starts_with("foxy-secret:secret_")
    );
    let export = root.path().join("exported-server.json");
    ok(
        root.path(),
        &[
            "server",
            "configuration",
            "export-reforger",
            &id,
            "--file",
            export.to_str().unwrap(),
            space[0],
            space[1],
        ],
        None,
    );
    let exported: Value = serde_json::from_slice(&fs::read(&export).unwrap()).unwrap();
    assert_eq!(exported["rcon"]["port"], 19999);
    assert!(exported["rcon"].get("password").is_none());
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn packet(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut covered = vec![0xFF, kind];
    covered.extend_from_slice(payload);
    let mut packet = b"BE".to_vec();
    packet.extend_from_slice(&crc32(&covered).to_le_bytes());
    packet.extend_from_slice(&covered);
    packet
}

struct Endpoint {
    address: String,
    commands: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn endpoint() -> Endpoint {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    let address = socket.local_addr().unwrap().to_string();
    let commands = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    {
        let commands = commands.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            while !stop.load(Ordering::Acquire) {
                let Ok((length, peer)) = socket.recv_from(&mut buffer) else {
                    continue;
                };
                let datagram = &buffer[..length];
                assert_eq!(&datagram[..2], b"BE");
                assert_eq!(
                    crc32(&datagram[6..]),
                    u32::from_le_bytes(datagram[2..6].try_into().unwrap())
                );
                match datagram[7] {
                    0x00 => {
                        let accepted = &datagram[8..] == PASSWORD.as_bytes();
                        socket
                            .send_to(&packet(0x00, &[u8::from(accepted)]), peer)
                            .unwrap();
                    }
                    0x01 => {
                        let sequence = datagram[8];
                        let text = datagram[9..].to_vec();
                        commands.lock().unwrap().push(text.clone());
                        if text == b"say -1 lost" {
                            continue;
                        }
                        let mut reply = vec![sequence];
                        if text == b"players" {
                            reply.extend_from_slice(b"Players on server: 0");
                        }
                        socket.send_to(&packet(0x01, &reply), peer).unwrap();
                    }
                    _ => {}
                }
            }
        });
    }
    Endpoint {
        address,
        commands,
        stop,
    }
}

#[test]
fn probe_distinguishes_refused_logins_acknowledged_commands_and_lost_replies() {
    let root = fixture();
    let server = endpoint();
    let probe = |extra: &[&str], password: &str| {
        let mut args = vec![
            "server",
            "rcon",
            "probe",
            "--endpoint",
            &server.address,
            "--password-stdin",
            "--timeout-ms",
            "1500",
        ];
        args.extend_from_slice(extra);
        run(root.path(), &args, Some(&format!("{password}\n")))
    };
    let (code, refused, text) = probe(&[], "wrong-password");
    assert_eq!(code, 4, "{text}");
    assert_eq!(refused["data"]["login"], "rejected");
    assert!(!text.contains("wrong-password"));
    let (code, _, _) = probe(&["--command", "players"], PASSWORD);
    assert_eq!(code, 2, "a command needs --yes");
    let (code, replied, text) = run(
        root.path(),
        &[
            "--yes",
            "server",
            "rcon",
            "probe",
            "--endpoint",
            &server.address,
            "--password-stdin",
            "--command",
            "players",
        ],
        Some(&format!("{PASSWORD}\n")),
    );
    assert_eq!(code, 0, "{text}");
    assert_eq!(replied["data"]["command"]["outcome"], "replied");
    assert_eq!(
        replied["data"]["command"]["response"],
        "Players on server: 0"
    );
    assert!(!text.contains(PASSWORD));
    let (code, lost, text) = run(
        root.path(),
        &[
            "--yes",
            "server",
            "rcon",
            "probe",
            "--endpoint",
            &server.address,
            "--password-stdin",
            "--timeout-ms",
            "2500",
            "--command",
            "say -1 lost",
        ],
        Some(&format!("{PASSWORD}\n")),
    );
    assert_eq!(code, 5, "{text}");
    assert_eq!(lost["data"]["command"]["outcome"], "unknown");
    assert_eq!(lost["data"]["command"]["reason"], "no_reply");
    let sent = server
        .commands
        .lock()
        .unwrap()
        .iter()
        .filter(|text| text.as_slice() == b"say -1 lost")
        .count();
    assert_eq!(sent, 1, "a lost command is never resent");
    let (code, _, _) = run(
        root.path(),
        &["server", "rcon", "probe", "--endpoint", &server.address],
        None,
    );
    assert_eq!(code, 2, "a password is never taken from arguments");
}
