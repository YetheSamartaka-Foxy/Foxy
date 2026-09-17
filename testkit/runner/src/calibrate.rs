//! Independent reference measurements (conventions/SPEED_OF_LIGHT.md, B1, B2,
//! B4, B6): the origin's sustained body throughput and request latency, the
//! repository volume's sequential read and write rates, and compute-only hash
//! capacity. Each lane is an opaque, dated baseline the ledger rows cite by
//! id, so a "calibrated estimate" ratio always names the measurement it was
//! taken against.

use crate::{fixture, guards};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// Where the lanes are kept: one file per repository root, replaced lane by
/// lane so a network re-run does not discard a disk measurement.
pub fn path(root: &Path) -> PathBuf {
    root.join("testkit/ledger/calibration.json")
}

pub struct Options {
    pub case: PathBuf,
    pub lanes: Vec<String>,
    pub seconds: u64,
    pub disk_mib: u64,
    pub connections: usize,
    pub chunk_bytes: u64,
    /// URLs for the `hosts` lane; the case address when empty.
    pub hosts: Vec<String>,
}

pub const LANES: &[&str] = &[
    "network", "latency", "disk", "hash", "metadata", "db", "hosts",
];

fn lane_id(lane: &str, measured_utc: &str, values: &Value) -> String {
    let digest = blake3::hash(serde_json::to_string(values).unwrap_or_default().as_bytes());
    format!(
        "{lane}-{}-{}",
        &measured_utc[..10].replace('-', ""),
        &digest.to_hex()[..8]
    )
}

/// Median of a sample; `None` for an empty one.
pub fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    Some(if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    })
}

/// Interval rates of a byte counter sampled at fixed ticks, as `bytes/s` per
/// interval with the interval's true width; the first `ramp` seconds are
/// dropped so a sustained figure is the plateau, not the ramp.
pub fn interval_rates(samples: &[(f64, u64)], ramp_s: f64) -> Vec<f64> {
    samples
        .windows(2)
        .filter(|pair| pair[1].0 > ramp_s)
        .filter_map(|pair| {
            let width = pair[1].0 - pair[0].0;
            (width > 0.0).then(|| pair[1].1.saturating_sub(pair[0].1) as f64 / width)
        })
        .collect()
}

pub struct OriginFile {
    pub url: String,
    pub length: u64,
}

/// The origin's files, largest first, from the same manifest walk the oracle
/// uses; only the top `limit` are kept.
pub fn origin_files(address: &str, limit: usize) -> Result<Vec<OriginFile>> {
    let base = format!("{}/", address.trim_end_matches('/'));
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let index = fixture::metadata(address)?;
    let mut files = Vec::new();
    for key in ["requiredMods", "optionalMods"] {
        for item in index[key].as_array().into_iter().flatten() {
            let Some(name) = item["modName"].as_str().or(item["name"].as_str()) else {
                continue;
            };
            let manifest: Value = client
                .get(format!("{base}{name}/foxy_addon.json"))
                .send()?
                .error_for_status()?
                .json()?;
            for file in manifest["files"].as_array().into_iter().flatten() {
                if let (Some(path), Some(length)) = (file["path"].as_str(), file["length"].as_u64())
                {
                    files.push(OriginFile {
                        url: format!("{base}{name}/{}", path.replace('\\', "/")),
                        length,
                    });
                }
            }
        }
    }
    files.sort_by_key(|file| std::cmp::Reverse(file.length));
    files.truncate(limit);
    ensure!(
        !files.is_empty(),
        "The origin publishes no files to calibrate against"
    );
    Ok(files)
}

