use crate::core::models::context::FoxyContext;
use crate::core::utils::manifest_cache::{ManifestCache, Validators};
use log::{debug, warn};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

const FETCH_JSON_MAX_RETRIES: u32 = 3;
const FETCH_JSON_BASE_DELAY_MS: u64 = 500;
const FETCH_JSON_TIMEOUT: Duration = Duration::from_secs(120);
/// Maximum decompressed response body size (200 MB). Protects against truncated
/// gzip streams or malformed servers sending unbounded data.
const FETCH_JSON_MAX_BODY_SIZE: usize = 200 * 1024 * 1024;

#[derive(Debug)]
struct HttpStatusError {
    status: reqwest::StatusCode,
    url: String,
}

impl fmt::Display for HttpStatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "JSON fetch for {} failed with status {}",
            self.url, self.status
        )
    }
}

impl Error for HttpStatusError {}

#[derive(Debug)]
struct FetchTimeoutError(String);

impl fmt::Display for FetchTimeoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for FetchTimeoutError {}

fn is_transient_metadata_error(error: &(dyn Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if error
            .downcast_ref::<HttpStatusError>()
            .is_some_and(|status| {
                status.status == reqwest::StatusCode::REQUEST_TIMEOUT
                    || status.status == reqwest::StatusCode::TOO_MANY_REQUESTS
                    || status.status.is_server_error()
            })
            || error.downcast_ref::<FetchTimeoutError>().is_some()
        {
            return true;
        }
        if error
            .downcast_ref::<reqwest::Error>()
            .is_some_and(|error| error.is_connect() || error.is_timeout() || error.is_body())
        {
            return true;
        }
        current = error.source();
    }
    false
}

/// Timing breakdown for a JSON metadata fetch.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FetchJsonTiming {
    /// Time to send the HTTP request and read the full response body.
    pub download: Duration,
    /// Size of the response body in bytes.
    pub response_bytes: usize,
    /// Time to skip leading non-JSON bytes and parse into the target type.
    pub parse: Duration,
    /// The server confirmed the cached body; nothing was downloaded.
    pub cached: bool,
}

pub(crate) async fn fetch_json_fresh(
    context: Arc<FoxyContext>,
    url: &str,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let (value, _) = fetch_json_timed_with_cache(context, url, None, true).await?;
    Ok(value)
}

/// Revalidate an addon manifest against the context's manifest cache.
pub(crate) async fn fetch_manifest_json_timed<T: DeserializeOwned + Send + 'static>(
    context: Arc<FoxyContext>,
    url: &str,
) -> Result<(T, FetchJsonTiming), Box<dyn std::error::Error + Send + Sync>> {
    let cache = context.manifest_cache.clone();
    if let Some(cache) = cache.clone() {
        static PRUNED: std::sync::Once = std::sync::Once::new();
        PRUNED.call_once(|| {
            tokio::task::spawn_blocking(move || {
                let dropped = cache.prune();
                if dropped > 0 {
                    debug!("Manifest cache dropped {dropped} unused files");
                }
            });
        });
    }
    fetch_json_timed_with_cache(context, url, cache.as_deref(), false).await
}

/// Recovery must not recreate a bad graph from a stale HTTP 304 response.
pub(crate) async fn fetch_manifest_json_timed_fresh<T: DeserializeOwned + Send + 'static>(
    context: Arc<FoxyContext>,
    url: &str,
) -> Result<(T, FetchJsonTiming), Box<dyn std::error::Error + Send + Sync>> {
    let cache = context.manifest_cache.clone();
    fetch_json_timed_with_cache(context, url, cache.as_deref(), true).await
}

