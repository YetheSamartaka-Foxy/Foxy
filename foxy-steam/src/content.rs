use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::path::{Component, Path};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use steamroom::apps::AccessToken;
use steamroom::cdn::{CdnClient, CdnServer, CdnServerPool};
use steamroom::client::{LoggedIn, SteamClient};
use steamroom::depot::manifest::DepotManifest;
use steamroom::depot::{AppId, CellId, DepotId, DepotKey, ManifestId};
use steamroom::enums::DepotFileFlags;
use steamroom::types::KeyValue;
use steamroom_client::download::{CdnChunkFetcher, DepotJob};
pub use steamroom_client::event::DownloadEvent;
use tokio::sync::{mpsc, watch};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_CDN_SERVERS: u32 = 20;
const MAX_MANIFEST_FILES: usize = 500_000;
const HEARTBEAT: Duration = Duration::from_secs(9);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    WindowsX64,
    LinuxX64,
}

impl Platform {
    fn os(self) -> &'static str {
        match self {
            Self::WindowsX64 => "windows",
            Self::LinuxX64 => "linux",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentError {
    InvalidAppInfo,
    BranchUnavailable,
    PasswordBranch,
    PlatformUnavailable,
    NoDepots,
    EntitlementRequired,
    ManifestUnavailable,
    ManifestMismatch,
    UnsafeManifestPath,
    UnsupportedLink,
    UnsupportedSharedDepot,
    Network,
    Download,
    VerificationFailed,
    Storage,
    TimedOut,
    Cancelled,
}

impl fmt::Display for ContentError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(match self {
            Self::InvalidAppInfo => "Steam returned app information Foxy cannot read",
            Self::BranchUnavailable => "The requested Steam branch does not exist for this app",
            Self::PasswordBranch => "The requested Steam branch requires a password",
            Self::PlatformUnavailable => "The app has no depots for the requested platform",
            Self::NoDepots => "The app has no installable depots for this branch",
            Self::EntitlementRequired => {
                "This Steam session is not entitled to the app depots; use a prepared session for an account that owns the app"
            }
            Self::ManifestUnavailable => "Steam did not provide the reviewed depot manifest",
            Self::ManifestMismatch => "The downloaded manifest does not match the review",
            Self::UnsafeManifestPath => "A depot manifest contains an unsafe file path",
            Self::UnsupportedLink => "A depot manifest contains a link Foxy does not install",
            Self::UnsupportedSharedDepot => "The app requires a shared payload depot Foxy cannot resolve",
            Self::Network => "The Steam content request failed; retry when the connection is available",
            Self::Download => "Depot download or verification failed",
            Self::VerificationFailed => {
                "Installed files do not match the reviewed depot manifests"
            }
            Self::Storage => "The staging directory could not be read or written",
            Self::TimedOut => "The Steam content request timed out",
            Self::Cancelled => "The Steam content operation was cancelled",
        })
    }
}

