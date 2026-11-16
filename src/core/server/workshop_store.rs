use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use foxy_steam::AuthError;
use foxy_steam::content::{self, ContentError, DownloadEvent, ExpectedEntry, VerifiedFile};
use foxy_steam::workshop::{self, ItemIssue, WorkshopItem};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use super::content_sources::{AddonSupply, ContentSources, RepositoryProfile};
use super::context::ServerContext;
use super::distribution::ARMA3_CLIENT_APP;
use super::native_install::{
    Session, create_staging, lock_file, logon, publish_no_replace, remove_staging, staging_name,
};
use super::{ContentStageControl, ContentStagePhase, ServerStore, ServerStoreError};

const STORE: &str = "server/workshop";
const DOCUMENT: &str = "workshop.json";
const INVENTORY: &str = "inventory.json";
const SCHEMA_VERSION: u32 = 1;
const REVIEW_DEADLINE: Duration = Duration::from_secs(5 * 60);
const DOWNLOAD_DEADLINE: Duration = Duration::from_secs(6 * 60 * 60);
const MAX_DOWNLOADS: usize = 8;

#[derive(Clone, Debug)]
pub struct WorkshopRequest {
    pub items: Vec<String>,
    pub account: Option<String>,
    pub repair: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkshopItemAction {
    Acquire,
    Update,
    Repair,
    Current,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlannedWorkshopItem {
    pub item_id: String,
    pub title: String,
    pub action: WorkshopItemAction,
    pub manifest_gid: Option<String>,
    pub pinned_manifest: Option<String>,
    pub file_size: u64,
    pub time_updated: u32,
    pub issue: Option<ItemIssue>,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkshopPlan {
    #[serde(skip)]
    context: ServerContext,
    pub account: Option<String>,
    pub repair: bool,
    pub items: Vec<PlannedWorkshopItem>,
    pub entitled: bool,
    pub document_generation: u64,
    pub required_bytes: u64,
    pub blockers: Vec<String>,
    pub fingerprint: String,
    pub writes_performed: bool,
    pub database_opened: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkshopItemStatus {
    Acquired,
    Reused,
    Current,
    Unavailable,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkshopItemOutcome {
    pub item_id: String,
    pub status: WorkshopItemStatus,
    pub manifest_gid: Option<String>,
    pub files: u64,
    pub bytes: u64,
    pub pinned: bool,
    pub staging_removed: Option<bool>,
    pub content_error: Option<ContentError>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkshopBatchStatus {
    Complete,
    Partial,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkshopOutcome {
    pub status: WorkshopBatchStatus,
    pub items: Vec<WorkshopItemOutcome>,
    pub pins_changed: bool,
    pub auth_error: Option<AuthError>,
    pub writes_performed: bool,
    pub database_opened: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackedWorkshopItem {
    pub item_id: String,
    pub title: String,
    pub consumer_app_id: u32,
    pub pinned_manifest: String,
    pub pinned_time_updated: u32,
    pub revisions: Vec<String>,
    pub profile: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkshopSnapshot {
    pub generation: u64,
    pub items: Vec<TrackedWorkshopItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorkshopVerification {
    pub item_id: String,
    pub manifest_gid: String,
    pub verified: bool,
    pub files: u64,
    pub bytes: u64,
    pub error: Option<String>,
    pub writes_performed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkshopDocument {
    schema_version: u32,
    game_space_id: String,
    generation: u64,
    items: Vec<TrackedWorkshopItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RevisionInventory {
    schema_version: u32,
    item_id: String,
    manifest_gid: String,
    time_updated: u32,
    directories: Vec<String>,
    files: Vec<InventoryFile>,
    bytes: u64,
    inventory_blake3: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InventoryFile {
    path: String,
    bytes: u64,
    sha1: String,
    blake3: String,
}

fn valid_item_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 20
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<u64>().is_ok_and(|id| id != 0)
}

fn valid_gid(value: &str) -> bool {
    valid_item_id(value)
}

pub(super) fn addon_name(item_id: &str) -> String {
    format!("@workshop_{item_id}")
}

fn profile_name(item_id: &str) -> String {
    format!("workshop-{item_id}")
}

fn inventory_hash(files: &[InventoryFile]) -> String {
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

impl RevisionInventory {
    fn new(
        item_id: &str,
        manifest_gid: &str,
        time_updated: u32,
        entries: &BTreeMap<String, ExpectedEntry>,
        verified: &[VerifiedFile],
    ) -> Self {
        let files: Vec<InventoryFile> = verified
            .iter()
            .map(|file| InventoryFile {
                path: file.path.clone(),
                bytes: file.bytes,
                sha1: file.sha1.clone(),
                blake3: file.blake3.clone(),
            })
            .collect();
        Self {
            schema_version: SCHEMA_VERSION,
            item_id: item_id.to_owned(),
            manifest_gid: manifest_gid.to_owned(),
            time_updated,
            directories: entries
                .iter()
                .filter(|(_, entry)| entry.directory)
                .map(|(path, _)| path.clone())
                .collect(),
            bytes: files.iter().map(|file| file.bytes).sum(),
            inventory_blake3: inventory_hash(&files),
            files,
        }
    }

    fn expected(&self) -> BTreeMap<String, ExpectedEntry> {
        let mut entries = BTreeMap::new();
        for directory in &self.directories {
            entries.insert(
                directory.clone(),
                ExpectedEntry {
                    depot_id: ARMA3_CLIENT_APP,
                    size: 0,
                    sha1: None,
                    directory: true,
                    executable: false,
                },
            );
        }
        for file in &self.files {
            entries.insert(
                file.path.clone(),
                ExpectedEntry {
                    depot_id: ARMA3_CLIENT_APP,
                    size: file.bytes,
                    sha1: Some(file.sha1.clone()),
                    directory: false,
                    executable: false,
                },
            );
        }
        entries
    }

    fn verify(&self, addon: &Path, cancelled: &dyn Fn() -> bool) -> Result<u64, ContentError> {
        let verified = content::verify_tree(addon, &self.expected(), cancelled)?;
        let recorded: BTreeMap<&str, &str> = self
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.blake3.as_str()))
            .collect();
        if verified.len() != self.files.len()
            || verified
                .iter()
                .any(|file| recorded.get(file.path.as_str()) != Some(&file.blake3.as_str()))
            || inventory_hash(&self.files) != self.inventory_blake3
        {
            return Err(ContentError::VerificationFailed);
        }
        Ok(self.bytes)
    }
}

fn write_new(path: &Path, value: &impl Serialize) -> Result<(), ServerStoreError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| ServerStoreError::Corrupt)?;
    let mut file = File::create_new(path).map_err(|_| ServerStoreError::Io)?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| ServerStoreError::Io)
}

impl ServerStore {
    fn workshop_root(&self) -> PathBuf {
        self.context().space_dir().join(STORE)
    }

    fn workshop_revision(&self, item_id: &str, manifest_gid: &str) -> PathBuf {
        self.workshop_root()
            .join("items")
            .join(item_id)
            .join(manifest_gid)
    }

    fn require_workshop(&self) -> Result<(), ServerStoreError> {
        if !self.capabilities().workshop_acquisition {
            return Err(ServerStoreError::UnsupportedGame);
        }
        Ok(())
    }

    fn read_workshop(&self) -> Result<WorkshopDocument, ServerStoreError> {
        let path = self.workshop_root().join(DOCUMENT);
        super::store::check_directory_chain(path.parent().ok_or(ServerStoreError::UnsafePath)?)?;
        super::store::check_regular_file(&path)?;
        let bytes = match super::store::read_bounded(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(WorkshopDocument {
                    schema_version: SCHEMA_VERSION,
                    game_space_id: self.context().space_id().into(),
                    generation: 0,
                    items: Vec::new(),
                });
            }
            Err(_) => return Err(ServerStoreError::Io),
        };
        let document: WorkshopDocument =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        if document.schema_version != SCHEMA_VERSION
            || document.game_space_id != self.context().space_id()
            || document.items.iter().any(|item| {
                !valid_item_id(&item.item_id)
                    || !valid_gid(&item.pinned_manifest)
                    || item.revisions.iter().any(|gid| !valid_gid(gid))
                    || item.profile != profile_name(&item.item_id)
            })
        {
            return Err(ServerStoreError::Corrupt);
        }
        Ok(document)
    }

    fn write_workshop(&self, document: &WorkshopDocument) -> Result<(), ServerStoreError> {
        let root = self.workshop_root();
        super::store::check_directory_chain(&root)?;
        fs::create_dir_all(&root).map_err(|_| ServerStoreError::Io)?;
        let bytes = serde_json::to_vec_pretty(document).map_err(|_| ServerStoreError::Corrupt)?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".workshop-")
            .tempfile_in(&root)
            .map_err(|_| ServerStoreError::Io)?;
        temporary
            .write_all(&bytes)
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|_| ServerStoreError::Io)?;
        temporary
            .persist(root.join(DOCUMENT))
            .map_err(|_| ServerStoreError::Io)?;
        Ok(())
    }

    fn read_inventory(
        &self,
        item_id: &str,
        manifest_gid: &str,
    ) -> Result<RevisionInventory, ServerStoreError> {
        let path = self
            .workshop_revision(item_id, manifest_gid)
            .join(INVENTORY);
        super::store::check_regular_file(&path)?;
        let bytes = super::store::read_bounded(&path).map_err(|_| ServerStoreError::NotFound)?;
        let inventory: RevisionInventory =
            serde_json::from_slice(&bytes).map_err(|_| ServerStoreError::Corrupt)?;
        if inventory.schema_version != SCHEMA_VERSION
            || inventory.item_id != item_id
            || inventory.manifest_gid != manifest_gid
        {
            return Err(ServerStoreError::Corrupt);
        }
        Ok(inventory)
    }

    pub fn workshop_items(&self) -> Result<WorkshopSnapshot, ServerStoreError> {
        self.require_workshop()?;
        let document = self.read_workshop()?;
        Ok(WorkshopSnapshot {
            generation: document.generation,
            items: document.items,
        })
    }

    pub fn verify_workshop_item(
        &self,
        item_id: &str,
        control: &ContentStageControl,
    ) -> Result<WorkshopVerification, ServerStoreError> {
        self.require_workshop()?;
        let item = self
            .read_workshop()?
            .items
            .into_iter()
            .find(|item| item.item_id == item_id)
            .ok_or(ServerStoreError::NotFound)?;
        let mut report = WorkshopVerification {
            item_id: item.item_id.clone(),
            manifest_gid: item.pinned_manifest.clone(),
            verified: false,
            files: 0,
            bytes: 0,
            error: None,
            writes_performed: false,
        };
        let inventory = match self.read_inventory(&item.item_id, &item.pinned_manifest) {
            Ok(inventory) => inventory,
            Err(error) => {
                report.error = Some(error.to_string());
                return Ok(report);
            }
        };
        control.start_phase(
            ContentStagePhase::Verifying,
            inventory.files.len() as u64,
            inventory.bytes,
        );
        let addon = self
            .workshop_revision(&item.item_id, &item.pinned_manifest)
            .join(addon_name(&item.item_id));
        let cancelled = || control.checkpoint().is_err();
        match inventory.verify(&addon, &cancelled) {
            Ok(bytes) => {
                report.verified = true;
                report.files = inventory.files.len() as u64;
                report.bytes = bytes;
            }
            Err(ContentError::Cancelled) => return Err(ServerStoreError::Cancelled),
            Err(error) => report.error = Some(error.to_string()),
        }
        control.finish(&Ok(()));
        Ok(report)
    }

    pub fn plan_workshop(
        &self,
        request: WorkshopRequest,
        control: &ContentStageControl,
    ) -> Result<WorkshopPlan, ServerStoreError> {
        self.require_workshop()?;
        if request.items.is_empty() || request.items.len() > workshop::MAX_ITEMS {
            return Err(ServerStoreError::Invalid(
                "Review between 1 and 256 Workshop items".into(),
            ));
        }
        let mut ids = Vec::new();
        for item in &request.items {
            let id = super::super::game::workshop::normalize_workshop_id(item)
                .filter(|id| valid_item_id(id))
                .ok_or_else(|| ServerStoreError::Invalid("Invalid Workshop item id".into()))?;
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if request
            .account
            .as_deref()
            .is_some_and(|alias| !super::steam_auth::valid_account_alias(alias))
        {
            return Err(ServerStoreError::Invalid(
                "Invalid Steam session alias".into(),
            ));
        }
        let document = self.read_workshop()?;
        if request.repair
            && ids
                .iter()
                .any(|id| !document.items.iter().any(|item| &item.item_id == id))
        {
            return Err(ServerStoreError::Invalid(
                "Only tracked Workshop items can be repaired".into(),
            ));
        }
        let input = super::steam_auth::unattended_input(self.context(), request.account.as_deref())
            .map_err(|error| ServerStoreError::Invalid(error.to_string()))?;
        let numeric: Vec<u64> = ids.iter().map(|id| id.parse().unwrap_or(0)).collect();

        control.start_phase(ContentStagePhase::Preparing, ids.len() as u64, 0);
        let session = Session::new(control)?;
        let cancel = session.cancel_rx();
        let reviewed = session.runtime.block_on(async {
            let logged_in = logon(input, cancel.clone())
                .await
                .map_err(|error| error.to_string())?;
            let review = content::cancellable(cancel, REVIEW_DEADLINE, async {
                let details = workshop::item_details(&logged_in.client, &numeric).await?;
                let entitled = workshop::ugc_entitled(&logged_in.client, ARMA3_CLIENT_APP).await?;
                Ok((details, entitled))
            })
            .await
            .map_err(|error| error.to_string());
            drop(logged_in);
            review
        });
        session.close();
        control.checkpoint()?;
        let (details, entitled) = reviewed.map_err(ServerStoreError::Invalid)?;

        let items: Vec<PlannedWorkshopItem> = details
            .iter()
            .map(|detail| plan_item(detail, &document, request.repair))
            .collect();
        let actionable = items.iter().filter(|item| needs_download(item)).count();
        let mut blockers = Vec::new();
        if actionable > 0 && !entitled {
            blockers.push(ContentError::EntitlementRequired.to_string());
        }
        if actionable == 0 {
            blockers.push("No reviewed Workshop item needs a download.".into());
        }
        let required_bytes = items
            .iter()
            .filter(|item| needs_download(item))
            .map(|item| item.file_size)
            .sum();
        let mut plan = WorkshopPlan {
            context: self.context().clone(),
            account: request.account,
            repair: request.repair,
            items,
            entitled,
            document_generation: document.generation,
            required_bytes,
            blockers,
            fingerprint: String::new(),
            writes_performed: false,
            database_opened: false,
        };
        plan.fingerprint = workshop_fingerprint(&plan);
        control.finish(&Ok(()));
        Ok(plan)
    }

    /// Acquires each reviewed item into a new verified revision and pins it. Items succeed or
    /// fail independently; the batch is complete only when every actionable item succeeded.
    pub fn apply_workshop(
        &self,
        plan: &WorkshopPlan,
        control: &ContentStageControl,
    ) -> Result<WorkshopOutcome, ServerStoreError> {
        self.require_workshop()?;
        if &plan.context != self.context() {
            return Err(ServerStoreError::StaleRevision);
        }
        if !plan.blockers.is_empty() {
            return Err(ServerStoreError::Invalid(plan.blockers.join(" ")));
        }
        if self.read_workshop()?.generation != plan.document_generation {
            return Err(ServerStoreError::StaleRevision);
        }
        let input = super::steam_auth::unattended_input(self.context(), plan.account.as_deref())
            .map_err(|error| ServerStoreError::Invalid(error.to_string()))?;
        let mut outcome = WorkshopOutcome {
            status: WorkshopBatchStatus::Failed,
            items: Vec::new(),
            pins_changed: false,
            auth_error: None,
            writes_performed: false,
            database_opened: false,
        };
        let actionable: Vec<&PlannedWorkshopItem> = plan
            .items
            .iter()
            .filter(|item| needs_download(item))
            .collect();
        let total = actionable.iter().map(|item| item.file_size).sum();
        control.start_phase(ContentStagePhase::Preparing, actionable.len() as u64, total);
        let session = Session::new(control)?;
        let cancel = session.cancel_rx();
        let numeric: Vec<u64> = actionable
            .iter()
            .map(|item| item.item_id.parse().unwrap_or(0))
            .collect();
        let connected = session.runtime.block_on(async {
            let logged_in = logon(input, cancel.clone())
                .await
                .map_err(|error| (Some(error), None))?;
            let current = content::cancellable(cancel.clone(), REVIEW_DEADLINE, async {
                let details = workshop::item_details(&logged_in.client, &numeric).await?;
                let servers = content::cdn_servers(&logged_in.client).await?;
                Ok((details, servers))
            })
            .await
            .map_err(|error| (None, Some(error)))?;
            Ok::<_, (Option<AuthError>, Option<ContentError>)>((logged_in, current))
        });
        let heartbeat = connected.as_ref().ok().map(|(logged_in, _)| {
            let _context = session.runtime.enter();
            content::spawn_heartbeat(&logged_in.client)
        });
        let (logged_in, (details, servers)) = match connected {
            Ok(connected) => connected,
            Err((auth_error, content_error)) => {
                session.close();
                outcome.auth_error = auth_error;
                let status = if control.checkpoint().is_err()
                    || content_error == Some(ContentError::Cancelled)
                {
                    WorkshopItemStatus::Cancelled
                } else {
                    WorkshopItemStatus::Failed
                };
                outcome.items = plan
                    .items
                    .iter()
                    .map(|item| skipped(item, status, content_error))
                    .collect();
                outcome.status = batch_status(&outcome.items, control);
                control.finish(&Ok(()));
                return Ok(outcome);
            }
        };
        let latest: BTreeMap<String, &WorkshopItem> = details
            .iter()
            .map(|item| (item.item_id.to_string(), item))
            .collect();
        control.start_phase(ContentStagePhase::Copying, actionable.len() as u64, total);

        for item in &plan.items {
            if !needs_download(item) {
                outcome.items.push(skipped(
                    item,
                    if item.action == WorkshopItemAction::Current {
                        WorkshopItemStatus::Current
                    } else {
                        WorkshopItemStatus::Unavailable
                    },
                    None,
                ));
                continue;
            }
            if control.checkpoint().is_err() {
                outcome.items.push(skipped(
                    item,
                    WorkshopItemStatus::Cancelled,
                    Some(ContentError::Cancelled),
                ));
                continue;
            }
            let planned_gid = item.manifest_gid.clone().unwrap_or_default();
            let still_current = latest.get(&item.item_id).is_some_and(|current| {
                item.action == WorkshopItemAction::Repair
                    || current.manifest_gid.map(|gid| gid.to_string()) == item.manifest_gid
            });
            if !still_current {
                outcome.items.push(skipped(
                    item,
                    WorkshopItemStatus::Failed,
                    Some(ContentError::ManifestMismatch),
                ));
                continue;
            }
            let result = self.acquire_item(
                &session,
                &logged_in.client,
                &servers,
                item,
                &planned_gid,
                control,
            );
            outcome.writes_performed |= result.staging_removed.is_some() || result.pinned;
            outcome.pins_changed |= result.pinned;
            outcome.items.push(result);
        }
        if let Some(heartbeat) = heartbeat {
            heartbeat.abort();
        }
        session.runtime.block_on(async move { drop(logged_in) });
        session.close();
        outcome.status = batch_status(&outcome.items, control);
        control.finish(&Ok(()));
        Ok(outcome)
    }

    fn acquire_item(
        &self,
        session: &Session,
        client: &foxy_steam::SteamClient<foxy_steam::LoggedIn>,
        servers: &[foxy_steam::CdnServer],
        item: &PlannedWorkshopItem,
        manifest_gid: &str,
        control: &ContentStageControl,
    ) -> WorkshopItemOutcome {
        let mut result = skipped(item, WorkshopItemStatus::Failed, None);
        let revision = self.workshop_revision(&item.item_id, manifest_gid);
        let Some(item_dir) = revision.parent().map(Path::to_path_buf) else {
            result.error = Some("Invalid Workshop revision path".into());
            return result;
        };
        let cancelled = || control.checkpoint().is_err();
        if item.action != WorkshopItemAction::Repair
            && let Ok(inventory) = self.read_inventory(&item.item_id, manifest_gid)
            && inventory
                .verify(&revision.join(addon_name(&item.item_id)), &cancelled)
                .is_ok()
        {
            result.status = WorkshopItemStatus::Reused;
            result.files = inventory.files.len() as u64;
            result.bytes = inventory.bytes;
            return self.finish_pin(item, manifest_gid, inventory.time_updated, result);
        }
        if super::store::check_directory_chain(&item_dir).is_err()
            || fs::create_dir_all(&item_dir).is_err()
        {
            result.error = Some("Cannot create the Workshop item folder".into());
            return result;
        }
        let Ok(prefix) = staging_name(&revision) else {
            result.error = Some("Invalid Workshop revision path".into());
            return result;
        };
        let staging = item_dir.join(format!("{prefix}{:016x}", rand::random::<u64>()));
        let lock_path = lock_file(&staging);
        let Ok(lock) = create_staging(&staging, &lock_path) else {
            result.error = Some("Cannot create the locked Workshop staging folder".into());
            return result;
        };
        result.staging_removed = Some(false);
        let addon = staging.join(addon_name(&item.item_id));
        if fs::create_dir(&addon).is_err() {
            result.staging_removed = Some(remove_staging(&staging, &lock_path, lock));
            result.error = Some("Cannot create the Workshop staging folder".into());
            return result;
        }
        let gid: u64 = manifest_gid.parse().unwrap_or(0);
        let reporter = control.clone();
        let cancel = session.cancel_rx();
        let downloaded = session.runtime.block_on(async {
            let (events_tx, mut events_rx) = mpsc::unbounded_channel();
            let forward = tokio::spawn(async move {
                while let Some(event) = events_rx.recv().await {
                    if let DownloadEvent::ChunkCompleted { bytes } = event {
                        reporter.record_bytes(bytes);
                    }
                }
            });
            let entries = content::cancellable(
                cancel,
                DOWNLOAD_DEADLINE,
                workshop::download_item(
                    client,
                    servers,
                    ARMA3_CLIENT_APP,
                    gid,
                    &addon,
                    events_tx,
                    MAX_DOWNLOADS,
                ),
            )
            .await;
            let _ = forward.await;
            entries
        });
        let entries = match downloaded {
            Ok(entries) => entries,
            Err(error) => {
                result.status = if error == ContentError::Cancelled {
                    WorkshopItemStatus::Cancelled
                } else {
                    WorkshopItemStatus::Failed
                };
                result.content_error = Some(error);
                result.staging_removed = Some(remove_staging(&staging, &lock_path, lock));
                return result;
            }
        };
        let verified = match content::verify_tree(&addon, &entries, &cancelled) {
            Ok(verified) => verified,
            Err(error) => {
                result.status = if error == ContentError::Cancelled {
                    WorkshopItemStatus::Cancelled
                } else {
                    WorkshopItemStatus::Failed
                };
                result.content_error = Some(error);
                result.staging_removed = Some(remove_staging(&staging, &lock_path, lock));
                return result;
            }
        };
        let inventory = RevisionInventory::new(
            &item.item_id,
            manifest_gid,
            item.time_updated,
            &entries,
            &verified,
        );
        if write_new(&staging.join(INVENTORY), &inventory).is_err() || cancelled() {
            if cancelled() {
                result.status = WorkshopItemStatus::Cancelled;
            } else {
                result.error = Some("Cannot write the Workshop revision inventory".into());
            }
            result.staging_removed = Some(remove_staging(&staging, &lock_path, lock));
            return result;
        }
        let damaged = if fs::symlink_metadata(&revision).is_ok() {
            let aside = item_dir.join(format!(
                ".foxy-damaged-{manifest_gid}-{:016x}",
                rand::random::<u64>()
            ));
            if fs::rename(&revision, &aside).is_err() {
                result.error = Some("Cannot move the damaged Workshop revision aside".into());
                result.staging_removed = Some(remove_staging(&staging, &lock_path, lock));
                return result;
            }
            Some(aside)
        } else {
            None
        };
        if publish_no_replace(&staging, &revision).is_err() {
            if let Some(aside) = &damaged {
                let _ = fs::rename(aside, &revision);
            }
            result.error = Some("Publishing the verified Workshop revision failed".into());
            result.staging_removed = Some(remove_staging(&staging, &lock_path, lock));
            return result;
        }
        drop(lock);
        let _ = fs::remove_file(&lock_path);
        result.staging_removed = Some(true);
        if let Some(aside) = damaged {
            let _ = fs::remove_dir_all(aside);
        }
        result.status = WorkshopItemStatus::Acquired;
        result.files = inventory.files.len() as u64;
        result.bytes = inventory.bytes;
        self.finish_pin(item, manifest_gid, item.time_updated, result)
    }

    fn finish_pin(
        &self,
        item: &PlannedWorkshopItem,
        manifest_gid: &str,
        time_updated: u32,
        mut result: WorkshopItemOutcome,
    ) -> WorkshopItemOutcome {
        match self.pin_workshop_revision(item, manifest_gid, time_updated) {
            Ok(()) => result.pinned = true,
            Err(error) => {
                result.status = WorkshopItemStatus::Failed;
                result.error = Some(format!(
                    "The verified revision is stored but was not pinned: {error}"
                ));
            }
        }
        result
    }

    pub(super) fn pin_workshop_revision(
        &self,
        item: &PlannedWorkshopItem,
        manifest_gid: &str,
        time_updated: u32,
    ) -> Result<(), ServerStoreError> {
        let revision = self.workshop_revision(&item.item_id, manifest_gid);
        self.read_inventory(&item.item_id, manifest_gid)?;
        let name = profile_name(&item.item_id);
        let snapshot = ContentSources::read_snapshot(&self.context().space_dir())?;
        let mut sources = snapshot.sources.clone();
        sources.profiles.retain(|profile| profile.name != name);
        sources.folders.retain(|folder| folder.id != name);
        sources
            .add_folder(&name, &revision)
            .map_err(|error| ServerStoreError::Invalid(error.to_string()))?;
        sources
            .add_profile(RepositoryProfile {
                name: name.clone(),
                remote_url: String::new(),
                local_path: revision.clone(),
                folder_id: name.clone(),
                supply: AddonSupply::AlreadyPresent,
                addons: vec![addon_name(&item.item_id)],
            })
            .map_err(|error| ServerStoreError::Invalid(error.to_string()))?;
        let mut document = self.read_workshop()?;
        document.generation = document
            .generation
            .checked_add(1)
            .ok_or(ServerStoreError::Corrupt)?;
        match document
            .items
            .iter_mut()
            .find(|tracked| tracked.item_id == item.item_id)
        {
            Some(tracked) => {
                tracked.title = item.title.clone();
                tracked.pinned_manifest = manifest_gid.to_owned();
                tracked.pinned_time_updated = time_updated;
                if !tracked.revisions.iter().any(|gid| gid == manifest_gid) {
                    tracked.revisions.push(manifest_gid.to_owned());
                }
            }
            None => document.items.push(TrackedWorkshopItem {
                item_id: item.item_id.clone(),
                title: item.title.clone(),
                consumer_app_id: ARMA3_CLIENT_APP,
                pinned_manifest: manifest_gid.to_owned(),
                pinned_time_updated: time_updated,
                revisions: vec![manifest_gid.to_owned()],
                profile: name,
            }),
        }
        document
            .items
            .sort_by(|left, right| left.item_id.cmp(&right.item_id));
        super::owner::with_write(&self.context().database_path(), || {
            Ok((|| {
                let current = ServerContext::capture(
                    self.context().data_root(),
                    Some(self.context().space_id()),
                )
                .map_err(ServerStoreError::Invalid)?;
                if &current != self.context()
                    || ContentSources::read_snapshot(&self.context().space_dir())?.fingerprint
                        != snapshot.fingerprint
                {
                    return Err(ServerStoreError::StaleRevision);
                }
                sources
                    .save(&self.context().space_dir())
                    .map_err(|_| ServerStoreError::Io)?;
                self.write_workshop(&document)
            })())
        })
        .map_err(|_| ServerStoreError::Ownership)?
    }
}

fn needs_download(item: &PlannedWorkshopItem) -> bool {
    matches!(
        item.action,
        WorkshopItemAction::Acquire | WorkshopItemAction::Update | WorkshopItemAction::Repair
    )
}

fn plan_item(
    detail: &WorkshopItem,
    document: &WorkshopDocument,
    repair: bool,
) -> PlannedWorkshopItem {
    let item_id = detail.item_id.to_string();
    let tracked = document.items.iter().find(|item| item.item_id == item_id);
    let pinned_manifest = tracked.map(|item| item.pinned_manifest.clone());
    let mut planned = PlannedWorkshopItem {
        item_id,
        title: detail.title.clone(),
        action: WorkshopItemAction::Unavailable,
        manifest_gid: None,
        pinned_manifest: pinned_manifest.clone(),
        file_size: detail.file_size,
        time_updated: detail.time_updated,
        issue: None,
    };
    if repair {
        planned.action = WorkshopItemAction::Repair;
        planned.manifest_gid = pinned_manifest;
        planned.time_updated = tracked.map_or(0, |item| item.pinned_time_updated);
        return planned;
    }
    match workshop::eligible(detail, ARMA3_CLIENT_APP) {
        Ok(gid) => {
            let gid = gid.to_string();
            planned.action = match &pinned_manifest {
                None => WorkshopItemAction::Acquire,
                Some(pinned) if *pinned == gid => WorkshopItemAction::Current,
                Some(_) => WorkshopItemAction::Update,
            };
            planned.manifest_gid = Some(gid);
        }
        Err(issue) => planned.issue = Some(issue),
    }
    planned
}

fn skipped(
    item: &PlannedWorkshopItem,
    status: WorkshopItemStatus,
    content_error: Option<ContentError>,
) -> WorkshopItemOutcome {
    WorkshopItemOutcome {
        item_id: item.item_id.clone(),
        status,
        manifest_gid: item.manifest_gid.clone(),
        files: 0,
        bytes: 0,
        pinned: false,
        staging_removed: None,
        content_error,
        error: item.issue.map(|issue| issue.message().to_owned()),
    }
}

fn batch_status(
    items: &[WorkshopItemOutcome],
    control: &ContentStageControl,
) -> WorkshopBatchStatus {
    let acted: Vec<&WorkshopItemOutcome> = items
        .iter()
        .filter(|item| {
            !matches!(
                item.status,
                WorkshopItemStatus::Current | WorkshopItemStatus::Unavailable
            )
        })
        .collect();
    let succeeded = acted
        .iter()
        .filter(|item| {
            matches!(
                item.status,
                WorkshopItemStatus::Acquired | WorkshopItemStatus::Reused
            )
        })
        .count();
    if !acted.is_empty() && succeeded == acted.len() {
        WorkshopBatchStatus::Complete
    } else if control.checkpoint().is_err()
        || acted
            .iter()
            .any(|item| item.status == WorkshopItemStatus::Cancelled)
    {
        WorkshopBatchStatus::Cancelled
    } else if succeeded > 0 {
        WorkshopBatchStatus::Partial
    } else {
        WorkshopBatchStatus::Failed
    }
}

fn workshop_fingerprint(plan: &WorkshopPlan) -> String {
    let value = serde_json::json!({
        "root": plan.context.data_root(),
        "space": plan.context.space_id(),
        "game_id": plan.context.game_id(),
        "account": plan.account,
        "repair": plan.repair,
        "items": plan.items,
        "entitled": plan.entitled,
        "generation": plan.document_generation,
        "blockers": plan.blockers,
    });
    blake3::hash(value.to_string().as_bytes())
        .to_hex()
        .to_string()
}

#[cfg(test)]
mod tests;
