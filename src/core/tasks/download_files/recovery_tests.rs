use super::bandwidth::AdaptiveBandwidthLimiter;
use super::metrics::{DownloadMetrics, DownloadSchedulerState};
use super::transfer::{download_file_ranges, is_download_cancelled_error};
use super::{DownloadResourceLimits, LARGE_FILE_THRESHOLD};
use crate::core::models::context::FoxyContext;
use crate::core::models::download_target_file::DownloadTargetFile;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

const RANGE_SIZE: usize = LARGE_FILE_THRESHOLD + 1;

fn target(url: &str, path: &std::path::Path, size: usize) -> DownloadTargetFile {
    DownloadTargetFile {
        file_id: 1,
        download_remote_url: Arc::from(url),
        download_local_path: Arc::from(path.to_string_lossy().as_ref()),
        size,
        expected_download_bytes: size,
        download_total: Arc::new(AtomicUsize::new(0)),
        download_cycle: Arc::new(AtomicUsize::new(0)),
    }
}

fn scheduler() -> Arc<DownloadSchedulerState> {
    let mut limits = DownloadResourceLimits::normal();
    limits.max_active_range_requests = 1;
    limits.min_ranges_per_file = 1;
    limits.max_ranges_per_file = 1;
    limits.range_chunk_target = RANGE_SIZE;
    limits.min_range_chunk = RANGE_SIZE;
    Arc::new(DownloadSchedulerState::new(limits))
}

async fn receive_request(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut byte = [0_u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await.unwrap() == 0 {
            break;
        }
        request.push(byte[0]);
    }
    request
}

async fn write_response(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    headers: &str,
    body: &[u8],
) {
    let response = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(response.as_bytes()).await.unwrap();
    let _ = stream.write_all(body).await;
}

async fn context() -> Arc<FoxyContext> {
    Arc::new(FoxyContext::new(
        crate::core::tasks::db_turso::build_test_database().await,
        reqwest::Client::new(),
    ))
}

async fn download(
    context: Arc<FoxyContext>,
    url: &str,
    path: &std::path::Path,
    size: usize,
    cancel_rx: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let (_pause_tx, pause_rx) = watch::channel(false);
    download_file_ranges(
        context,
        &target(url, path, size),
        url,
        &path.to_string_lossy(),
        Arc::new(AdaptiveBandwidthLimiter::from_mbps(None)),
        pause_rx,
        cancel_rx,
        None,
        true,
        Arc::new(DownloadMetrics::new()),
        scheduler(),
    )
    .await?;
    Ok(())
}

