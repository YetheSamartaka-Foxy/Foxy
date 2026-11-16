use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use prost::Message;
use serde::Serialize;
use steamroom::client::{LoggedIn, SteamClient};
use steamroom::generated::{CPublishedFileGetDetailsRequest, CPublishedFileGetDetailsResponse};
use tokio::sync::mpsc;

use crate::content::{self, ContentError, DepotRequest, DownloadEvent, ExpectedEntry};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);
pub const MAX_ITEMS: usize = 256;
const RESULT_OK: u32 = 1;
const FILE_TYPE_COMMUNITY: u32 = 0;
const FILE_TYPE_COLLECTION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkshopItem {
    pub item_id: u64,
    pub result: u32,
    pub consumer_app_id: u32,
    pub creator_app_id: u32,
    pub title: String,
    pub file_size: u64,
    pub manifest_gid: Option<u64>,
    pub legacy_file: bool,
    pub time_updated: u32,
    pub visibility: u32,
    pub banned: bool,
    pub file_type: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemIssue {
    NotFound,
    WrongApp,
    Banned,
    Collection,
    UnsupportedType,
    LegacyFile,
    NoContent,
}

impl ItemIssue {
    pub fn message(self) -> &'static str {
        match self {
            Self::NotFound => {
                "Steam did not return this Workshop item; it may be removed or private"
            }
            Self::WrongApp => "This Workshop item belongs to another game",
            Self::Banned => "Steam reports this Workshop item as banned",
            Self::Collection => "Collections are not downloaded; review their items individually",
            Self::UnsupportedType => "This Workshop entry is not a downloadable mod",
            Self::LegacyFile => {
                "This item uses legacy single-file storage that Foxy does not download"
            }
            Self::NoContent => "Steam reports no downloadable content for this item",
        }
    }
}

/// The manifest to download, or why the item cannot be acquired for `app_id`.
pub fn eligible(item: &WorkshopItem, app_id: u32) -> Result<u64, ItemIssue> {
    if item.result != RESULT_OK {
        return Err(ItemIssue::NotFound);
    }
    if item.consumer_app_id != app_id {
        return Err(ItemIssue::WrongApp);
    }
    if item.banned {
        return Err(ItemIssue::Banned);
    }
    match item.file_type {
        FILE_TYPE_COMMUNITY => {}
        FILE_TYPE_COLLECTION => return Err(ItemIssue::Collection),
        _ => return Err(ItemIssue::UnsupportedType),
    }
    match item.manifest_gid {
        Some(gid) => Ok(gid),
        None if item.legacy_file => Err(ItemIssue::LegacyFile),
        None => Err(ItemIssue::NoContent),
    }
}

fn from_details(details: &steamroom::generated::PublishedFileDetails) -> Option<WorkshopItem> {
    Some(WorkshopItem {
        item_id: details.publishedfileid?,
        result: details.result.unwrap_or(0),
        consumer_app_id: details.consumer_appid.unwrap_or(0),
        creator_app_id: details.creator_appid.unwrap_or(0),
        title: details
            .title
            .clone()
            .unwrap_or_default()
            .chars()
            .filter(|character| !character.is_control())
            .take(256)
            .collect(),
        file_size: details.file_size.unwrap_or(0),
        manifest_gid: details.hcontent_file.filter(|gid| *gid != 0),
        legacy_file: details
            .file_url
            .as_deref()
            .is_some_and(|url| !url.is_empty()),
        time_updated: details.time_updated.unwrap_or(0),
        visibility: details.visibility.unwrap_or(0),
        banned: details.banned.unwrap_or(false),
        file_type: details.file_type.unwrap_or(0),
    })
}

