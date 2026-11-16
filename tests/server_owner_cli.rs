use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    fs::write(
        root_path.join("games.json"),
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
    fs::write(root_path.join("app_settings.json"), "{}").unwrap();
    for space in ["alpha", "beta"] {
        let dir = root_path.join("games").join(space);
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
    assert_eq!(result.as_object().unwrap().len(), 4);
    if expected_code != 0 {
        assert!(result["data"].is_null());
    }
    result
}

#[test]
fn detached_owner_preview_and_missing_consent_do_not_start_an_owner() {
    let root = fixture();
    let preview = invoke(
        root.path(),
        &[
            "server",
            "supervise",
            "--detach",
            "--dry-run",
            "--game-space",
            "alpha",
        ],
        0,
    );
    assert_eq!(preview["data"]["preview"], true);
    assert_eq!(preview["data"]["detach"], true);
    invoke(
        root.path(),
        &["server", "owner", "--game-space", "alpha"],
        4,
    );
    invoke(
        root.path(),
        &["server", "supervise", "--detach", "--game-space", "alpha"],
        2,
    );
    invoke(
        root.path(),
        &["server", "owner", "--game-space", "alpha"],
        4,
    );
    let rejected = cli(
        root.path(),
        &[
            "server",
            "supervise",
            "--detach",
            "--shutdown",
            "--yes",
            "--game-space",
            "alpha",
        ],
    )
    .output()
    .unwrap();
    assert_eq!(rejected.status.code(), Some(2));
    assert!(!rejected.stderr.is_empty());
    invoke(
        root.path(),
        &["server", "owner", "--game-space", "alpha"],
        4,
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

    fn wait_for_exit(&mut self, timeout: Duration) -> std::process::ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("Foreground owner did not exit before the deadline");
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn stop(&mut self) -> std::process::ExitStatus {
        self.lifetime.take();
        self.wait_for_exit(Duration::from_secs(8))
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

fn owner_snapshot(root: &Path, space: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let output = cli(root, &["server", "owner", "--game-space", space])
            .output()
            .unwrap();
        if output.status.success() {
            let result: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result["ok"], true);
            assert_eq!(result["data"]["database_opened"], false);
            return result["data"]["owner"].clone();
        }
        assert_eq!(output.status.code(), Some(4));
        if Instant::now() >= deadline {
            panic!("Foreground owner for {space} did not become inspectable");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn database_path(root: &Path, space: &str) -> std::path::PathBuf {
    root.join("games").join(space).join("database.db")
}

#[test]
fn a_crashed_metadata_owner_can_be_replaced_after_its_real_claim_is_gone() {
    let root = fixture();
    let mut previous = OwnerProcess::start(root.path(), "alpha");
    let before = owner_snapshot(root.path(), "alpha");
    previous.child.kill().unwrap();
    previous.child.wait().unwrap();
    invoke(
        root.path(),
        &["server", "owner", "--game-space", "alpha"],
        4,
    );
    let mut replacement = OwnerProcess::start(root.path(), "alpha");
    let after = owner_snapshot(root.path(), "alpha");
    assert_ne!(before["scope"]["owner_id"], after["scope"]["owner_id"]);
    assert_ne!(before["scope"]["generation"], after["scope"]["generation"]);
    assert!(!database_path(root.path(), "alpha").exists());
    assert!(replacement.stop().success());
}

#[test]
fn foreground_owner_is_scoped_and_allows_only_metadata_writes() {
    let root = fixture();
    let mut alpha = OwnerProcess::start(root.path(), "alpha");
    let alpha_snapshot = owner_snapshot(root.path(), "alpha");
    assert_eq!(alpha_snapshot["scope"]["space_id"], "alpha");
    assert_eq!(alpha_snapshot["game_id"], "arma3");
    assert_eq!(alpha_snapshot["database_claimed"], true);
    assert_eq!(alpha_snapshot["database_opened"], false);
    assert_eq!(
        alpha_snapshot["lifecycle_available"],
        cfg!(all(
            target_arch = "x86_64",
            any(windows, target_os = "linux")
        ))
    );
    assert_eq!(alpha_snapshot["active_sessions"], 0);
    assert!(!database_path(root.path(), "alpha").exists());

    let separate_attachment = invoke(
        root.path(),
        &["server", "owner", "--game-space", "alpha"],
        0,
    );
    for _ in 0..12 {
        let snapshot = invoke(
            root.path(),
            &["server", "owner", "--game-space", "alpha"],
            0,
        );
        assert_eq!(snapshot["data"]["owner"]["scope"], alpha_snapshot["scope"]);
    }
    assert_eq!(
        separate_attachment["data"]["owner"]["scope"],
        alpha_snapshot["scope"]
    );
    invoke(root.path(), &["server", "owner", "--game-space", "beta"], 4);

    invoke(
        root.path(),
        &[
            "server",
            "supervise",
            "--game-space",
            "alpha",
            "--parent-bound",
        ],
        6,
    );
    invoke(root.path(), &["repo", "list"], 6);

    let added = invoke(
        root.path(),
        &[
            "server",
            "instance",
            "add",
            "--game-space",
            "alpha",
            "--name",
            "MainServer",
        ],
        0,
    );
    let id = added["data"]["instance"]["id"].as_str().unwrap();
    assert_eq!(added["data"]["instance"]["revision"], 1);
    assert_eq!(added["data"]["database_opened"], false);
    invoke(
        root.path(),
        &[
            "server",
            "instance",
            "rename",
            id,
            "--game-space",
            "alpha",
            "--name",
            "TrainingServer",
            "--expected-revision",
            "1",
        ],
        0,
    );
    invoke(
        root.path(),
        &[
            "server",
            "instance",
            "rename",
            id,
            "--game-space",
            "alpha",
            "--name",
            "StaleEdit",
            "--expected-revision",
            "1",
        ],
        2,
    );
    let shown = invoke(
        root.path(),
        &["server", "instance", "show", id, "--game-space", "alpha"],
        0,
    );
    assert_eq!(shown["data"]["instance"]["revision"], 2);
    assert_eq!(shown["data"]["instance"]["display_name"], "TrainingServer");
    assert!(!database_path(root.path(), "alpha").exists());

    let mut beta = OwnerProcess::start(root.path(), "beta");
    let beta_snapshot = owner_snapshot(root.path(), "beta");
    assert_eq!(beta_snapshot["scope"]["space_id"], "beta");
    assert_eq!(beta_snapshot["game_id"], "reforger");
    assert_ne!(
        beta_snapshot["scope"]["context_key"],
        alpha_snapshot["scope"]["context_key"]
    );
    assert!(!database_path(root.path(), "beta").exists());
    assert_eq!(beta.stop().code(), Some(0));
    assert_eq!(alpha.stop().code(), Some(0));
}

#[test]
fn parent_eof_drains_owner_and_offline_metadata_writes_continue() {
    let root = fixture();
    let mut owner = OwnerProcess::start(root.path(), "alpha");
    let _snapshot = owner_snapshot(root.path(), "alpha");
    let created = invoke(
        root.path(),
        &[
            "server",
            "instance",
            "add",
            "--game-space",
            "alpha",
            "--name",
            "OwnedServer",
        ],
        0,
    );
    assert_eq!(created["data"]["database_opened"], false);
    assert_eq!(owner.stop().code(), Some(0));

    invoke(
        root.path(),
        &["server", "owner", "--game-space", "alpha"],
        4,
    );
    let id = created["data"]["instance"]["id"].as_str().unwrap();
    invoke(
        root.path(),
        &[
            "server",
            "instance",
            "rename",
            id,
            "--game-space",
            "alpha",
            "--name",
            "OfflineServer",
            "--expected-revision",
            "1",
        ],
        0,
    );
    assert!(!database_path(root.path(), "alpha").exists());
}

#[test]
fn owner_inspection_and_dry_run_without_owner_do_not_create_database_or_owner() {
    let root = fixture();
    invoke(
        root.path(),
        &["server", "owner", "--game-space", "alpha"],
        4,
    );
    invoke(
        root.path(),
        &[
            "--dry-run",
            "server",
            "instance",
            "add",
            "--game-space",
            "alpha",
            "--name",
            "PreviewServer",
        ],
        0,
    );
    invoke(
        root.path(),
        &[
            "--dry-run",
            "server",
            "supervise",
            "--game-space",
            "alpha",
            "--parent-bound",
        ],
        2,
    );
    assert!(!database_path(root.path(), "alpha").exists());
    assert!(!root.path().join("games/alpha/server").exists());
}