impl std::error::Error for ContentError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    OtherPlatform,
    OtherArchitecture,
    Language,
    LowViolence,
    SharedInstall,
    Dlc,
    NoManifestForBranch,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SelectedDepot {
    pub depot_id: u32,
    pub name: Option<String>,
    pub manifest_gid: u64,
    pub size: Option<u64>,
    pub download: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SkippedDepot {
    pub depot_id: u32,
    pub reason: SkipReason,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DepotSelection {
    pub app_id: u32,
    pub branch: String,
    pub build_id: Option<u64>,
    pub platform: Platform,
    pub depots: Vec<SelectedDepot>,
    pub skipped: Vec<SkippedDepot>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ContentReview {
    pub selection: DepotSelection,
    pub change_number: Option<u32>,
    pub entitled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExpectedEntry {
    pub depot_id: u32,
    pub size: u64,
    pub sha1: Option<String>,
    pub directory: bool,
    pub executable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InstalledDepot {
    pub depot_id: u32,
    pub manifest_gid: u64,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InstallReport {
    pub depots: Vec<InstalledDepot>,
    pub entries: BTreeMap<String, ExpectedEntry>,
}

fn text<'a>(value: &'a KeyValue, key: &str) -> Option<&'a str> {
    value.get(key).and_then(KeyValue::as_str)
}

fn number(value: &KeyValue, key: &str) -> Option<u64> {
    let entry = value.get(key)?;
    entry
        .as_u64()
        .or_else(|| entry.as_i32().and_then(|value| u64::try_from(value).ok()))
        .or_else(|| entry.as_str().and_then(|value| value.trim().parse().ok()))
}

fn children(value: &KeyValue) -> impl Iterator<Item = (&String, &KeyValue)> {
    match &value.value {
        steamroom::types::KvValue::Children(map) => Some(map.iter()),
        _ => None,
    }
    .into_iter()
    .flatten()
}

fn manifest_gid(depot: &KeyValue, branch: &str) -> Option<(u64, Option<u64>, Option<u64>)> {
    let entry = depot.get("manifests")?.get(branch)?;
    if let Some(gid) = entry.as_str().and_then(|value| value.trim().parse().ok()) {
        return Some((gid, None, None));
    }
    Some((
        number(entry, "gid")?,
        number(entry, "size"),
        number(entry, "download"),
    ))
}

/// Chooses the depots one platform needs for a branch, in ascending depot order.
pub fn select_depots(
    app: &KeyValue,
    app_id: u32,
    branch: &str,
    platform: Platform,
) -> Result<DepotSelection, ContentError> {
    if branch.is_empty()
        || branch.len() > 64
        || !branch
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(ContentError::BranchUnavailable);
    }
    let depots = app.get("depots").ok_or(ContentError::InvalidAppInfo)?;
    let branch_info = depots
        .get("branches")
        .and_then(|branches| branches.get(branch))
        .ok_or(ContentError::BranchUnavailable)?;
    if number(branch_info, "pwdrequired").is_some_and(|value| value != 0) {
        return Err(ContentError::PasswordBranch);
    }
    let build_id = number(branch_info, "buildid");

    let mut ordered = Vec::new();
    for (key, depot) in children(depots) {
        if let Ok(depot_id) = key.parse::<u32>() {
            ordered.push((depot_id, depot));
        }
    }
    ordered.sort_by_key(|(depot_id, _)| *depot_id);

    let mut selected = Vec::new();
    let mut skipped = Vec::new();
    let mut password_only = false;
    for (depot_id, depot) in ordered {
        let config = depot.get("config");
        let setting = |key: &str| config.and_then(|config| text(config, key));
        let reason = if setting("oslist").is_some_and(|list| {
            !list.trim().is_empty()
                && !list
                    .split(',')
                    .any(|os| os.trim().eq_ignore_ascii_case(platform.os()))
        }) {
            Some(SkipReason::OtherPlatform)
        } else if setting("osarch")
            .is_some_and(|arch| !arch.trim().is_empty() && arch.trim() != "64")
        {
            Some(SkipReason::OtherArchitecture)
        } else if setting("language").is_some_and(|language| {
            !language.trim().is_empty() && !language.trim().eq_ignore_ascii_case("english")
        }) {
            Some(SkipReason::Language)
        } else if setting("lowviolence").is_some_and(|value| value.trim() == "1") {
            Some(SkipReason::LowViolence)
        } else if text(depot, "sharedinstall").is_some_and(|value| value.trim() == "1")
            || depot.get("depotfromapp").is_some()
        {
            if number(depot, "depotfromapp") != Some(228_980)
                || text(depot, "sharedinstall").is_none_or(|value| value.trim() != "1")
            {
                return Err(ContentError::UnsupportedSharedDepot);
            }
            Some(SkipReason::SharedInstall)
        } else if depot.get("dlcappid").is_some() {
            Some(SkipReason::Dlc)
        } else {
            None
        };
        if let Some(reason) = reason {
            skipped.push(SkippedDepot { depot_id, reason });
            continue;
        }
        match manifest_gid(depot, branch) {
            Some((gid, size, download)) if gid != 0 => selected.push(SelectedDepot {
                depot_id,
                name: text(depot, "name").map(str::to_owned),
                manifest_gid: gid,
                size,
                download,
            }),
            _ => {
                password_only |= depot
                    .get("encryptedmanifests")
                    .and_then(|manifests| manifests.get(branch))
                    .is_some();
                skipped.push(SkippedDepot {
                    depot_id,
                    reason: SkipReason::NoManifestForBranch,
                });
            }
        }
    }
    if selected.is_empty() {
        return Err(if password_only {
            ContentError::PasswordBranch
        } else if skipped.iter().any(|depot| {
            matches!(
                depot.reason,
                SkipReason::OtherPlatform | SkipReason::OtherArchitecture
            )
        }) {
            ContentError::PlatformUnavailable
        } else {
            ContentError::NoDepots
        });
    }
    Ok(DepotSelection {
        app_id,
        branch: branch.to_owned(),
        build_id,
        platform,
        depots: selected,
        skipped,
    })
}

/// Parses PICS app data, which Steam sends as NUL-terminated text KeyValues.
pub fn parse_app_info(data: &[u8]) -> Result<KeyValue, ContentError> {
    let trimmed = data.strip_suffix(&[0]).unwrap_or(data);
    match std::str::from_utf8(trimmed) {
        Ok(text) if text.trim_start().starts_with('"') => {
            KeyValue::from_text(text).map_err(|_| ContentError::InvalidAppInfo)
        }
        _ => KeyValue::from_binary(data).map_err(|_| ContentError::InvalidAppInfo),
    }
}

/// Runs one content step, stopping at cancellation or the deadline.
pub async fn cancellable<T>(
    mut cancel: watch::Receiver<bool>,
    deadline: Duration,
    operation: impl Future<Output = Result<T, ContentError>>,
) -> Result<T, ContentError> {
    if *cancel.borrow() {
        return Err(ContentError::Cancelled);
    }
    tokio::pin!(operation);
    let timer = tokio::time::sleep(deadline);
    tokio::pin!(timer);
    loop {
        tokio::select! {
            biased;
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    return Err(ContentError::Cancelled);
                }
            }
            _ = &mut timer => return Err(ContentError::TimedOut),
            result = &mut operation => return result,
        }
    }
}

async fn request<T, E>(operation: impl Future<Output = Result<T, E>>) -> Result<T, ContentError> {
    tokio::time::timeout(REQUEST_TIMEOUT, operation)
        .await
        .map_err(|_| ContentError::TimedOut)?
        .map_err(|_| ContentError::Network)
}

/// A denied key arrives as an access-denied or failed-EResult response; anything else is a
/// transport failure.
fn key_failure(error: steamroom::Error) -> ContentError {
    match error {
        steamroom::Error::Connection(
            steamroom::error::ConnectionError::DepotAccessDenied(_)
            | steamroom::error::ConnectionError::ServiceMethodFailed(_),
        ) => ContentError::EntitlementRequired,
        _ => ContentError::Network,
    }
}

async fn depot_key(
    client: &SteamClient<LoggedIn>,
    app_id: u32,
    depot_id: u32,
) -> Result<DepotKey, ContentError> {
    tokio::time::timeout(
        REQUEST_TIMEOUT,
        client.get_depot_decryption_key(DepotId(depot_id), AppId(app_id)),
    )
    .await
    .map_err(|_| ContentError::TimedOut)?
    .map_err(key_failure)
}

/// Whether the session may decrypt `depot_id`; transport failures are errors, not denials.
pub async fn depot_entitled(
    client: &SteamClient<LoggedIn>,
    app_id: u32,
    depot_id: u32,
) -> Result<bool, ContentError> {
    match depot_key(client, app_id, depot_id).await {
        Ok(_) => Ok(true),
        Err(ContentError::EntitlementRequired) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Keeps the logged-in CM session alive during long CDN transfers. Steam drops an idle
/// session, which would fail the next depot-key or manifest request mid-batch.
pub fn spawn_heartbeat(client: &SteamClient<LoggedIn>) -> tokio::task::JoinHandle<()> {
    let client = client.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(HEARTBEAT).await;
            if client.send_heartbeat().await.is_err() {
                return;
            }
        }
    })
}

