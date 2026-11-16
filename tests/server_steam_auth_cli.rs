use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SECRET_MARKER: &str = "synthetic_private_marker_never_emitted";
const GAME_SPACE: &str = "alpha";

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    fs::write(
        root_path.join("games.json"),
        serde_json::to_vec_pretty(&json!({
            "active_game_space_id": GAME_SPACE,
            "last_opened_game_space_id": GAME_SPACE,
            "game_spaces": [
                {"id": GAME_SPACE, "game_id": "arma3", "display_name": "Alpha", "created_at": 0}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(root_path.join("app_settings.json"), "{}").unwrap();
    let games = root_path.join("games");
    let space = games.join(GAME_SPACE);
    fs::create_dir(&games).unwrap();
    fs::create_dir(&space).unwrap();
    fs::write(space.join("game_settings.json"), "{}").unwrap();
    set_private_dir(&root_path);
    set_private_dir(&games);
    set_private_dir(&space);
    root
}

fn set_private_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn auth_dir(root: &Path) -> PathBuf {
    root.join("games")
        .join(GAME_SPACE)
        .join("server")
        .join("steam-auth")
}

fn cli(root: &Path, args: &[impl AsRef<std::ffi::OsStr>]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .arg("--config-dir")
        .arg(root)
        .arg("--json")
        .args(args)
        .env("FOXY_CONFIG_DIR", root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}

fn run_cli(
    root: &Path,
    args: &[impl AsRef<std::ffi::OsStr>],
    stdin: Stdio,
    stderr: Stdio,
    inherit_console: bool,
    timeout: Duration,
) -> Output {
    let mut command = cli(root, args);
    #[cfg(windows)]
    if inherit_console {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0);
    }
    #[cfg(not(windows))]
    let _ = inherit_console;
    let mut child = command
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(stderr)
        .spawn()
        .unwrap();
    wait_for_child(&mut child, timeout);
    child.wait_with_output().unwrap()
}

fn wait_for_child(child: &mut Child, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait().unwrap().is_some() {
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Steam auth CLI child exceeded its bounded wait");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn parsed(output: &Output, expected_code: i32) -> Value {
    let value: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("Steam auth CLI did not emit JSON"));
    check_no_secret(output);
    let result = &value["data"]["outcome"];
    check(
        output.status.code() == Some(expected_code),
        &format!(
            "unexpected Steam auth exit code {:?} (phase {}, status {}, error {}, errors {})",
            output.status.code(),
            result["phase"],
            result["status"],
            result["error"],
            value["errors"]
        ),
    );
    check(
        value.get("action").and_then(Value::as_str).is_some(),
        "missing action envelope field",
    );
    check(value.get("data").is_some(), "missing data envelope field");
    value
}

fn check(condition: bool, label: &str) {
    assert!(condition, "{label}");
}

fn check_no_secret(output: &Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    check(
        !stdout.contains(SECRET_MARKER),
        "credential marker appeared in stdout",
    );
    check(
        !stderr.contains(SECRET_MARKER),
        "credential marker appeared in stderr",
    );
    let value: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("Steam auth CLI did not emit JSON"));
    check(
        !contains_sensitive_key(&value),
        "credential field appeared in CLI JSON",
    );
}

fn contains_sensitive_key(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, child)| {
            matches!(key.as_str(), "password" | "account_name" | "refresh_token")
                || contains_sensitive_key(child)
        }),
        Value::Array(items) => items.iter().any(contains_sensitive_key),
        _ => false,
    }
}

fn account_command(verb: &str, alias: &str) -> Vec<String> {
    vec![
        "server".into(),
        "steam".into(),
        "--game-space".into(),
        GAME_SPACE.into(),
        verb.into(),
        "--account".into(),
        alias.into(),
    ]
}

fn outcome(value: &Value) -> &Value {
    value
        .get("data")
        .and_then(|data| data.get("outcome"))
        .unwrap_or_else(|| panic!("missing Steam auth outcome"))
}

fn assert_common_outcome(value: &Value) {
    check(value["ok"] == true, "expected success envelope");
    check(
        outcome(value)["database_opened"] == false,
        "Steam auth opened the database",
    );
}

