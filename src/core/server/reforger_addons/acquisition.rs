use std::fs::{self, File};
use std::io::{self, Read};
use std::net::{Ipv4Addr, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use foxy_supervisor::leases::InstallationLocks;
use foxy_supervisor::process::CommandSpec;
#[cfg(windows)]
use foxy_supervisor::process::ConsoleSignalHelper;
use foxy_supervisor::session::{ForegroundSession, SessionIdentity};
use serde::Serialize;

use super::super::{
    ContentStageControl, ContentStagePhase, ReforgerServerDefinition, ServerDefinition,
    ServerDefinitionKind, ServerInstallation, ServerInstanceId, ServerStore, ServerStoreError,
};
use super::{ReforgerAddonChannel, ReforgerAddonLock};

const ACQUISITION_TIMEOUT: Duration = Duration::from_secs(240);
const SOFT_STOP_TIMEOUT: Duration = Duration::from_secs(10);
const FORCE_STOP_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CAPTURE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DISCOVERED_ADDONS: usize = 256;

#[derive(Clone, Debug, Serialize)]
pub struct ReforgerAddonAcquisitionPlan {
    #[serde(skip)]
    context: super::super::ServerContext,
    pub instance_id: ServerInstanceId,
    pub expected_instance_revision: u64,
    pub catalog_fingerprint: String,
    pub bindings_fingerprint: Option<String>,
    pub selected_lock_fingerprint: Option<String>,
    pub installation: ServerInstallation,
    pub channel: ReforgerAddonChannel,
    pub scenario_provider: Option<String>,
    pub scenario_id: String,
    pub base_resource_fingerprint: String,
    pub base_resource_index_match: bool,
    pub requested: Vec<super::super::ReforgerAddonRequirement>,
    pub mods_required_by_default: Option<bool>,
    pub fingerprint: String,
    pub blockers: Vec<String>,
    pub writes_performed: bool,
    pub process_started: bool,
    #[serde(skip)]
    settings: ReforgerServerDefinition,
}

struct SessionGuard(ForegroundSession);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if self.0.event().exit.is_none() {
            let _ = self.0.force_stop(FORCE_STOP_TIMEOUT);
        }
    }
}