/// Read-only review: app info, depot choice and, when requested, depot key entitlement.
pub async fn review(
    client: &SteamClient<LoggedIn>,
    app_id: u32,
    branch: &str,
    platform: Platform,
    check_entitlement: bool,
) -> Result<ContentReview, ContentError> {
    let tokens = request(client.pics_get_access_tokens(&[AppId(app_id)])).await?;
    let token = tokens
        .into_iter()
        .find(|token| token.app_id.0 == app_id)
        .unwrap_or(AccessToken {
            app_id: AppId(app_id),
            token: 0,
        });
    let info = request(client.pics_get_product_info(&[token]))
        .await?
        .into_iter()
        .find(|info| info.app_id.map(|id| id.0) == Some(app_id))
        .ok_or(ContentError::InvalidAppInfo)?;
    let data = info.kv_data.ok_or(ContentError::InvalidAppInfo)?;
    let app = parse_app_info(&data)?;
    let selection = select_depots(&app, app_id, branch, platform)?;
    let mut entitled = check_entitlement;
    if check_entitlement {
        for depot in &selection.depots {
            if !depot_entitled(client, app_id, depot.depot_id).await? {
                entitled = false;
                break;
            }
        }
    }
    Ok(ContentReview {
        selection,
        change_number: info.change_number,
        entitled,
    })
}

fn safe_relative(path: &str) -> bool {
    if path.is_empty() || path.contains('\0') || path.contains(':') {
        return false;
    }
    let path = Path::new(path);
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Validates every manifest entry before any byte is written.
pub fn manifest_entries(
    manifest: &DepotManifest,
    depot_id: u32,
) -> Result<BTreeMap<String, ExpectedEntry>, ContentError> {
    if manifest.files.len() > MAX_MANIFEST_FILES {
        return Err(ContentError::ManifestUnavailable);
    }
    let mut entries = BTreeMap::new();
    for file in &manifest.files {
        let path = file.normalized_path();
        let path = path.trim_end_matches('/');
        if !safe_relative(path) {
            return Err(ContentError::UnsafeManifestPath);
        }
        let flags = DepotFileFlags::from_bits_truncate(file.flags);
        if flags.is_symlink() || file.link_target.is_some() {
            return Err(ContentError::UnsupportedLink);
        }
        entries.insert(
            path.to_owned(),
            ExpectedEntry {
                depot_id,
                size: file.size,
                sha1: file
                    .sha_content
                    .map(|hash| hash.iter().map(|byte| format!("{byte:02x}")).collect()),
                directory: flags.is_directory(),
                executable: flags.is_executable(),
            },
        );
    }
    Ok(entries)
}

/// Content servers for one content operation; empty lists are a network failure.
pub async fn cdn_servers(client: &SteamClient<LoggedIn>) -> Result<Vec<CdnServer>, ContentError> {
    let servers = request(client.get_cdn_servers(CellId(0), Some(MAX_CDN_SERVERS))).await?;
    if servers.is_empty() {
        return Err(ContentError::Network);
    }
    Ok(servers)
}

pub struct DepotRequest<'a> {
    pub app_id: u32,
    pub depot_id: u32,
    pub manifest_gid: u64,
    pub branch: Option<&'a str>,
}

