use anyhow::{Context, Result, ensure};
use http_body_util::BodyExt;
use hyper::{
    body::{Body, Bytes, Frame, SizeHint},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    path::Path,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    task::{Context as TaskContext, Poll},
    thread,
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, sync::oneshot, task::JoinSet};
use tower::ServiceExt;
use tower_http::services::ServeDir;

/// Response-shape controls for adverse-origin lanes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Impairment {
    pub delay_ms: u64,
    pub truncate_first_responses: u64,
    pub truncate_after_bytes: usize,
    pub truncate_path_suffix: Option<String>,
    pub burst_after_bytes: u64,
    pub burst_first_bps: u64,
    pub burst_second_bps: u64,
}

impl Impairment {
    pub fn from_case(origin: &Value) -> Self {
        Self {
            delay_ms: origin["delay_ms"].as_u64().unwrap_or(0),
            truncate_first_responses: origin["truncate_first_responses"].as_u64().unwrap_or(0),
            truncate_after_bytes: origin["truncate_after_bytes"]
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(0),
            truncate_path_suffix: origin["truncate_path_suffix"].as_str().map(str::to_owned),
            burst_after_bytes: origin["burst_after_bytes"].as_u64().unwrap_or(0),
            burst_first_bps: origin["burst_first_bps"].as_u64().unwrap_or(0),
            burst_second_bps: origin["burst_second_bps"].as_u64().unwrap_or(0),
        }
    }

    fn can_truncate(&self, method: &hyper::Method, path: &str) -> bool {
        self.truncate_first_responses > 0
            && self.truncate_after_bytes > 0
            && method == hyper::Method::GET
            && self
                .truncate_path_suffix
                .as_deref()
                .is_none_or(|suffix| path.ends_with(suffix))
    }

    fn can_burst(&self, method: &hyper::Method, path: &str) -> bool {
        self.burst_after_bytes > 0
            && self.burst_first_bps > 0
            && self.burst_second_bps > 0
            && method == hyper::Method::GET
            && path.ends_with(".bin")
    }
}

#[derive(Default)]
struct BurstStats {
    requests: AtomicU64,
    first_bytes: AtomicU64,
    first_us: AtomicU64,
    second_bytes: AtomicU64,
    second_us: AtomicU64,
}

impl BurstStats {
    fn snapshot(&self) -> [u64; 5] {
        [
            self.requests.load(Ordering::Relaxed),
            self.first_bytes.load(Ordering::Relaxed),
            self.first_us.load(Ordering::Relaxed),
            self.second_bytes.load(Ordering::Relaxed),
            self.second_us.load(Ordering::Relaxed),
        ]
    }
}

struct BurstBody {
    bytes: Bytes,
    cursor: usize,
    split: usize,
    first_bps: u64,
    second_bps: u64,
    timer: Option<Pin<Box<tokio::time::Sleep>>>,
    started: Instant,
    second_started: Option<Instant>,
    stats: Arc<BurstStats>,
}

impl Body for BurstBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.cursor == self.bytes.len() {
            return Poll::Ready(None);
        }
        let limit = if self.cursor < self.split {
            self.split
        } else {
            self.bytes.len()
        };
        let end = (self.cursor + 64 * 1024).min(limit);
        if self.timer.is_none() {
            let rate = if self.cursor < self.split {
                self.first_bps
            } else {
                self.second_bps
            };
            self.timer = Some(Box::pin(tokio::time::sleep(Duration::from_secs_f64(
                (end - self.cursor) as f64 / rate as f64,
            ))));
        }
        if self.timer.as_mut().unwrap().as_mut().poll(cx).is_pending() {
            return Poll::Pending;
        }
        self.timer = None;
        let frame = Frame::data(self.bytes.slice(self.cursor..end));
        if self.cursor < self.split {
            self.stats
                .first_bytes
                .fetch_add((end - self.cursor) as u64, Ordering::Relaxed);
            if end == self.split {
                self.stats
                    .first_us
                    .fetch_add(self.started.elapsed().as_micros() as u64, Ordering::Relaxed);
                self.second_started = Some(Instant::now());
            }
        } else {
            self.stats
                .second_bytes
                .fetch_add((end - self.cursor) as u64, Ordering::Relaxed);
            if end == self.bytes.len() {
                self.stats.second_us.fetch_add(
                    self.second_started.unwrap().elapsed().as_micros() as u64,
                    Ordering::Relaxed,
                );
            }
        }
        self.cursor = end;
        Poll::Ready(Some(Ok(frame)))
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact((self.bytes.len() - self.cursor) as u64)
    }
}

struct TruncatedBody {
    bytes: Option<Bytes>,
    claimed_len: u64,
}

impl Body for TruncatedBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(self.bytes.take().map(|bytes| Ok(Frame::data(bytes))))
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.claimed_len)
    }
}

