use std::fs;
use std::io::{self, Read};
use std::path::Path;

pub(super) struct Credentials {
    pub username: String,
    pub password: String,
}

pub(super) fn load(path: &Path) -> io::Result<Credentials> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.is_symlink() || metadata.len() > 16 * 1024 {
        return Err(invalid());
    }
    let mut text = String::new();
    fs::File::open(path)?
        .take(16 * 1024 + 1)
        .read_to_string(&mut text)?;
    if text.len() > 16 * 1024 {
        return Err(invalid());
    }
    parse(&text)
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid local credentials config.",
    )
}

fn value(text: &str) -> io::Result<String> {
    let text = text.trim();
    let text = if text.starts_with(['\'', '"']) {
        if text.len() < 2 || text.as_bytes().first() != text.as_bytes().last() {
            return Err(invalid());
        }
        &text[1..text.len() - 1]
    } else {
        text
    };
    if text.is_empty() || text.len() > 4096 || text.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(text.to_owned())
}

fn parse(text: &str) -> io::Result<Credentials> {
    let (mut username, mut password) = (None, None);
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, raw) = line.split_once('=').ok_or_else(invalid)?;
        let slot = match key.trim() {
            "FOXY_STEAMCMD_USERNAME" => &mut username,
            "FOXY_STEAMCMD_PASSWORD" => &mut password,
            _ => return Err(invalid()),
        };
        if slot.is_some() {
            return Err(invalid());
        }
        *slot = Some(value(raw)?);
    }
    let username = username.ok_or_else(invalid)?;
    if !username
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(invalid());
    }
    Ok(Credentials {
        username,
        password: password.ok_or_else(invalid)?,
    })
}

#[test]
fn local_config_preserves_literal_password_characters_without_shell_expansion() {
    let credentials = parse(
        "# private fixture\r\nFOXY_STEAMCMD_USERNAME=fixture_account\r\nFOXY_STEAMCMD_PASSWORD=\"a=b#c$`\\value\"\r\n",
    ).unwrap();
    assert_eq!(credentials.username, "fixture_account");
    assert_eq!(credentials.password, "a=b#c$`\\value");
}

#[test]
fn ambiguous_empty_and_command_injecting_config_is_refused_without_values_in_errors() {
    for text in [
        "FOXY_STEAMCMD_USERNAME=fixture\nFOXY_STEAMCMD_PASSWORD=",
        "FOXY_STEAMCMD_USERNAME=fixture\nFOXY_STEAMCMD_PASSWORD='unterminated",
        "FOXY_STEAMCMD_USERNAME=fixture;quit\nFOXY_STEAMCMD_PASSWORD=example",
        "FOXY_STEAMCMD_USERNAME=fixture\nFOXY_STEAMCMD_PASSWORD=example\nFOXY_STEAMCMD_PASSWORD=other",
        "FOXY_STEAMCMD_USERNAME=fixture\nFOXY_STEAMCMD_PASSWORD=example\nUNEXPECTED=value",
    ] {
        let error = parse(text).err().unwrap();
        assert_eq!(error.to_string(), "Invalid local credentials config.");
    }
}
