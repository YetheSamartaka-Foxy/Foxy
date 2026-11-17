use super::store;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::content_staging::{ContentStageControl, StageEntryKind, parse_relative};
use super::{ServerDefinitionKind, ServerPlatform, ServerStoreError};

const MAX_ENTRIES: usize = 100_000;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 8192;
const BUFFER_SIZE: usize = 64 * 1024;
const MAX_PATH_BYTES_TOTAL: usize = 32 * 1024 * 1024;
const DIRECTORIES: &[&str] = &[
    "addons",
    "dta",
    "battleye",
    "dll",
    "linux64",
    "keys",
    "expansion",
    "aow",
    "argo",
    "curator",
    "enoch",
    "heli",
    "jets",
    "kart",
    "mark",
    "orange",
    "tacops",
    "tank",
];
const REFORGER_DIRECTORIES: &[&str] = &["addons", "battleye"];

mod copy;
mod inventory;
#[cfg(test)]
mod tests;
mod validation;

pub(super) use copy::copy;
pub(super) use inventory::inventory;
use validation::{
    checked_dir, checked_file, copy_file, digest, executable, open_read, validate_inventory,
};

fn selected_directories(game: ServerDefinitionKind) -> &'static [&'static str] {
    match game {
        ServerDefinitionKind::Arma3 => DIRECTORIES,
        ServerDefinitionKind::Reforger => REFORGER_DIRECTORIES,
    }
}

fn is_linux_library(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let Some((_, version)) = lower.rsplit_once(".so") else {
        return false;
    };
    version.is_empty()
        || (version.starts_with('.')
            && version[1..]
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadEntry {
    pub path: String,
    pub kind: StageEntryKind,
    pub bytes: u64,
    pub blake3: Option<String>,
    pub executable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadInventory {
    pub platform: ServerPlatform,
    pub entries: Vec<PayloadEntry>,
    pub total_bytes: u64,
}
