use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use foxy_supervisor::leases::InstallationLocks;
use foxy_supervisor::process::CommandSpec;
use foxy_supervisor::session::{ForegroundSession, SessionIdentity};

#[cfg(target_os = "linux")]
pub(super) mod console;
mod key_destinations;
#[cfg(windows)]
pub(super) mod keys;
mod layout;
mod mission;
mod payload;
mod proof;
mod query_keys;
mod report;
mod runtime;
use payload::{copy_payload, ordinary, verify_inventory};
use report::{Observation, parse_report, query_info};

pub(super) fn observe() -> io::Result<()> {
    layout::observe()
}

fn input_directory(name: &str) -> io::Result<PathBuf> {
    let path =
        PathBuf::from(std::env::var_os(name).ok_or_else(|| io::Error::other(name.to_owned()))?);
    if !path.is_absolute() {
        return Err(io::Error::other("Native input must be absolute"));
    }
    ordinary(&path, true)?;
    let path = path.canonicalize()?;
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        if let Some(Component::Prefix(prefix)) = path.components().next()
            && let Prefix::VerbatimDisk(drive) = prefix.kind()
        {
            let mut plain = PathBuf::from(format!("{}:", char::from(drive)));
            for component in path.components().skip(1) {
                plain.push(component.as_os_str());
            }
            return Ok(plain);
        }
        Err(io::Error::other(
            "Native engine probe requires a local disk",
        ))
    }
    #[cfg(not(windows))]
    Ok(path)
}

struct Instance {
    name: &'static str,
    payload: PathBuf,
    report_directories: Vec<PathBuf>,
    environment: Vec<(OsString, OsString)>,
    arguments: Vec<OsString>,
    port: u16,
    reservations: Vec<UdpSocket>,
    hostname: String,
    fixture: mission::Mission,
    generation: u64,
    log_offsets: BTreeMap<PathBuf, u64>,
    #[cfg(target_os = "linux")]
    log_directory: PathBuf,
}

impl Instance {
    fn prepare_with_keys(
        root: &Path,
        payload: &Path,
        name: &'static str,
        shared: bool,
        public_keys: Option<&Path>,
    ) -> io::Result<Self> {
        Self::prepare_inner(root, payload, name, shared, public_keys, None)
    }

    /// Regular clients cannot join loopback=1 servers; admission fixtures listen normally with a random password and no UPnP.
    #[cfg(windows)]
    fn prepare_client_joinable(
        root: &Path,
        payload: &Path,
        name: &'static str,
        password: &str,
    ) -> io::Result<Self> {
        Self::prepare_inner(root, payload, name, true, None, Some(password))
    }

