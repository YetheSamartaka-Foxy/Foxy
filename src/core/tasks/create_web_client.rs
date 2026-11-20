use log::{debug, error, info};
use reqwest::Client;
use std::time::Duration;

const DIRECT_DOWNLOAD_HTTP2_STREAM_WINDOW_BYTES: u32 = 8 * 1024 * 1024;
const DIRECT_DOWNLOAD_HTTP2_CONNECTION_WINDOW_BYTES: u32 = 16 * 1024 * 1024;

/// Create reqwest client with HTTP/1.1 and HTTP/2 support.
///
/// - **HTTPS servers**: HTTP/2 is negotiated automatically via ALPN (TLS).
/// - **Plain HTTP servers**: HTTP/1.1 with persistent connections (keep-alive).
///   HTTP/2 cleartext (h2c) is not attempted since most mod repository servers
///   don't support it, and attempting h2c on a non-supporting server causes
///   connection failures.
/// - **Compression**: gzip/brotli/deflate are enabled. The client sends
///   `Accept-Encoding: gzip, br, deflate` and decompresses transparently.
///   If the server serves compressed responses, large manifests (e.g. 67MB JSON)
///   can transfer at ~1/10th the size.
/// - **Timeouts**: connect timeout prevents hanging on unreachable servers;
///   pool idle timeout prevents stale connections from accumulating.
pub(crate) async fn create_web_client() -> Client {
    debug!("Creating HTTP client for core tasks");
    let client = crate::core::utils::proxy::client_builder()
        .tcp_nodelay(true)
        .connect_timeout(Duration::from_secs(15))
        .pool_idle_timeout(None)
        .pool_max_idle_per_host(256)
        // HTTP/2 adaptive flow control for HTTPS connections
        .http2_adaptive_window(true)
        // Transparent decompression - server must respond with Content-Encoding
        .gzip(true)
        .brotli(true)
        .deflate(true)
        .build()
        .unwrap_or_else(|err| {
            error!(
                "Failed to build full HTTP client: {}, trying minimal fallback",
                err
            );
            crate::core::utils::proxy::client_builder()
                .tcp_nodelay(true)
                .connect_timeout(Duration::from_secs(15))
                .build()
                .unwrap_or_else(|fallback_err| {
                    error!("Failed to build even minimal HTTP client: {}", fallback_err);
                    panic!("Failed to build HTTP client: {}", fallback_err);
                })
        });

    info!(
        "HTTP client created: gzip=true brotli=true deflate=true http2_adaptive_window=true connect_timeout=15s pool_idle=none pool_max_idle_per_host=256"
    );
    client
}

/// Create the blocking reqwest client for standalone Direct Download.
///
/// Leaves adaptive window scaling disabled so the fixed 8 MiB stream and 16 MiB
/// connection HTTP/2 flow-control windows apply while preserving proxy settings
/// and HTTP/1.1 negotiation.
pub(crate) fn create_direct_download_client() -> Result<reqwest::blocking::Client, reqwest::Error> {
    let client = crate::core::utils::proxy::blocking_client_builder()
        .http2_initial_stream_window_size(DIRECT_DOWNLOAD_HTTP2_STREAM_WINDOW_BYTES)
        .http2_initial_connection_window_size(DIRECT_DOWNLOAD_HTTP2_CONNECTION_WINDOW_BYTES)
        .build()?;
    info!(
        "Direct download HTTP client created: http2_initial_stream_window_size={} http2_initial_connection_window_size={} http2_adaptive_window=false",
        DIRECT_DOWNLOAD_HTTP2_STREAM_WINDOW_BYTES, DIRECT_DOWNLOAD_HTTP2_CONNECTION_WINDOW_BYTES,
    );
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn direct_download_client_builds_and_negotiates_http1() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/sample.bin", server.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = [0u8; 4096];
            let count = stream.read(&mut request).unwrap();
            assert!(
                String::from_utf8_lossy(&request[..count]).starts_with("GET /sample.bin HTTP/1.1")
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
            )
            .unwrap();
        });

        let client = create_direct_download_client().unwrap();
        let body = client
            .get(&url)
            .timeout(Duration::from_secs(3))
            .send()
            .unwrap()
            .error_for_status()
            .unwrap()
            .text()
            .unwrap();
        assert_eq!(body, "ok");
        handle.join().unwrap();
    }
}