/// Workshop details for each requested id, in request order; unknown ids report `NotFound`.
pub async fn item_details(
    client: &SteamClient<LoggedIn>,
    ids: &[u64],
) -> Result<Vec<WorkshopItem>, ContentError> {
    if ids.is_empty() || ids.len() > MAX_ITEMS {
        return Err(ContentError::InvalidAppInfo);
    }
    let request = CPublishedFileGetDetailsRequest {
        publishedfileids: ids.to_vec(),
        ..Default::default()
    };
    let response = tokio::time::timeout(
        REQUEST_TIMEOUT,
        client.call_service_method("PublishedFile.GetDetails#1", &request.encode_to_vec()),
    )
    .await
    .map_err(|_| ContentError::TimedOut)?
    .map_err(|_| ContentError::Network)?;
    let decoded: CPublishedFileGetDetailsResponse = response
        .decode()
        .map_err(|_| ContentError::InvalidAppInfo)?;
    let mut found: BTreeMap<u64, WorkshopItem> = decoded
        .publishedfiledetails
        .iter()
        .filter_map(from_details)
        .map(|item| (item.item_id, item))
        .collect();
    Ok(ids
        .iter()
        .map(|id| {
            found.remove(id).unwrap_or(WorkshopItem {
                item_id: *id,
                result: 0,
                consumer_app_id: 0,
                creator_app_id: 0,
                title: String::new(),
                file_size: 0,
                manifest_gid: None,
                legacy_file: false,
                time_updated: 0,
                visibility: 0,
                banned: false,
                file_type: 0,
            })
        })
        .collect())
}

/// Whether the session can decrypt the app's Workshop content depot.
pub async fn ugc_entitled(
    client: &SteamClient<LoggedIn>,
    app_id: u32,
) -> Result<bool, ContentError> {
    content::depot_entitled(client, app_id, app_id).await
}

/// Downloads one item's UGC manifest into `addon_dir`, which must be empty.
pub async fn download_item(
    client: &SteamClient<LoggedIn>,
    servers: &[steamroom::cdn::CdnServer],
    app_id: u32,
    manifest_gid: u64,
    addon_dir: &Path,
    events: mpsc::UnboundedSender<DownloadEvent>,
    max_downloads: usize,
) -> Result<BTreeMap<String, ExpectedEntry>, ContentError> {
    let prepared = content::prepare_depot(
        client,
        servers,
        DepotRequest {
            app_id,
            depot_id: app_id,
            manifest_gid,
            branch: None,
        },
    )
    .await?;
    content::fetch_depot(servers, &prepared, addon_dir, events, max_downloads).await?;
    Ok(prepared.entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> WorkshopItem {
        WorkshopItem {
            item_id: 42,
            result: RESULT_OK,
            consumer_app_id: 107_410,
            creator_app_id: 107_410,
            title: "Synthetic".into(),
            file_size: 10,
            manifest_gid: Some(7),
            legacy_file: false,
            time_updated: 1,
            visibility: 0,
            banned: false,
            file_type: FILE_TYPE_COMMUNITY,
        }
    }

    #[test]
    fn eligibility_reports_one_explicit_reason_per_item() {
        assert_eq!(eligible(&item(), 107_410), Ok(7));
        type Change = fn(&mut WorkshopItem);
        let cases: [(Change, ItemIssue); 7] = [
            (|item| item.result = 9, ItemIssue::NotFound),
            (|item| item.consumer_app_id = 1, ItemIssue::WrongApp),
            (|item| item.banned = true, ItemIssue::Banned),
            (
                |item| item.file_type = FILE_TYPE_COLLECTION,
                ItemIssue::Collection,
            ),
            (|item| item.file_type = 5, ItemIssue::UnsupportedType),
            (
                |item| {
                    item.manifest_gid = None;
                    item.legacy_file = true;
                },
                ItemIssue::LegacyFile,
            ),
            (|item| item.manifest_gid = None, ItemIssue::NoContent),
        ];
        for (change, issue) in cases {
            let mut candidate = item();
            change(&mut candidate);
            assert_eq!(eligible(&candidate, 107_410), Err(issue));
            assert!(!issue.message().is_empty());
        }
    }

    #[test]
    fn details_are_sanitized_and_zero_manifest_means_none() {
        let details = steamroom::generated::PublishedFileDetails {
            publishedfileid: Some(42),
            result: Some(1),
            consumer_appid: Some(107_410),
            title: Some("Bad\u{7}Title\nx".into()),
            hcontent_file: Some(0),
            file_url: Some("https://example.invalid/file".into()),
            ..Default::default()
        };
        let item = from_details(&details).unwrap();
        assert_eq!(item.title, "BadTitlex");
        assert_eq!(item.manifest_gid, None);
        assert!(item.legacy_file);
        assert_eq!(eligible(&item, 107_410), Err(ItemIssue::LegacyFile));
        assert!(from_details(&steamroom::generated::PublishedFileDetails::default()).is_none());
    }
}
