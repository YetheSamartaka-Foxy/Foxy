use crate::core::models::context::FoxyContext;
use crate::core::utils::manifest_cache::{ManifestCache, Validators};
use log::{debug, warn};
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};

const FETCH_JSON_MAX_RETRIES: u32 = 3;
const FETCH_JSON_BASE_DELAY_MS: u64 = 500;
const FETCH_JSON_TIMEOUT: Duration = Duration::from_secs(120);
/// Maximum decompressed response body size (200 MB). Protects against truncated
/// gzip streams or malformed servers sending unbounded data.
const FETCH_JSON_MAX_BODY_SIZE: usize = 200 * 1024 * 1024;

/// Timing breakdown from a `fetch_json_timed` call.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FetchJsonTiming {
    /// Time to send the HTTP request and read the full response body.
    pub download: Duration,
    /// Size of the response body in bytes.
    pub response_bytes: usize,
    /// Time to strip BOM, clean the response string, and parse JSON.
    pub parse: Duration,
    /// The server confirmed the cached body; nothing was downloaded.
    pub cached: bool,
}

pub(crate) async fn fetch_json(
    context: Arc<FoxyContext>,
    url: &str,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let (value, _timing) = fetch_json_timed(context, url).await?;
    Ok(value)
}

pub(crate) async fn fetch_json_timed(
    context: Arc<FoxyContext>,
    url: &str,
) -> Result<(Value, FetchJsonTiming), Box<dyn std::error::Error + Send + Sync>> {
    fetch_json_timed_with_cache(context, url, None).await
}

/// Like [`fetch_json_timed`], revalidating against the context's manifest cache.
pub(crate) async fn fetch_manifest_json_timed(
    context: Arc<FoxyContext>,
    url: &str,
) -> Result<(Value, FetchJsonTiming), Box<dyn std::error::Error + Send + Sync>> {
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
    fetch_json_timed_with_cache(context, url, cache.as_deref()).await
}

async fn fetch_json_timed_with_cache(
    context: Arc<FoxyContext>,
    url: &str,
    cache: Option<&ManifestCache>,
) -> Result<(Value, FetchJsonTiming), Box<dyn std::error::Error + Send + Sync>> {
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

        match fetch_json_single(&context, url, cache).await {
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

    Err(last_error.unwrap_or_else(|| "fetch_json: all retries exhausted".into()))
}

async fn fetch_json_single(
    context: &FoxyContext,
    url: &str,
    cache: Option<&ManifestCache>,
) -> Result<(Value, FetchJsonTiming), Box<dyn std::error::Error + Send + Sync>> {
    let download_start = Instant::now();
    let cached_validators = match cache {
        Some(cache) => {
            let (cache, key) = (cache.clone(), url.to_owned());
            tokio::task::spawn_blocking(move || cache.validators(&key))
                .await
                .ok()
                .flatten()
        }
        None => None,
    };
    let mut request = context.client.get(url);
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
            format!(
                "JSON fetch timed out after {:?} for {}",
                FETCH_JSON_TIMEOUT, url
            )
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
        return Err(format!(
            "JSON fetch for {} failed with status {}",
            url,
            resp.status()
        )
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
            format!(
                "JSON body read timed out after {:?} for {}",
                FETCH_JSON_TIMEOUT, url
            )
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

    let (data, parse) = parse_json_body(url, body.to_vec()).await?;
    if let Some(cache) = cache {
        let (cache, key) = (cache.clone(), url.to_owned());
        tokio::task::spawn_blocking(move || {
            if let Err(err) = cache.store(&key, &validators, &body) {
                warn!("Manifest cache could not keep {key}: {err}");
            }
        });
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

/// Strip a BOM or other leading bytes, then parse; returns the parse time.
async fn parse_json_body(
    url: &str,
    body: Vec<u8>,
) -> Result<(Value, Duration), Box<dyn std::error::Error + Send + Sync>> {
    // Decode UTF-8 after we've verified the transport.
    let response = String::from_utf8(body)
        .map_err(|e| format!("Response from {} was not valid UTF-8: {}", url, e))?;

    let parse_start = Instant::now();

    // Remove BOM/unwanted chars
    let mut start = 0;
    while start < response.len() {
        let byte = response.as_bytes()[start];

        // Break when we find a valid JSON starting character (either '{', '[' or whitespace)
        if byte == b'{' || byte == b'[' || byte.is_ascii_whitespace() {
            break;
        }

        // Move to the next byte if the current byte is part of a BOM or non-JSON character
        start += 1;
    }
    if start > 0 {
        warn!(
            "Stripped {} leading non-JSON bytes before parsing response from {}",
            start, url
        );
    }

    let cleaned_response = response[start..].replace(['\r', '\n'], "");
    let cleaned_response = cleaned_response.trim();

    let data: Value = tokio::task::spawn_blocking({
        let cleaned_response = cleaned_response.to_owned();
        move || serde_json::from_str(&cleaned_response)
    })
    .await??;
    let parse = parse_start.elapsed();
    debug!("Parsed JSON payload from {} (parse={:.0?})", url, parse);
    Ok((data, parse))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

        let (first, timing) = fetch_manifest_json_timed(context.clone(), &url)
            .await
            .unwrap();
        assert!(!timing.cached);
        stored(&cache, &url).await;

        let (second, timing) = fetch_manifest_json_timed(context.clone(), &url)
            .await
            .unwrap();
        assert!(timing.cached);
        assert_eq!(first, second);
        assert_eq!(full.load(Ordering::SeqCst), 1);

        cache
            .store(&url, &cache.validators(&url).unwrap(), b"{not json")
            .unwrap();
        let (third, timing) = fetch_manifest_json_timed(context, &url).await.unwrap();
        assert!(!timing.cached);
        assert_eq!(third, first);
        assert_eq!(full.load(Ordering::SeqCst), 2);
    }
}