/// Key and validated manifest for one depot, fetched before any chunk transfer starts.
pub struct PreparedDepot {
    depot_id: u32,
    manifest_gid: u64,
    key: DepotKey,
    manifest: DepotManifest,
    pub entries: BTreeMap<String, ExpectedEntry>,
}

pub async fn prepare_depot(
    client: &SteamClient<LoggedIn>,
    servers: &[CdnServer],
    depot: DepotRequest<'_>,
) -> Result<PreparedDepot, ContentError> {
    let app = AppId(depot.app_id);
    let depot_id = DepotId(depot.depot_id);
    let manifest_id = ManifestId(depot.manifest_gid);
    let key = depot_key(client, depot.app_id, depot.depot_id).await?;
    let code =
        request(client.get_manifest_request_code(app, depot_id, manifest_id, depot.branch, None))
            .await?
            .ok_or(ContentError::ManifestUnavailable)?;
    let cdn = CdnClient::new().map_err(|_| ContentError::Network)?;
    let pool = CdnServerPool::new(servers.to_vec());
    let raw = tokio::time::timeout(
        REQUEST_TIMEOUT * 4,
        cdn.download_manifest_pooled(&pool, depot_id, manifest_id, code, None),
    )
    .await
    .map_err(|_| ContentError::TimedOut)?
    .map_err(|_| ContentError::ManifestUnavailable)?;
    let mut manifest = steamroom_client::manifest::parse_cdn_manifest(&raw)
        .map_err(|_| ContentError::ManifestUnavailable)?;
    if manifest.filenames_encrypted {
        manifest
            .decrypt_filenames(&key)
            .map_err(|_| ContentError::ManifestUnavailable)?;
    }
    if manifest
        .manifest_id
        .is_some_and(|id| id.0 != depot.manifest_gid)
        || manifest.depot_id.is_some_and(|id| id.0 != depot.depot_id)
    {
        return Err(ContentError::ManifestMismatch);
    }
    let entries = manifest_entries(&manifest, depot.depot_id)?;
    Ok(PreparedDepot {
        depot_id: depot.depot_id,
        manifest_gid: depot.manifest_gid,
        key,
        manifest,
        entries,
    })
}

/// Transfers one prepared depot from the CDN into `install_dir`; returns downloaded bytes.
pub async fn fetch_depot(
    servers: &[CdnServer],
    prepared: &PreparedDepot,
    install_dir: &Path,
    events: mpsc::UnboundedSender<DownloadEvent>,
    max_downloads: usize,
) -> Result<u64, ContentError> {
    let fetcher = CdnChunkFetcher::new(
        CdnClient::new().map_err(|_| ContentError::Network)?,
        CdnServerPool::new(servers.to_vec()),
        None,
    );
    let job = DepotJob::builder()
        .depot_id(DepotId(prepared.depot_id))
        .depot_key(prepared.key.clone())
        .install_dir(install_dir.to_path_buf())
        .max_downloads(max_downloads.max(1))
        .verify(true)
        .event_sender(events)
        .build()
        .map_err(|_| ContentError::Download)?;
    let stats = job
        .download(&prepared.manifest, Arc::new(fetcher))
        .await
        .map_err(|_| ContentError::Download)?;
    remove_job_scratch(install_dir, &prepared.entries)?;
    Ok(stats.bytes_downloaded)
}

