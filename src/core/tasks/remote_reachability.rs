use std::time::Duration;

use log::warn;
use reqwest::Client;
use serde_json::Value;

use crate::core::models::repository::normalize_repository_url;
use crate::core::utils::format::sanitize_log_url;

const REACHABILITY_ATTEMPTS: u32 = 2;
const REACHABILITY_RETRY_DELAY: Duration = Duration::from_millis(750);
const REACHABILITY_TIMEOUT: Duration = Duration::from_secs(20);

/// Confirm the repository's `repo.json` is served right now before a destructive
/// operation (force redownload of a repository or an addon) removes local data.
/// The probe is bounded so an unreachable server fails within seconds instead of
/// sitting in the long retry loop of the regular manifest fetch, and it demands
/// a parseable JSON object so a captive portal or a mis-pointed URL cannot pass.
pub(crate) async fn ensure_remote_repository_reachable(
    client: &Client,
    repository_url: &str,
) -> Result<(), String> {
    let manifest_url = format!("{}repo.json", normalize_repository_url(repository_url));
    let mut last_error = String::new();
    for attempt in 1..=REACHABILITY_ATTEMPTS {
        if attempt > 1 {
            tokio::time::sleep(REACHABILITY_RETRY_DELAY).await;
        }
        match probe_manifest(client, &manifest_url).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                warn!(
                    "Remote reachability probe {}/{} failed for {}: {}",
                    attempt,
                    REACHABILITY_ATTEMPTS,
                    sanitize_log_url(&manifest_url),
                    err
                );
                last_error = err;
            }
        }
    }
    Err(format!(
        "Repository is not reachable ({}): {}",
        sanitize_log_url(&manifest_url),
        last_error
    ))
}

async fn probe_manifest(client: &Client, manifest_url: &str) -> Result<(), String> {
    let response = tokio::time::timeout(REACHABILITY_TIMEOUT, client.get(manifest_url).send())
        .await
        .map_err(|_| format!("timed out after {:?}", REACHABILITY_TIMEOUT))?
        .map_err(describe_request_error)?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status));
    }
    let body = tokio::time::timeout(REACHABILITY_TIMEOUT, response.bytes())
        .await
        .map_err(|_| format!("body read timed out after {:?}", REACHABILITY_TIMEOUT))?
        .map_err(describe_request_error)?;
    manifest_body_is_repository(&body)
}

/// `reqwest::Error`'s `Display` stops at "error sending request"; the cause the
/// user needs (connection refused, DNS failure, timeout) sits in the source chain.
fn describe_request_error(err: reqwest::Error) -> String {
    let err = err.without_url();
    let mut text = err.to_string();
    let mut source = std::error::Error::source(&err);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// A reachable repository answers with a JSON object; anything else (an HTML
/// login page, an empty file, a directory listing) means the URL is not
/// serving a repository even though the host answered.
fn manifest_body_is_repository(body: &[u8]) -> Result<(), String> {
    let text = std::str::from_utf8(body).map_err(|_| "manifest is not valid UTF-8".to_string())?;
    let trimmed = text.trim_start_matches('\u{feff}').trim();
    match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Object(_)) => Ok(()),
        Ok(_) => Err("manifest is not a JSON object".to_string()),
        Err(err) => Err(format!("manifest is not valid JSON: {}", err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_json_object_manifest() {
        assert!(manifest_body_is_repository(br#"{"repoName":"x","checksum":"abc"}"#).is_ok());
    }

    #[test]
    fn accepts_bom_prefixed_manifest() {
        let body = "\u{feff}{\"checksum\":\"abc\"}";
        assert!(manifest_body_is_repository(body.as_bytes()).is_ok());
    }

    #[test]
    fn rejects_html_login_page() {
        let err = manifest_body_is_repository(b"<html><body>Sign in</body></html>").unwrap_err();
        assert!(err.contains("not valid JSON"), "{err}");
    }

    #[test]
    fn rejects_empty_body() {
        assert!(manifest_body_is_repository(b"").is_err());
    }

    #[test]
    fn rejects_non_object_json() {
        let err = manifest_body_is_repository(b"[1,2,3]").unwrap_err();
        assert!(err.contains("not a JSON object"), "{err}");
    }

    #[tokio::test]
    async fn unreachable_host_fails_quickly_with_context() {
        let client = Client::builder()
            .connect_timeout(Duration::from_millis(500))
            .build()
            .expect("client");
        let started = std::time::Instant::now();
        let err = ensure_remote_repository_reachable(&client, "http://127.0.0.1:9/repo")
            .await
            .unwrap_err();
        assert!(err.contains("not reachable"), "{err}");
        assert!(err.contains("127.0.0.1:9/repo/repo.json"), "{err}");
        assert!(
            err.contains("error sending request: "),
            "cause chain missing: {err}"
        );
        assert!(started.elapsed() < Duration::from_secs(15));
    }
}
