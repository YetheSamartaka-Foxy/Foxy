//! Mod manifests kept beside `database.db` with the validators the server
//! sent, so a check after a database wipe asks the server whether each
//! manifest changed instead of downloading all of them again. The server
//! answers every use, so a cached body is never served stale.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const CACHE_DIR: &str = "manifest_cache";
/// Entries the server has not confirmed or replaced for this long are dropped.
const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(180 * 24 * 60 * 60);

/// What the server sent to identify one version of a manifest.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Validators {
    pub(crate) etag: Option<String>,
    pub(crate) last_modified: Option<String>,
}

impl Validators {
    pub(crate) fn from_headers(headers: &reqwest::header::HeaderMap) -> Self {
        let text = |name: reqwest::header::HeaderName| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        Self {
            etag: text(reqwest::header::ETAG),
            last_modified: text(reqwest::header::LAST_MODIFIED),
        }
    }

    fn is_empty(&self) -> bool {
        self.etag.is_none() && self.last_modified.is_none()
    }
}

#[derive(Serialize, Deserialize)]
struct Meta {
    url: String,
    validators: Validators,
}

#[derive(Clone, Debug)]
pub(crate) struct ManifestCache {
    dir: PathBuf,
}

impl ManifestCache {
    pub(crate) fn in_space(space_dir: &Path) -> Self {
        Self {
            dir: space_dir.join(CACHE_DIR),
        }
    }

    fn paths(&self, url: &str) -> (PathBuf, PathBuf) {
        let key = blake3::hash(url.as_bytes()).to_hex();
        let key = &key.as_str()[..32];
        (
            self.dir.join(format!("{key}.meta")),
            self.dir.join(format!("{key}.body")),
        )
    }

    /// The validators stored for `url`, when a body for it is on disk.
    pub(crate) fn validators(&self, url: &str) -> Option<Validators> {
        let (meta_path, body_path) = self.paths(url);
        let meta: Meta = serde_json::from_slice(&std::fs::read(meta_path).ok()?).ok()?;
        (meta.url == url && !meta.validators.is_empty() && body_path.is_file())
            .then_some(meta.validators)
    }

    /// The cached body for `url`, marking the entry as still in use.
    pub(crate) fn body(&self, url: &str) -> Option<Vec<u8>> {
        let (meta_path, body_path) = self.paths(url);
        let body = std::fs::read(body_path).ok()?;
        if let Ok(meta) = std::fs::File::options().write(true).open(meta_path) {
            let _ = meta.set_modified(std::time::SystemTime::now());
        }
        Some(body)
    }

    /// Drop entries unused for [`STALE_AFTER`] and bodies no entry points at.
    pub(crate) fn prune(&self) -> usize {
        self.prune_older_than(std::time::SystemTime::now() - STALE_AFTER)
    }

    fn prune_older_than(&self, cutoff: std::time::SystemTime) -> usize {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return 0;
        };
        let mut dropped = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            let modified = entry.metadata().and_then(|meta| meta.modified()).ok();
            let stale = modified.is_none_or(|modified| modified < cutoff);
            let is_meta = path.extension().is_some_and(|ext| ext == "meta");
            let orphan_body = !is_meta && !path.with_extension("meta").is_file();
            if stale && (is_meta || orphan_body) {
                let _ = std::fs::remove_file(&path);
                if is_meta {
                    let _ = std::fs::remove_file(path.with_extension("body"));
                }
                dropped += 1;
            }
        }
        dropped
    }

    /// Drop the entry for `url`, so the next request downloads it in full.
    pub(crate) fn forget(&self, url: &str) {
        let _ = std::fs::remove_file(self.paths(url).0);
    }

    /// Replace the entry for `url`. The body lands before the validators that
    /// point at it, so a crash between the two leaves an entry that only ever
    /// costs a full download.
    pub(crate) fn store(
        &self,
        url: &str,
        validators: &Validators,
        body: &[u8],
    ) -> std::io::Result<()> {
        if validators.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir)?;
        let (meta_path, body_path) = self.paths(url);
        let _ = std::fs::remove_file(&meta_path);
        write_replacing(&body_path, body)?;
        let meta = serde_json::to_vec(&Meta {
            url: url.to_owned(),
            validators: validators.clone(),
        })
        .map_err(std::io::Error::other)?;
        write_replacing(&meta_path, &meta)
    }
}

fn write_replacing(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validators(etag: &str) -> Validators {
        Validators {
            etag: Some(etag.to_owned()),
            last_modified: None,
        }
    }

    #[test]
    fn a_stored_manifest_comes_back_with_its_validators() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ManifestCache::in_space(dir.path());
        assert!(cache.validators("u/a").is_none());
        cache.store("u/a", &validators("\"1\""), b"{}").unwrap();
        assert_eq!(cache.validators("u/a"), Some(validators("\"1\"")));
        assert_eq!(cache.body("u/a").unwrap(), b"{}");
        assert!(cache.validators("u/b").is_none());

        cache.store("u/a", &validators("\"2\""), b"[1]").unwrap();
        assert_eq!(cache.validators("u/a"), Some(validators("\"2\"")));
        assert_eq!(cache.body("u/a").unwrap(), b"[1]");
    }

    #[test]
    fn a_response_without_validators_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ManifestCache::in_space(dir.path());
        cache.store("u/a", &Validators::default(), b"{}").unwrap();
        assert!(cache.validators("u/a").is_none());
    }

    #[test]
    fn pruning_drops_unused_entries_and_orphan_bodies_only() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ManifestCache::in_space(dir.path());
        cache.store("u/old", &validators("\"1\""), b"{}").unwrap();
        cache.store("u/kept", &validators("\"1\""), b"{}").unwrap();
        cache.forget("u/kept");
        cache.store("u/kept", &validators("\"2\""), b"{}").unwrap();
        cache.store("u/gone", &validators("\"1\""), b"{}").unwrap();
        cache.forget("u/gone");
        let cutoff = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        let old_meta = cache.paths("u/old").0;
        std::fs::File::options()
            .write(true)
            .open(&old_meta)
            .unwrap()
            .set_modified(cutoff - std::time::Duration::from_secs(120))
            .unwrap();
        std::fs::File::options()
            .write(true)
            .open(cache.paths("u/kept").0)
            .unwrap()
            .set_modified(cutoff + std::time::Duration::from_secs(60))
            .unwrap();
        assert_eq!(cache.prune_older_than(cutoff), 2);
        assert!(cache.validators("u/old").is_none());
        assert!(!cache.paths("u/old").1.exists());
        assert!(!cache.paths("u/gone").1.exists());
        assert!(cache.validators("u/kept").is_some());
    }

    #[test]
    fn an_entry_whose_body_is_gone_is_not_offered() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ManifestCache::in_space(dir.path());
        cache.store("u/a", &validators("\"1\""), b"{}").unwrap();
        std::fs::remove_file(cache.paths("u/a").1).unwrap();
        assert!(cache.validators("u/a").is_none());
    }
}