/// B1: sustained body-byte throughput of the origin over `seconds`, at one
/// connection and at Foxy's request budget, from 2 MiB range requests over
/// the largest files. Rates are interval-correct; the sustained figure is the
/// median plateau interval, the peak the best 500 ms window.
pub fn network(address: &str, seconds: u64, connections: usize, chunk: u64) -> Result<Value> {
    let files = origin_files(address, 16)?;
    let single = range_load(&files, 1, chunk, seconds.min(10))?;
    let aggregate = range_load(&files, connections, chunk, seconds)?;
    // B7: the aggregate curve between one connection and the budget, so a
    // per-connection ceiling is read as a curve, never as one constant.
    let mut curve = vec![json!({"connections": 1, "sustained_bps": single["sustained_bps"]})];
    for step in [8usize, 24, 48] {
        if step < connections {
            let load = range_load(&files, step, chunk, seconds.min(8))?;
            curve.push(json!({"connections": step, "sustained_bps": load["sustained_bps"]}));
        }
    }
    curve.push(json!({"connections": connections, "sustained_bps": aggregate["sustained_bps"]}));
    Ok(json!({
        "origin": guards::origin_host(address),
        "address": address,
        "files": files.len(),
        "largest_file_bytes": files[0].length,
        "chunk_bytes": chunk,
        "single_connection": single,
        "aggregate": aggregate,
        "curve": curve,
        "sustained_bps": aggregate["sustained_bps"],
        "peak_window_bps": aggregate["peak_window_bps"],
        "per_connection_bps": single["sustained_bps"],
    }))
}