    fn prepare_inner(
        root: &Path,
        payload: &Path,
        name: &'static str,
        shared: bool,
        public_keys: Option<&Path>,
        client_password: Option<&str>,
    ) -> io::Result<Self> {
        let instance = root.join("instances").join(name);
        fs::create_dir_all(&instance)?;
        let profiles = instance.join("profiles");
        fs::create_dir(&profiles)?;
        let (environment, report_directories) = runtime::prepare_home(&instance, &profiles)?;
        let log_directory = instance.join("logs");
        fs::create_dir(&log_directory)?;
        let mut report_directories = report_directories;
        report_directories.push(log_directory.clone());
        let configuration = instance.join("server.cfg");
        let hostname = format!(
            "FoxyNative{}_{name}",
            root.file_name().unwrap().to_string_lossy()
        );
        fs::write(
            &configuration,
            format!(
                "hostname = \"{hostname}\";\nmaxPlayers = 1;\npersistent = 1;\nloopback = {};\nupnp = 0;\nBattlEye = 0;\nverifySignatures = 2;\nallowedFilePatching = 0;\n",
                u8::from(public_keys.is_none() && client_password.is_none())
            ),
        )?;
        if let Some(password) = client_password {
            if password.is_empty() || !password.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
                return Err(io::Error::other(
                    "Client fixture password must be alphanumeric",
                ));
            }
            let mut configuration_file =
                fs::OpenOptions::new().append(true).open(&configuration)?;
            writeln!(
                configuration_file,
                "password = \"{password}\";\nskipLobby = 1;"
            )?;
        }
        let basic = instance.join("basic.cfg");
        fs::write(&basic, b"MaxCustomFileSize = 0;\n")?;
        let reservations = reserve_ports()?;
        let port = reservations[0].local_addr()?.port();
        let mut configuration_file = fs::OpenOptions::new().append(true).open(&configuration)?;
        writeln!(configuration_file, "steamQueryPort = {};", port + 1)?;
        let mut arguments = Vec::new();
        if client_password.is_none() {
            arguments.push(OsString::from("-ip=127.0.0.1"));
        }
        arguments.extend([
            OsString::from(format!("-port={port}")),
            OsString::from(format!("-config={}", configuration.display())),
            OsString::from(format!("-cfg={}", basic.display())),
            OsString::from(format!("-profiles={}", profiles.display())),
            OsString::from(format!("-name=FoxyNative{name}")),
            OsString::from("-world=empty"),
            OsString::from("-noSound"),
            OsString::from("-noSplash"),
            OsString::from("-noPause"),
            OsString::from("-autoInit"),
        ]);
        let missions;
        let content;
        if shared {
            let relative = Path::new("private-instances")
                .join(root.file_name().unwrap())
                .join(name);
            runtime::prepare_shared_directories(payload, root.file_name().unwrap(), name)?;
            missions = payload.join(&relative).join("missions");
            content = payload.join(&relative).join("content");
            arguments.push(OsString::from(format!(
                "-serverMod={}",
                relative.join("content").display()
            )));
            arguments.push(OsString::from(format!(
                "-mpmissions={}",
                relative.join("missions").display()
            )));
            arguments.push(OsString::from(format!(
                "-keysFolder={}",
                relative.join("keys").display()
            )));
        } else {
            missions = payload.join(if cfg!(windows) {
                "MPMissions"
            } else {
                "mpmissions"
            });
            fs::create_dir(&missions)?;
            content = payload.join("foxy-native-content");
            arguments.push(OsString::from("-serverMod=foxy-native-content"));
        }
        if let Some(public_keys) = public_keys {
            key_destinations::install(
                public_keys,
                payload,
                root.file_name().unwrap(),
                name,
                shared,
            )?;
        }
        let fixture = mission::write(&missions, &content, name, &hostname)?;
        writeln!(
            configuration_file,
            "class Missions {{ class NativeProof {{ template = \"{}\"; difficulty = \"Regular\"; }}; }};",
            fixture.template
        )?;
        Ok(Self {
            name,
            payload: payload.to_path_buf(),
            report_directories,
            environment,
            arguments,
            port,
            reservations,
            hostname,
            fixture,
            generation: 0,
            log_offsets: BTreeMap::new(),
            #[cfg(target_os = "linux")]
            log_directory,
        })
    }

    fn start(&mut self, locks: &InstallationLocks) -> io::Result<ForegroundSession> {
        self.log_offsets.clear();
        for directory in &self.report_directories {
            for entry in fs::read_dir(directory)? {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    self.log_offsets
                        .insert(entry.path(), entry.metadata()?.len());
                }
            }
        }
        self.generation += 1;
        #[cfg(windows)]
        let executable = "arma3server_x64.exe";
        #[cfg(target_os = "linux")]
        let executable = "arma3server_x64";
        let identity = SessionIdentity {
            owner_id: "native_layout_probe".into(),
            owner_generation: 1,
            game_space_id: "native_probe".into(),
            instance_id: self.name.into(),
            operation_id: "bootstrap_observation".into(),
            run_generation: self.generation,
        };
        self.reservations.clear();
        let spec = CommandSpec {
            executable: self.payload.join(executable),
            directory: self.payload.clone(),
            arguments: self.arguments.clone(),
            environment: self.environment.clone(),
        };
        #[cfg(target_os = "linux")]
        let spec = {
            let request = console::Request {
                executable: spec.executable,
                directory: spec.directory.clone(),
                arguments: spec
                    .arguments
                    .iter()
                    .map(|argument| {
                        argument
                            .to_str()
                            .map(str::to_owned)
                            .ok_or_else(|| io::Error::other("Non-UTF8 native fixture argument"))
                    })
                    .collect::<io::Result<_>>()?,
                log: self
                    .log_directory
                    .join(format!("console-{}.log", self.generation)),
            };
            let path = self
                .log_directory
                .join(format!("request-{}.json", self.generation));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            file.write_all(&serde_json::to_vec(&request)?)?;
            file.sync_all()?;
            let mut environment = spec.environment;
            environment.push((
                OsString::from("FOXY_ARMA_CONSOLE_REQUEST"),
                path.into_os_string(),
            ));
            CommandSpec {
                executable: std::env::current_exe()?,
                directory: spec.directory,
                arguments: vec![
                    OsString::from("native_engine_console_fixture"),
                    OsString::from("--exact"),
                    OsString::from("--ignored"),
                    OsString::from("--test-threads=1"),
                ],
                environment,
            }
        };
        ForegroundSession::spawn(
            identity,
            &spec,
            locks.try_admit()?,
            &AtomicBool::new(false),
            || Ok(()),
        )
    }

    fn report_text(&self) -> io::Result<String> {
        use std::io::{Seek, SeekFrom};
        let mut text = String::new();
        let mut count = 0;
        for directory in &self.report_directories {
            for entry in fs::read_dir(directory)? {
                count += 1;
                if count > 4096 {
                    return Err(io::Error::other("Native report inventory exceeds bounds"));
                }
                let entry = entry?;
                if !entry.path().extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("rpt") || extension.eq_ignore_ascii_case("log")
                }) {
                    continue;
                }
                ordinary(&entry.path(), false)?;
                let mut file = fs::File::open(entry.path())?;
                let size = file.metadata()?.len();
                let offset = self
                    .log_offsets
                    .get(&entry.path())
                    .copied()
                    .filter(|offset| *offset <= size)
                    .unwrap_or(0);
                file.seek(SeekFrom::Start(offset))?;
                file.take(16 * 1024 * 1024 + 1).read_to_string(&mut text)?;
                if text.len() > 16 * 1024 * 1024 {
                    return Err(io::Error::other("Native report exceeds bounds"));
                }
                text.push('\n');
            }
        }
        Ok(text)
    }

    fn proof(&self) -> io::Result<proof::Proof> {
        Ok(proof::parse(
            &self.report_text()?,
            &self.fixture.identity,
            &self.hostname,
        ))
    }

    fn observation(&self) -> io::Result<Observation> {
        let text = self.report_text()?;
        let mut observation = parse_report(&text, self.port);
        let proof = proof::parse(&text, &self.fixture.identity, &self.hostname);
        observation.ready = !observation.version.is_empty()
            && proof.mission_seen
            && proof.content_isolated
            && proof.save_requested;
        Ok(observation)
    }

    fn query_ready(&self) -> bool {
        query_info(self.port + 1, &self.hostname)
    }
}

fn reserve_ports() -> io::Result<Vec<UdpSocket>> {
    for _ in 0..32 {
        let first = UdpSocket::bind("127.0.0.1:0")?;
        let port = first.local_addr()?.port();
        if port > u16::MAX - 4 {
            continue;
        }
        let mut sockets = vec![first];
        for offset in 1..=4 {
            if let Ok(socket) = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, port + offset)) {
                sockets.push(socket);
            } else {
                break;
            }
        }
        if sockets.len() == 5 {
            return Ok(sockets);
        }
    }
    Err(io::Error::other(
        "No private native probe port range available",
    ))
}
