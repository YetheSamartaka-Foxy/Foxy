use log::{debug, error, info};
use reqwest::Client;
use std::time::Duration;

pub(crate) async fn create_web_client() -> Client {
    debug!("Creating HTTP client for core tasks");
    let client = crate::core::utils::proxy::client_builder()
        .tcp_nodelay(true)
        .connect_timeout(Duration::from_secs(15))
        .pool_idle_timeout(None)
        .pool_max_idle_per_host(256)
        .http2_adaptive_window(true)
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