fn range_load(files: &[OriginFile], connections: usize, chunk: u64, seconds: u64) -> Result<Value> {
    ensure!(
        connections > 0 && chunk > 0 && seconds > 0,
        "Load parameters must be positive"
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .pool_max_idle_per_host(connections)
        .build()?;
    let received = Arc::new(AtomicU64::new(0));
    let requests = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let mut samples = vec![(0.0, 0u64)];
    let errors = thread::scope(|scope| -> Result<u64> {
        let mut workers = Vec::new();
        for worker in 0..connections {
            let client = client.clone();
            let received = received.clone();
            let requests = requests.clone();
            let stop = stop.clone();
            workers.push(scope.spawn(move || -> u64 {
                let mut errors = 0u64;
                let mut cursor = worker as u64 * chunk;
                let mut index = worker % files.len();
                let mut buffer = vec![0u8; 256 * 1024];
                while !stop.load(Ordering::Relaxed) {
                    let file = &files[index];
                    if cursor >= file.length {
                        cursor = 0;
                        index = (index + 1) % files.len();
                        continue;
                    }
                    let end = (cursor + chunk).min(file.length) - 1;
                    requests.fetch_add(1, Ordering::Relaxed);
                    match client
                        .get(&file.url)
                        .header("Range", format!("bytes={cursor}-{end}"))
                        .send()
                        .and_then(reqwest::blocking::Response::error_for_status)
                    {
                        Ok(mut response) => loop {
                            match response.read(&mut buffer) {
                                Ok(0) => break,
                                Ok(n) => {
                                    received.fetch_add(n as u64, Ordering::Relaxed);
                                }
                                Err(_) => {
                                    errors += 1;
                                    break;
                                }
                            }
                            if stop.load(Ordering::Relaxed) {
                                break;
                            }
                        },
                        Err(_) => errors += 1,
                    }
                    cursor = end + 1 + (connections as u64 - 1) * chunk;
                }
                errors
            }));
        }
        let deadline = Duration::from_secs(seconds);
        while started.elapsed() < deadline {
            thread::sleep(Duration::from_millis(500));
            samples.push((
                started.elapsed().as_secs_f64(),
                received.load(Ordering::Relaxed),
            ));
        }
        stop.store(true, Ordering::Relaxed);
        Ok(workers.into_iter().map(|w| w.join().unwrap_or(1)).sum())
    })?;
    let elapsed = started.elapsed().as_secs_f64();
    let total = received.load(Ordering::Relaxed);
    let rates = interval_rates(&samples, 2.0_f64.min(elapsed / 2.0));
    let sustained = median(&rates).unwrap_or(0.0);
    let peak = rates.iter().copied().fold(0.0_f64, f64::max);
    // The first four seconds of 500 ms windows: the path's own ramp at this
    // connection count, which the app's `ramp_s` is read against.
    let ramp: Vec<f64> = interval_rates(&samples, 0.0)
        .into_iter()
        .take(8)
        .map(f64::round)
        .collect();
    Ok(json!({
        "connections": connections,
        "seconds": seconds,
        "bytes": total,
        "requests": requests.load(Ordering::Relaxed),
        "errors": errors,
        "average_bps": (total as f64 / elapsed).round(),
        "sustained_bps": sustained.round(),
        "peak_window_bps": peak.round(),
        "intervals": rates.len(),
        "ramp_intervals_bps": ramp,
    }))
}

/// B4: connect time, a fresh request (new connection, full `repo.json` body)
/// and a reused request on a kept-alive connection, each a median over
/// several samples, in seconds.
pub fn latency(address: &str, samples: usize) -> Result<Value> {
    let base = format!("{}/", address.trim_end_matches('/'));
    let url = reqwest::Url::parse(&base)?;
    let host = url.host_str().context("Origin address has no host")?;
    let port = url.port_or_known_default().unwrap_or(80);
    let target = (host, port)
        .to_socket_addrs()?
        .next()
        .context("Origin address does not resolve")?;
    let mut connect = Vec::new();
    for _ in 0..samples {
        let started = Instant::now();
        let stream = TcpStream::connect_timeout(&target, Duration::from_secs(10))?;
        connect.push(started.elapsed().as_secs_f64());
        drop(stream);
    }
    let mut fresh = Vec::new();
    for _ in 0..samples {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        let started = Instant::now();
        client
            .get(format!("{base}repo.json"))
            .send()?
            .error_for_status()?
            .bytes()?;
        fresh.push(started.elapsed().as_secs_f64());
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    client.get(format!("{base}repo.json")).send()?.bytes()?;
    let mut reused = Vec::new();
    for _ in 0..samples {
        let started = Instant::now();
        client
            .get(format!("{base}repo.json"))
            .send()?
            .error_for_status()?
            .bytes()?;
        reused.push(started.elapsed().as_secs_f64());
    }
    let round = |v: Option<f64>| v.map(|v| (v * 1e6).round() / 1e6);
    Ok(json!({
        "origin": guards::origin_host(address),
        "samples": samples,
        "connect_s": round(median(&connect)),
        "fresh_request_s": round(median(&fresh)),
        "reused_request_s": round(median(&reused)),
        "fresh_request_min_s": round(fresh.iter().copied().reduce(f64::min)),
    }))
}

/// B4 for any origin, the exact URL included: plain HTTP or HTTPS (the
/// handshake lands in the fresh request), one entry per host so a multi-host
/// startup graph can cite a reference per branch.
pub fn host_latency(url: &str, samples: usize) -> Result<Value> {
    let parsed = reqwest::Url::parse(url)?;
    let host = parsed.host_str().context("URL has no host")?;
    let port = parsed
        .port_or_known_default()
        .context("URL has no known port")?;
    let target = (host, port)
        .to_socket_addrs()?
        .next()
        .context("URL host does not resolve")?;
    let mut connect = Vec::new();
    for _ in 0..samples {
        let started = Instant::now();
        let stream = TcpStream::connect_timeout(&target, Duration::from_secs(10))?;
        connect.push(started.elapsed().as_secs_f64());
        drop(stream);
    }
    let get = |client: &reqwest::blocking::Client| -> Result<u16> {
        let response = client.get(url).send()?;
        let status = response.status().as_u16();
        response.bytes()?;
        Ok(status)
    };
    let mut fresh = Vec::new();
    let mut status = 0;
    for _ in 0..samples {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("foxy-testkit")
            .build()?;
        let started = Instant::now();
        status = get(&client)?;
        fresh.push(started.elapsed().as_secs_f64());
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("foxy-testkit")
        .build()?;
    get(&client)?;
    let mut reused = Vec::new();
    for _ in 0..samples {
        let started = Instant::now();
        get(&client)?;
        reused.push(started.elapsed().as_secs_f64());
    }
    let round = |v: Option<f64>| v.map(|v| (v * 1e6).round() / 1e6);
    Ok(json!({
        "host": format!("{host}:{port}"),
        "scheme": parsed.scheme(),
        "url": url,
        "status": status,
        "samples": samples,
        "connect_s": round(median(&connect)),
        "fresh_request_s": round(median(&fresh)),
        "reused_request_s": round(median(&reused)),
        "fresh_request_min_s": round(fresh.iter().copied().reduce(f64::min)),
    }))
}

/// B2/B3: sequential write with a durable flush, an unbuffered sequential
/// read (page cache bypassed, so the device answers) and a buffered re-read
/// (page cache) of one `mib` MiB file beside the repository path.
pub fn disk(repository_path: &Path, mib: u64) -> Result<Value> {
    ensure!(mib >= 64, "Disk calibration needs at least 64 MiB");
    let parent = repository_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(repository_path);
    fs::create_dir_all(parent)?;
    let file = parent.join(format!("foxy-testkit-calibrate-{}.tmp", std::process::id()));
    let result = disk_lanes(&file, mib * 1024 * 1024);
    let _ = fs::remove_file(&file);
    let mut value = result?;
    value["storage_class"] = guards::storage_class(parent).into();
    value["volume"] = parent
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .into();
    Ok(value)
}

fn disk_lanes(file: &Path, bytes: u64) -> Result<Value> {
    let block = 8 * 1024 * 1024usize;
    let pattern: Vec<u8> = (0..block)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let started = Instant::now();
    {
        let mut out = fs::File::create(file)?;
        let mut written = 0u64;
        while written < bytes {
            let len = ((bytes - written) as usize).min(block);
            out.write_all(&pattern[..len])?;
            written += len as u64;
        }
        out.sync_all()?;
    }
    let write_s = started.elapsed().as_secs_f64();
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    // The warm re-reads come first, while the written pages are still cached;
    // the unbuffered reads bypass the cache whatever its state. The parallel
    // lanes are the references for a hash pass, which reads with many workers.
    let warm_s = read_lane(file, bytes, block, 1, false)?;
    let warm_parallel_s = read_lane(file, bytes, block, threads, false)?;
    let unbuffered_s = read_lane(file, bytes, block, 1, true)?;
    let unbuffered_parallel_s = read_lane(file, bytes, block, threads, true)?;
    let rate = |secs: Option<f64>| secs.map(|s| (bytes as f64 / s).round());
    Ok(json!({
        "bytes": bytes,
        "block_bytes": block,
        "threads": threads,
        "durable_write_bps": rate(Some(write_s)),
        "unbuffered_read_bps": rate(unbuffered_s),
        "unbuffered_read_parallel_bps": rate(unbuffered_parallel_s),
        "warm_read_bps": rate(warm_s),
        "warm_read_parallel_bps": rate(warm_parallel_s),
    }))
}

/// Read the whole file with `threads` workers over disjoint block-aligned
/// ranges (positioned reads, no shared cursor), buffered or bypassing the
/// page cache; the wall time in seconds. Unbuffered reads are only available
/// on Windows and report `None` elsewhere.
fn read_lane(
    file: &Path,
    bytes: u64,
    block: usize,
    threads: usize,
    unbuffered: bool,
) -> Result<Option<f64>> {
    #[cfg(not(windows))]
    if unbuffered {
        let _ = (file, bytes, block, threads);
        return Ok(None);
    }
    let blocks = bytes.div_ceil(block as u64);
    let started = Instant::now();
    thread::scope(|scope| -> Result<()> {
        let workers: Vec<_> = (0..threads)
            .map(|worker| {
                scope.spawn(move || -> Result<()> {
                    let input = open_for_read(file, unbuffered)?;
                    let mut raw = vec![0u8; block + 4096];
                    let offset = raw.as_ptr().align_offset(4096);
                    let buffer = &mut raw[offset..offset + block];
                    let mut index = worker as u64;
                    while index < blocks {
                        let start = index * block as u64;
                        let want = ((bytes - start) as usize).min(block);
                        let mut done = 0usize;
                        while done < want {
                            let n = positioned_read(
                                &input,
                                &mut buffer[done..want],
                                start + done as u64,
                            )?;
                            ensure!(n > 0, "Short read while calibrating the disk");
                            done += n;
                        }
                        index += threads as u64;
                    }
                    Ok(())
                })
            })
            .collect();
        for worker in workers {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("Disk calibration worker panicked"))??;
        }
        Ok(())
    })?;
    Ok(Some(started.elapsed().as_secs_f64()))
}

#[cfg(windows)]
fn open_for_read(file: &Path, unbuffered: bool) -> Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
    const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    if unbuffered {
        options.custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN);
    }
    Ok(options.open(file)?)
}

