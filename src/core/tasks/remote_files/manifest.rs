use super::{FetchJsonTiming, FoxyContext, ModManifest};
use crate::core::utils::fetch_json::{fetch_manifest_json_timed, fetch_manifest_json_timed_fresh};
use std::sync::Arc;

type ManifestResult =
    Result<(ModManifest, FetchJsonTiming), Box<dyn std::error::Error + Send + Sync>>;

pub(super) async fn fetch_validated_manifest(
    context: Arc<FoxyContext>,
    url: &str,
    fresh: bool,
) -> ManifestResult {
    let (manifest, timing) = if fresh {
        fetch_manifest_json_timed_fresh(context.clone(), url).await?
    } else {
        fetch_manifest_json_timed(context.clone(), url).await?
    };
    match validated(manifest).await {
        Ok(manifest) => Ok((manifest, timing)),
        Err(error) if !fresh => {
            log::warn!(
                "Addon manifest has inconsistent file-part metadata; retrying without cached validators: {}",
                error
            );
            let (manifest, timing) = fetch_manifest_json_timed_fresh(context, url).await?;
            Ok((validated(manifest).await?, timing))
        }
        Err(error) => Err(error),
    }
}

async fn validated(
    manifest: ModManifest,
) -> Result<ModManifest, Box<dyn std::error::Error + Send + Sync>> {
    tokio::task::spawn_blocking(move || {
        validate(&manifest)?;
        Ok(manifest)
    })
    .await?
}

fn validate(manifest: &ModManifest) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for (index, file) in manifest.files.iter().enumerate() {
        if file.length < 0 {
            return Err(format!("Addon manifest file {} has a negative length", index + 1).into());
        }
        if file.parts.is_empty() {
            continue;
        }
        let mut cursor = 0i64;
        for part in &file.parts {
            if part.start != cursor || part.length < 0 {
                return Err(format!(
                    "Addon manifest file {} has invalid part coverage: expected start {}, got start {} and length {}",
                    index + 1, cursor, part.start, part.length
                ).into());
            }
            cursor = cursor
                .checked_add(part.length)
                .ok_or("Addon manifest part span overflow")?;
        }
        if cursor != file.length {
            return Err(format!(
                "Addon manifest file {} has inconsistent part coverage: expected {} bytes, got {}. The server metadata must be republished if a fresh refresh still fails.",
                index + 1, file.length, cursor
            ).into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest(length: i64, parts: &[(i64, i64)]) -> ModManifest {
        serde_json::from_value(json!({"files": [{
            "path": "addons/file.pbo", "checksum": "FILE", "length": length,
            "parts": parts.iter().map(|(start, length)| json!({
                "path": "entry", "checksum": "PART", "start": start, "length": length
            })).collect::<Vec<_>>()
        }]}))
        .unwrap()
    }

    #[test]
    fn part_coverage_accepts_empty_entries_and_whole_file_manifests() {
        assert!(validate(&manifest(10, &[(0, 4), (4, 0), (4, 6)])).is_ok());
        assert!(validate(&manifest(10, &[])).is_ok());
        assert!(validate(&manifest(0, &[(0, 0)])).is_ok());
    }

    #[test]
    fn part_coverage_rejects_both_reported_layout_lengths() {
        assert!(validate(&manifest(10285, &[(0, 5125)])).is_err());
        assert!(validate(&manifest(40077, &[(0, 40583)])).is_err());
    }

    #[test]
    fn part_coverage_rejects_gaps_overlaps_negative_spans_and_overflow() {
        for (length, parts) in [
            (10, vec![(1, 9)]),
            (10, vec![(0, 4), (5, 5)]),
            (10, vec![(0, 4), (3, 7)]),
            (10, vec![(0, -1)]),
            (-1, vec![]),
            (i64::MAX, vec![(0, i64::MAX), (i64::MAX, 1)]),
        ] {
            assert!(validate(&manifest(length, &parts)).is_err());
        }
    }

    async fn assert_cached_layout_recovery(server_repairs_layout: bool) {
        use crate::core::utils::manifest_cache::{ManifestCache, Validators};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/@mod_01/mod.srf", listener.local_addr().unwrap());
        let invalid = r#"{"files":[{"path":"file.bin","checksum":"FILE","length":4,"parts":[{"path":"entry","checksum":"PART","start":0,"length":5}]}]}"#;
        let valid = invalid.replace("\"length\":5", "\"length\":4");
        let server = tokio::spawn(async move {
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let length = stream.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..length]).to_ascii_lowercase();
                let (status, body) = if attempt == 0 {
                    assert!(request.contains("if-none-match:"));
                    ("304 Not Modified", "")
                } else {
                    assert!(!request.contains("if-none-match:"));
                    assert!(request.contains("cache-control: no-cache"));
                    (
                        "200 OK",
                        if server_repairs_layout {
                            valid.as_str()
                        } else {
                            invalid
                        },
                    )
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let directory = tempfile::tempdir().unwrap();
        let cache = ManifestCache::in_space(directory.path());
        cache
            .store(
                &url,
                &Validators {
                    etag: Some("\"stale\"".into()),
                    last_modified: None,
                },
                invalid.as_bytes(),
            )
            .unwrap();
        let context = Arc::new(
            FoxyContext::new(
                crate::core::tasks::db_turso::build_test_database().await,
                reqwest::Client::new(),
            )
            .with_manifest_cache(cache),
        );
        let result = fetch_validated_manifest(context, &url, false).await;
        if server_repairs_layout {
            let (manifest, timing) = result.unwrap();
            assert!(!timing.cached);
            assert_eq!(manifest.files[0].parts[0].length, 4);
        } else {
            assert!(
                result
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("inconsistent part coverage")
            );
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn invalid_cached_layout_is_refetched_without_validators() {
        assert_cached_layout_recovery(true).await;
    }

    #[tokio::test]
    async fn invalid_fresh_layout_is_rejected_before_application() {
        assert_cached_layout_recovery(false).await;
    }
}
