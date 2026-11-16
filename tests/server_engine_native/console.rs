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

pub(crate) fn run() -> io::Result<()> {
    let path = std::env::var_os("FOXY_ARMA_CONSOLE_REQUEST")
        .ok_or_else(|| io::Error::other("Missing contained console request"))?;
    super::payload::ordinary(std::path::Path::new(&path), false)?;
    let mut bytes = vec![];
    fs::File::open(path)?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        return Err(io::Error::other("Console request exceeds bounds"));
    }
    let request: Request = serde_json::from_slice(&bytes)?;
    if !request.executable.is_absolute()
        || !request.directory.is_absolute()
        || !request.log.is_absolute()
        || request.arguments.len() > 64
        || request
            .arguments
            .iter()
            .any(|argument| argument.len() > 4096)
        || request.executable != request.directory.join("arma3server_x64")
    {
        return Err(io::Error::other("Invalid contained console request"));
    }
    super::payload::ordinary(
        request
            .log
            .parent()
            .ok_or_else(|| io::Error::other("Missing log parent"))?,
        true,
    )?;
    let log = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&request.log)?;
    let status = Command::new(&request.executable)
        .current_dir(request.directory)
        .args(request.arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .status()?;
    if !status.success() {
        return Err(io::Error::other(
            "Contained native engine exited unsuccessfully",
        ));
    }
    Ok(())
}
