use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const INSTANCE_ID: &str = "server_00000000000000000000000000000001";

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("games.json"),
        serde_json::to_vec_pretty(&json!({
            "active_game_space_id": "alpha",
            "last_opened_game_space_id": "alpha",
            "game_spaces": [
                {"id": "alpha", "game_id": "arma3", "display_name": "Alpha", "created_at": 0},
                {"id": "beta", "game_id": "reforger", "display_name": "Beta", "created_at": 0}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(root.path().join("app_settings.json"), "{}").unwrap();
    for space in ["alpha", "beta"] {
        let dir = root.path().join("games").join(space);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("game_settings.json"), "{}").unwrap();
    }
    root
}

fn cli(root: &Path, args: &[&str]) -> Command {
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

fn invoke(root: &Path, args: &[&str], expected_code: i32) -> Value {
    let output = cli(root, args).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(expected_code),
        "args={args:?}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["ok"], expected_code == 0);
    if expected_code != 0 {
        assert!(result["data"].is_null());
        assert!(result["errors"].is_array());
    }
    result
}

fn assert_exit(root: &Path, args: &[&str], expected_code: i32) {
    let output = cli(root, args).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(expected_code),
        "args={args:?}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct OwnerProcess {
    child: Child,
    lifetime: Option<ChildStdin>,
}

impl OwnerProcess {
    fn start(root: &Path, space: &str) -> Self {
        let mut child = cli(
            root,
            &[
                "server",
                "supervise",
                "--game-space",
                space,
                "--parent-bound",
            ],
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
        let lifetime = child.stdin.take();
        Self { child, lifetime }
    }

    fn stop(&mut self) {
        self.lifetime.take();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(Instant::now() < deadline, "Foreground owner did not exit");
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for OwnerProcess {
    fn drop(&mut self) {
        self.lifetime.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn database_path(root: &Path, space: &str) -> std::path::PathBuf {
    root.join("games").join(space).join("database.db")
}

#[test]
fn lifecycle_dry_runs_are_write_free_and_status_uses_the_foreground_owner() {
    let root = fixture();
    let stop = invoke(
        root.path(),
        &[
            "--dry-run",
            "server",
            "instance",
            "stop",
            INSTANCE_ID,
            "--game-space",
            "alpha",
            "--expected-run-generation",
            "9",
            "--timeout-ms",
            "1200",
            "--force",
        ],
        0,
    );
    assert_eq!(stop["data"]["database_opened"], false);
    assert_eq!(stop["data"]["writes_performed"], false);
    assert_eq!(stop["data"]["owner_contacted"], false);
    assert_eq!(stop["data"]["request_sent"], false);
    assert_eq!(stop["data"]["intent"]["instance_id"], INSTANCE_ID);
    assert_eq!(stop["data"]["intent"]["expected_run_generation"], 9);
    assert_eq!(stop["data"]["intent"]["timeout_ms"], 1200);
    assert_eq!(stop["data"]["intent"]["force"], true);
    assert!(!database_path(root.path(), "alpha").exists());
    assert!(!root.path().join("games/alpha/server").exists());

    let status_preview = invoke(
        root.path(),
        &[
            "--dry-run",
            "server",
            "instance",
            "status",
            INSTANCE_ID,
            "--game-space",
            "alpha",
        ],
        0,
    );
    assert_eq!(status_preview["data"]["database_opened"], false);
    assert_eq!(status_preview["data"]["owner_contacted"], false);
    assert_eq!(status_preview["data"]["request_sent"], false);
    assert!(!database_path(root.path(), "alpha").exists());

    let mut owner = OwnerProcess::start(root.path(), "alpha");
    let deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        let output = cli(
            root.path(),
            &[
                "server",
                "instance",
                "status",
                INSTANCE_ID,
                "--game-space",
                "alpha",
            ],
        )
        .output()
        .unwrap();
        if output.status.success() {
            break serde_json::from_slice::<Value>(&output.stdout).unwrap();
        }
        assert_eq!(output.status.code(), Some(4));
        assert!(
            Instant::now() < deadline,
            "Owner status did not become available"
        );
        thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(status["data"]["status"]["phase"], "not_owned");
    assert_eq!(status["data"]["status"]["instance_id"], INSTANCE_ID);
    assert_eq!(status["data"]["database_opened"], false);
    assert_eq!(status["data"]["writes_performed"], false);
    assert_eq!(status["data"]["process_authority"], false);
    assert_eq!(status["data"]["readiness_claimed"], false);
    assert!(!database_path(root.path(), "alpha").exists());
    owner.stop();
    assert!(!database_path(root.path(), "alpha").exists());
}

#[test]
fn lifecycle_cli_requires_generation_and_yes_and_reports_a_missing_instance() {
    let root = fixture();
    assert_exit(
        root.path(),
        &[
            "server",
            "instance",
            "stop",
            INSTANCE_ID,
            "--game-space",
            "alpha",
        ],
        2,
    );
    assert_exit(
        root.path(),
        &[
            "server",
            "instance",
            "restart",
            INSTANCE_ID,
            "--game-space",
            "alpha",
            "--expected-run-generation",
            "1",
        ],
        2,
    );
    for extra in [Vec::<&str>::new(), vec!["--force"]] {
        let mut args = vec![
            "server",
            "instance",
            "stop",
            INSTANCE_ID,
            "--game-space",
            "alpha",
            "--expected-run-generation",
            "1",
        ];
        args.extend(extra);
        let result = invoke(root.path(), &args, 2);
        assert!(
            result["errors"][0]
                .as_str()
                .unwrap_or_default()
                .contains("requires --yes")
        );
    }
    let missing = invoke(
        root.path(),
        &[
            "server",
            "instance",
            "start-plan",
            INSTANCE_ID,
            "--game-space",
            "beta",
        ],
        3,
    );
    assert!(
        missing["errors"][0]
            .as_str()
            .unwrap_or_default()
            .contains("not found")
    );
    assert!(!database_path(root.path(), "alpha").exists());
    assert!(!database_path(root.path(), "beta").exists());
    assert!(!root.path().join("games/alpha/server").exists());
    assert!(!root.path().join("games/beta/server").exists());
}

#[test]
fn repeated_failed_lifecycle_requests_remain_acknowledged_by_one_owner() {
    let root = fixture();
    let mut owner = OwnerProcess::start(root.path(), "alpha");
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let output = cli(root.path(), &["server", "owner", "--game-space", "alpha"])
            .output()
            .unwrap();
        if output.status.success() {
            break;
        }
        assert_eq!(output.status.code(), Some(4));
        assert!(
            Instant::now() < deadline,
            "Foreground owner did not become inspectable"
        );
        thread::sleep(Duration::from_millis(25));
    }

    for _ in 0..40 {
        let result = invoke(
            root.path(),
            &[
                "server",
                "instance",
                "stop",
                INSTANCE_ID,
                "--game-space",
                "alpha",
                "--expected-run-generation",
                "1",
                "--timeout-ms",
                "1000",
                "--yes",
            ],
            4,
        );
        let errors = result["errors"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            errors.contains("not found"),
            "Expected a known missing-instance refusal, got {errors}"
        );
        assert!(!errors.contains("ledger"));
        assert!(!errors.contains("unknown"));
        assert!(!database_path(root.path(), "alpha").exists());
    }

    owner.stop();
    assert!(!database_path(root.path(), "alpha").exists());
}

#[cfg(windows)]
#[test]
fn internal_console_signal_rejects_zero_and_bad_handles_without_filesystem_effects() {
    let root = fixture();
    for handle in ["0", "invalid"] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
        command
            .args(["server", "console-signal", handle])
            .env("FOXY_CONFIG_DIR", root.path());
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
        assert_eq!(command.status().unwrap().code(), Some(2));
        assert!(!database_path(root.path(), "alpha").exists());
        assert!(!root.path().join("games/alpha/server").exists());
    }
}