/// Prefetch every key/manifest so CM requests never wait behind long CDN transfers.
/// Later depots override earlier paths, matching Steam's install order.
pub async fn install(
    client: &SteamClient<LoggedIn>,
    review: &ContentReview,
    staging: &Path,
    events: mpsc::UnboundedSender<DownloadEvent>,
    max_downloads: usize,
) -> Result<InstallReport, ContentError> {
    let selection = &review.selection;
    let servers = cdn_servers(client).await?;
    let mut prepared = Vec::with_capacity(selection.depots.len());
    for depot in &selection.depots {
        prepared.push(
            prepare_depot(
                client,
                &servers,
                DepotRequest {
                    app_id: selection.app_id,
                    depot_id: depot.depot_id,
                    manifest_gid: depot.manifest_gid,
                    branch: Some(&selection.branch),
                },
            )
            .await?,
        );
    }
    let mut report = InstallReport {
        depots: Vec::new(),
        entries: BTreeMap::new(),
    };
    for depot in prepared {
        let bytes = fetch_depot(&servers, &depot, staging, events.clone(), max_downloads).await?;
        report.depots.push(InstalledDepot {
            depot_id: depot.depot_id,
            manifest_gid: depot.manifest_gid,
            files: depot
                .entries
                .values()
                .filter(|entry| !entry.directory)
                .count() as u64,
            bytes,
        });
        report.entries.extend(depot.entries);
    }
    remove_job_scratch(staging, &report.entries)?;
    Ok(report)
}

/// The depot job keeps empty work folders under `.DepotDownloader`; anything else there is
/// left for verification to reject.
fn remove_job_scratch(
    staging: &Path,
    entries: &BTreeMap<String, ExpectedEntry>,
) -> Result<(), ContentError> {
    const SCRATCH: &str = ".DepotDownloader";
    if entries
        .keys()
        .any(|path| path == SCRATCH || path.starts_with(".DepotDownloader/"))
    {
        return Ok(());
    }
    let scratch = staging.join(SCRATCH);
    match std::fs::symlink_metadata(&scratch) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(ContentError::Storage),
    }
    let work = scratch.join("staging");
    if std::fs::symlink_metadata(&work).is_ok_and(|metadata| metadata.is_dir()) {
        let _ = std::fs::remove_dir(&work);
    }
    let _ = std::fs::remove_dir(&scratch);
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VerifiedFile {
    pub path: String,
    pub bytes: u64,
    pub sha1: String,
    pub blake3: String,
    pub executable: bool,
}

/// Requires the exact unlinked file inventory, sizes and manifest SHA-1 values.
/// BLAKE3 is recorded in the same pass for Foxy's inventory.
pub fn verify_tree(
    root: &Path,
    entries: &BTreeMap<String, ExpectedEntry>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<VerifiedFile>, ContentError> {
    verify_tree_with_exclusion(root, entries, None, cancelled)
}

fn verify_tree_with_exclusion(
    root: &Path,
    entries: &BTreeMap<String, ExpectedEntry>,
    excluded_root_file: Option<&str>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<VerifiedFile>, ContentError> {
    use sha1::Digest;
    use std::io::Read;

    let mut found = BTreeMap::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((directory, prefix)) = stack.pop() {
        if cancelled() {
            return Err(ContentError::Cancelled);
        }
        for entry in std::fs::read_dir(&directory).map_err(|_| ContentError::Storage)? {
            let entry = entry.map_err(|_| ContentError::Storage)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| ContentError::VerificationFailed)?;
            if prefix.is_empty() && excluded_root_file == Some(name.as_str()) {
                continue;
            }
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry.file_type().map_err(|_| ContentError::Storage)?;
            if kind.is_symlink() || is_reparse_point(&entry.path())? {
                return Err(ContentError::VerificationFailed);
            }
            if kind.is_dir() {
                stack.push((entry.path(), relative.clone()));
            }
            found.insert(relative, kind.is_dir());
        }
    }

    let mut verified = Vec::new();
    for (path, entry) in entries {
        let is_dir = found.remove(path).ok_or(ContentError::VerificationFailed)?;
        if is_dir != entry.directory {
            return Err(ContentError::VerificationFailed);
        }
        let mut parent = Path::new(path).parent();
        while let Some(ancestor) = parent.filter(|ancestor| !ancestor.as_os_str().is_empty()) {
            found.remove(&ancestor.to_string_lossy().replace('\\', "/"));
            parent = ancestor.parent();
        }
        if entry.directory {
            continue;
        }
        let mut file = std::fs::File::open(root.join(path)).map_err(|_| ContentError::Storage)?;
        let mut sha1 = sha1::Sha1::new();
        let mut blake3 = blake3::Hasher::new();
        let mut buffer = vec![0u8; 1 << 20];
        let mut bytes = 0u64;
        loop {
            if cancelled() {
                return Err(ContentError::Cancelled);
            }
            let read = file.read(&mut buffer).map_err(|_| ContentError::Storage)?;
            if read == 0 {
                break;
            }
            sha1.update(&buffer[..read]);
            blake3.update(&buffer[..read]);
            bytes += read as u64;
        }
        let sha1: String = sha1
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if bytes != entry.size
            || entry
                .sha1
                .as_ref()
                .is_some_and(|expected| *expected != sha1)
        {
            return Err(ContentError::VerificationFailed);
        }
        verified.push(VerifiedFile {
            path: path.clone(),
            bytes,
            sha1,
            blake3: blake3.finalize().to_hex().to_string(),
            executable: entry.executable,
        });
    }
    if !found.is_empty() {
        return Err(ContentError::VerificationFailed);
    }
    Ok(verified)
}

fn is_reparse_point(path: &Path) -> Result<bool, ContentError> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let attributes = std::fs::symlink_metadata(path)
            .map_err(|_| ContentError::Storage)?
            .file_attributes();
        Ok(attributes & 0x400 != 0)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(false)
    }
}