impl ServerStore {
    pub fn plan_reforger_addon_acquisition(
        &self,
        id: &ServerInstanceId,
        channel: ReforgerAddonChannel,
        scenario_provider: Option<&str>,
    ) -> Result<ReforgerAddonAcquisitionPlan, ServerStoreError> {
        self.validate_record_context()?;
        let scenario_provider = scenario_provider.map(str::to_ascii_lowercase);
        let (catalog, catalog_hash, _) = self.read()?;
        let instance = catalog
            .instances
            .iter()
            .find(|instance| &instance.id == id)
            .ok_or(ServerStoreError::NotFound)?;
        let settings = match &instance.definition {
            ServerDefinition::Reforger(settings) => settings.as_ref().clone(),
            _ => return Err(ServerStoreError::UnsupportedGame),
        };
        let installation_id = instance
            .installation_id
            .as_ref()
            .ok_or(ServerStoreError::InstallationNotFound)?;
        let installation = catalog
            .installations
            .iter()
            .find(|installation| &installation.id == installation_id)
            .cloned()
            .ok_or(ServerStoreError::Corrupt)?;
        self.check_installation_root(&installation)?;
        installation.verify()?;
        let mut blockers = Vec::new();
        let bindings = self.content_bindings(id)?;
        if !bindings.review_current
            || bindings
                .document
                .as_ref()
                .is_none_or(|document| !document.bindings.is_empty())
        {
            blockers.push(
                "Save current empty repository bindings before acquiring the Reforger addon set."
                    .into(),
            );
        }
        if !foxy_supervisor::process::support().available {
            blockers.push(
                "Native Reforger acquisition requires supported ordinary-user process containment."
                    .into(),
            );
        }
        if self.context().game_id() != "reforger"
            || installation.game != ServerDefinitionKind::Reforger
        {
            blockers.push("A Reforger server installation is required.".into());
        }
        if channel != ReforgerAddonChannel::Stable {
            blockers.push("Only the stable Reforger addon channel is supported.".into());
        }
        if super::super::engine_layout::observe_tested_layout(&installation).is_none() {
            blockers.push(
                "The current Reforger executable does not match an accepted engine layout.".into(),
            );
        }
        let scenario_id = settings.scenario_id.clone().unwrap_or_default();
        let scenario_valid = super::super::definition::scenario_resource_valid(&scenario_id);
        if !scenario_valid {
            blockers.push("Select a valid scenario before acquiring its addons.".into());
        }
        let base_resource_bytes = super::super::content_staging::read_bounded_controlled(
            &installation
                .directory
                .join("addons/data/resourceDatabase.rdb"),
            super::resources::MAX_RESOURCE_DATABASE_BYTES,
            &ContentStageControl::default(),
        )?;
        let base_resource_index_match = scenario_valid
            && super::resources::contains_resource(&base_resource_bytes, &scenario_id)
                .map_err(ServerStoreError::Invalid)?;
        let base_resource_fingerprint = blake3::hash(&base_resource_bytes).to_hex().to_string();
        if scenario_provider.as_deref().is_some_and(|provider| {
            provider.len() != 16 || !provider.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            blockers.push("The scenario provider must be a 16-character addon GUID.".into());
        }
        let catalog_fingerprint = catalog_hash
            .map(|hash| hash.to_hex().to_string())
            .unwrap_or_default();
        let selected_lock_fingerprint = self
            .selected_reforger_addon_lock(id)?
            .map(|lock| {
                serde_json::to_vec(&lock)
                    .map(|bytes| blake3::hash(&bytes).to_hex().to_string())
                    .map_err(|_| ServerStoreError::Corrupt)
            })
            .transpose()?;
        let requested = effective_addons(&settings, scenario_provider.as_deref());
        let mut plan = ReforgerAddonAcquisitionPlan {
            context: self.context().clone(),
            instance_id: id.clone(),
            expected_instance_revision: instance.revision,
            catalog_fingerprint,
            bindings_fingerprint: bindings.fingerprint,
            selected_lock_fingerprint,
            installation,
            channel,
            scenario_provider,
            scenario_id,
            base_resource_fingerprint,
            base_resource_index_match,
            requested,
            mods_required_by_default: settings.mods_required_by_default,
            fingerprint: String::new(),
            blockers,
            writes_performed: false,
            process_started: false,
            settings,
        };
        plan.fingerprint = acquisition_fingerprint(&plan)?;
        Ok(plan)
    }

    pub fn acquire_reforger_addons(
        &self,
        plan: ReforgerAddonAcquisitionPlan,
        control: &ContentStageControl,
    ) -> Result<ReforgerAddonLock, ServerStoreError> {
        let _pin = super::super::owner::pin_write(&self.context().database_path())
            .map_err(|_| ServerStoreError::Ownership)?;
        if plan.context != *self.context() || !plan.blockers.is_empty() {
            let message = if plan.blockers.is_empty() {
                "Acquisition plan belongs to another context.".to_owned()
            } else {
                plan.blockers.join(" ")
            };
            return Err(ServerStoreError::Invalid(message));
        }
        if plan.process_started
            || plan.writes_performed
            || acquisition_fingerprint(&plan)? != plan.fingerprint
        {
            return Err(ServerStoreError::StaleRevision);
        }
        control.checkpoint()?;
        let reviewed = self.plan_reforger_addon_acquisition(
            &plan.instance_id,
            plan.channel,
            plan.scenario_provider.as_deref(),
        )?;
        if !reviewed.blockers.is_empty()
            || reviewed.fingerprint != plan.fingerprint
            || plan.requested != effective_addons(&plan.settings, plan.scenario_provider.as_deref())
            || plan.mods_required_by_default != plan.settings.mods_required_by_default
            || reviewed.expected_instance_revision != plan.expected_instance_revision
            || reviewed.catalog_fingerprint != plan.catalog_fingerprint
            || reviewed.selected_lock_fingerprint != plan.selected_lock_fingerprint
            || reviewed.installation != plan.installation
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let result = self.acquire_reforger_addons_inner(&plan, control);
        control.finish(&result);
        result
    }

    fn acquire_reforger_addons_inner(
        &self,
        plan: &ReforgerAddonAcquisitionPlan,
        control: &ContentStageControl,
    ) -> Result<ReforgerAddonLock, ServerStoreError> {
        control.checkpoint()?;
        control.start_phase(ContentStagePhase::Preparing, 0, 0);
        plan.installation.verify()?;
        let source_locks = InstallationLocks::capture(&plan.installation.directory)
            .map_err(|_| ServerStoreError::Ownership)?;
        let _source_runtime = source_locks
            .try_admit()
            .and_then(|admission| admission.try_runtime())
            .map_err(|_| ServerStoreError::Ownership)?;
        plan.installation.verify()?;
        let source_inventory = super::super::runtime_payload::inventory(
            ServerDefinitionKind::Reforger,
            &plan.installation.directory,
            plan.installation.platform,
            control,
        )?;
        if !source_inventory.entries.iter().any(|entry| {
            entry.path == "addons/data/resourceDatabase.rdb"
                && entry.blake3.as_deref() == Some(plan.base_resource_fingerprint.as_str())
        }) {
            return Err(ServerStoreError::StaleRevision);
        }
        let instance_root = self
            .context()
            .space_dir()
            .join("server/instances")
            .join(plan.instance_id.as_str());
        super::super::store::check_directory_chain(&instance_root)?;
        fs::create_dir_all(&instance_root).map_err(|_| ServerStoreError::Io)?;
        super::super::store::check_directory_chain(&instance_root)?;
        let acquisition_root = instance_root.join("reforger-acquisition");
        self.check_deployment_destination(&acquisition_root, None)?;
        super::super::store::check_directory_chain(&acquisition_root)?;
        fs::create_dir_all(&acquisition_root).map_err(|_| ServerStoreError::Io)?;
        super::super::store::check_directory_chain(&acquisition_root)?;
        let available =
            fs4::available_space(&acquisition_root).map_err(|_| ServerStoreError::Io)?;
        let required = source_inventory
            .total_bytes
            .checked_add(crate::core::utils::disk_space::DISK_SPACE_MARGIN_BYTES)
            .ok_or(ServerStoreError::Corrupt)?;
        if available < required {
            return Err(ServerStoreError::Invalid(
                "There is not enough free disk space for the isolated Reforger acquisition copy and its safety margin. Download size is unknown.".into(),
            ));
        }
        let job = tempfile::Builder::new()
            .prefix("job-")
            .tempdir_in(&acquisition_root)
            .map_err(|_| ServerStoreError::Io)?
            .keep();
        private_directory(&job)?;
        let engine = job.join("engine");
        control.start_phase(
            ContentStagePhase::Copying,
            source_inventory.entries.len() as u64,
            source_inventory.total_bytes,
        );
        super::super::runtime_payload::copy(
            ServerDefinitionKind::Reforger,
            &plan.installation.directory,
            &engine,
            &source_inventory,
            control,
        )?;
        let engine_locks =
            InstallationLocks::capture(&engine).map_err(|_| ServerStoreError::Ownership)?;
        let engine_admission = engine_locks
            .try_admit()
            .map_err(|_| ServerStoreError::Ownership)?;
        let copied_before = super::super::runtime_payload::inventory(
            ServerDefinitionKind::Reforger,
            &engine,
            plan.installation.platform,
            control,
        )?;
        if copied_before != source_inventory {
            return Err(ServerStoreError::StaleRevision);
        }
        let run = job.join("run");
        create_private_runtime(&run)?;
        let config_path = run.join("server.json");
        let profiles = run.join("home/profiles");
        let data = run.join("home/data");
        let cache = run.join("home/cache");
        let logs = run.join("home/runtime");
        for directory in [&profiles, &data, &cache, &logs] {
            fs::create_dir_all(directory).map_err(|_| ServerStoreError::Io)?;
            private_directory(directory)?;
        }
        let hostname = format!("FoxyAcquire_{}", random_token());
        let mut bind_socket =
            Some(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|_| ServerStoreError::Io)?);
        let bind_port = bind_socket
            .as_ref()
            .ok_or(ServerStoreError::Io)?
            .local_addr()
            .map_err(|_| ServerStoreError::Io)?
            .port();
        let mut a2s_socket =
            Some(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|_| ServerStoreError::Io)?);
        let a2s_port = a2s_socket
            .as_ref()
            .ok_or(ServerStoreError::Io)?
            .local_addr()
            .map_err(|_| ServerStoreError::Io)?
            .port();
        let settings = ReforgerServerDefinition {
            name: hostname.clone(),
            bind_port,
            bind_address: Some(Ipv4Addr::LOCALHOST),
            a2s_port: Some(a2s_port),
            max_players: 4,
            visible: false,
            scenario_id: Some(plan.scenario_id.clone()),
            addons: plan.requested.clone(),
            cross_platform: None,
            mods_required_by_default: plan.settings.mods_required_by_default,
            game_properties: Default::default(),
            preserved: Default::default(),
            redacted_secret_paths: Vec::new(),
            rcon: None,
        };
        let config = super::super::render_server_json(&settings);
        fs::write(
            &config_path,
            serde_json::to_vec_pretty(&config).map_err(|_| ServerStoreError::Corrupt)?,
        )
        .map_err(|_| ServerStoreError::Io)?;
        let executable = engine.join(&plan.installation.executable);
        let project = engine.join("addons/data/ArmaReforger.gproj");
        let spec = CommandSpec {
            executable: PathBuf::from(argument_path(&executable)?),
            directory: PathBuf::from(argument_path(&engine)?),
            arguments: vec![
                "-config".into(),
                argument_path(&config_path)?.into(),
                "-gproj".into(),
                argument_path(&project)?.into(),
                "-profile".into(),
                argument_path(&profiles)?.into(),
                "-addonDownloadDir".into(),
                argument_path(&data)?.into(),
                "-addonTempDir".into(),
                argument_path_with_separator(&cache)?.into(),
                "-logsDir".into(),
                argument_path(&logs)?.into(),
                "-nothrow".into(),
                "-maxFPS".into(),
                "30".into(),
            ],
            environment: private_environment(&run, plan.installation.platform)?
                .into_iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect(),
        };
        let stdout_path = logs.join("acquisition.stdout.log");
        let stdout = File::create(&stdout_path).map_err(|_| ServerStoreError::Io)?;
        control.checkpoint()?;
        #[cfg(windows)]
        let signal_helper = ConsoleSignalHelper {
            executable: std::env::current_exe().map_err(|_| ServerStoreError::Io)?,
            arguments: vec![
                "server".into(),
                "console-signal".into(),
                "--ctrl-break".into(),
            ],
        };
        #[cfg(windows)]
        let signal_helper = Some(&signal_helper);
        #[cfg(target_os = "linux")]
        let signal_helper = None;
        let identity = SessionIdentity {
            owner_id: "foxy_reforger_addon_acquisition".into(),
            owner_generation: 1,
            game_space_id: self.context().space_id().into(),
            instance_id: plan.instance_id.as_str().into(),
            operation_id: format!("addon_{}", random_token()),
            run_generation: 1,
        };
        let mut session = SessionGuard(
            ForegroundSession::spawn_controlled(
                identity,
                &spec,
                &stdout,
                engine_admission,
                control.cancellation_flag(),
                || {
                    drop(bind_socket.take());
                    drop(a2s_socket.take());
                    plan.installation
                        .verify()
                        .map_err(|_| io::Error::other("Reforger installation changed"))
                },
            )
            .map_err(map_io)?,
        );
        let deadline = Instant::now() + ACQUISITION_TIMEOUT;
        control.start_phase(ContentStagePhase::Checking, 0, 0);
        let mut ready = false;
        let mut exited = false;
        let mut observation_error = None;
        loop {
            if control.checkpoint().is_err() {
                break;
            }
            match session.0.observe() {
                Ok(Some(_)) => {
                    exited = true;
                    break;
                }
                Ok(None) => {}
                Err(error) => {
                    observation_error = Some(map_io(error));
                    break;
                }
            }
            let exact_hostname = a2s_hostname(a2s_port).as_deref() == Some(hostname.as_str());
            let online = match logs_show_online(&logs) {
                Ok(online) => online,
                Err(error) => {
                    observation_error = Some(error);
                    break;
                }
            };
            if exact_hostname && online {
                ready = true;
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let cancelled = control.checkpoint().is_err();
        let _ = session.0.deliver_soft_stop(signal_helper);
        let graceful_waited = match session.0.wait(SOFT_STOP_TIMEOUT).map_err(map_io)? {
            Some(exit) => !exit.forced && exit.root_exit_code == Some(0),
            None => {
                if session
                    .0
                    .force_stop(FORCE_STOP_TIMEOUT)
                    .map_err(map_io)?
                    .is_none()
                {
                    return Err(ServerStoreError::StageCleanupIncomplete);
                }
                false
            }
        };
        let cancelled = cancelled || control.checkpoint().is_err();
        if !graceful_waited {
            return Err(if cancelled {
                ServerStoreError::Cancelled
            } else {
                ServerStoreError::Io
            });
        }
        if cancelled {
            return Err(ServerStoreError::Cancelled);
        }
        if exited || !ready {
            if let Some(error) = observation_error {
                return Err(error);
            }
            return Err(ServerStoreError::Invalid(
                "The isolated Reforger server did not reach online state with its fresh A2S hostname.".into(),
            ));
        }
        let post_inventory = super::super::runtime_payload::inventory(
            ServerDefinitionKind::Reforger,
            &engine,
            plan.installation.platform,
            control,
        )?;
        if post_inventory != copied_before || post_inventory != source_inventory {
            return Err(ServerStoreError::Invalid(
                "The isolated Reforger process modified its copied native payload.".into(),
            ));
        }
        let source_after = super::super::runtime_payload::inventory(
            ServerDefinitionKind::Reforger,
            &plan.installation.directory,
            plan.installation.platform,
            control,
        )?;
        if source_after != source_inventory {
            return Err(ServerStoreError::StaleRevision);
        }
        let addons = discover_addon_roots(&data)?;
        let current_lock_fingerprint = self
            .selected_reforger_addon_lock(&plan.instance_id)?
            .map(|lock| {
                serde_json::to_vec(&lock)
                    .map(|bytes| blake3::hash(&bytes).to_hex().to_string())
                    .map_err(|_| ServerStoreError::Corrupt)
            })
            .transpose()?;
        if current_lock_fingerprint != plan.selected_lock_fingerprint {
            return Err(ServerStoreError::StaleRevision);
        }
        let base_resources = super::super::content_staging::read_bounded_controlled(
            &engine.join("addons/data/resourceDatabase.rdb"),
            super::resources::MAX_RESOURCE_DATABASE_BYTES,
            control,
        )?;
        if blake3::hash(&base_resources).to_hex().as_str() != plan.base_resource_fingerprint {
            return Err(ServerStoreError::StaleRevision);
        }
        drop(session);
        drop(engine_locks);
        let mut copied_entries = vec![super::super::content_staging::StageEntry {
            path: "engine".into(),
            kind: super::super::StageEntryKind::Directory,
            bytes: 0,
            blake3: None,
        }];
        copied_entries.extend(source_inventory.entries.iter().map(|entry| {
            super::super::content_staging::StageEntry {
                path: format!("engine/{}", entry.path),
                kind: entry.kind,
                bytes: entry.bytes,
                blake3: entry.blake3.clone(),
            }
        }));
        super::super::content_staging::verify_subtree(&copied_entries, &job, "engine", control)?;
        let job_path = job.canonicalize().map_err(|_| ServerStoreError::Io)?;
        let engine_path = engine.canonicalize().map_err(|_| ServerStoreError::Io)?;
        if engine_path != job_path.join("engine") {
            return Err(ServerStoreError::UnsafePath);
        }
        fs::remove_dir_all(&engine_path).map_err(|_| ServerStoreError::StageCleanupIncomplete)?;
        self.publish_reforger_addons(
            &plan.instance_id,
            &addons,
            plan.channel,
            plan.scenario_provider.as_deref(),
            &base_resources,
            control,
        )
    }
}

fn effective_addons(
    settings: &ReforgerServerDefinition,
    provider: Option<&str>,
) -> Vec<super::super::ReforgerAddonRequirement> {
    let mut requested = settings.addons.clone();
    if let Some(provider) = provider {
        if let Some(addon) = requested
            .iter_mut()
            .find(|addon| addon.addon_id.eq_ignore_ascii_case(provider))
        {
            addon.required = Some(true);
        } else {
            requested.push(super::super::ReforgerAddonRequirement {
                addon_id: provider.into(),
                version: None,
                required: Some(true),
                preserved: Default::default(),
            });
        }
    }
    for addon in &mut requested {
        addon.addon_id.make_ascii_uppercase();
    }
    requested
}

fn acquisition_fingerprint(
    plan: &ReforgerAddonAcquisitionPlan,
) -> Result<String, ServerStoreError> {
    let mut input = plan.clone();
    input.fingerprint.clear();
    let bytes = serde_json::to_vec(&(
        plan.context.data_root(),
        plan.context.space_id(),
        plan.context.game_id(),
        &input,
        &plan.settings,
    ))
    .map_err(|_| ServerStoreError::Corrupt)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn discover_addon_roots(data: &Path) -> Result<Vec<PathBuf>, ServerStoreError> {
    let addons = data.join("addons");
    match fs::symlink_metadata(&addons) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(ServerStoreError::UnsafePath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(ServerStoreError::Io),
    }
    let mut stack = vec![addons];
    let mut roots = Vec::new();
    let mut entries = 0usize;
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory).map_err(|_| ServerStoreError::Io)? {
            entries += 1;
            if entries > 100_000 {
                return Err(ServerStoreError::Invalid(
                    "Addon download tree exceeds its entry limit.".into(),
                ));
            }
            let entry = entry.map_err(|_| ServerStoreError::Io)?;
            let metadata = fs::symlink_metadata(entry.path()).map_err(|_| ServerStoreError::Io)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() && !metadata.is_file() {
                return Err(ServerStoreError::UnsafePath);
            }
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if entry.file_name() == "ServerData.json" {
                if roots.len() >= MAX_DISCOVERED_ADDONS {
                    return Err(ServerStoreError::Invalid(
                        "Too many addon metadata files were downloaded.".into(),
                    ));
                }
                let root = entry
                    .path()
                    .parent()
                    .ok_or(ServerStoreError::UnsafePath)?
                    .to_path_buf();
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
        }
    }
    roots.sort();
    Ok(roots)
}

fn a2s_hostname(port: u16) -> Option<String> {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).ok()?;
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .ok()?;
    socket.connect((Ipv4Addr::LOCALHOST, port)).ok()?;
    socket
        .set_write_timeout(Some(Duration::from_millis(500)))
        .ok()?;
    let query = b"\xff\xff\xff\xff\x54Source Engine Query\0";
    let mut request = query.to_vec();
    let mut packet = [0u8; 4096];
    for _ in 0..3 {
        socket.send(&request).ok()?;
        let count = socket.recv(&mut packet).ok()?;
        if count == 9 && packet[..5] == *b"\xff\xff\xff\xffA" {
            request.truncate(query.len());
            request.extend_from_slice(&packet[5..9]);
            continue;
        }
        return parse_a2s_hostname(&packet[..count]);
    }
    None
}