async fn serve_transient_failures_then_body(
    listener: tokio::net::TcpListener,
    expected_requests: usize,
    body: Vec<u8>,
    ranged: bool,
) {
    for attempt in 0..expected_requests {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = receive_request(&mut stream).await;
        if ranged {
            assert!(
                String::from_utf8_lossy(&request)
                    .to_ascii_lowercase()
                    .contains("range: bytes=0-")
            );
        }
        if attempt < 2 {
            drop(stream);
        } else if attempt < 5 {
            write_response(
                &mut stream,
                "503 Service Unavailable",
                "Retry-After: 0\r\n",
                &[],
            )
            .await;
        } else {
            let headers = if ranged {
                format!(
                    "Content-Range: bytes 0-{}/{}\r\n",
                    body.len() - 1,
                    body.len()
                )
            } else {
                String::new()
            };
            write_response(
                &mut stream,
                if ranged {
                    "206 Partial Content"
                } else {
                    "200 OK"
                },
                &headers,
                &body,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn transient_http_failures_recover_simple_and_ranged_downloads() {
    for ranged in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        let size = if ranged { RANGE_SIZE } else { 32 };
        let body = vec![b'r'; size];
        let server = tokio::spawn(serve_transient_failures_then_body(
            listener,
            6,
            body.clone(),
            ranged,
        ));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recovered.bin");
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        tokio::time::timeout(
            Duration::from_secs(5),
            download(context().await, &url, &path, size, cancel_rx),
        )
        .await
        .unwrap()
        .unwrap();
        server.await.unwrap();
        assert_eq!(tokio::fs::read(path).await.unwrap(), body);
    }
}

#[tokio::test]
async fn truncated_range_resumes_at_received_offset_after_transient_reconnect_failure() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/file", listener.local_addr().unwrap());
    let body = vec![b'v'; RANGE_SIZE];
    let resumed_start = 512 * 1024;
    let expected_body = body.clone();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        let first_request = receive_request(&mut first).await;
        assert!(
            String::from_utf8_lossy(&first_request)
                .to_ascii_lowercase()
                .contains("range: bytes=0-")
        );
        let headers = format!(
            "Content-Range: bytes 0-{}/{}\r\n",
            body.len() - 1,
            body.len()
        );
        let response = format!(
            "HTTP/1.1 206 Partial Content\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        first.write_all(response.as_bytes()).await.unwrap();
        first.write_all(&body[..resumed_start]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(first);

        let (mut failed_reconnect, _) = listener.accept().await.unwrap();
        let request = receive_request(&mut failed_reconnect).await;
        assert!(
            String::from_utf8_lossy(&request)
                .to_ascii_lowercase()
                .contains(&format!("range: bytes={resumed_start}-"))
        );
        write_response(
            &mut failed_reconnect,
            "503 Service Unavailable",
            "Retry-After: 0\r\n",
            &[],
        )
        .await;

        let (mut resumed, _) = listener.accept().await.unwrap();
        let request = receive_request(&mut resumed).await;
        assert!(
            String::from_utf8_lossy(&request)
                .to_ascii_lowercase()
                .contains(&format!("range: bytes={resumed_start}-"))
        );
        let headers = format!(
            "Content-Range: bytes {}-{}/{}\r\n",
            resumed_start,
            body.len() - 1,
            body.len()
        );
        write_response(
            &mut resumed,
            "206 Partial Content",
            &headers,
            &body[resumed_start..],
        )
        .await;
    });
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("resumed.bin");
    let (_cancel_tx, cancel_rx) = watch::channel(false);
    tokio::time::timeout(
        Duration::from_secs(5),
        download(context().await, &url, &path, RANGE_SIZE, cancel_rx),
    )
    .await
    .unwrap()
    .unwrap();
    server.await.unwrap();
    assert_eq!(tokio::fs::read(path).await.unwrap(), expected_body);
}

#[tokio::test]
async fn invalid_resumed_content_range_fails_without_promoting_target() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/file", listener.local_addr().unwrap());
    let body = vec![b'q'; RANGE_SIZE];
    let resumed_start = 512 * 1024;
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        receive_request(&mut first).await;
        let headers = format!(
            "Content-Range: bytes 0-{}/{}\r\n",
            body.len() - 1,
            body.len()
        );
        let response = format!(
            "HTTP/1.1 206 Partial Content\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        first.write_all(response.as_bytes()).await.unwrap();
        first.write_all(&body[..resumed_start]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(first);

        let (mut resumed, _) = listener.accept().await.unwrap();
        receive_request(&mut resumed).await;
        let remaining = body.len() - resumed_start;
        let headers = format!(
            "Content-Range: bytes 0-{}/{}\r\n",
            remaining - 1,
            body.len()
        );
        write_response(
            &mut resumed,
            "206 Partial Content",
            &headers,
            &body[..remaining],
        )
        .await;
    });
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("invalid-resume.bin");
    let (_cancel_tx, cancel_rx) = watch::channel(false);
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        download(context().await, &url, &path, RANGE_SIZE, cancel_rx),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    assert!(!path.exists());
    server.await.unwrap();
}

#[tokio::test]
async fn cancellation_interrupts_stalled_headers_and_body() {
    for (size, stall_body) in [
        (1000, false),
        (1000, true),
        (RANGE_SIZE, false),
        (RANGE_SIZE, true),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            receive_request(&mut stream).await;
            if stall_body {
                let header = if size > LARGE_FILE_THRESHOLD {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/{}\r\n",
                        size - 1,
                        size
                    )
                } else {
                    "HTTP/1.1 200 OK\r\n".to_string()
                };
                stream
                    .write_all(
                        format!("{header}Content-Length: {size}\r\nConnection: close\r\n\r\nx")
                            .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
            ready_tx.send(()).unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cancelled.bin");
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let task =
            tokio::spawn(
                async move { download(context().await, &url, &path, size, cancel_rx).await },
            );
        tokio::time::timeout(Duration::from_secs(5), ready_rx)
            .await
            .unwrap()
            .unwrap();
        cancel_tx.send(true).unwrap();
        let error = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(is_download_cancelled_error(&error));
        server.abort();
    }
}
