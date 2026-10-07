use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use super::{SteamWorkshopItem, WorkshopFile, ids_from_token, normalize_workshop_id};

pub const SHARE_SEPARATOR: char = '|';
const LOCAL_PREFIX: &str = "local:";
const VERSION_SEPARATOR: char = '@';
const LOAD_ORDER_SEPARATOR: char = ';';

/// One entry of a shared mod list. The wire format is the pipe-separated code
/// players paste to each other; `version` is a Foxy extension that pins the
/// exact frozen snapshot a friend was running.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedItem {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_order: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl SharedItem {
    pub fn is_resolvable(&self) -> bool {
        !self.item_id.is_empty()
    }

    pub fn label(&self) -> String {
        match (self.name.as_deref(), self.item_id.as_str()) {
            (Some(name), "") => name.to_string(),
            (Some(name), id) => format!("{} ({})", name, id),
            (None, id) => id.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShareCodeOptions {
    /// Append `;<order>` to each entry. Understood by Foxy and by the
    /// WH3 Mod Manager share format.
    pub include_load_order: bool,
    /// Append `@<version>` to each entry. Foxy-only: other tools read the
    /// whole identifier as the Workshop id and fail to match it.
    pub include_versions: bool,
}

/// Parse a pasted shared mod list. Accepts the pipe-separated share code,
/// newline or comma separated lists, bare ids, and Steam Workshop URLs.
pub fn parse_share_code(input: &str) -> Vec<SharedItem> {
    let mut out: Vec<SharedItem> = Vec::new();
    let mut seen = HashSet::new();
    for entry in input.split(|ch: char| {
        ch == SHARE_SEPARATOR || ch == ',' || ch == '\n' || ch == '\r' || ch == '\t'
    }) {
        for item in parse_share_entry(entry) {
            if seen.insert(dedup_key(&item)) {
                out.push(item);
            }
        }
    }
    out
}

fn dedup_key(item: &SharedItem) -> String {
    if item.item_id.is_empty() {
        format!(
            "name:{}",
            item.name.as_deref().unwrap_or_default().to_lowercase()
        )
    } else {
        format!("id:{}", item.item_id)
    }
}

fn parse_share_entry(entry: &str) -> Vec<SharedItem> {
    let entry = entry.trim();
    if entry.is_empty() {
        return Vec::new();
    }
    let (identifier, load_order) = split_load_order(entry);
    if let Some(local) = identifier.strip_prefix(LOCAL_PREFIX) {
        return vec![parse_local_entry(local, load_order)];
    }
    let (identifier, version) = split_version(identifier);
    // A bare id keeps its version pin; anything else (a URL, a pasted console
    // snippet) goes through the shared id scanner and cannot carry one.
    if let Some(item_id) = normalize_workshop_id(identifier) {
        return vec![SharedItem {
            item_id,
            name: None,
            load_order,
            version,
        }];
    }
    ids_from_token(identifier)
        .into_iter()
        .map(|item_id| SharedItem {
            item_id,
            name: None,
            load_order,
            version: None,
        })
        .collect()
}

fn parse_local_entry(local: &str, load_order: Option<u32>) -> SharedItem {
    let (encoded_name, fallback_id) = match local.rsplit_once(':') {
        Some((name, candidate)) => match normalize_workshop_id(candidate) {
            Some(id) => (name, Some(id)),
            None => (local, None),
        },
        None => (local, None),
    };
    let (encoded_name, version) = split_version(encoded_name);
    SharedItem {
        item_id: fallback_id.unwrap_or_default(),
        name: Some(percent_decode(encoded_name)),
        load_order,
        version,
    }
}

fn split_load_order(entry: &str) -> (&str, Option<u32>) {
    match entry.split_once(LOAD_ORDER_SEPARATOR) {
        Some((identifier, order)) => match order.trim().parse::<u32>() {
            Ok(order) => (identifier.trim(), Some(order)),
            Err(_) => (entry, None),
        },
        None => (entry, None),
    }
}

fn split_version(identifier: &str) -> (&str, Option<String>) {
    match identifier.split_once(VERSION_SEPARATOR) {
        Some((identifier, version)) if !version.trim().is_empty() => {
            (identifier.trim(), Some(version.trim().to_string()))
        }
        _ => (identifier, None),
    }
}

pub fn render_share_code(items: &[SharedItem], options: ShareCodeOptions) -> String {
    items
        .iter()
        .map(|item| render_share_entry(item, options))
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>()
        .join(&SHARE_SEPARATOR.to_string())
}

fn render_share_entry(item: &SharedItem, options: ShareCodeOptions) -> String {
    let mut entry = if item.item_id.is_empty() {
        let Some(name) = item.name.as_deref().filter(|name| !name.trim().is_empty()) else {
            return String::new();
        };
        format!("{}{}", LOCAL_PREFIX, percent_encode(name))
    } else {
        item.item_id.clone()
    };
    if options.include_versions
        && let Some(version) = item.version.as_deref().filter(|value| !value.is_empty())
    {
        entry.push(VERSION_SEPARATOR);
        entry.push_str(version);
    }
    if options.include_load_order
        && let Some(load_order) = item.load_order
    {
        entry.push(LOAD_ORDER_SEPARATOR);
        entry.push_str(&load_order.to_string());
    }
    entry
}

/// The managed items of one app, in launch order, as shareable entries.
pub fn shared_items_from_store(
    store: &WorkshopFile,
    app_id: u32,
    include_disabled: bool,
) -> Vec<SharedItem> {
    let mut entries: Vec<&SteamWorkshopItem> = store
        .entries
        .iter()
        .filter(|entry| entry.app_id == app_id)
        .filter(|entry| include_disabled || entry.enabled)
        .collect();
    entries.sort_by_key(|entry| super::launch_order_key(entry));
    entries
        .into_iter()
        .map(|entry| SharedItem {
            item_id: entry.item_id.clone(),
            name: entry.title.clone(),
            load_order: entry.load_order,
            version: entry.version.clone(),
        })
        .collect()
}

/// How a pasted share code differs from what this space has enabled. Drives the
/// "what do I still need" answer both the GUI and a future CLI diff report.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareComparison {
    /// In the pasted code but not enabled here.
    pub missing: Vec<SharedItem>,
    /// Enabled here but not in the pasted code.
    pub extra: Vec<SharedItem>,
    /// Named in the pasted code without a Workshop id, so Foxy cannot fetch it.
    pub unresolvable: Vec<SharedItem>,
    /// Both sides carry the same mods but hand them to the game in a different
    /// order, which changes which mod wins a conflict.
    pub order_differs: bool,
}

impl ShareComparison {
    pub fn matches(&self) -> bool {
        self.missing.is_empty() && self.extra.is_empty() && !self.order_differs
    }
}

pub fn compare_share_lists(local: &[SharedItem], remote: &[SharedItem]) -> ShareComparison {
    let local_ids: HashSet<&str> = local
        .iter()
        .filter(|item| item.is_resolvable())
        .map(|item| item.item_id.as_str())
        .collect();
    let remote_ids: HashSet<&str> = remote
        .iter()
        .filter(|item| item.is_resolvable())
        .map(|item| item.item_id.as_str())
        .collect();

    let shared_local: Vec<&str> = local
        .iter()
        .filter(|item| remote_ids.contains(item.item_id.as_str()))
        .map(|item| item.item_id.as_str())
        .collect();
    let shared_remote: Vec<&str> = remote
        .iter()
        .filter(|item| local_ids.contains(item.item_id.as_str()))
        .map(|item| item.item_id.as_str())
        .collect();

    ShareComparison {
        missing: remote
            .iter()
            .filter(|item| item.is_resolvable() && !local_ids.contains(item.item_id.as_str()))
            .cloned()
            .collect(),
        extra: local
            .iter()
            .filter(|item| item.is_resolvable() && !remote_ids.contains(item.item_id.as_str()))
            .cloned()
            .collect(),
        unresolvable: remote
            .iter()
            .filter(|item| !item.is_resolvable())
            .cloned()
            .collect(),
        order_differs: shared_local != shared_remote,
    }
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or_default();
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pipe_separated_workshop_ids() {
        let items = parse_share_code("3461495833|3485519396|2859968660");

        assert_eq!(
            items
                .iter()
                .map(|item| item.item_id.as_str())
                .collect::<Vec<_>>(),
            vec!["3461495833", "3485519396", "2859968660"]
        );
        assert!(items.iter().all(|item| item.load_order.is_none()));
    }

    #[test]
    fn parses_load_order_version_and_local_entries() {
        let items = parse_share_code("111;3|222@abc123;1|local:My%20Mod:333;2|local:Only%20Local");

        assert_eq!(items.len(), 4);
        assert_eq!(items[0].item_id, "111");
        assert_eq!(items[0].load_order, Some(3));
        assert_eq!(items[1].version.as_deref(), Some("abc123"));
        assert_eq!(items[1].load_order, Some(1));
        assert_eq!(items[2].item_id, "333");
        assert_eq!(items[2].name.as_deref(), Some("My Mod"));
        assert!(!items[3].is_resolvable());
        assert_eq!(items[3].name.as_deref(), Some("Only Local"));
    }

    #[test]
    fn parses_urls_and_deduplicates() {
        let items = parse_share_code(
            "https://steamcommunity.com/sharedfiles/filedetails/?id=111\n111, 222",
        );

        assert_eq!(
            items
                .iter()
                .map(|item| item.item_id.as_str())
                .collect::<Vec<_>>(),
            vec!["111", "222"]
        );
    }

    #[test]
    fn renders_plain_and_extended_codes() {
        let items = vec![
            SharedItem {
                item_id: "111".to_string(),
                name: Some("Alpha".to_string()),
                load_order: Some(1),
                version: Some("abc".to_string()),
            },
            SharedItem {
                item_id: String::new(),
                name: Some("Local Mod".to_string()),
                load_order: Some(2),
                version: None,
            },
        ];

        assert_eq!(
            render_share_code(&items, ShareCodeOptions::default()),
            "111|local:Local%20Mod"
        );
        assert_eq!(
            render_share_code(
                &items,
                ShareCodeOptions {
                    include_load_order: true,
                    include_versions: true,
                }
            ),
            "111@abc;1|local:Local%20Mod;2"
        );
    }

    #[test]
    fn compare_reports_missing_extra_unresolvable_and_order() {
        let local = parse_share_code("111|222|333");
        let remote = parse_share_code("222|111|444|local:Hand%20Made");

        let comparison = compare_share_lists(&local, &remote);

        assert!(!comparison.matches());
        assert_eq!(
            comparison
                .missing
                .iter()
                .map(|item| item.item_id.as_str())
                .collect::<Vec<_>>(),
            vec!["444"]
        );
        assert_eq!(
            comparison
                .extra
                .iter()
                .map(|item| item.item_id.as_str())
                .collect::<Vec<_>>(),
            vec!["333"]
        );
        assert_eq!(comparison.unresolvable.len(), 1);
        assert!(comparison.order_differs);
    }

    #[test]
    fn compare_matches_identical_lists() {
        let local = parse_share_code("111|222");
        let remote = parse_share_code("111|222");

        assert!(compare_share_lists(&local, &remote).matches());
    }

    #[test]
    fn share_code_round_trips_through_parse() {
        let options = ShareCodeOptions {
            include_load_order: true,
            include_versions: true,
        };
        let items = vec![SharedItem {
            item_id: "2790007728".to_string(),
            name: None,
            load_order: Some(7),
            version: Some("deadbeef".to_string()),
        }];

        assert_eq!(parse_share_code(&render_share_code(&items, options)), items);
    }
}