async fn fetch_json_timed_with_cache<T: DeserializeOwned + Send + 'static>(
    context: Arc<FoxyContext>,
    url: &str,
    cache: Option<&ManifestCache>,
    fresh: bool,
) -> Result<(T, FetchJsonTiming), Box<dyn std::error::Error + Send + Sync>> {
    debug!("Fetching JSON from {}", url);

    let mut last_error: Option<Box<dyn std::error::Error + Send + Sync>> = None;
    for attempt in 0..=FETCH_JSON_MAX_RETRIES {
        if attempt > 0 {
            let delay = FETCH_JSON_BASE_DELAY_MS * (1 << (attempt - 1).min(3));
            warn!(
                "Retrying JSON fetch from {} (attempt {}/{}) after {}ms",
                url,
                attempt + 1,
                FETCH_JSON_MAX_RETRIES + 1,
                delay
            );
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }

        match fetch_json_single(&context, url, cache, fresh).await {
            Ok(result) => return Ok(result),
            Err(err) => {
                warn!(
                    "JSON fetch attempt {} for {} failed: {}",
                    attempt + 1,
                    url,
                    err
                );
                last_error = Some(err);
            }
        }
    }

    let error = last_error.unwrap_or_else(|| "fetch_json: all retries exhausted".into());
    if is_transient_metadata_error(error.as_ref()) {
        context.mark_transient_metadata_failure();
    }
    Err(error)
}