#[cfg(windows)]
fn positioned_read(file: &fs::File, buffer: &mut [u8], offset: u64) -> Result<usize> {
    use std::os::windows::fs::FileExt;
    Ok(file.seek_read(buffer, offset)?)
}

#[cfg(not(windows))]
fn open_for_read(file: &Path, _unbuffered: bool) -> Result<fs::File> {
    Ok(fs::File::open(file)?)
}

#[cfg(not(windows))]
fn positioned_read(file: &fs::File, buffer: &mut [u8], offset: u64) -> Result<usize> {
    use std::os::unix::fs::FileExt;
    Ok(file.read_at(buffer, offset)?)
}
/// B6: compute-only hash capacity over an in-memory buffer, one thread and
/// every core, for BLAKE3 and MD5. No file is read, so this is the CPU term
/// of the hash bound, never the read term.
pub fn hash(mib: u64) -> Result<Value> {
    ensure!(mib >= 16, "Hash calibration needs at least 16 MiB");
    let bytes = (mib * 1024 * 1024) as usize;
    let buffer: Vec<u8> = (0..bytes)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 11) as u8)
        .collect();
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    let time = |f: &(dyn Fn(&[u8]) + Sync)| -> f64 {
        let started = Instant::now();
        f(&buffer);
        started.elapsed().as_secs_f64()
    };
    let parallel = |f: &(dyn Fn(&[u8]) + Sync)| -> f64 {
        let slice = bytes / threads;
        let started = Instant::now();
        thread::scope(|scope| {
            for chunk in buffer.chunks(slice.max(1)) {
                scope.spawn(move || f(chunk));
            }
        });
        started.elapsed().as_secs_f64()
    };
    let blake3 = |data: &[u8]| {
        std::hint::black_box(blake3::hash(data));
    };
    let md5 = |data: &[u8]| {
        use md5::Digest;
        std::hint::black_box(md5::Md5::digest(data));
    };
    let rate = |secs: f64| (bytes as f64 / secs).round();
    Ok(json!({
        "bytes": bytes,
        "threads": threads,
        "blake3_single_bps": rate(time(&blake3)),
        "blake3_all_cores_bps": rate(parallel(&blake3)),
        "md5_single_bps": rate(time(&md5)),
        "md5_all_cores_bps": rate(parallel(&md5)),
    }))
}

