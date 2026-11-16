use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::super::preparation::Prepared;

pub(super) struct ParentOwner {
    child: Child,
    lifetime: Option<ChildStdin>,
}

impl ParentOwner {
    pub(super) fn start(config: &Path) -> io::Result<Self> {
        let mut command = cli(config, &["server", "supervise", "--parent-bound"]);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let lifetime = child.stdin.take();
        let mut owner = Self { child, lifetime };
        if let Err(error) = super::wait_owner_ready(config) {
            owner.stop()?;
            return Err(error);
        }
        if owner.child.try_wait()?.is_some() {
            return Err(io::Error::other(
                "Parent-bound metadata owner exited during startup",
            ));
        }
        Ok(owner)
    }

    pub(super) fn is_running(&mut self) -> io::Result<bool> {
        Ok(self.child.try_wait()?.is_none())
    }

    pub(super) fn kill_and_wait(&mut self) -> io::Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
        }
        self.lifetime.take();
        wait_child(&mut self.child, Duration::from_secs(8)).map(|_| ())
    }

    pub(super) fn stop(&mut self) -> io::Result<()> {
        self.lifetime.take();
        match wait_child(&mut self.child, Duration::from_secs(10)) {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                if self.child.try_wait()?.is_none() {
                    self.child.kill()?;
                }
                wait_child(&mut self.child, Duration::from_secs(8)).map(|_| ())
            }
            Err(error) => Err(error),
        }
    }
}

impl Drop for ParentOwner {
    fn drop(&mut self) {
        self.lifetime.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

pub(super) struct Controller {
    child: Child,
    output: PathBuf,
}

impl Controller {
    pub(super) fn start(
        config: &Path,
        id: &str,
        prepared: &Prepared,
        selection: u64,
        activation: u64,
        fingerprint: &str,
    ) -> io::Result<Self> {
        let selection = selection.to_string();
        let activation = activation.to_string();
        let mut command = cli(
            config,
            &[
                "--yes",
                "server",
                "deploy",
                "activation",
                "apply",
                id,
                &prepared.preparation,
                "--expected-deployment-generation",
                &selection,
                "--expected-activation-generation",
                &activation,
                "--expected-plan-fingerprint",
                fingerprint,
            ],
        );
        let output = config.join(format!("activation-controller-{selection}.json"));
        let stdout = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)?;
        let child = command
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self { child, output })
    }

    pub(super) fn kill_and_wait(&mut self) -> io::Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
        }
        wait_child(&mut self.child, Duration::from_secs(10)).map(|_| ())
    }

    pub(super) fn wait_after_owner_loss(&mut self) -> io::Result<ExitStatus> {
        let status = match wait_child(&mut self.child, Duration::from_secs(45)) {
            Ok(status) => Ok(status),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                if self.child.try_wait()?.is_none() {
                    self.child.kill()?;
                }
                wait_child(&mut self.child, Duration::from_secs(10))?;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Controller did not report the unknown outcome before its deadline",
                ));
            }
            Err(error) => Err(error),
        }?;
        let mut bytes = Vec::new();
        std::fs::File::open(&self.output)?
            .take(64 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 64 * 1024 {
            return Err(io::Error::other("Controller output exceeded bounds"));
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        if value["ok"] != false
            || value["action"] != "server.deploy.activation.apply"
            || !value["errors"].as_array().is_some_and(|errors| {
                errors.iter().any(|error| {
                    error
                        .as_str()
                        .is_some_and(|message| message.contains("outcome is unknown"))
                })
            })
        {
            return Err(io::Error::other(
                "Controller did not report an unknown deployment outcome",
            ));
        }
        Ok(status)
    }

    pub(super) fn is_running(&mut self) -> io::Result<bool> {
        Ok(self.child.try_wait()?.is_none())
    }
}

pub(super) fn cli(config: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .arg("--config-dir")
        .arg(config)
        .arg("--json")
        .args(args)
        .args(["--game-space", "arma3"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}

fn wait_child(child: &mut Child, timeout: Duration) -> io::Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "retained child wait timed out",
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}
