use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub executable: PathBuf,
    pub directory: PathBuf,
    pub arguments: Vec<String>,
    pub log: PathBuf,
}

pub(super) fn run() -> io::Result<()> {
    let request_path = std::env::var_os("FOXY_ARMA_ACTIVATION_CONSOLE_REQUEST")
        .ok_or_else(|| io::Error::other("Missing contained native activation request"))?;
    let request_path = PathBuf::from(request_path);
    let metadata = fs::symlink_metadata(&request_path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err(io::Error::other("Invalid native activation request file"));
    }
    let mut bytes = Vec::new();
    fs::File::open(request_path)?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        return Err(io::Error::other("Native activation request exceeds bound"));
    }
    let request: Request = serde_json::from_slice(&bytes)?;
    if !request.executable.is_absolute()
        || !request.directory.is_absolute()
        || !request.log.is_absolute()
        || request.arguments.len() > 64
        || request.arguments.iter().any(|arg| arg.len() > 4096)
        || request.executable != request.directory.join("arma3server_x64")
    {
        return Err(io::Error::other("Invalid native activation request values"));
    }
    let log_metadata = fs::symlink_metadata(
        request
            .log
            .parent()
            .ok_or_else(|| io::Error::other("Missing native activation log directory"))?,
    )?;
    if log_metadata.file_type().is_symlink() || !log_metadata.is_dir() {
        return Err(io::Error::other("Unsafe native activation log directory"));
    }
    let log = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&request.log)?;
    let status = Command::new(&request.executable)
        .current_dir(&request.directory)
        .args(request.arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .status()?;
    if !status.success() {
        return Err(io::Error::other(
            "Contained native activation engine exited unsuccessfully",
        ));
    }
    Ok(())
}
