use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn fresh_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}{:x}", std::process::id(), nanos)
}

pub(super) fn argument_path(path: &Path) -> io::Result<String> {
    let value = path
        .to_str()
        .ok_or_else(|| io::Error::other("Native argument path is not valid Unicode"))?
        .replace('\\', "/");
    let value = value
        .strip_prefix("//?/UNC/")
        .map(|tail| format!("//{tail}"))
        .or_else(|| value.strip_prefix("//?/").map(str::to_owned))
        .unwrap_or(value);
    #[cfg(windows)]
    let value = value.replace('/', "\\");
    Ok(value)
}

pub(super) fn argument_path_with_trailing_separator(path: &Path) -> io::Result<String> {
    let mut value = argument_path(path)?;
    if !value.ends_with(['/', '\\']) {
        value.push(std::path::MAIN_SEPARATOR);
    }
    Ok(value)
}

pub(super) fn normalized_path(path: &Path) -> String {
    normalized_path_text(&path.to_string_lossy())
}

pub(super) fn normalized_path_text(path: &str) -> String {
    let value = path.replace('\\', "/");
    let value = value
        .strip_prefix("//?/UNC/")
        .map(|tail| format!("//{tail}"))
        .or_else(|| value.strip_prefix("//?/").map(str::to_owned))
        .unwrap_or(value);
    value.replace('\\', "/").to_ascii_lowercase()
}

pub(super) fn is_under(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|tail| tail.starts_with('/'))
}

pub(super) fn private_environment(
    home: &Path,
    roots: &[PathBuf],
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut environment = vec![("HOME".into(), home.as_os_str().to_owned())];
    #[cfg(windows)]
    environment.extend([
        ("USERPROFILE".into(), home.as_os_str().to_owned()),
        ("APPDATA".into(), roots[5].as_os_str().to_owned()),
        ("LOCALAPPDATA".into(), roots[6].as_os_str().to_owned()),
        ("TEMP".into(), roots[4].as_os_str().to_owned()),
        ("TMP".into(), roots[4].as_os_str().to_owned()),
    ]);
    #[cfg(target_os = "linux")]
    environment.extend([
        ("XDG_CONFIG_HOME".into(), roots[0].as_os_str().to_owned()),
        ("XDG_DATA_HOME".into(), roots[1].as_os_str().to_owned()),
        ("XDG_CACHE_HOME".into(), roots[2].as_os_str().to_owned()),
        ("XDG_RUNTIME_DIR".into(), roots[3].as_os_str().to_owned()),
        ("TMPDIR".into(), roots[4].as_os_str().to_owned()),
    ]);
    environment
}
