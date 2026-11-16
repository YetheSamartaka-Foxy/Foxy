use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use foxy_steam::AuthError;
use foxy_steam::auth::{self, AuthPhase, LoginInput};
use foxy_steam::content::{
    self, ContentError, ContentReview, DepotSelection, DownloadEvent, InstalledDepot, Platform,
    VerifiedFile,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

use super::context::ServerContext;
use super::installation::{
    BuildEvidence, InstallationOwnership, ServerInstallation, ServerPlatform, executable_name,
};
use super::{
    ContentChannel, ContentStageControl, ContentStagePhase, DraftChange, ServerDefinitionKind,
    ServerInstanceId, ServerStore, ServerStoreError,
};

pub(super) const MANIFEST_FILE: &str = "foxy-installation.json";
const STAGING_PREFIX: &str = ".foxy-install-";
const REVIEW_DEADLINE: Duration = Duration::from_secs(5 * 60);
const DOWNLOAD_DEADLINE: Duration = Duration::from_secs(12 * 60 * 60);
const DISK_MARGIN: u64 = 512 * 1024 * 1024;
const MAX_DOWNLOADS: usize = 16;

mod maintenance;
pub use maintenance::{
    NativeInstallationCheck, NativeInstallationValidation, NativeMaintenanceAction,
    NativeMaintenanceReview,
};

#[derive(Clone, Debug)]
pub struct NativeInstallRequest {
    pub instance_id: ServerInstanceId,
    pub directory: PathBuf,
    pub branch: String,
    pub channel: Option<ContentChannel>,
    pub account: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NativeInstallPlan {
    #[serde(skip)]
    context: ServerContext,
    pub instance_id: ServerInstanceId,
    pub previous_installation: Option<String>,
    pub previous_physical_key: Option<String>,
    pub instance_revision: u64,
    pub catalog_fingerprint: Option<String>,
    pub maintenance: Option<NativeMaintenanceReview>,
    pub game: ServerDefinitionKind,
    pub platform: ServerPlatform,
    pub app_id: u32,
    pub channel: ContentChannel,
    pub branch: String,
    pub account: Option<String>,
    pub directory: PathBuf,
    pub review: ContentReview,
    pub required_bytes: u64,
    pub available_bytes: Option<u64>,
    pub interrupted_staging: Vec<PathBuf>,
    pub blockers: Vec<String>,
    pub fingerprint: String,
    pub writes_performed: bool,
    pub database_opened: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeInstallStatus {
    Installed,
    PublishedUnbound,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
pub struct NativeInstallOutcome {
    pub status: NativeInstallStatus,
    pub instance_id: ServerInstanceId,
    pub directory: PathBuf,
    pub build_id: Option<u64>,
    pub depots: Vec<InstalledDepot>,
    pub files: u64,
    pub bytes: u64,
    pub published: bool,
    pub downloads_started: bool,
    pub bound: bool,
    pub installation: Option<ServerInstallation>,
    pub staging_removed: Option<bool>,
    pub auth_error: Option<AuthError>,
    pub content_error: Option<ContentError>,
    pub error: Option<String>,
    pub writes_performed: Option<bool>,
    pub database_opened: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManagedManifest {
    pub schema_version: u32,
    pub game: ServerDefinitionKind,
    pub platform: ServerPlatform,
    pub app_id: u32,
    #[serde(default, skip_serializing_if = "ContentChannel::is_stable")]
    pub channel: ContentChannel,
    pub branch: String,
    pub build_id: Option<u64>,
    pub change_number: Option<u32>,
    pub depots: Vec<ManagedDepot>,
    pub files: u64,
    pub bytes: u64,
    pub inventory_blake3: String,
    pub executable: ManagedExecutable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManagedDepot {
    pub depot_id: u32,
    #[serde(with = "u64_string")]
    pub manifest_gid: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManagedExecutable {
    pub path: String,
    pub bytes: u64,
    pub blake3: String,
}

mod u64_string {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

fn steam_platform(platform: ServerPlatform) -> Platform {
    match platform {
        ServerPlatform::WindowsX64 => Platform::WindowsX64,
        ServerPlatform::LinuxX64 => Platform::LinuxX64,
    }
}

fn dedicated_app(
    game: ServerDefinitionKind,
    channel: ContentChannel,
) -> Result<u32, ServerStoreError> {
    super::distribution::select_dedicated_app(game, channel)
        .map(|selection| selection.server_app_id)
        .map_err(|_| ServerStoreError::Invalid("Arma 3 has no separate experimental dedicated app. Select its Steam branch instead.".into()))
}

pub(super) fn managed_manifest_matches(
    directory: &Path,
    game: ServerDefinitionKind,
    platform: ServerPlatform,
    executable: &str,
    evidence: &super::installation::ExecutableEvidence,
) -> bool {
    let path = directory.join(MANIFEST_FILE);
    if super::store::check_regular_file(&path).is_err() {
        return false;
    }
    let Ok(bytes) = super::store::read_bounded(&path) else {
        return false;
    };
    serde_json::from_slice::<ManagedManifest>(&bytes).is_ok_and(|manifest| {
        manifest.schema_version == 1
            && manifest.game == game
            && manifest.platform == platform
            && dedicated_app(game, manifest.channel).ok() == Some(manifest.app_id)
            && manifest.executable.path == executable
            && manifest.executable.bytes == evidence.bytes
            && manifest.executable.blake3 == evidence.blake3
    })
}

fn inventory_hash(files: &[VerifiedFile]) -> String {
    let mut hasher = blake3::Hasher::new();
    for file in files {
        for part in [
            file.path.as_str(),
            &file.bytes.to_string(),
            &file.sha1,
            &file.blake3,
        ] {
            hasher.update(part.as_bytes());
            hasher.update(&[0]);
        }
    }
    hasher.finalize().to_hex().to_string()
}

fn validate_branch(branch: &str) -> Result<(), ServerStoreError> {
    if branch.is_empty()
        || branch.len() > 64
        || !branch
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(ServerStoreError::Invalid(
            "Invalid Steam branch name".into(),
        ));
    }
    Ok(())
}

pub(super) fn staging_name(directory: &Path) -> Result<String, ServerStoreError> {
    let name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && !name.starts_with('.'))
        .ok_or(ServerStoreError::UnsafePath)?;
    Ok(format!("{STAGING_PREFIX}{name}-"))
}

pub(super) fn interrupted_staging(directory: &Path) -> Result<Vec<PathBuf>, ServerStoreError> {
    let parent = directory.parent().ok_or(ServerStoreError::UnsafePath)?;
    let prefix = staging_name(directory)?;
    let mut found = Vec::new();
    for entry in fs::read_dir(parent).map_err(|_| ServerStoreError::Io)? {
        let entry = entry.map_err(|_| ServerStoreError::Io)?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        if suffix.len() != 16
            || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !entry.file_type().is_ok_and(|kind| kind.is_dir())
        {
            continue;
        }
        let lock = parent.join(format!("{name}.lock"));
        let Ok(file) = File::options().read(true).write(true).open(&lock) else {
            continue;
        };
        if file.try_lock().is_ok() {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found)
}

fn validate_destination(
    store: &ServerStore,
    directory: &Path,
) -> Result<PathBuf, ServerStoreError> {
    if !directory.is_absolute()
        || directory
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(ServerStoreError::Invalid(
            "The installation directory must be an absolute path".into(),
        ));
    }
    let parent = directory.parent().ok_or(ServerStoreError::UnsafePath)?;
    super::store::check_directory_chain(parent)?;
    let parent = parent.canonicalize().map_err(|_| {
        ServerStoreError::Invalid("The installation parent folder must exist".into())
    })?;
    let name = directory.file_name().ok_or(ServerStoreError::UnsafePath)?;
    let directory = parent.join(name);
    require_separate_parent(&parent)?;
    staging_name(&directory)?;
    match fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(ServerStoreError::Invalid(
                "Choose a new installation folder; existing folders are never overwritten".into(),
            ));
        }
        Err(_) => return Err(ServerStoreError::Io),
    }
    let root = store
        .context()
        .data_root()
        .canonicalize()
        .map_err(|_| ServerStoreError::UnsafePath)?;
    if super::installation::paths_overlap(&directory, &root) {
        return Err(ServerStoreError::Invalid(
            "A managed installation must be separate from Foxy's data directory".into(),
        ));
    }
    let registry: crate::core::game::spaces::GamesRegistryFile = serde_json::from_slice(
        &super::store::read_bounded(&store.context().data_root().join("games.json"))
            .map_err(|_| ServerStoreError::Corrupt)?,
    )
    .map_err(|_| ServerStoreError::Corrupt)?;
    for space in registry.game_spaces {
        let context = ServerContext::capture(store.context().data_root(), Some(&space.id))
            .map_err(ServerStoreError::Invalid)?;
        let other = match ServerStore::new(context) {
            Ok(other) => other,
            Err(ServerStoreError::UnsupportedGame) => continue,
            Err(error) => return Err(error),
        };
        if other.load()?.installations.iter().any(|installation| {
            super::installation::paths_overlap(&installation.directory, &directory)
        }) {
            return Err(ServerStoreError::Invalid("The installation folder overlaps a recorded installation in this configuration root".into()));
        }
    }
    Ok(directory)
}

fn require_separate_parent(parent: &Path) -> Result<(), ServerStoreError> {
    for ancestor in parent.ancestors() {
        let mut markers = vec![MANIFEST_FILE];
        for game in [ServerDefinitionKind::Arma3, ServerDefinitionKind::Reforger] {
            for platform in [ServerPlatform::WindowsX64, ServerPlatform::LinuxX64] {
                markers.push(executable_name(game, platform));
            }
        }
        for marker in markers {
            match fs::symlink_metadata(ancestor.join(marker)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err(ServerStoreError::Invalid("The installation parent must be outside every existing dedicated installation, including installations in other configuration roots".into())),
            }
        }
    }
    Ok(())
}

fn plan_fingerprint(plan: &NativeInstallPlan) -> String {
    let value = serde_json::json!({
        "root": plan.context.data_root(),
        "space": plan.context.space_id(),
        "game_id": plan.context.game_id(),
        "game": plan.game,
        "instance": plan.instance_id,
        "previous": plan.previous_installation,
        "previous_physical_key": plan.previous_physical_key,
        "instance_revision": plan.instance_revision,
        "catalog_fingerprint": plan.catalog_fingerprint,
        "maintenance": plan.maintenance,
        "platform": plan.platform,
        "app": plan.app_id,
        "channel": plan.channel,
        "branch": plan.branch,
        "account": plan.account,
        "directory": plan.directory,
        "selection": plan.review.selection,
        "entitled": plan.review.entitled,
        "blockers": plan.blockers,
    });
    blake3::hash(value.to_string().as_bytes())
        .to_hex()
        .to_string()
}

pub(super) struct Session {
    pub(super) runtime: tokio::runtime::Runtime,
    cancel_tx: watch::Sender<bool>,
    stop: Arc<AtomicBool>,
}

impl Session {
    pub(super) fn new(control: &ContentStageControl) -> Result<Self, ServerStoreError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| ServerStoreError::Io)?;
        let (cancel_tx, _) = watch::channel(false);
        let stop = Arc::new(AtomicBool::new(false));
        let bridge = cancel_tx.clone();
        let watcher = control.clone();
        let finished = stop.clone();
        runtime.spawn(async move {
            while !finished.load(Ordering::Acquire) {
                if watcher.checkpoint().is_err() {
                    bridge.send_replace(true);
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
        Ok(Self {
            runtime,
            cancel_tx,
            stop,
        })
    }

    pub(super) fn cancel_rx(&self) -> watch::Receiver<bool> {
        self.cancel_tx.subscribe()
    }

    /// Stops background download tasks before the caller touches the staging folder.
    pub(super) fn close(self) {
        self.stop.store(true, Ordering::Release);
        self.runtime.shutdown_timeout(Duration::from_secs(30));
    }
}

pub(super) async fn logon(
    input: LoginInput,
    cancel: watch::Receiver<bool>,
) -> Result<auth::AuthenticatedSession, AuthError> {
    let prepared = matches!(input, LoginInput::Prepared(_));
    let (phases, _) = watch::channel(AuthPhase::Connecting);
    let (_codes_tx, codes) = mpsc::channel(1);
    auth::authenticate(input, phases, codes, cancel)
        .await
        .map_err(|error| {
            if prepared && error == AuthError::RejectedCredentials {
                AuthError::AuthenticationRequired
            } else {
                error
            }
        })
}

impl ServerStore {
    pub fn plan_native_install(
        &self,
        request: NativeInstallRequest,
        control: &ContentStageControl,
    ) -> Result<NativeInstallPlan, ServerStoreError> {
        if !self.capabilities().native_installation {
            return Err(ServerStoreError::UnsupportedGame);
        }
        validate_branch(&request.branch)?;
        if request
            .account
            .as_deref()
            .is_some_and(|alias| !super::steam_auth::valid_account_alias(alias))
        {
            return Err(ServerStoreError::Invalid(
                "Invalid Steam session alias".into(),
            ));
        }
        let instance = self.show(&request.instance_id)?;
        let catalog_fingerprint = self.read()?.1.map(|hash| hash.to_hex().to_string());
        let previous_physical_key = instance
            .installation_id
            .as_ref()
            .map(|id| {
                let installation = self
                    .load()?
                    .installations
                    .into_iter()
                    .find(|installation| &installation.id == id)
                    .ok_or(ServerStoreError::InstallationNotFound)?;
                foxy_supervisor::leases::InstallationLocks::capture(&installation.directory)
                    .and_then(|locks| locks.coordination_key())
                    .map_err(|_| maintenance::blocked())
            })
            .transpose()?;
        let game = instance.definition.kind();
        let channel = request.channel.unwrap_or_default();
        let app_id = dedicated_app(game, channel)?;
        let platform = ServerPlatform::current()?;
        let directory = validate_destination(self, &request.directory)?;
        let interrupted = interrupted_staging(&directory)?;
        let input = super::steam_auth::unattended_input(self.context(), request.account.as_deref())
            .map_err(|error| ServerStoreError::Invalid(error.to_string()))?;

        control.start_phase(ContentStagePhase::Preparing, 0, 0);
        let session = Session::new(control)?;
        let cancel = session.cancel_rx();
        let branch = request.branch.clone();
        let result = session.runtime.block_on(async {
            let logged_in = logon(input, cancel.clone())
                .await
                .map_err(|error| error.to_string())?;
            let review = content::cancellable(
                cancel,
                REVIEW_DEADLINE,
                content::review(
                    &logged_in.client,
                    app_id,
                    &branch,
                    steam_platform(platform),
                    true,
                ),
            )
            .await
            .map_err(|error| error.to_string());
            drop(logged_in);
            review
        });
        session.close();
        control.checkpoint()?;
        let review = result.map_err(ServerStoreError::Invalid)?;

        let required_bytes = review
            .selection
            .depots
            .iter()
            .map(|depot| depot.size.unwrap_or(0))
            .sum::<u64>();
        let available_bytes = directory
            .parent()
            .and_then(|parent| fs4::available_space(parent).ok());
        let mut blockers = Vec::new();
        if !review.entitled {
            blockers.push(ContentError::EntitlementRequired.to_string());
        }
        if review
            .selection
            .depots
            .iter()
            .any(|depot| depot.size.is_none())
        {
            blockers
                .push("Steam did not report every depot size; disk space cannot be checked".into());
        } else if available_bytes
            .is_some_and(|available| available < required_bytes.saturating_add(DISK_MARGIN))
        {
            blockers.push("Not enough free disk space for the reviewed build".into());
        }
        let mut plan = NativeInstallPlan {
            context: self.context().clone(),
            instance_id: instance.id.clone(),
            previous_installation: instance.installation_id.map(String::from),
            previous_physical_key,
            instance_revision: instance.revision,
            catalog_fingerprint,
            maintenance: None,
            game,
            platform,
            app_id,
            channel,
            branch: request.branch,
            account: request.account,
            directory,
            review,
            required_bytes,
            available_bytes,
            interrupted_staging: interrupted,
            blockers,
            fingerprint: String::new(),
            writes_performed: false,
            database_opened: false,
        };
        plan.fingerprint = plan_fingerprint(&plan);
        control.finish(&Ok(()));
        Ok(plan)
    }

    pub fn apply_native_install(
        &self,
        plan: &NativeInstallPlan,
        control: &ContentStageControl,
    ) -> Result<NativeInstallOutcome, ServerStoreError> {
        if &plan.context != self.context() {
            return Err(ServerStoreError::StaleRevision);
        }
        if dedicated_app(plan.game, plan.channel)? != plan.app_id
            || plan.review.selection.app_id != plan.app_id
            || plan.review.selection.branch != plan.branch
            || plan.review.selection.platform != steam_platform(plan.platform)
        {
            return Err(ServerStoreError::StaleRevision);
        }
        if !plan.blockers.is_empty() {
            return Err(ServerStoreError::Invalid(plan.blockers.join(" ")));
        }
        let instance = self.show(&plan.instance_id)?;
        if instance.definition.kind() != plan.game
            || instance.installation_id.clone().map(String::from) != plan.previous_installation
            || instance.revision != plan.instance_revision
            || self.read()?.1.map(|hash| hash.to_hex().to_string()) != plan.catalog_fingerprint
            || plan.fingerprint != plan_fingerprint(plan)
        {
            return Err(ServerStoreError::StaleRevision);
        }
        let previous = self.previous_installation(plan)?;
        let locks = previous
            .as_ref()
            .map(|installation| {
                foxy_supervisor::leases::InstallationLocks::capture(&installation.directory)
                    .map_err(|_| maintenance::blocked())
            })
            .transpose()?;
        let _maintenance = locks
            .as_ref()
            .map(|locks| {
                let admission = locks.try_admit().map_err(|_| maintenance::blocked())?;
                admission
                    .try_maintenance()
                    .map_err(|(_, _)| maintenance::blocked())
            })
            .transpose()?;
        self.revalidate_maintenance(plan, locks.as_ref())?;
        if validate_destination(self, &plan.directory)? != plan.directory {
            return Err(ServerStoreError::StaleRevision);
        }
        let input = super::steam_auth::unattended_input(self.context(), plan.account.as_deref())
            .map_err(|error| ServerStoreError::Invalid(error.to_string()))?;
        let executable = executable_name(plan.game, plan.platform).to_owned();
        let mut outcome = run_install(
            InstallSpec {
                app_id: plan.app_id,
                channel: plan.channel,
                branch: &plan.branch,
                platform: plan.platform,
                game: plan.game,
                directory: &plan.directory,
                expected: Some(&plan.review.selection),
                executable: Some(&executable),
                interrupted: &plan.interrupted_staging,
            },
            input,
            control,
        );
        outcome.instance_id = plan.instance_id.clone();
        if !outcome.published {
            control.finish(&match outcome.status {
                NativeInstallStatus::Cancelled => Err(ServerStoreError::Cancelled),
                _ => Err::<(), _>(ServerStoreError::Io),
            });
            return Ok(outcome);
        }

        control.start_phase(ContentStagePhase::Checking, 1, 0);
        let recorded = control
            .checkpoint()
            .and_then(|()| self.revalidate_maintenance(plan, locks.as_ref()))
            .and_then(|()| self.record_managed(plan, &plan.instance_id));
        match recorded {
            Ok(installation) => {
                outcome.status = NativeInstallStatus::Installed;
                outcome.bound = true;
                outcome.installation = Some(installation);
            }
            Err(error) => {
                outcome.status = NativeInstallStatus::PublishedUnbound;
                outcome.error = Some(error.to_string());
            }
        }
        control.finish(&if outcome.bound {
            Ok(())
        } else {
            Err(ServerStoreError::Io)
        });
        Ok(outcome)
    }

    fn record_managed(
        &self,
        plan: &NativeInstallPlan,
        instance_id: &ServerInstanceId,
    ) -> Result<ServerInstallation, ServerStoreError> {
        let installation = ServerInstallation::inspect(&plan.directory, plan.game)?;
        if installation.ownership != InstallationOwnership::Managed
            || installation.build_evidence != BuildEvidence::NativeDepotManifest
        {
            return Err(ServerStoreError::Corrupt);
        }
        let current = self.show(instance_id)?;
        if current.installation_id.clone().map(String::from) != plan.previous_installation
            || current.revision != plan.instance_revision
            || self.read()?.1.map(|hash| hash.to_hex().to_string()) != plan.catalog_fingerprint
        {
            return Err(ServerStoreError::Invalid(
                "The instance binding changed during installation; adopt the published folder to bind it".into(),
            ));
        }
        let draft = self.plan(DraftChange::Adopt {
            id: instance_id.clone(),
            expected_revision: current.revision,
            installation,
        })?;
        let recorded = draft
            .instance
            .installation_id
            .clone()
            .ok_or(ServerStoreError::Corrupt)?;
        self.apply(draft)?;
        self.load()?
            .installations
            .into_iter()
            .find(|installation| installation.id == recorded)
            .ok_or(ServerStoreError::Corrupt)
    }
}

pub(super) struct InstallSpec<'a> {
    pub app_id: u32,
    pub channel: ContentChannel,
    pub branch: &'a str,
    pub platform: ServerPlatform,
    pub game: ServerDefinitionKind,
    pub directory: &'a Path,
    pub expected: Option<&'a DepotSelection>,
    pub executable: Option<&'a str>,
    pub interrupted: &'a [PathBuf],
}

fn empty_outcome(spec: &InstallSpec<'_>) -> NativeInstallOutcome {
    NativeInstallOutcome {
        status: NativeInstallStatus::Failed,
        instance_id: ServerInstanceId::default(),
        directory: spec.directory.to_path_buf(),
        build_id: None,
        depots: Vec::new(),
        files: 0,
        bytes: 0,
        published: false,
        downloads_started: false,
        bound: false,
        installation: None,
        staging_removed: None,
        auth_error: None,
        content_error: None,
        error: None,
        writes_performed: Some(false),
        database_opened: false,
    }
}

pub(super) fn remove_staging(staging: &Path, lock_path: &Path, lock: File) -> bool {
    let removed = match fs::remove_dir_all(staging) {
        Ok(()) => true,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    };
    if removed {
        drop(lock);
        let _ = fs::remove_file(lock_path);
    }
    removed
}

pub(super) fn run_install(
    spec: InstallSpec<'_>,
    input: LoginInput,
    control: &ContentStageControl,
) -> NativeInstallOutcome {
    let mut outcome = empty_outcome(&spec);
    let fail = |outcome: &mut NativeInstallOutcome, message: &str| {
        outcome.status = NativeInstallStatus::Failed;
        outcome.error = Some(message.to_owned());
    };
    let Some(parent) = spec.directory.parent() else {
        fail(&mut outcome, "Invalid installation folder");
        return outcome;
    };
    let Ok(prefix) = staging_name(spec.directory) else {
        fail(&mut outcome, "Invalid installation folder");
        return outcome;
    };
    if require_separate_parent(parent).is_err() {
        fail(
            &mut outcome,
            "The installation parent overlaps another dedicated installation",
        );
        return outcome;
    }
    for interrupted in spec.interrupted {
        let lock_path = lock_file(interrupted);
        let name_matches = interrupted
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(&prefix));
        let lock = File::options().read(true).write(true).open(&lock_path);
        if let (true, Ok(lock)) = (name_matches && interrupted.parent() == Some(parent), lock)
            && lock.try_lock().is_ok()
        {
            outcome.writes_performed = Some(true);
            remove_staging(interrupted, &lock_path, lock);
        }
    }

    control.start_phase(ContentStagePhase::Preparing, 0, 0);
    let session = match Session::new(control) {
        Ok(session) => session,
        Err(_) => {
            fail(&mut outcome, "Cannot start the native Steam worker");
            return outcome;
        }
    };
    let staging = parent.join(format!("{prefix}{:016x}", rand::random::<u64>()));
    let lock_path = lock_file(&staging);
    let platform = steam_platform(spec.platform);
    let cancel = session.cancel_rx();
    let failure = |outcome: &mut NativeInstallOutcome,
                   auth_error: Option<AuthError>,
                   content_error: Option<ContentError>| {
        outcome.auth_error = auth_error;
        outcome.content_error = content_error;
        outcome.status = if content_error == Some(ContentError::Cancelled)
            || auth_error == Some(AuthError::Cancelled)
            || control.checkpoint().is_err()
        {
            NativeInstallStatus::Cancelled
        } else {
            NativeInstallStatus::Failed
        };
    };

    let reviewed = session.runtime.block_on(async {
        let logged_in = logon(input, cancel.clone())
            .await
            .map_err(|error| (Some(error), None))?;
        let review = content::cancellable(
            cancel.clone(),
            REVIEW_DEADLINE,
            content::review(&logged_in.client, spec.app_id, spec.branch, platform, true),
        )
        .await
        .map_err(|error| (None, Some(error)))?;
        if spec
            .expected
            .is_some_and(|expected| *expected != review.selection)
        {
            return Err((None, Some(ContentError::ManifestMismatch)));
        }
        if !review.entitled {
            return Err((None, Some(ContentError::EntitlementRequired)));
        }
        Ok((logged_in, review))
    });
    let (logged_in, review) = match reviewed {
        Ok(reviewed) => reviewed,
        Err((auth_error, content_error)) => {
            session.close();
            failure(&mut outcome, auth_error, content_error);
            return outcome;
        }
    };

    if require_separate_parent(parent).is_err() {
        session.runtime.block_on(async move { drop(logged_in) });
        session.close();
        fail(
            &mut outcome,
            "The installation parent now overlaps another dedicated installation",
        );
        return outcome;
    }
    let lock = match create_staging(&staging, &lock_path) {
        Ok(lock) => lock,
        Err(()) => {
            session.runtime.block_on(async move { drop(logged_in) });
            session.close();
            fail(
                &mut outcome,
                "Cannot create the locked installation staging folder",
            );
            return outcome;
        }
    };
    outcome.writes_performed = Some(true);
    let total: u64 = review
        .selection
        .depots
        .iter()
        .map(|depot| depot.size.unwrap_or(0))
        .sum();
    control.start_phase(ContentStagePhase::Copying, 0, total);
    outcome.downloads_started = true;
    let reporter = control.clone();
    let downloaded = session.runtime.block_on(async {
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();
        let forward = tokio::spawn(async move {
            while let Some(event) = events_rx.recv().await {
                match event {
                    DownloadEvent::ChunkCompleted { bytes } => reporter.record_bytes(bytes),
                    DownloadEvent::FileCompleted { .. } | DownloadEvent::FileSkipped { .. } => {
                        reporter.record_file()
                    }
                    _ => {}
                }
            }
        });
        let heartbeat = content::spawn_heartbeat(&logged_in.client);
        let report = content::cancellable(
            cancel,
            DOWNLOAD_DEADLINE,
            content::install(
                &logged_in.client,
                &review,
                &staging,
                events_tx,
                MAX_DOWNLOADS,
            ),
        )
        .await;
        heartbeat.abort();
        drop(logged_in);
        let _ = forward.await;
        report
    });
    session.close();
    let abort = |outcome: &mut NativeInstallOutcome, lock: File| {
        outcome.staging_removed = Some(remove_staging(&staging, &lock_path, lock));
    };
    let report = match downloaded {
        Ok(report) => report,
        Err(error) => {
            failure(&mut outcome, None, Some(error));
            abort(&mut outcome, lock);
            return outcome;
        }
    };
    outcome.build_id = review.selection.build_id;
    outcome.depots = report.depots.clone();

    let files = report
        .entries
        .values()
        .filter(|entry| !entry.directory)
        .count() as u64;
    let bytes = report
        .entries
        .values()
        .filter(|entry| !entry.directory)
        .map(|entry| entry.size)
        .sum();
    control.start_phase(ContentStagePhase::Verifying, files, bytes);
    let cancelled = || control.checkpoint().is_err();
    let verified = match content::verify_tree(&staging, &report.entries, &cancelled) {
        Ok(verified) => verified,
        Err(error) => {
            failure(&mut outcome, None, Some(error));
            abort(&mut outcome, lock);
            return outcome;
        }
    };
    outcome.files = verified.len() as u64;
    outcome.bytes = verified.iter().map(|file| file.bytes).sum();
    let executable = match spec.executable {
        Some(name) => match verified.iter().find(|file| file.path == name) {
            Some(_) if mark_executable(&staging.join(name)).is_err() => {
                fail(
                    &mut outcome,
                    "Cannot mark the dedicated server executable as runnable",
                );
                abort(&mut outcome, lock);
                return outcome;
            }
            Some(file) => ManagedExecutable {
                path: file.path.clone(),
                bytes: file.bytes,
                blake3: file.blake3.clone(),
            },
            None => {
                fail(
                    &mut outcome,
                    "The reviewed build does not contain the dedicated server executable",
                );
                abort(&mut outcome, lock);
                return outcome;
            }
        },
        None => ManagedExecutable {
            path: String::new(),
            bytes: 0,
            blake3: String::new(),
        },
    };
    let manifest = ManagedManifest {
        schema_version: 1,
        game: spec.game,
        platform: spec.platform,
        app_id: spec.app_id,
        channel: spec.channel,
        branch: spec.branch.to_owned(),
        build_id: review.selection.build_id,
        change_number: review.change_number,
        depots: report
            .depots
            .iter()
            .map(|depot| ManagedDepot {
                depot_id: depot.depot_id,
                manifest_gid: depot.manifest_gid,
            })
            .collect(),
        files: outcome.files,
        bytes: outcome.bytes,
        inventory_blake3: inventory_hash(&verified),
        executable,
    };
    let written = serde_json::to_vec_pretty(&manifest)
        .map_err(|_| ())
        .and_then(|bytes| {
            let mut file = File::create_new(staging.join(MANIFEST_FILE)).map_err(|_| ())?;
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| ())
        });
    if written.is_err() {
        fail(
            &mut outcome,
            "Cannot write the managed installation manifest",
        );
        abort(&mut outcome, lock);
        return outcome;
    }
    if control.checkpoint().is_err() {
        outcome.status = NativeInstallStatus::Cancelled;
        abort(&mut outcome, lock);
        return outcome;
    }
    if require_separate_parent(parent).is_err() || fs::symlink_metadata(spec.directory).is_ok() {
        fail(
            &mut outcome,
            "The installation folder appeared during download; nothing was replaced",
        );
        abort(&mut outcome, lock);
        return outcome;
    }
    if publish_no_replace(&staging, spec.directory).is_err() {
        fail(
            &mut outcome,
            "Publishing the verified installation failed; nothing was replaced",
        );
        abort(&mut outcome, lock);
        return outcome;
    }
    #[cfg(unix)]
    let _ = File::open(parent).and_then(|directory| directory.sync_all());
    outcome.published = true;
    outcome.staging_removed = Some(true);
    drop(lock);
    let _ = fs::remove_file(&lock_path);
    outcome.status = NativeInstallStatus::PublishedUnbound;
    outcome
}

/// Use no-replace publication: ordinary Linux rename could overwrite a raced-in empty directory.
pub(super) fn publish_no_replace(staging: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let from = CString::new(staging.as_os_str().as_bytes())?;
        let to = CString::new(destination.as_os_str().as_bytes())?;
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS)) {
            return Err(error);
        }
    }
    if fs::symlink_metadata(destination).is_ok() {
        return Err(std::io::ErrorKind::AlreadyExists.into());
    }
    fs::rename(staging, destination)
}

/// Some depot manifests omit execute bits; grant the required executable's existing read bits as execute bits only.
fn mark_executable(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        let mode = metadata.permissions().mode();
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(mode | ((mode & 0o444) >> 2) | 0o100),
        )?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub(super) fn lock_file(staging: &Path) -> PathBuf {
    let mut name = staging.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// The lock file exists before the folder so a concurrent review never treats live staging
/// as interrupted.
pub(super) fn create_staging(staging: &Path, lock_path: &Path) -> Result<File, ()> {
    let lock = File::create_new(lock_path).map_err(|_| ())?;
    if lock.try_lock().is_err() || fs::create_dir(staging).is_err() {
        drop(lock);
        let _ = fs::remove_file(lock_path);
        return Err(());
    }
    Ok(lock)
}

#[cfg(test)]
mod tests;
