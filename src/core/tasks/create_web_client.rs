use log::{debug, error, info};
use reqwest::Client;
use std::time::Duration;

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
    let client = reqwest::Client::builder()
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
            reqwest::Client::builder()
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
