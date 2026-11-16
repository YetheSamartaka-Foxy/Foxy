use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use super::Platform;

pub(super) struct Request(PathBuf);

impl Request {
    pub fn login(home: &Path, username: &str, cached_only: bool) -> io::Result<Self> {
        Self::create(home, &render(username, cached_only))
    }

    pub fn install(
        home: &Path,
        directory: &Path,
        username: &str,
        cached_only: bool,
        app: u32,
        platform: Platform,
    ) -> io::Result<Self> {
        let directory = install_path(directory)?;
        let text = render_install(directory, username, cached_only, app, platform);
        Self::create(home, &text)
    }

    fn create(home: &Path, text: &str) -> io::Result<Self> {
        let mut nonce = [0; 16];
        File::open("/dev/urandom")?.read_exact(&mut nonce)?;
        let nonce = nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = home.join(format!("steamcmd-request-{nonce}.txt"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let request = Self(path);
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        Ok(request)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn render(username: &str, cached_only: bool) -> String {
    format!(
        "@ShutdownOnFailedCommand 1\n@NoPromptForPassword {}\nlogin {username}\nquit\n",
        u8::from(cached_only)
    )
}

fn render_install(
    directory: &str,
    username: &str,
    cached_only: bool,
    app: u32,
    platform: Platform,
) -> String {
    format!(
        "@ShutdownOnFailedCommand 1\n@NoPromptForPassword {}\n@sSteamCmdForcePlatformType {}\nforce_install_dir \"{directory}\"\nlogin {username}\napp_update {app} -beta public validate\nquit\n",
        u8::from(cached_only),
        platform.steamcmd_name()
    )
}

fn install_path(path: &Path) -> io::Result<&str> {
    let text = path
        .to_str()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| io::Error::other("Invalid install directory."))?;
    if !path.is_absolute()
        || text
            .chars()
            .any(|character| character.is_control() || matches!(character, '"' | ';' | '\\'))
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(io::Error::other("Invalid install directory."));
    }
    Ok(text)
}

#[test]
fn install_paths_allow_quoted_spaces_and_refuse_command_injection() {
    assert_eq!(
        install_path(Path::new("/tmp/fixture source")).unwrap(),
        "/tmp/fixture source"
    );
    for path in [
        "relative",
        "/tmp/../source",
        "/tmp/source\nquit",
        "/tmp/source\"",
        "/tmp/source;quit",
    ] {
        assert!(install_path(Path::new(path)).is_err());
    }
    let request = render_install(
        "/tmp/fixture source",
        "fixture_account",
        true,
        233780,
        Platform::Linux,
    );
    assert!(request.starts_with("@ShutdownOnFailedCommand 1\n@NoPromptForPassword 1\n"));
    assert!(request.contains("@sSteamCmdForcePlatformType linux\n"));
    assert!(
        request.find("force_install_dir").unwrap() < request.find("login fixture_account").unwrap()
    );
    assert!(request.ends_with("app_update 233780 -beta public validate\nquit\n"));
}

#[test]
fn install_platform_is_typed_and_precedes_install_commands() {
    for (platform, expected) in [(Platform::Linux, "linux"), (Platform::Windows, "windows")] {
        let request = render_install("/tmp/fixture", "fixture_account", false, 233780, platform);
        let platform_line = format!("@sSteamCmdForcePlatformType {expected}\n");
        let platform_index = request.find(&platform_line).unwrap();
        assert!(platform_index < request.find("force_install_dir").unwrap());
        assert!(request.find("force_install_dir").unwrap() < request.find("login").unwrap());
        assert!(request.ends_with("app_update 233780 -beta public validate\nquit\n"));
    }
}

#[test]
fn request_stops_on_failure_and_cache_verification_disables_password_prompts() {
    assert_eq!(
        render("fixture_account", true),
        "@ShutdownOnFailedCommand 1\n@NoPromptForPassword 1\nlogin fixture_account\nquit\n"
    );
    assert!(render("fixture_account", false).contains("@NoPromptForPassword 0\n"));
}

#[test]
#[ignore = "Requires an explicitly selected real SteamCMD runtime; never logs into an account"]
fn native_failed_login_command_exits_nonzero() -> io::Result<()> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let directory = std::env::var_os("FOXY_STEAMCMD_DIR")
        .ok_or_else(|| io::Error::other("Set FOXY_STEAMCMD_DIR."))?;
    let home = std::env::var_os("FOXY_STEAMCMD_HOME")
        .ok_or_else(|| io::Error::other("Set FOXY_STEAMCMD_HOME."))?;
    let request = Request::create(
        Path::new(&home),
        "@ShutdownOnFailedCommand 1\nlogin\nquit\n",
    )?;
    let mut child = Command::new(Path::new(&directory).join("steamcmd.sh"))
        .current_dir(&directory)
        .env("HOME", home)
        .arg("+runscript")
        .arg(request.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait()? {
            assert!(
                !status.success(),
                "Vendor failed-command execution must not report success"
            );
            break;
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Vendor failure probe timed out.",
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}