pub struct Origin {
    address: SocketAddr,
    burst: Arc<BurstStats>,
    impairment: Impairment,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Origin {
    pub fn start(root: &Path, port: u16) -> Result<Self> {
        Self::start_impaired(root, port, Impairment::default())
    }

    pub fn start_impaired(root: &Path, port: u16, impairment: Impairment) -> Result<Self> {
        ensure!(root.is_dir(), "Origin root must be an existing directory");
        ensure!(
            [
                impairment.burst_after_bytes,
                impairment.burst_first_bps,
                impairment.burst_second_bps
            ]
            .iter()
            .all(|value| *value == 0)
                || [
                    impairment.burst_after_bytes,
                    impairment.burst_first_bps,
                    impairment.burst_second_bps
                ]
                .iter()
                .all(|value| *value > 0),
            "Origin burst requires all three positive fields"
        );
        ensure!(
            impairment.burst_after_bytes == 0 || impairment.truncate_first_responses == 0,
            "Origin burst and truncation cannot share a response"
        );
        let root = root.to_owned();
        let burst = Arc::new(BurstStats::default());
        let thread_burst = Arc::clone(&burst);
        let configured_impairment = impairment.clone();
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
                let truncated_responses = Arc::new(AtomicU64::new(0));
                let mut connections = JoinSet::new();
                loop {
                    tokio::select! {
                        _ = &mut shutdown_rx => break,
                        Some(_) = connections.join_next(), if !connections.is_empty() => {},
                        accepted = listener.accept() => {
                            let Ok((stream, _)) = accepted else { break };
                            let service = service.clone();
                            let impairment = impairment.clone();
                            let truncated_responses = Arc::clone(&truncated_responses);
                            let burst_stats = Arc::clone(&thread_burst);
                            connections.spawn(async move {
                                let service = service_fn(move |mut request: hyper::Request<hyper::body::Incoming>| {
                                    let service = service.clone();
                                    let impairment = impairment.clone();
                                    let truncated_responses = Arc::clone(&truncated_responses);
                                    let burst_stats = Arc::clone(&burst_stats);
                                    async move {
                                        if impairment.delay_ms > 0 {
                                            tokio::time::sleep(std::time::Duration::from_millis(impairment.delay_ms)).await;
                                        }
                                        let truncate = impairment.can_truncate(request.method(), request.uri().path());
                                        let burst = impairment.can_burst(request.method(), request.uri().path());
                                        // ServeDir rejects oversized suffixes; clamp through its own safe HEAD lookup.
                                        if let Some(suffix) = request.headers().get("range").and_then(|v| v.to_str().ok())
                                            .and_then(|v| v.strip_prefix("bytes=-")).filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                                            .and_then(|v| v.parse::<u64>().ok()) {
                                            let head = hyper::Request::builder().method("HEAD").uri(request.uri().clone())
                                                .body(http_body_util::Empty::<hyper::body::Bytes>::new()).unwrap();
                                            let response = service
                                                .clone()
                                                .oneshot(head)
                                                .await
                                                .unwrap_or_else(|never| match never {});
                                            if response.status().is_success()
                                                && let Some(size) = response.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok())
                                                && size > 0 && suffix > size {
                                                request.headers_mut().insert("range", "bytes=0-".parse().unwrap());
                                            }
                                        }
                                        let response = service
                                            .oneshot(request)
                                            .await
                                            .unwrap_or_else(|never| match never {});
                                        let claimed = truncate
                                            && truncated_responses
                                                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                                                    (count < impairment.truncate_first_responses)
                                                        .then_some(count + 1)
                                                })
                                                .is_ok();
                                        if burst && response.status().is_success() {
                                            let (parts, body) = response.into_parts();
                                            let bytes = body.collect().await?.to_bytes();
                                            let split = (impairment.burst_after_bytes as usize).min(bytes.len());
                                            burst_stats.requests.fetch_add(1, Ordering::Relaxed);
                                            let body = BurstBody {
                                                bytes,
                                                cursor: 0,
                                                split,
                                                first_bps: impairment.burst_first_bps,
                                                second_bps: impairment.burst_second_bps,
                                                timer: None,
                                                started: Instant::now(),
                                                second_started: None,
                                                stats: burst_stats,
                                            }.boxed_unsync();
                                            return Ok::<_, std::io::Error>(hyper::Response::from_parts(parts, body));
                                        }
                                        if !claimed || !response.status().is_success() {
                                            return Ok::<_, std::io::Error>(
                                                response.map(BodyExt::boxed_unsync),
                                            );
                                        }
                                        let (parts, body) = response.into_parts();
                                        let bytes = body.collect().await?.to_bytes();
                                        let sent = impairment
                                            .truncate_after_bytes
                                            .min(bytes.len().saturating_sub(1));
                                        let body = TruncatedBody {
                                            bytes: Some(bytes.slice(..sent)),
                                            claimed_len: bytes.len() as u64,
                                        }
                                        .boxed_unsync();
                                        Ok::<_, std::io::Error>(hyper::Response::from_parts(
                                            parts, body,
                                        ))
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
            burst,
            impairment: configured_impairment,
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

    pub fn burst_snapshot(&self) -> [u64; 5] {
        self.burst.snapshot()
    }

    pub fn burst_delta(&self, before: [u64; 5]) -> Value {
        if self.impairment.burst_after_bytes == 0 {
            return Value::Null;
        }
        let after = self.burst.snapshot();
        let delta =
            std::array::from_fn::<_, 5, _>(|index| after[index].saturating_sub(before[index]));
        let rate = |bytes: u64, micros: u64| -> Option<f64> {
            (micros > 0).then(|| bytes as f64 * 1_000_000.0 / micros as f64)
        };
        let first_rate = rate(delta[1], delta[2]);
        let second_rate = rate(delta[3], delta[4]);
        json!({
            "requests": delta[0],
            "first_bytes": delta[1],
            "first_s": delta[2] as f64 / 1_000_000.0,
            "first_bps": self.impairment.burst_first_bps,
            "first_observed_bps": first_rate,
            "first_overshoot_percent": first_rate.map(|value| (100.0 * (value / self.impairment.burst_first_bps as f64 - 1.0)).max(0.0)),
            "second_bytes": delta[3],
            "second_s": delta[4] as f64 / 1_000_000.0,
            "second_bps": self.impairment.burst_second_bps,
            "second_observed_bps": second_rate,
            "second_overshoot_percent": second_rate.map(|value| (100.0 * (value / self.impairment.burst_second_bps as f64 - 1.0)).max(0.0)),
        })
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
        let origin = Origin::start_impaired(
            directory.path(),
            0,
            Impairment {
                delay_ms: 300,
                ..Impairment::default()
            },
        )?;
        let client = reqwest::blocking::Client::new();
        let started = Instant::now();
        let response = client.get(format!("{}bytes", origin.url())).send()?;
        assert_eq!(response.text()?, "0123456789");
        assert!(started.elapsed() >= std::time::Duration::from_millis(300));
        assert_eq!(
            Impairment::from_case(&json!({"delay_ms": 7})),
            Impairment {
                delay_ms: 7,
                ..Impairment::default()
            }
        );
        assert_eq!(Impairment::from_case(&json!({})), Impairment::default());
        Ok(())
    }

    #[test]
    fn truncation_impairment_only_cuts_the_configured_responses() -> Result<()> {
        let directory = tempfile::tempdir()?;
        std::fs::write(directory.path().join("bytes.bin"), b"0123456789")?;
        let impairment = Impairment::from_case(&json!({
            "truncate_first_responses": 1,
            "truncate_after_bytes": 4,
            "truncate_path_suffix": ".bin"
        }));
        let origin = Origin::start_impaired(directory.path(), 0, impairment.clone())?;
        let client = reqwest::blocking::Client::new();
        let url = format!("{}bytes.bin", origin.url());
        let first = client.get(&url).send();
        assert!(first.is_err() || first.unwrap().bytes().is_err());
        assert_eq!(client.get(&url).send()?.bytes()?.as_ref(), b"0123456789");
        assert_eq!(impairment.truncate_first_responses, 1);
        assert_eq!(impairment.truncate_after_bytes, 4);
        assert_eq!(impairment.truncate_path_suffix.as_deref(), Some(".bin"));
        Ok(())
    }

    #[test]
    fn burst_changes_rate_within_one_verified_body() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let payload = vec![0x5au8; 2 * 1024 * 1024];
        std::fs::write(directory.path().join("bytes.bin"), &payload)?;
        let origin = Origin::start_impaired(
            directory.path(),
            0,
            Impairment {
                burst_after_bytes: 1024 * 1024,
                burst_first_bps: 4 * 1024 * 1024,
                burst_second_bps: 16 * 1024 * 1024,
                ..Impairment::default()
            },
        )?;
        let before = origin.burst_snapshot();
        let received = reqwest::blocking::get(format!("{}bytes.bin", origin.url()))?
            .error_for_status()?
            .bytes()?;
        assert_eq!(received.as_ref(), payload);
        let phase = origin.burst_delta(before);
        assert_eq!(phase["requests"], 1);
        assert_eq!(phase["first_bytes"], 1024 * 1024);
        assert_eq!(phase["second_bytes"], 1024 * 1024);
        assert!(phase["first_s"].as_f64().unwrap() > phase["second_s"].as_f64().unwrap());
        assert!(phase["first_overshoot_percent"].as_f64().unwrap() < 10.0);
        assert!(phase["second_overshoot_percent"].as_f64().unwrap() < 10.0);
        Ok(())
    }
}