/// B5: metadata enumeration rate of the case's repository tree, the way the
/// quick scan walks an addon (`read_dir`, file type, metadata per entry):
/// entries per second on a first pass and on an immediate second pass, so a
/// warm quick scan has a matched reference and a cold one an honest lower
/// rate. The first pass is only cold when nothing touched the tree before.
pub fn metadata(repository_path: &Path) -> Result<Value> {
    ensure!(
        repository_path.is_dir(),
        "Metadata calibration needs the repository path to exist"
    );
    let mut passes = Vec::new();
    for _ in 0..2 {
        let started = Instant::now();
        let (entries, directories, bytes) = walk(repository_path)?;
        let elapsed = started.elapsed().as_secs_f64();
        passes.push(json!({
            "entries": entries,
            "directories": directories,
            "bytes": bytes,
            "elapsed_s": (elapsed * 1e6).round() / 1e6,
            "entries_per_s": (entries as f64 / elapsed.max(1e-9)).round(),
        }));
    }
    Ok(json!({
        "first_pass": passes[0],
        "warm_pass": passes[1],
        "first_pass_entries_per_s": passes[0]["entries_per_s"],
        "warm_entries_per_s": passes[1]["entries_per_s"],
        "storage_class": guards::storage_class(repository_path),
        "volume": repository_path
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    }))
}