async fn fetch_json_single<T: DeserializeOwned + Send + 'static>(
    context: &FoxyContext,
    url: &str,
    cache: Option<&ManifestCache>,
    fresh: bool,
) -> Result<(T, FetchJsonTiming), Box<dyn std::error::Error + Send + Sync>> {
    let download_start = Instant::now();
    let cached_validators = match (cache, fresh) {
        (_, true) => None,
        (Some(cache), false) => {
            let (cache, key) = (cache.clone(), url.to_owned());
            tokio::task::spawn_blocking(move || cache.validators(&key))
                .await
                .ok()
                .flatten()
        }
        (None, false) => None,
    };
    let mut request = context.client.get(url);
    if fresh {
        request = request.header(reqwest::header::CACHE_CONTROL, "no-cache");
    }
    if let Some(validators) = cached_validators.as_ref() {
        if let Some(etag) = validators.etag.as_deref() {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        if let Some(modified) = validators.last_modified.as_deref() {
            request = request.header(reqwest::header::IF_MODIFIED_SINCE, modified);
        }
    }
    let resp = tokio::time::timeout(FETCH_JSON_TIMEOUT, request.send())
        .await
        .map_err(|_| {
            FetchTimeoutError(format!(
                "JSON fetch timed out after {:?} for {}",
                FETCH_JSON_TIMEOUT, url
            ))
        })??;
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED
        && let Some(cache) = cache
        && cached_validators.is_some()
    {
        let (reader, key) = (cache.clone(), url.to_owned());
        let body = tokio::task::spawn_blocking(move || reader.body(&key))
            .await?
            .ok_or_else(|| format!("Cached manifest body for {url} is gone"))?;
        let download = download_start.elapsed();
        let (value, parse) = match parse_json_body(url, body).await {
            Ok(parsed) => parsed,
            Err(err) => {
                cache.forget(url);
                return Err(err);
            }
        };
        return Ok((
            value,
            FetchJsonTiming {
                download,
                response_bytes: 0,
                parse,
                cached: true,
            },
        ));
    }
    if !resp.status().is_success() {
        return Err(HttpStatusError {
            status: resp.status(),
            url: url.to_owned(),
        }
        .into());
    }
    let content_encoding = resp
        .headers()
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("identity")
        .to_owned();
    let expected_bytes = resp.content_length();
    let validators = Validators::from_headers(resp.headers());
    let body = tokio::time::timeout(FETCH_JSON_TIMEOUT, resp.bytes())
        .await
        .map_err(|_| {
            FetchTimeoutError(format!(
                "JSON body read timed out after {:?} for {}",
                FETCH_JSON_TIMEOUT, url
            ))
        })??;
    let download = download_start.elapsed();
    let response_bytes = body.len();

    if response_bytes > FETCH_JSON_MAX_BODY_SIZE {
        return Err(format!(
            "JSON response from {} exceeds max body size ({} bytes > {} limit)",
            url, response_bytes, FETCH_JSON_MAX_BODY_SIZE
        )
        .into());
    }

    // Validate Content-Length against the actual bytes received.
    if content_encoding == "identity"
        && let Some(expected) = expected_bytes
        && response_bytes != expected as usize
    {
        return Err(format!(
            "JSON response from {} size mismatch: Content-Length={} but received {} bytes",
            url, expected, response_bytes
        )
        .into());
    }

    debug!(
        "Fetched response body for {} ({} bytes, encoding={}, download={:.0?})",
        url, response_bytes, content_encoding, download
    );

    let (data, parse) = parse_json_body(url, body.clone()).await?;
    if let Some(cache) = cache {
        let (cache, key) = (cache.clone(), url.to_owned());
        tokio::task::spawn_blocking(move || {
            if let Err(err) = cache.store(&key, &validators, &body) {
                warn!("Manifest cache could not keep {key}: {err}");
            }
        })
        .await?;
    }

    Ok((
        data,
        FetchJsonTiming {
            download,
            response_bytes,
            parse,
            cached: false,
        },
    ))
}

/// Skip a BOM or other leading bytes, then parse; returns the parse time.
async fn parse_json_body<T, B>(
    url: &str,
    body: B,
) -> Result<(T, Duration), Box<dyn std::error::Error + Send + Sync>>
where
    T: DeserializeOwned + Send + 'static,
    B: AsRef<[u8]> + Send + 'static,
{
    let parse_start = Instant::now();
    let start = json_start(body.as_ref());
    if start > 0 {
        warn!(
            "Stripped {} leading non-JSON bytes before parsing response from {}",
            start, url
        );
    }
    let owned_url = url.to_owned();
    let data =
        tokio::task::spawn_blocking(move || parse_json_slice(&owned_url, &body.as_ref()[start..]))
            .await??;
    let parse = parse_start.elapsed();
    debug!("Parsed JSON payload from {} (parse={:.0?})", url, parse);
    Ok((data, parse))
}

fn json_start(body: &[u8]) -> usize {
    body.iter()
        .position(|&byte| byte == b'{' || byte == b'[' || byte.is_ascii_whitespace())
        .unwrap_or(body.len())
}

/// Line breaks are dropped before a retry because some servers emit raw ones
/// inside strings; outside strings they are whitespace, so a clean body parses
/// the same either way.
fn parse_json_slice<T: DeserializeOwned>(
    url: &str,
    bytes: &[u8],
) -> Result<T, Box<dyn std::error::Error + Send + Sync>> {
    match serde_json::from_slice(bytes) {
        Ok(data) => Ok(data),
        Err(err) if !bytes.iter().any(|&b| b == b'\r' || b == b'\n') => Err(err.into()),
        Err(_) => {
            let text = std::str::from_utf8(bytes)
                .map_err(|e| format!("Response from {} was not valid UTF-8: {}", url, e))?;
            Ok(serde_json::from_str(&text.replace(['\r', '\n'], ""))?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn only_retryable_http_statuses_and_fetch_timeouts_are_transient() {
        for status in [
            reqwest::StatusCode::REQUEST_TIMEOUT,
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            reqwest::StatusCode::from_u16(599).unwrap(),
        ] {
            assert!(is_transient_metadata_error(&HttpStatusError {
                status,
                url: "http://repo.example.invalid/".to_owned(),
            }));
        }
        for status in [
            reqwest::StatusCode::OK,
            reqwest::StatusCode::BAD_REQUEST,
            reqwest::StatusCode::NOT_FOUND,
        ] {
            assert!(!is_transient_metadata_error(&HttpStatusError {
                status,
                url: "http://repo.example.invalid/".to_owned(),
            }));
        }
        assert!(is_transient_metadata_error(&FetchTimeoutError(
            "timed out".to_owned()
        )));
        assert!(!is_transient_metadata_error(
            &serde_json::from_str::<Value>("not json").unwrap_err()
        ));
    }

    /// Serves one JSON body with an ETag and answers a matching
    /// `If-None-Match` with 304; counts the full bodies it sent.
    fn spawn_manifest_server(body: &'static str) -> (String, Arc<AtomicUsize>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/@mod/foxy_addon.json",
            listener.local_addr().unwrap()
        );
        let full = Arc::new(AtomicUsize::new(0));
        let counter = full.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let counter = counter.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        let mut matches = false;
                        loop {
                            let mut header = String::new();
                            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                                return;
                            }
                            let header = header.trim();
                            if header.is_empty() {
                                break;
                            }
                            if let Some((name, value)) = header.split_once(": ")
                                && name.eq_ignore_ascii_case("if-none-match")
                            {
                                matches = value == "\"v1\"";
                            }
                        }
                        let response = if matches {
                            "HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\nContent-Length: 0\r\n\r\n"
                                .to_owned()
                        } else {
                            counter.fetch_add(1, Ordering::SeqCst);
                            format!(
                                "HTTP/1.1 200 OK\r\nETag: \"v1\"\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                                body.len()
                            )
                        };
                        if stream.write_all(response.as_bytes()).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (url, full)
    }

    fn spawn_fresh_manifest_server(with_validators: bool) -> (String, [Arc<AtomicUsize>; 4]) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/manifest.json", listener.local_addr().unwrap());
        let old_validator_requests = Arc::new(AtomicUsize::new(0));
        let current_validator_requests = Arc::new(AtomicUsize::new(0));
        let no_cache_requests = Arc::new(AtomicUsize::new(0));
        let fresh_conditional_requests = Arc::new(AtomicUsize::new(0));
        let stats = [
            old_validator_requests.clone(),
            current_validator_requests.clone(),
            no_cache_requests.clone(),
            fresh_conditional_requests.clone(),
        ];
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let old_validator_requests = old_validator_requests.clone();
                let current_validator_requests = current_validator_requests.clone();
                let no_cache_requests = no_cache_requests.clone();
                let fresh_conditional_requests = fresh_conditional_requests.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        let mut validator = None;
                        let mut no_cache = false;
                        loop {
                            let mut header = String::new();
                            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                                return;
                            }
                            let header = header.trim();
                            if header.is_empty() {
                                break;
                            }
                            if let Some((name, value)) = header.split_once(": ") {
                                if name.eq_ignore_ascii_case("if-none-match") {
                                    validator = Some(value.to_owned());
                                }
                                if name.eq_ignore_ascii_case("if-modified-since") {
                                    validator = Some(value.to_owned());
                                }
                                no_cache |= name.eq_ignore_ascii_case("cache-control")
                                    && value.eq_ignore_ascii_case("no-cache");
                            }
                        }
                        if no_cache {
                            no_cache_requests.fetch_add(1, Ordering::SeqCst);
                            if validator.is_some() {
                                fresh_conditional_requests.fetch_add(1, Ordering::SeqCst);
                            }
                        }
                        let response = match validator.as_deref() {
                            Some("\"v1\"") => {
                                old_validator_requests.fetch_add(1, Ordering::SeqCst);
                                "HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\nContent-Length: 0\r\n\r\n".to_owned()
                            }
                            Some("\"v2\"") => {
                                current_validator_requests.fetch_add(1, Ordering::SeqCst);
                                "HTTP/1.1 304 Not Modified\r\nETag: \"v2\"\r\nContent-Length: 0\r\n\r\n".to_owned()
                            }
                            _ => {
                                let body = r#"{"version":2}"#;
                                let etag = if with_validators {
                                    "ETag: \"v2\"\r\n"
                                } else {
                                    ""
                                };
                                format!(
                                    "HTTP/1.1 200 OK\r\n{etag}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                                    body.len()
                                )
                            }
                        };
                        if stream.write_all(response.as_bytes()).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (url, stats)
    }

    #[test]
    fn a_body_parses_after_leading_bytes_and_line_breaks() {
        let body = b"\xEF\xBB\xBF{\r\n  \"files\": [1, 2]\r\n}\r\n";
        let parsed: Value = parse_json_slice("u", &body[json_start(body)..]).unwrap();
        assert_eq!(parsed, serde_json::json!({"files": [1, 2]}));
    }

    #[test]
    fn a_raw_line_break_inside_a_string_is_dropped() {
        let parsed: Value = parse_json_slice("u", b"{\"path\": \"a\r\nb\"}").unwrap();
        assert_eq!(parsed, serde_json::json!({"path": "ab"}));
    }

    #[test]
    fn a_body_that_is_not_json_is_an_error() {
        assert!(parse_json_slice::<Value>("u", b"{not json").is_err());
        assert!(parse_json_slice::<Value>("u", b"{not\njson").is_err());
        assert_eq!(json_start(b"abc"), 3);
    }

    async fn stored(cache: &ManifestCache, url: &str) {
        for _ in 0..200 {
            if cache.validators(url).is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("the manifest was never cached");
    }

    #[tokio::test]
    async fn a_manifest_the_server_confirms_is_read_from_the_cache() {
        let (url, full) = spawn_manifest_server(r#"{"files":[1]}"#);
        let space = tempfile::tempdir().unwrap();
        let context = Arc::new(
            FoxyContext::new(
                crate::core::tasks::db_turso::build_test_database().await,
                reqwest::Client::new(),
            )
            .with_manifest_cache(ManifestCache::in_space(space.path())),
        );
        let cache = context.manifest_cache.clone().unwrap();

        let (first, timing) = fetch_manifest_json_timed::<Value>(context.clone(), &url)
            .await
            .unwrap();
        assert!(!timing.cached);
        stored(&cache, &url).await;

        let (second, timing) = fetch_manifest_json_timed::<Value>(context.clone(), &url)
            .await
            .unwrap();
        assert!(timing.cached);
        assert_eq!(first, second);
        assert_eq!(full.load(Ordering::SeqCst), 1);

        cache
            .store(&url, &cache.validators(&url).unwrap(), b"{not json")
            .unwrap();
        let (third, timing) = fetch_manifest_json_timed::<Value>(context, &url)
            .await
            .unwrap();
        assert!(!timing.cached);
        assert_eq!(third, first);
        assert_eq!(full.load(Ordering::SeqCst), 2);
    }

    async fn assert_fresh_manifest_fetch(with_validators: bool) {
        let (url, stats) = spawn_fresh_manifest_server(with_validators);
        let space = tempfile::tempdir().unwrap();
        let context = Arc::new(
            FoxyContext::new(
                crate::core::tasks::db_turso::build_test_database().await,
                reqwest::Client::new(),
            )
            .with_manifest_cache(ManifestCache::in_space(space.path())),
        );
        let cache = context.manifest_cache.clone().unwrap();
        cache
            .store(
                &url,
                &Validators {
                    etag: Some("\"v1\"".to_owned()),
                    last_modified: Some("yesterday".to_owned()),
                },
                br#"{"version":1}"#,
            )
            .unwrap();

        let (fresh, timing) = fetch_manifest_json_timed_fresh::<Value>(context.clone(), &url)
            .await
            .unwrap();
        assert!(!timing.cached);
        assert_eq!(fresh, serde_json::json!({"version": 2}));
        assert_eq!(
            cache
                .validators(&url)
                .and_then(|validators| validators.etag)
                .as_deref(),
            with_validators.then_some("\"v2\"")
        );

        let (followup, timing) = fetch_manifest_json_timed::<Value>(context, &url)
            .await
            .unwrap();
        assert_eq!(timing.cached, with_validators);
        assert_eq!(followup, fresh);
        assert_eq!(stats[0].load(Ordering::SeqCst), 0);
        assert_eq!(
            stats[1].load(Ordering::SeqCst),
            usize::from(with_validators)
        );
        assert_eq!(stats[2].load(Ordering::SeqCst), 1);
        assert_eq!(stats[3].load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_fresh_manifest_fetch_bypasses_and_replaces_cached_validators() {
        assert_fresh_manifest_fetch(true).await;
    }

    #[tokio::test]
    async fn a_fresh_response_without_validators_retires_the_old_cache_entry() {
        assert_fresh_manifest_fetch(false).await;
    }
}