fn parse_a2s_hostname(packet: &[u8]) -> Option<String> {
    if packet.len() < 7 || !packet.starts_with(b"\xff\xff\xff\xffI") {
        return None;
    }
    let name = &packet[6..];
    let end = name.iter().position(|byte| *byte == 0)?;
    String::from_utf8(name[..end].to_vec()).ok()
}

fn logs_show_online(root: &Path) -> Result<bool, ServerStoreError> {
    let mut stack = vec![root.to_path_buf()];
    let mut total = 0u64;
    let mut entries = 0usize;
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(directory).map_err(|_| ServerStoreError::Io)? {
            entries += 1;
            if entries > 100_000 {
                return Err(ServerStoreError::Invalid(
                    "Native acquisition logs exceed their entry limit.".into(),
                ));
            }
            let entry = entry.map_err(|_| ServerStoreError::Io)?;
            let metadata = fs::symlink_metadata(entry.path()).map_err(|_| ServerStoreError::Io)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() && !metadata.is_file() {
                return Err(ServerStoreError::UnsafePath);
            }
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if entry.path().extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case("log") || extension.eq_ignore_ascii_case("rpt")
            }) {
                let remaining = MAX_CAPTURE_BYTES.saturating_sub(total);
                let file = File::open(entry.path()).map_err(|_| ServerStoreError::Io)?;
                let mut bytes = Vec::new();
                file.take(remaining.saturating_add(1))
                    .read_to_end(&mut bytes)
                    .map_err(|_| ServerStoreError::Io)?;
                if bytes.len() as u64 > remaining {
                    return Err(ServerStoreError::Invalid(
                        "Native acquisition logs exceed 32 MiB.".into(),
                    ));
                }
                total = total.saturating_add(bytes.len() as u64);
                let text = String::from_utf8_lossy(&bytes);
                if text
                    .to_ascii_lowercase()
                    .contains("entered online game state.")
                {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn create_private_runtime(root: &Path) -> Result<(), ServerStoreError> {
    fs::create_dir(root).map_err(|_| ServerStoreError::Io)?;
    private_directory(root)?;
    for relative in [
        "home",
        "home/profiles",
        "home/data",
        "home/cache",
        "home/runtime",
        "home/config",
    ] {
        let path = root.join(relative);
        fs::create_dir_all(&path).map_err(|_| ServerStoreError::Io)?;
        private_directory(&path)?;
    }
    Ok(())
}

fn private_directory(path: &Path) -> Result<(), ServerStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| ServerStoreError::Io)?;
    }
    #[cfg(windows)]
    let _ = path;
    Ok(())
}