fn walk(root: &Path) -> Result<(u64, u64, u64)> {
    let (mut entries, mut directories, mut bytes) = (0u64, 0u64, 0u64);
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let meta = entry.metadata()?;
            entries += 1;
            if file_type.is_dir() {
                directories += 1;
                pending.push(entry.path());
            } else {
                bytes += meta.len();
            }
        }
    }
    Ok((entries, directories, bytes))
}

/// DB: the Turso workload reference for O7 on the case's volume: `rows`
/// part rows inserted into a `subfiles`-shaped table with its unique index,
/// in batches of 256 rows per transaction at `synchronous=NORMAL`, then one
/// scoped delete and a checkpoint. Uncontended (one writer), so it is the
/// gate-1 reference the `db_persist` windows are read against.
pub fn db(repository_path: &Path, rows: u64) -> Result<Value> {
    let parent = repository_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(repository_path);
    fs::create_dir_all(parent)?;
    let file = parent.join(format!("foxy-testkit-calibrate-{}.db", std::process::id()));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(db_workload(&file, rows));
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(parent.join(format!(
            "foxy-testkit-calibrate-{}.db{suffix}",
            std::process::id()
        )));
    }
    let mut value = result?;
    value["storage_class"] = guards::storage_class(parent).into();
    value["volume"] = parent
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .into();
    Ok(value)
}