/// Inventories every ordinary file in a staged tree, excluding one root-level receipt file.
pub fn inventory_tree(
    root: &Path,
    excluded_root_file: &str,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<VerifiedFile>, ContentError> {
    if !safe_relative(excluded_root_file) || Path::new(excluded_root_file).components().count() != 1
    {
        return Err(ContentError::UnsafeManifestPath);
    }
    let mut entries = BTreeMap::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((directory, prefix)) = stack.pop() {
        if cancelled() {
            return Err(ContentError::Cancelled);
        }
        for entry in std::fs::read_dir(&directory).map_err(|_| ContentError::Storage)? {
            if cancelled() {
                return Err(ContentError::Cancelled);
            }
            let entry = entry.map_err(|_| ContentError::Storage)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| ContentError::VerificationFailed)?;
            if !safe_relative(&name) {
                return Err(ContentError::VerificationFailed);
            }
            if prefix.is_empty() && name == excluded_root_file {
                let kind = entry.file_type().map_err(|_| ContentError::Storage)?;
                if kind.is_symlink() || is_reparse_point(&entry.path())? || !kind.is_file() {
                    return Err(ContentError::VerificationFailed);
                }
                continue;
            }
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry.file_type().map_err(|_| ContentError::Storage)?;
            if kind.is_symlink()
                || is_reparse_point(&entry.path())?
                || (!kind.is_dir() && !kind.is_file())
            {
                return Err(ContentError::VerificationFailed);
            }
            if kind.is_dir() {
                stack.push((entry.path(), relative.clone()));
                entries.insert(
                    relative,
                    ExpectedEntry {
                        depot_id: 0,
                        size: 0,
                        sha1: None,
                        directory: true,
                        executable: false,
                    },
                );
            } else {
                let metadata = entry.metadata().map_err(|_| ContentError::Storage)?;
                entries.insert(
                    relative,
                    ExpectedEntry {
                        depot_id: 0,
                        size: metadata.len(),
                        sha1: None,
                        directory: false,
                        executable: false,
                    },
                );
            }
        }
    }
    verify_tree_with_exclusion(root, &entries, Some(excluded_root_file), cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &str = r#""233780"
{
    "common" { "name" "Synthetic Dedicated Server" }
    "depots"
    {
        "100"
        {
            "name" "Shared redistributables"
            "depotfromapp" "228980"
            "sharedinstall" "1"
        }
        "101"
        {
            "name" "Windows server"
            "config" { "oslist" "windows" }
            "manifests" { "public" { "gid" "1111" "size" "500" "download" "200" } }
        }
        "102"
        {
            "name" "Linux server"
            "config" { "oslist" "linux" }
            "manifests" { "public" "2222" }
        }
        "103"
        {
            "name" "Common content"
            "manifests" { "public" { "gid" "3333" "size" "900" } "creator" { "gid" "3334" } }
        }
        "104"
        {
            "config" { "oslist" "windows" "osarch" "32" }
            "manifests" { "public" { "gid" "4444" } }
        }
        "105"
        {
            "config" { "language" "german" }
            "manifests" { "public" { "gid" "5555" } }
        }
        "106"
        {
            "dlcappid" "999"
            "manifests" { "public" { "gid" "6666" } }
        }
        "107"
        {
            "encryptedmanifests" { "secret" { "gid" "x" } }
        }
        "branches"
        {
            "public" { "buildid" "17000000" }
            "creator" { "buildid" "17000001" }
            "secret" { "buildid" "17000002" "pwdrequired" "1" }
        }
    }
}
"#;

    fn app() -> KeyValue {
        let mut bytes = APP.as_bytes().to_vec();
        bytes.push(0);
        parse_app_info(&bytes).unwrap()
    }

    #[test]
    fn platform_selection_is_ordered_and_explains_every_skip() {
        let windows = select_depots(&app(), 233780, "public", Platform::WindowsX64).unwrap();
        assert_eq!(windows.build_id, Some(17_000_000));
        assert_eq!(
            windows
                .depots
                .iter()
                .map(|depot| (depot.depot_id, depot.manifest_gid))
                .collect::<Vec<_>>(),
            vec![(101, 1111), (103, 3333)]
        );
        assert_eq!(windows.depots[0].size, Some(500));
        assert_eq!(windows.depots[0].download, Some(200));
        let reasons: BTreeMap<u32, SkipReason> = windows
            .skipped
            .iter()
            .map(|depot| (depot.depot_id, depot.reason))
            .collect();
        assert_eq!(reasons[&100], SkipReason::SharedInstall);
        assert_eq!(reasons[&102], SkipReason::OtherPlatform);
        assert_eq!(reasons[&104], SkipReason::OtherArchitecture);
        assert_eq!(reasons[&105], SkipReason::Language);
        assert_eq!(reasons[&106], SkipReason::Dlc);
        assert_eq!(reasons[&107], SkipReason::NoManifestForBranch);

        let linux = select_depots(&app(), 233780, "public", Platform::LinuxX64).unwrap();
        assert_eq!(
            linux
                .depots
                .iter()
                .map(|depot| (depot.depot_id, depot.manifest_gid))
                .collect::<Vec<_>>(),
            vec![(102, 2222), (103, 3333)]
        );
    }

    #[test]
    fn required_shared_payloads_are_not_silently_skipped_as_common_redistributables() {
        for text in [
            APP.replace("\"depotfromapp\" \"228980\"", "\"depotfromapp\" \"123456\""),
            APP.replace("\"sharedinstall\" \"1\"", "\"sharedinstall\" \"0\""),
            APP.replace("\"depotfromapp\" \"228980\"", ""),
        ] {
            let metadata = parse_app_info(text.as_bytes()).unwrap();
            assert_eq!(
                select_depots(&metadata, 233780, "public", Platform::WindowsX64),
                Err(ContentError::UnsupportedSharedDepot)
            );
        }
        let other_platform = APP.replace(
            "\"depotfromapp\" \"228980\"",
            "\"config\" { \"oslist\" \"linux\" } \"depotfromapp\" \"123456\"",
        );
        let metadata = parse_app_info(other_platform.as_bytes()).unwrap();
        assert!(select_depots(&metadata, 233780, "public", Platform::WindowsX64).is_ok());
        assert_eq!(
            select_depots(&metadata, 233780, "public", Platform::LinuxX64),
            Err(ContentError::UnsupportedSharedDepot)
        );
    }

    #[test]
    fn branch_rules_refuse_missing_password_and_unsafe_names() {
        let creator = select_depots(&app(), 233780, "creator", Platform::LinuxX64).unwrap();
        assert_eq!(creator.depots.len(), 1);
        assert_eq!(creator.depots[0].manifest_gid, 3334);
        assert_eq!(
            select_depots(&app(), 233780, "missing", Platform::WindowsX64),
            Err(ContentError::BranchUnavailable)
        );
        assert_eq!(
            select_depots(&app(), 233780, "secret", Platform::WindowsX64),
            Err(ContentError::PasswordBranch)
        );
        assert_eq!(
            select_depots(&app(), 233780, "../public", Platform::WindowsX64),
            Err(ContentError::BranchUnavailable)
        );
    }

    #[test]
    fn platform_only_apps_report_platform_unavailable() {
        let only_windows = KeyValue::from_text(
            r#""1" { "depots" { "10" { "config" { "oslist" "windows" } "manifests" { "public" "1" } } "branches" { "public" { "buildid" "1" } } } }"#,
        )
        .unwrap();
        assert_eq!(
            select_depots(&only_windows, 1, "public", Platform::LinuxX64),
            Err(ContentError::PlatformUnavailable)
        );
        let empty = KeyValue::from_text(
            r#""1" { "depots" { "branches" { "public" { "buildid" "1" } } } }"#,
        )
        .unwrap();
        assert_eq!(
            select_depots(&empty, 1, "public", Platform::LinuxX64),
            Err(ContentError::NoDepots)
        );
        assert_eq!(
            parse_app_info(b"not kv").err(),
            Some(ContentError::InvalidAppInfo)
        );
    }

    #[test]
    fn manifest_paths_must_stay_relative_and_links_are_refused() {
        use steamroom::depot::manifest::ManifestFile;
        let mut ok = ManifestFile::new("keys\\a3.bikey".into(), 3);
        ok.sha_content = Some([0xab; 20]);
        let mut folder = ManifestFile::new("mpmissions".into(), 0);
        folder.flags = DepotFileFlags::DIRECTORY.bits();
        let entries = manifest_entries(&DepotManifest::new(vec![ok.clone(), folder]), 7).unwrap();
        assert_eq!(
            entries["keys/a3.bikey"].sha1.as_deref(),
            Some("ab".repeat(20).as_str())
        );
        assert!(entries["mpmissions"].directory);
        for unsafe_name in ["..\\x", "/etc/passwd", "C:\\x", "a\\..\\..\\b", ""] {
            assert_eq!(
                manifest_entries(
                    &DepotManifest::new(vec![ManifestFile::new(unsafe_name.into(), 1)]),
                    7
                ),
                Err(ContentError::UnsafeManifestPath),
                "{unsafe_name}"
            );
        }
        let mut link = ManifestFile::new("lib.so".into(), 0);
        link.link_target = Some("other.so".into());
        assert_eq!(
            manifest_entries(&DepotManifest::new(vec![link]), 7),
            Err(ContentError::UnsupportedLink)
        );
    }

    #[test]
    fn only_denials_count_as_missing_entitlement() {
        use steamroom::error::ConnectionError;
        assert_eq!(
            key_failure(steamroom::Error::Connection(
                ConnectionError::DepotAccessDenied(7)
            )),
            ContentError::EntitlementRequired
        );
        assert_eq!(
            key_failure(steamroom::Error::Connection(ConnectionError::Io(
                std::io::ErrorKind::ConnectionReset.into()
            ))),
            ContentError::Network
        );
    }

    #[test]
    fn staged_tree_must_match_the_manifest_exactly() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("keys")).unwrap();
        std::fs::create_dir(root.path().join("mpmissions")).unwrap();
        std::fs::write(root.path().join("keys/a3.bikey"), b"abc").unwrap();
        let mut entries = BTreeMap::new();
        entries.insert(
            "keys/a3.bikey".to_owned(),
            ExpectedEntry {
                depot_id: 1,
                size: 3,
                sha1: Some("a9993e364706816aba3e25717850c26c9cd0d89d".into()),
                directory: false,
                executable: false,
            },
        );
        entries.insert(
            "mpmissions".to_owned(),
            ExpectedEntry {
                depot_id: 1,
                size: 0,
                sha1: None,
                directory: true,
                executable: false,
            },
        );
        let verified = verify_tree(root.path(), &entries, &|| false).unwrap();
        assert_eq!(verified.len(), 1);
        assert_eq!(
            verified[0].blake3,
            blake3::hash(b"abc").to_hex().to_string()
        );
        assert_eq!(
            verify_tree(root.path(), &entries, &|| true),
            Err(ContentError::Cancelled)
        );

        std::fs::write(root.path().join("extra.txt"), b"x").unwrap();
        assert_eq!(
            verify_tree(root.path(), &entries, &|| false),
            Err(ContentError::VerificationFailed)
        );
        std::fs::remove_file(root.path().join("extra.txt")).unwrap();
        std::fs::write(root.path().join("keys/a3.bikey"), b"abd").unwrap();
        assert_eq!(
            verify_tree(root.path(), &entries, &|| false),
            Err(ContentError::VerificationFailed)
        );
        std::fs::remove_file(root.path().join("keys/a3.bikey")).unwrap();
        assert_eq!(
            verify_tree(root.path(), &entries, &|| false),
            Err(ContentError::VerificationFailed)
        );
    }

    #[test]
    fn inventory_hashes_sorted_files_and_excludes_only_the_root_receipt() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("nested")).unwrap();
        std::fs::write(root.path().join("z.txt"), b"z").unwrap();
        std::fs::write(root.path().join("nested/a.txt"), b"a").unwrap();
        std::fs::write(root.path().join("receipt.json"), b"receipt").unwrap();
        std::fs::write(root.path().join("nested/receipt.json"), b"nested receipt").unwrap();

        let files = inventory_tree(root.path(), "receipt.json", &|| false).unwrap();
        assert_eq!(
            files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec!["nested/a.txt", "nested/receipt.json", "z.txt"]
        );
        assert_eq!(files[0].bytes, 1);
        assert_eq!(files[0].sha1, "86f7e437faa5a7fce15d1ddcb9eaeaea377667b8");
        assert_eq!(files[0].blake3, blake3::hash(b"a").to_hex().to_string());
        assert_eq!(files[2].sha1, "395df8f7c51f007019cb30201c49e884b46b92fa");
    }

    #[test]
    fn inventory_rejects_links_and_honors_cancellation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file"), b"data").unwrap();
        assert_eq!(
            inventory_tree(root.path(), "receipt", &|| true),
            Err(ContentError::Cancelled)
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.path().join("file"), root.path().join("linked"))
                .unwrap();
            assert_eq!(
                inventory_tree(root.path(), "receipt", &|| false),
                Err(ContentError::VerificationFailed)
            );
        }
        assert_eq!(
            inventory_tree(root.path(), "nested/receipt", &|| false),
            Err(ContentError::UnsafeManifestPath)
        );
    }

    #[tokio::test]
    async fn cancellable_stops_before_and_during_a_step() {
        let (cancel_tx, cancel_rx) = watch::channel(true);
        assert_eq!(
            cancellable(cancel_rx.clone(), Duration::from_secs(5), async {
                Ok::<_, ContentError>(1)
            })
            .await,
            Err(ContentError::Cancelled)
        );
        cancel_tx.send_replace(false);
        let pending = cancellable(
            cancel_rx,
            Duration::from_secs(5),
            std::future::pending::<Result<(), ContentError>>(),
        );
        let cancel = async {
            tokio::task::yield_now().await;
            cancel_tx.send_replace(true);
        };
        let (result, ()) = tokio::join!(pending, cancel);
        assert_eq!(result, Err(ContentError::Cancelled));
    }
}
