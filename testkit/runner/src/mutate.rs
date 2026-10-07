use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

pub struct Mutation {
    pub executable: PathBuf,
    pub root: PathBuf,
    /// The first journal is `mutations.json`; every further `mutate` in the
    /// same iteration gets its own numbered journal, restored in reverse.
    pub journal: PathBuf,
    pub timeout: Duration,
    pub journals: Vec<PathBuf>,
}

impl Mutation {
    pub fn apply(&mut self, operation: &Value) -> Result<Value> {
        if let Some(path) = operation["path"].as_str() {
            self.root = PathBuf::from(path);
        }
        let journal = if self.journals.is_empty() {
            self.journal.clone()
        } else {
            self.journal
                .with_file_name(format!("mutations-{}.json", self.journals.len()))
        };
        let mut command = Command::new(&self.executable);
        command
            .arg("mutate")
            .arg("--root")
            .arg(&self.root)
            .arg("--journal")
            .arg(&journal)
            .arg("--profile")
            .arg(operation["profile"].as_str().unwrap_or("single-entry"))
            .arg("--seed")
            .arg(operation["seed"].as_u64().unwrap_or(1).to_string());
        for (key, flag) in [
            ("entries", "--entries"),
            ("files", "--files"),
            ("bytes", "--truncate-by"),
        ] {
            if let Some(value) = operation.get(key) {
                command.arg(flag).arg(
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string()),
                );
            }
        }
        if operation["preserve_mtime"].as_bool().unwrap_or(false) {
            command.arg("--preserve-mtime");
        }
        for target in operation["targets"].as_array().into_iter().flatten() {
            command.arg("--target").arg(
                target
                    .as_str()
                    .context("Mutation targets must be strings")?,
            );
        }
        let output = crate::launch::process(&mut command, self.timeout)?;
        ensure!(
            output.status.success(),
            "Mutation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.journals.push(journal);
        Ok(serde_json::from_slice(&output.stdout)?)
    }
    pub fn restore(&mut self) -> Result<()> {
        while let Some(journal) = self.journals.pop() {
            if journal.exists() {
                restore(&self.executable, &self.root, &journal, self.timeout)?;
            }
        }
        Ok(())
    }
}

impl Drop for Mutation {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            eprintln!("Mutation restore failed; journal retained for recovery: {error}");
        }
    }
}

fn restore(exe: &Path, root: &Path, journal: &Path, timeout: Duration) -> Result<()> {
    let output = crate::launch::process(
        Command::new(exe)
            .arg("restore")
            .arg("--root")
            .arg(root)
            .arg("--journal")
            .arg(journal),
        timeout,
    )?;
    ensure!(
        output.status.success(),
        "Mutation restore failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_file(journal)?;
    Ok(())
}