#[test]
fn steam_auth_inspection_and_gates_are_offline_write_free_and_database_free() {
    let root = fixture();
    let root_path = root.path().canonicalize().unwrap();
    let auth = auth_dir(&root_path);

    let status = run_cli(
        &root_path,
        &account_command("status", "default"),
        Stdio::null(),
        Stdio::piped(),
        false,
        Duration::from_secs(20),
    );
    let status_json = parsed(&status, 0);
    assert_common_outcome(&status_json);
    check(
        outcome(&status_json)["prepared"] == false,
        "missing status was not reported as unprepared",
    );
    check(
        outcome(&status_json)["writes_performed"] == false,
        "status reported writes",
    );
    check(!auth.exists(), "status created the session namespace");

    let invalid_alias = run_cli(
        &root_path,
        &account_command("status", "../bad"),
        Stdio::null(),
        Stdio::piped(),
        false,
        Duration::from_secs(20),
    );
    let invalid_json = parsed(&invalid_alias, 2);
    assert_common_outcome(&invalid_json);
    check(
        outcome(&invalid_json)["error"] == "invalid_input",
        "invalid alias was not rejected",
    );

    let absent_credentials = root_path.join("credentials-does-not-exist.json");
    let dry_run = run_cli(
        &root_path,
        &[
            OsString::from("--dry-run"),
            OsString::from("server"),
            OsString::from("steam"),
            OsString::from("--game-space"),
            OsString::from(GAME_SPACE),
            OsString::from("login"),
            OsString::from("--credentials-file"),
            absent_credentials.as_os_str().to_owned(),
        ],
        Stdio::null(),
        Stdio::piped(),
        false,
        Duration::from_secs(20),
    );
    let dry_json = parsed(&dry_run, 0);
    check(dry_json["ok"] == true, "dry-run did not succeed");
    check(
        dry_json["data"]["dry_run"] == true,
        "dry-run marker missing",
    );
    check(
        dry_json["data"]["writes_performed"] == false,
        "dry-run reported writes",
    );
    check(
        dry_json["data"]["database_opened"] == false,
        "dry-run opened database",
    );
    check(
        !absent_credentials.exists(),
        "dry-run created the credential source",
    );
    check(!auth.exists(), "dry-run created the session namespace");

    let resume = run_cli(
        &root_path,
        &account_command("resume", "default"),
        Stdio::null(),
        Stdio::piped(),
        false,
        Duration::from_secs(20),
    );
    let resume_json = parsed(&resume, 4);
    assert_common_outcome(&resume_json);
    check(
        outcome(&resume_json)["error"] == "authentication_required",
        "missing resume did not request authentication",
    );
    check(
        outcome(&resume_json)["prepared"] == false,
        "missing resume reported a prepared token",
    );
    check(
        outcome(&resume_json)["writes_performed"] == false,
        "missing resume reported writes",
    );
    check(
        !auth.exists(),
        "missing resume created the session namespace",
    );

    let forget_args = account_command("forget", "default");
    let forget_args: Vec<_> = forget_args.iter().map(String::as_str).collect();
    let forget = run_cli(
        &root_path,
        &forget_args,
        Stdio::null(),
        Stdio::piped(),
        false,
        Duration::from_secs(20),
    );
    let forget_json = parsed(&forget, 2);
    check(
        forget_json["ok"] == false,
        "forget without confirmation was accepted",
    );
    check(
        forget_json["errors"].as_array().is_some_and(|errors| {
            errors
                .iter()
                .any(|error| error.as_str().is_some_and(|s| s.contains("--yes")))
        }),
        "forget confirmation error missing",
    );

    let wipe = run_cli(
        &root_path,
        &["--wipe-db", "--yes", "server", "steam", "status"],
        Stdio::null(),
        Stdio::piped(),
        false,
        Duration::from_secs(20),
    );
    let wipe_json = parsed(&wipe, 2);
    check(
        wipe_json["ok"] == false,
        "Steam auth accepted database wipe",
    );
    check(
        !root_path.join("games/alpha/database.db").exists(),
        "Steam auth created database.db",
    );
    check(
        !auth.exists(),
        "write-free Steam checks created the session namespace",
    );

    let lock_path = root_path.join("games/alpha/database.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    lock.try_lock().unwrap();
    let locked_status = run_cli(
        &root_path,
        &account_command("status", "default"),
        Stdio::null(),
        Stdio::piped(),
        false,
        Duration::from_secs(20),
    );
    let locked_json = parsed(&locked_status, 0);
    assert_common_outcome(&locked_json);
    check(
        locked_json["ok"] == true,
        "status depended on the database lock",
    );
    check(
        !root_path.join("games/alpha/database.db").exists(),
        "status created database.db",
    );
    drop(lock);

    let forget_yes = run_cli(
        &root_path,
        &[
            "--yes",
            "server",
            "steam",
            "--game-space",
            GAME_SPACE,
            "forget",
        ],
        Stdio::null(),
        Stdio::piped(),
        false,
        Duration::from_secs(20),
    );
    let forget_yes_json = parsed(&forget_yes, 0);
    assert_common_outcome(&forget_yes_json);
    check(
        outcome(&forget_yes_json)["status"] == "forgotten",
        "confirmed forget was not accepted",
    );
    check(
        !root_path.join("games/alpha/database.db").exists(),
        "confirmed forget created database.db",
    );
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
#[test]
#[ignore = "requires a real Steam account and attended Steam Guard input"]
fn native_windows_or_linux_account_login_and_prepared_reuse() {
    let credentials = std::env::var_os("FOXY_NATIVE_STEAM_CREDENTIALS")
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("FOXY_NATIVE_STEAM_CREDENTIALS is required"));
    let scratch = std::env::var_os("FOXY_NATIVE_STEAM_SCRATCH")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    fs::create_dir_all(&scratch).unwrap();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root_path = scratch.join(format!("foxy-native-steam-{}-{stamp}", std::process::id()));
    fs::create_dir(&root_path).unwrap();
    let root_path = root_path.canonicalize().unwrap();
    write_fixture(&root_path);

    let mut phase_count = 0u64;
    let mut login_args = vec![
        OsString::from("server"),
        OsString::from("steam"),
        OsString::from("--game-space"),
        OsString::from(GAME_SPACE),
        OsString::from("login"),
        OsString::from("--credentials-file"),
        credentials.as_os_str().to_owned(),
    ];
    login_args.push(OsString::from("--account"));
    login_args.push(OsString::from("default"));
    let login = run_cli(
        &root_path,
        &login_args,
        Stdio::inherit(),
        Stdio::inherit(),
        true,
        Duration::from_secs(315),
    );
    let login_json = parsed(&login, 0);
    let login_outcome = outcome(&login_json);
    check(
        login_outcome["authenticated"] == true,
        "native login did not authenticate",
    );
    check(
        login_outcome["prepared"] == true,
        "native login did not prepare credentials",
    );
    check(
        login_outcome["credentials_saved"] == true,
        "native login did not save credentials",
    );
    check(
        login_outcome["disconnected"] == true,
        "native login did not disconnect",
    );
    phase_count += u64::from(login_outcome["phase"] == "authenticated");

    let record = auth_dir(&root_path).join("default.json");
    let prepared_before = fs::read(&record).unwrap();
    for _ in 0..2 {
        let resume = run_cli(
            &root_path,
            &account_command("resume", "default"),
            Stdio::null(),
            Stdio::inherit(),
            false,
            Duration::from_secs(315),
        );
        let resume_json = parsed(&resume, 0);
        let resume_outcome = outcome(&resume_json);
        check(
            resume_outcome["authenticated"] == true,
            "prepared resume did not authenticate",
        );
        check(
            resume_outcome["prepared"] == true,
            "prepared resume lost its credential",
        );
        check(
            resume_outcome["disconnected"] == true,
            "prepared resume did not disconnect",
        );
        phase_count += u64::from(resume_outcome["phase"] == "authenticated");
    }

    let alternate = run_cli(
        &root_path,
        &account_command("resume", "alternate"),
        Stdio::null(),
        Stdio::inherit(),
        false,
        Duration::from_secs(30),
    );
    let alternate_json = parsed(&alternate, 4);
    check(
        outcome(&alternate_json)["error"] == "authentication_required",
        "alternate alias unexpectedly resumed",
    );
    check(
        fs::read(&record).unwrap() == prepared_before,
        "alternate resume changed prepared bytes",
    );

    let anonymous = run_cli(
        &root_path,
        &[
            OsString::from("server"),
            OsString::from("steam"),
            OsString::from("--game-space"),
            OsString::from(GAME_SPACE),
            OsString::from("anonymous"),
        ],
        Stdio::null(),
        Stdio::inherit(),
        false,
        Duration::from_secs(315),
    );
    let anonymous_json = parsed(&anonymous, 0);
    check(
        outcome(&anonymous_json)["authenticated"] == true,
        "anonymous Steam connection failed",
    );
    check(
        outcome(&anonymous_json)["disconnected"] == true,
        "anonymous Steam connection did not disconnect",
    );
    phase_count += u64::from(outcome(&anonymous_json)["phase"] == "authenticated");

    let final_resume = run_cli(
        &root_path,
        &account_command("resume", "default"),
        Stdio::null(),
        Stdio::inherit(),
        false,
        Duration::from_secs(315),
    );
    let final_json = parsed(&final_resume, 0);
    check(
        outcome(&final_json)["authenticated"] == true,
        "saved account did not resume after anonymous session",
    );
    check(
        fs::read(&record).unwrap() == prepared_before,
        "successful resume changed prepared bytes",
    );
    phase_count += u64::from(outcome(&final_json)["phase"] == "authenticated");

    let summary = json!({
        "login_authenticated": true,
        "prepared_resumes": 3,
        "alternate_resume_rejected": true,
        "anonymous_authenticated": true,
        "final_account_resume": true,
        "authenticated_terminal_phases": phase_count
    });
    fs::write(
        root_path.join("gui-info.json"),
        serde_json::to_vec_pretty(&json!({
            "config_dir": root_path,
            "game_space": GAME_SPACE,
            "alias": "default",
            "summary": summary
        }))
        .unwrap(),
    )
    .unwrap();
    println!("server_steam_auth_cli.rs native terminal_phases={phase_count}");
}

fn write_fixture(root: &Path) {
    fs::write(
        root.join("games.json"),
        serde_json::to_vec_pretty(&json!({
            "active_game_space_id": GAME_SPACE,
            "last_opened_game_space_id": GAME_SPACE,
            "game_spaces": [
                {"id": GAME_SPACE, "game_id": "arma3", "display_name": "Alpha", "created_at": 0}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(root.join("app_settings.json"), "{}").unwrap();
    let games = root.join("games");
    let space = games.join(GAME_SPACE);
    fs::create_dir(&games).unwrap();
    fs::create_dir(&space).unwrap();
    fs::write(space.join("game_settings.json"), "{}").unwrap();
    set_private_dir(root);
    set_private_dir(&games);
    set_private_dir(&space);
}