fn private_environment(
    root: &Path,
    platform: super::super::ServerPlatform,
) -> Result<Vec<(String, String)>, ServerStoreError> {
    let mappings: &[(&str, &str)] = match platform {
        super::super::ServerPlatform::LinuxX64 => &[
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "home/config"),
            ("XDG_DATA_HOME", "home/data"),
            ("XDG_CACHE_HOME", "home/cache"),
            ("XDG_RUNTIME_DIR", "home/runtime"),
            ("TMPDIR", "home/temp"),
        ],
        super::super::ServerPlatform::WindowsX64 => &[
            ("HOME", "home"),
            ("USERPROFILE", "home"),
            ("APPDATA", "home/config"),
            ("LOCALAPPDATA", "home/data"),
            ("TEMP", "home/temp"),
            ("TMP", "home/temp"),
        ],
    };
    mappings
        .iter()
        .map(|(key, relative)| {
            let path = root.join(relative);
            fs::create_dir_all(&path).map_err(|_| ServerStoreError::Io)?;
            private_directory(&path)?;
            Ok(((*key).into(), argument_path(&path)?))
        })
        .collect()
}

fn argument_path(path: &Path) -> Result<String, ServerStoreError> {
    super::super::deployment_activation::launch::argument_path(path)
}

fn argument_path_with_separator(path: &Path) -> Result<String, ServerStoreError> {
    let mut value = argument_path(path)?;
    if !value.ends_with(std::path::MAIN_SEPARATOR) {
        value.push(std::path::MAIN_SEPARATOR);
    }
    Ok(value)
}