async fn db_workload(file: &Path, rows: u64) -> Result<Value> {
    let database = turso::Builder::new_local(&file.to_string_lossy())
        .build()
        .await?;
    let conn = database.connect()?;
    for pragma in [
        "journal_mode = WAL",
        "synchronous = NORMAL",
        "cache_size = -16384",
    ] {
        let mut stmt = conn.query(&format!("PRAGMA {pragma}"), ()).await?;
        while stmt.next().await?.is_some() {}
    }
    conn.execute(
        "CREATE TABLE subfiles (id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL, path TEXT, \
         local_length INTEGER, local_start INTEGER, remote_length INTEGER, remote_start INTEGER, \
         local_checksum TEXT, remote_checksum TEXT, data_order INTEGER)",
        (),
    )
    .await?;
    conn.execute(
        "CREATE UNIQUE INDEX idx_subfiles_file_id_path ON subfiles (file_id, path)",
        (),
    )
    .await?;
    let batch = 256u64;
    let placeholders = vec!["(?, ?, ?, ?, ?, ?)"; batch as usize].join(", ");
    let sql = format!(
        "INSERT INTO subfiles (file_id, path, remote_length, remote_start, remote_checksum, data_order) VALUES {placeholders}"
    );
    let started = Instant::now();
    let mut inserted = 0u64;
    let mut transactions = 0u64;
    while inserted < rows {
        let count = (rows - inserted).min(batch);
        let mut params: Vec<turso::Value> = Vec::with_capacity(count as usize * 6);
        for index in inserted..inserted + count {
            let file_id = (index / 40) as i64;
            params.push(turso::Value::Integer(file_id));
            params.push(turso::Value::Text(format!(
                "addons/part_{:06}_{:03}.bin",
                file_id,
                index % 40
            )));
            params.push(turso::Value::Integer(1_048_576));
            params.push(turso::Value::Integer((index % 40) as i64 * 1_048_576));
            params.push(turso::Value::Text(format!("{:064x}", index)));
            params.push(turso::Value::Integer((index % 40) as i64));
        }
        if count == batch {
            conn.execute("BEGIN", ()).await?;
            conn.execute(&sql, params).await?;
            conn.execute("COMMIT", ()).await?;
        } else {
            let placeholders = vec!["(?, ?, ?, ?, ?, ?)"; count as usize].join(", ");
            conn.execute(
                &format!(
                    "INSERT INTO subfiles (file_id, path, remote_length, remote_start, remote_checksum, data_order) VALUES {placeholders}"
                ),
                params,
            )
            .await?;
        }
        transactions += 1;
        inserted += count;
    }
    let insert_s = started.elapsed().as_secs_f64();
    // The hash persist shape: keyed updates of local state, 256 rows per
    // transaction, over the rows the inserts just wrote.
    let update_sql =
        "UPDATE subfiles SET local_checksum = ?, local_length = ?, local_start = ? WHERE id = ?";
    let started = Instant::now();
    let mut updated = 0u64;
    let mut update_transactions = 0u64;
    let update_rows = rows.min(65_536);
    while updated < update_rows {
        let count = (update_rows - updated).min(batch);
        conn.execute("BEGIN", ()).await?;
        for index in updated..updated + count {
            conn.execute(
                update_sql,
                (
                    turso::Value::Text(format!("{:064x}", index + 1)),
                    turso::Value::Integer(1_048_576),
                    turso::Value::Integer((index % 40) as i64 * 1_048_576),
                    turso::Value::Integer(index as i64 + 1),
                ),
            )
            .await?;
        }
        conn.execute("COMMIT", ()).await?;
        update_transactions += 1;
        updated += count;
    }
    let update_s = started.elapsed().as_secs_f64();
    let started = Instant::now();
    let deleted = conn
        .execute(
            "DELETE FROM subfiles WHERE file_id < ?",
            (i64::try_from(rows / 80)?,),
        )
        .await?;
    let delete_s = started.elapsed().as_secs_f64();
    let started = Instant::now();
    let mut stmt = conn.query("PRAGMA wal_checkpoint(TRUNCATE)", ()).await?;
    while stmt.next().await?.is_some() {}
    let checkpoint_s = started.elapsed().as_secs_f64();
    let round = |v: f64| (v * 1e6).round() / 1e6;
    Ok(json!({
        "rows": inserted,
        "batch_rows": batch,
        "transactions": transactions,
        "insert_s": round(insert_s),
        "insert_rows_per_s": (inserted as f64 / insert_s.max(1e-9)).round(),
        "update_rows": updated,
        "update_transactions": update_transactions,
        "update_s": round(update_s),
        "update_rows_per_s": (updated as f64 / update_s.max(1e-9)).round(),
        "delete_rows": deleted,
        "delete_s": round(delete_s),
        "delete_rows_per_s": (deleted as f64 / delete_s.max(1e-9)).round(),
        "checkpoint_s": round(checkpoint_s),
        "journal_mode": "wal",
        "synchronous": "normal",
        "write_gate": 1,
    }))
}

