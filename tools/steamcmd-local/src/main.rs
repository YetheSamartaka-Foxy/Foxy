use std::io;
use std::path::PathBuf;

mod credentials;
mod protocol;
#[cfg(target_os = "linux")]
mod request;
#[cfg(target_os = "linux")]
mod session;
#[cfg(target_os = "linux")]
mod terminal;

struct Options {
    credentials: PathBuf,
    directory: PathBuf,
    home: PathBuf,
    cached_only: bool,
    install_app: Option<u32>,
    install_directory: PathBuf,
    platform: Platform,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Platform {
    Linux,
    Windows,
}

impl Platform {
    fn parse(value: &str) -> io::Result<Self> {
        match value {
            "linux" => Ok(Self::Linux),
            "windows" => Ok(Self::Windows),
            _ => Err(io::Error::other(
                "Use --platform linux or --platform windows.",
            )),
        }
    }

    fn steamcmd_name(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Windows => "windows",
        }
    }
}

fn options() -> io::Result<Options> {
    let mut options = Options {
        credentials: PathBuf::from(".local/steamcmd.env"),
        directory: PathBuf::new(),
        home: PathBuf::new(),
        cached_only: false,
        install_app: None,
        install_directory: PathBuf::new(),
        platform: Platform::Linux,
    };
    let mut arguments = std::env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--cached-only" {
            options.cached_only = true;
            continue;
        }
        if argument == "--install-app" {
            options.install_app = Some(
                arguments
                    .next()
                    .and_then(|value| value.to_str().and_then(|value| value.parse::<u32>().ok()))
                    .filter(|value| *value != 0)
                    .ok_or_else(|| io::Error::other("Provide a positive --install-app id."))?,
            );
            continue;
        }
        if argument == "--platform" {
            let value = arguments
                .next()
                .and_then(|value| value.into_string().ok())
                .ok_or_else(|| io::Error::other("Use --platform linux or --platform windows."))?;
            options.platform = Platform::parse(&value)?;
            continue;
        }
        let target = match argument.to_str() {
            Some("--credentials") => &mut options.credentials,
            Some("--steamcmd-dir") => &mut options.directory,
            Some("--steam-home") => &mut options.home,
            Some("--install-dir") => &mut options.install_directory,
            _ => return Err(io::Error::other("Unsupported option.")),
        };
        *target = arguments
            .next()
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::other("Missing option value."))?;
    }
    if !options.directory.is_absolute() || !options.home.is_absolute() {
        return Err(io::Error::other(
            "Provide absolute --steamcmd-dir and --steam-home paths.",
        ));
    }
    if options.install_app.is_some() == options.install_directory.as_os_str().is_empty() {
        return Err(io::Error::other(
            "Use --install-app and --install-dir together.",
        ));
    }
    Ok(options)
}

fn run() -> io::Result<()> {
    let options = options()?;
    let credentials = credentials::load(&options.credentials)?;
    #[cfg(target_os = "linux")]
    {
        session::login(&options, &credentials)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (options, credentials);
        Err(io::Error::other("Run this helper on Linux or inside WSL2."))
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("SteamCMD local login stopped: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::Platform;

    #[test]
    fn platform_parser_accepts_only_supported_values() {
        assert_eq!(Platform::parse("linux").unwrap().steamcmd_name(), "linux");
        assert_eq!(
            Platform::parse("windows").unwrap().steamcmd_name(),
            "windows"
        );
        for value in ["", "Linux", "win", "windows;quit", "linux\nquit"] {
            assert!(Platform::parse(value).is_err());
        }
    }
}