fn random_token() -> String {
    format!("{:016x}", rand::random::<u64>())
}

fn map_io(error: io::Error) -> ServerStoreError {
    if error.kind() == io::ErrorKind::Interrupted {
        ServerStoreError::Cancelled
    } else {
        ServerStoreError::Io
    }
}

#[cfg(test)]
mod tests {
    use super::{effective_addons, parse_a2s_hostname};

    #[test]
    fn acquisition_matches_the_hostname_instead_of_the_map_or_a_challenge() {
        assert_eq!(
            parse_a2s_hostname(b"\xff\xff\xff\xffI\x11MainRepo\0Arland\0").as_deref(),
            Some("MainRepo")
        );
        assert!(parse_a2s_hostname(b"\xff\xff\xff\xffA\x01\x02\x03\x04").is_none());
        assert!(parse_a2s_hostname(b"\xff\xff\xff\xffI\x11truncated").is_none());
        assert!(parse_a2s_hostname(b"\x00\x00\x00\x00I\x11MainRepo\0").is_none());
    }

    #[test]
    fn engine_requests_use_uppercase_and_require_the_scenario_provider_without_changing_intent() {
        let crate::core::server::ServerDefinition::Reforger(mut settings) =
            crate::core::server::ServerDefinition::draft(
                crate::core::server::ServerDefinitionKind::Reforger,
                "MainRepo",
            )
        else {
            unreachable!()
        };
        settings.addons = vec![crate::core::server::ReforgerAddonRequirement {
            addon_id: "abcdef0123456789".into(),
            version: None,
            required: Some(false),
            preserved: Default::default(),
        }];
        let request = effective_addons(&settings, Some("ABCDEF0123456789"));
        assert_eq!(request.len(), 1);
        assert_eq!(request[0].addon_id, "ABCDEF0123456789");
        assert_eq!(request[0].required, Some(true));
        assert_eq!(settings.addons[0].required, Some(false));
        assert_eq!(settings.addons[0].addon_id, "abcdef0123456789");
        assert_eq!(
            effective_addons(&settings, Some("bbbbbbbbbbbbbbbb")).len(),
            2
        );
    }
}