/// Run the requested lanes for a case and merge them into the calibration
/// file, each stamped with an id, the time and the environment fingerprint.
pub fn execute(root: &Path, options: &Options) -> Result<Value> {
    let header: Value = serde_json::from_slice(&fs::read(&options.case)?)?;
    let address = header["repository"]["address"]
        .as_str()
        .context("Case needs a repository address")?
        .to_owned();
    let repository_path = header["repository"]["path"]
        .as_str()
        .map(PathBuf::from)
        .context("Case needs a repository path")?;
    for lane in &options.lanes {
        ensure!(
            LANES.contains(&lane.as_str()),
            "Unknown calibration lane {lane}"
        );
    }
    let output = path(root);
    let mut file = if output.exists() {
        crate::case::read_json(&output)?
    } else {
        json!({"lanes":{}})
    };
    let environment = guards::environment_fingerprint(&guards::origin_host(&address));
    let mut results = json!({});
    for lane in &options.lanes {
        if lane == "hosts" {
            let hosts: Vec<String> = if options.hosts.is_empty() {
                vec![format!("{}/repo.json", address.trim_end_matches('/'))]
            } else {
                options.hosts.clone()
            };
            for url in hosts {
                let values = host_latency(&url, 10)?;
                let measured_utc = chrono::Utc::now().to_rfc3339();
                let key = values["host"].as_str().unwrap_or("unknown").to_owned();
                let entry = json!({
                    "id": lane_id("hosts", &measured_utc, &values),
                    "measured_utc": measured_utc,
                    "environment": environment,
                    "case_id": header["id"],
                    "values": values,
                });
                results["hosts"][&key] = entry.clone();
                file["lanes"]["hosts"][&key] = entry;
            }
            continue;
        }
        let values = match lane.as_str() {
            "network" => network(
                &address,
                options.seconds,
                options.connections,
                options.chunk_bytes,
            )?,
            "latency" => latency(&address, 10)?,
            "disk" => disk(&repository_path, options.disk_mib)?,
            "hash" => hash(256)?,
            "metadata" => metadata(&repository_path)?,
            "db" => db(&repository_path, 200_000)?,
            _ => unreachable!(),
        };
        let measured_utc = chrono::Utc::now().to_rfc3339();
        let entry = json!({
            "id": lane_id(lane, &measured_utc, &values),
            "measured_utc": measured_utc,
            "environment": environment,
            "case_id": header["id"],
            "values": values,
        });
        results[lane] = entry.clone();
        // Volume-bound lanes are keyed by volume: an NVMe and a rotational case
        // must not overwrite each other's reference.
        if matches!(lane.as_str(), "disk" | "db" | "metadata") {
            let volume = entry["values"]["volume"]
                .as_str()
                .unwrap_or("unknown")
                .to_ascii_lowercase();
            file["lanes"][lane.as_str()][volume] = entry;
        } else {
            file["lanes"][lane] = entry;
        }
    }
    file["updated_utc"] = chrono::Utc::now().to_rfc3339().into();
    fixture::write_json(&output, &file)?;
    Ok(json!({"status":"ok","path":output,"lanes":results}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_rates_use_true_widths_and_drop_the_ramp() {
        let samples = [(0.0, 0), (0.5, 100), (1.0, 300), (1.8, 700), (2.0, 900)];
        let rounded = |rates: Vec<f64>| rates.iter().map(|r| r.round()).collect::<Vec<_>>();
        let rates = interval_rates(&samples, 0.0);
        assert_eq!(rounded(rates.clone()), [200.0, 400.0, 500.0, 1000.0]);
        assert_eq!(rounded(interval_rates(&samples, 1.0)), [500.0, 1000.0]);
        assert_eq!(median(&rates), Some(450.0));
        assert_eq!(median(&[]), None);
    }

    #[test]
    fn lane_ids_are_dated_and_content_addressed() {
        let a = lane_id(
            "network",
            "2026-09-16T10:00:00Z",
            &json!({"sustained_bps":1}),
        );
        let b = lane_id(
            "network",
            "2026-09-16T11:00:00Z",
            &json!({"sustained_bps":1}),
        );
        let c = lane_id(
            "network",
            "2026-09-16T10:00:00Z",
            &json!({"sustained_bps":2}),
        );
        assert!(a.starts_with("network-20260916-"));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn hash_lane_reports_positive_rates() {
        let value = hash(16).unwrap();
        for key in [
            "blake3_single_bps",
            "blake3_all_cores_bps",
            "md5_single_bps",
            "md5_all_cores_bps",
        ] {
            assert!(value[key].as_f64().unwrap() > 0.0, "{key}");
        }
    }

    #[test]
    fn disk_lane_measures_and_removes_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let repository = dir.path().join("payload");
        let value = disk(&repository, 64).unwrap();
        assert!(value["durable_write_bps"].as_f64().unwrap() > 0.0);
        assert!(value["warm_read_bps"].as_f64().unwrap() > 0.0);
        assert!(fs::read_dir(dir.path()).unwrap().all(|e| {
            !e.unwrap()
                .file_name()
                .to_string_lossy()
                .contains("calibrate")
        }));
    }
}
