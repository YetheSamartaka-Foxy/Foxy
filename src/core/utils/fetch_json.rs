use crate::core::models::context::FoxyContext;
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

        match fetch_json_single(&context, url).await {
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
) -> Result<(Value, FetchJsonTiming), Box<dyn std::error::Error + Send + Sync>> {
    let download_start = Instant::now();
    let resp = tokio::time::timeout(FETCH_JSON_TIMEOUT, context.client.get(url).send())
        .await
        .map_err(|_| {
            format!(
                "JSON fetch timed out after {:?} for {}",
                FETCH_JSON_TIMEOUT, url
            )
        })??;
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
    let wire_bytes = resp.content_length();
    let response = tokio::time::timeout(FETCH_JSON_TIMEOUT, resp.text())
        .await
        .map_err(|_| {
            format!(
                "JSON body read timed out after {:?} for {}",
                FETCH_JSON_TIMEOUT, url
            )
        })??;
    let download = download_start.elapsed();
    let response_bytes = response.len();

    if response_bytes > FETCH_JSON_MAX_BODY_SIZE {
        return Err(format!(
            "JSON response from {} exceeds max body size ({} bytes > {} limit)",
            url, response_bytes, FETCH_JSON_MAX_BODY_SIZE
        )
        .into());
    }

    // Validate received size matches Content-Length when the server declared it
    // and the response was not transparently decompressed.
    if content_encoding == "identity"
        && let Some(expected) = wire_bytes
        && response_bytes != expected as usize
    {
        return Err(format!(
            "JSON response from {} size mismatch: Content-Length={} but received {} bytes (possible truncation)",
            url, expected, response_bytes
        )
        .into());
    }

    debug!(
        "Fetched response body for {} ({} bytes decompressed, wire={}, encoding={}, download={:.0?})",
        url,
        response_bytes,
        wire_bytes.map_or("unknown".to_string(), |b| format!("{} bytes", b)),
        content_encoding,
        download
    );

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

    let cleaned_response = &response[start..]
        .replace("\r", "")
        .replace("\n", "")
        .trim()
        .to_string();

    let data: Value = tokio::task::spawn_blocking({
        let cleaned_response = cleaned_response.to_owned();
        move || serde_json::from_str(&cleaned_response)
    })
    .await??;
    let parse = parse_start.elapsed();
    debug!("Parsed JSON payload from {} (parse={:.0?})", url, parse);

    Ok((
        data,
        FetchJsonTiming {
            download,
            response_bytes,
            parse,
        },
    ))
}
