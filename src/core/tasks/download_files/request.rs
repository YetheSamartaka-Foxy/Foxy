use anyhow::anyhow;
use rand::RngExt;
use reqwest::{Client, Response, StatusCode};
use std::future::Future;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::{sleep, timeout};

use super::metrics::DownloadMetrics;
use super::transfer::{download_cancelled_error, ensure_not_cancelled, wait_for_download_resume};

const HEADER_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_RETRIES: usize = 5;

pub(super) async fn cancellable<T>(
    cancel_rx: &mut watch::Receiver<bool>,
    future: impl Future<Output = T>,
) -> anyhow::Result<T> {
    ensure_not_cancelled(cancel_rx)?;
    tokio::pin!(future);
    loop {
        tokio::select! {
            biased;
            changed = cancel_rx.changed() => {
                if changed.is_err() {
                    return Err(download_cancelled_error());
                }
                ensure_not_cancelled(cancel_rx)?;
            }
            result = &mut future => return Ok(result),
        }
    }
}

fn retryable_status(status: StatusCode) -> bool {
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

pub(super) async fn download_response(
    client: &Client,
    url: &str,
    range: Option<(u64, Option<u64>)>,
    pause_rx: &mut watch::Receiver<bool>,
    cancel_rx: &mut watch::Receiver<bool>,
    metrics: &DownloadMetrics,
) -> anyhow::Result<Response> {
    for attempt in 0..=REQUEST_RETRIES {
        wait_for_download_resume(pause_rx, cancel_rx).await?;
        let mut request = client
            .get(url)
            .header(reqwest::header::ACCEPT_ENCODING, "identity");
        if let Some((start, end)) = range {
            request = request.header(
                reqwest::header::RANGE,
                format!(
                    "bytes={start}-{}",
                    end.map(|end| end.to_string()).unwrap_or_default()
                ),
            );
        }
        let result = cancellable(cancel_rx, timeout(HEADER_TIMEOUT, request.send())).await?;
        let (error, retryable, retry_after) = match result {
            Ok(Ok(response)) => {
                let status = response.status();
                if !status.is_client_error() && !status.is_server_error() {
                    return Ok(response);
                }
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(|seconds| Duration::from_secs(seconds.min(30)));
                let error = match response.error_for_status() {
                    Err(error) => error.without_url(),
                    Ok(response) => return Ok(response),
                };
                (
                    anyhow::Error::from(error),
                    retryable_status(status),
                    retry_after,
                )
            }
            Ok(Err(error)) => {
                let retryable = error.is_timeout() || error.is_connect() || error.is_request();
                (anyhow::Error::from(error.without_url()), retryable, None)
            }
            Err(_) => (anyhow!("download response headers timed out"), true, None),
        };
        if !retryable || attempt == REQUEST_RETRIES {
            return Err(error.context(format!(
                "download request failed after {} attempt(s)",
                attempt + 1
            )));
        }
        let delay = retry_after.unwrap_or_else(|| {
            let base = 250 << attempt;
            Duration::from_millis(base + rand::rng().random_range(0..base / 2))
        });
        log::debug!(
            "Retrying download request: attempt={} delay_ms={} error={:#}",
            attempt + 1,
            delay.as_millis(),
            error
        );
        metrics
            .counters
            .range_retries
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        cancellable(cancel_rx, sleep(delay)).await?;
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_busy_servers_but_not_missing_or_forbidden_files() {
        for status in [408, 429, 500, 502, 503, 504] {
            assert!(retryable_status(StatusCode::from_u16(status).unwrap()));
        }
        for status in [200, 206, 400, 401, 403, 404, 410, 416] {
            assert!(!retryable_status(StatusCode::from_u16(status).unwrap()));
        }
    }

    #[tokio::test]
    async fn cancellation_channel_closure_stops_pending_work() {
        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        drop(cancel_tx);
        let error = cancellable(&mut cancel_rx, std::future::pending::<()>())
            .await
            .unwrap_err();
        assert!(super::super::transfer::is_download_cancelled_error(&error));
    }

    #[tokio::test]
    async fn persistent_busy_response_exhausts_a_finite_retry_budget() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for _ in 0..=REQUEST_RETRIES {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                assert!(stream.read(&mut request).await.unwrap() > 0);
                stream
                    .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nRetry-After: 0\r\nConnection: close\r\n\r\n")
                    .await
                    .unwrap();
            }
        });
        let (_cancel_tx, mut cancel_rx) = watch::channel(false);
        let (_pause_tx, mut pause_rx) = watch::channel(false);
        let metrics = DownloadMetrics::new();
        let error = timeout(
            Duration::from_secs(5),
            download_response(
                &Client::new(),
                &url,
                None,
                &mut pause_rx,
                &mut cancel_rx,
                &metrics,
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<reqwest::Error>().unwrap().status(),
            Some(StatusCode::SERVICE_UNAVAILABLE)
        );
        assert_eq!(
            metrics
                .counters
                .range_retries
                .load(std::sync::atomic::Ordering::Relaxed),
            REQUEST_RETRIES
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn paused_retry_does_not_contact_the_server_until_resumed() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        let (pause_tx, mut pause_rx) = watch::channel(false);
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(first.read(&mut request).await.unwrap() > 0);
            pause_tx.send(true).unwrap();
            first
                .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nRetry-After: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            drop(first);
            assert!(
                timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
            pause_tx.send(false).unwrap();
            let (mut resumed, _) = listener.accept().await.unwrap();
            assert!(resumed.read(&mut request).await.unwrap() > 0);
            resumed
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });
        let (_cancel_tx, mut cancel_rx) = watch::channel(false);
        let response = timeout(
            Duration::from_secs(5),
            download_response(
                &Client::new(),
                &url,
                None,
                &mut pause_rx,
                &mut cancel_rx,
                &DownloadMetrics::new(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        server.await.unwrap();
    }
}
