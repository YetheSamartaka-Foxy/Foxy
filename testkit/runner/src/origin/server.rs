use anyhow::{Context, Result, ensure};
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{net::SocketAddr, path::Path, sync::mpsc, thread, time::Instant};
use tokio::{net::TcpListener, sync::oneshot, task::JoinSet};
use tower::ServiceExt;
use tower_http::services::ServeDir;

/// Response-shape controls for an adverse-origin lane: every response is
/// held back by `delay_ms` before the first byte.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Impairment {
    pub delay_ms: u64,
}

impl Impairment {
    pub fn from_case(origin: &Value) -> Self {
        Self {
            delay_ms: origin["delay_ms"].as_u64().unwrap_or(0),
        }
    }
}

pub struct Origin {
    address: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Origin {
    pub fn start(root: &Path, port: u16) -> Result<Self> {
        Self::start_impaired(root, port, Impairment::default())
    }

    pub fn start_impaired(root: &Path, port: u16, impairment: Impairment) -> Result<Self> {
        ensure!(root.is_dir(), "Origin root must be an existing directory");
        let root = root.to_owned();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let thread = thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            runtime.block_on(async move {
                let listener = match TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await {
                    Ok(listener) => listener,
                    Err(error) => { let _ = ready_tx.send(Err(error)); return; }
                };
                let _ = ready_tx.send(listener.local_addr());
                let service = ServeDir::new(root);
                let mut connections = JoinSet::new();
                loop {
                    tokio::select! {
                        _ = &mut shutdown_rx => break,
                        Some(_) = connections.join_next(), if !connections.is_empty() => {},
                        accepted = listener.accept() => {
                            let Ok((stream, _)) = accepted else { break };
                            let service = service.clone();
                            connections.spawn(async move {
                                let service = service_fn(move |mut request: hyper::Request<hyper::body::Incoming>| {
                                    let service = service.clone();
                                    async move {
                                        if impairment.delay_ms > 0 {
                                            tokio::time::sleep(std::time::Duration::from_millis(impairment.delay_ms)).await;
                                        }
                                        // ServeDir rejects oversized suffixes; clamp through its own safe HEAD lookup.
                                        if let Some(suffix) = request.headers().get("range").and_then(|v| v.to_str().ok())
                                            .and_then(|v| v.strip_prefix("bytes=-")).filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                                            .and_then(|v| v.parse::<u64>().ok()) {
                                            let head = hyper::Request::builder().method("HEAD").uri(request.uri().clone())
                                                .body(http_body_util::Empty::<hyper::body::Bytes>::new()).unwrap();
                                            let response = service.clone().oneshot(head).await?;
                                            if response.status().is_success()
                                                && let Some(size) = response.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok())
                                                && size > 0 && suffix > size {
                                                request.headers_mut().insert("range", "bytes=0-".parse().unwrap());
                                            }
                                        }
                                        service.oneshot(request).await
                                    }
                                });
                                let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
                            });
                        }
                    }
                }
                connections.abort_all();
                while connections.join_next().await.is_some() {}
            });
        });
        let address = ready_rx.recv().context("Origin startup thread exited")??;
        Ok(Self {
            address,
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
        })
    }

    pub fn url(&self) -> String {
        format!("http://{}/", self.address)
    }
    pub fn bound(&self) -> Value {
        json!({"host": self.address.ip().to_string(), "port": self.address.port()})
    }
}

impl Drop for Origin {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn bench(url: &str, requests: usize, concurrency: usize, files: usize) -> Result<Value> {
    ensure!(
        requests > 0 && concurrency > 0 && files > 0,
        "Benchmark counts must be positive"
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let started = Instant::now();
    let mut durations = thread::scope(|scope| -> Result<Vec<f64>> {
        let mut workers = Vec::new();
        for worker in 0..concurrency.min(requests) {
            let client = client.clone();
            workers.push(scope.spawn(move || -> Result<Vec<f64>> {
                let mut samples = Vec::new();
                for index in (worker..requests).step_by(concurrency) {
                    let start = Instant::now();
                    let bytes = client
                        .get(format!(
                            "{}file-{}.bin",
                            url.trim_end_matches('/').to_owned() + "/",
                            index % files
                        ))
                        .send()?
                        .error_for_status()?
                        .bytes()?;
                    ensure!(
                        bytes.len() == 4096,
                        "Benchmark corpus files must be 4096 bytes"
                    );
                    samples.push(start.elapsed().as_secs_f64() * 1000.0);
                }
                Ok(samples)
            }));
        }
        let mut samples = Vec::new();
        for worker in workers {
            samples.extend(
                worker
                    .join()
                    .map_err(|_| anyhow::anyhow!("Benchmark worker panicked"))??,
            );
        }
        Ok(samples)
    })?;
    let elapsed = started.elapsed().as_secs_f64();
    durations.sort_by(f64::total_cmp);
    Ok(
        json!({"requests": requests, "concurrency": concurrency, "elapsed_s": elapsed,
        "requests_per_s": requests as f64 / elapsed,
        "p50_ms": durations[(durations.len()-1)*50/100], "p95_ms": durations[(durations.len()-1)*95/100]}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn http_contract_and_shutdown() -> Result<()> {
        let directory = tempfile::tempdir()?;
        std::fs::write(directory.path().join("bytes"), b"0123456789")?;
        let origin = Origin::start(directory.path(), 0)?;
        let url = origin.url();
        let client = reqwest::blocking::Client::new();
        for (range, status, body) in [
            ("bytes=2-4", 206, "234"),
            ("bytes=7-", 206, "789"),
            ("bytes=-3", 206, "789"),
            ("bytes=9-9", 206, "9"),
            ("bytes=10-", 416, ""),
            ("bytes=5-2", 416, ""),
            ("bytes=-20", 206, "0123456789"),
            ("nonsense", 416, ""),
        ] {
            let response = client
                .get(format!("{url}bytes"))
                .header("Range", range)
                .send()?;
            assert_eq!(response.status().as_u16(), status, "{range}");
            if status == 416 {
                assert_eq!(response.headers()["content-range"], "bytes */10");
            }
            assert_eq!(response.text()?, body, "{range}");
        }
        let response = client.head(format!("{url}bytes")).send()?;
        assert_eq!(response.headers()["content-length"], "10");
        assert!(response.bytes()?.is_empty());
        assert!(
            !client
                .get(format!("{url}%2e%2e%5cbytes"))
                .send()?
                .status()
                .is_success()
        );
        drop(origin);
        assert!(client.get(format!("{url}bytes")).send().is_err());
        Ok(())
    }
    #[test]
    fn delay_impairment_holds_every_response() -> Result<()> {
        let directory = tempfile::tempdir()?;
        std::fs::write(directory.path().join("bytes"), b"0123456789")?;
        let origin = Origin::start_impaired(directory.path(), 0, Impairment { delay_ms: 300 })?;
        let client = reqwest::blocking::Client::new();
        let started = Instant::now();
        let response = client.get(format!("{}bytes", origin.url())).send()?;
        assert_eq!(response.text()?, "0123456789");
        assert!(started.elapsed() >= std::time::Duration::from_millis(300));
        assert_eq!(
            Impairment::from_case(&json!({"delay_ms": 7})),
            Impairment { delay_ms: 7 }
        );
        assert_eq!(Impairment::from_case(&json!({})), Impairment::default());
        Ok(())
    }
}
