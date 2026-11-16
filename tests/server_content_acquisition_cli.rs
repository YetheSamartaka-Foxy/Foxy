use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{Value, json};

const GAME: &str = "arma3";
const ADDON: &str = "@mod_01";

fn invoke(root: &Path, args: &[&str], code: i32) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    command
        .arg("--config-dir")
        .arg(root)
        .arg("--json")
        .args(args);
    command.env("FOXY_CONFIG_DIR", root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(code),
        "args={args:?}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    if code == 0 {
        assert_eq!(value["ok"], true);
    }
    assert_eq!(value.as_object().unwrap().len(), 4);
    value
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("games.json"),
        include_str!("../examples/json/appdata/games.json"),
    )
    .unwrap();
    root
}

fn inventory(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(base: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.insert(path.strip_prefix(base).unwrap().to_path_buf(), Vec::new());
                visit(base, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(base).unwrap().to_path_buf(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

fn add_instance(root: &Path) -> String {
    invoke(
        root,
        &[
            "server",
            "instance",
            "add",
            "--game-space",
            GAME,
            "--name",
            "MainServer",
        ],
        0,
    )["data"]["instance"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn register_download_profile(root: &Path, url: &str) {
    let folder = root.join("repository-profile");
    fs::create_dir_all(folder.join(ADDON)).unwrap();
    fs::write(
        folder.join(ADDON).join("payload.bin"),
        b"source profile stays untouched",
    )
    .unwrap();
    invoke(
        root,
        &[
            "server",
            "content",
            "add-folder",
            "--game-space",
            GAME,
            "--id",
            "RepoAlpha",
            "--path",
            folder.to_str().unwrap(),
        ],
        0,
    );
    invoke(
        root,
        &[
            "server",
            "content",
            "add-profile",
            "--game-space",
            GAME,
            "--name",
            "RepoAlpha",
            "--folder",
            "RepoAlpha",
            "--url",
            url,
            "--local-path",
            root.join("client-repository").to_str().unwrap(),
            "--supply",
            "download",
            "--addon",
            ADDON,
        ],
        0,
    );
}

fn save_download_binding(root: &Path, instance_id: &str) {
    let bindings = root.join("bindings.json");
    fs::write(
        &bindings,
        br#"[{"source_id":"RepoAlpha","role":"required","order":1}]"#,
    )
    .unwrap();
    let plan = invoke(
        root,
        &[
            "server",
            "content",
            "bindings",
            "plan",
            instance_id,
            "--game-space",
            GAME,
            "--bindings",
            bindings.to_str().unwrap(),
        ],
        0,
    );
    let fingerprint = plan["data"]["plan"]["fingerprint"].as_str().unwrap();
    invoke(
        root,
        &[
            "server",
            "content",
            "bindings",
            "save",
            instance_id,
            "--game-space",
            GAME,
            "--bindings",
            bindings.to_str().unwrap(),
            "--expected-plan-fingerprint",
            fingerprint,
        ],
        0,
    );
}

#[test]
fn acquisition_plan_is_offline_write_free_and_apply_requires_exact_review() {
    let root = fixture();
    let id = add_instance(root.path());
    register_download_profile(root.path(), "http://127.0.0.1:9/repo/");
    save_download_binding(root.path(), &id);

    let before = inventory(root.path());
    let plan = invoke(
        root.path(),
        &[
            "server",
            "content",
            "acquire",
            "plan",
            &id,
            "--game-space",
            GAME,
        ],
        0,
    );
    let reviewed = &plan["data"]["plan"];
    let fingerprint = reviewed["fingerprint"].as_str().unwrap();
    assert_eq!(reviewed["writes_performed"], false);
    assert_eq!(reviewed["downloads_started"], false);
    assert_eq!(plan["data"]["database_opened"], false);
    assert_eq!(inventory(root.path()), before);

    let dry_apply = invoke(
        root.path(),
        &[
            "--dry-run",
            "server",
            "content",
            "acquire",
            "apply",
            &id,
            "--game-space",
            GAME,
            "--expected-plan-fingerprint",
            fingerprint,
        ],
        0,
    );
    assert_eq!(dry_apply["data"]["dry_run"], true);
    assert_eq!(dry_apply["data"]["writes_performed"], false);
    assert_eq!(dry_apply["data"]["downloads_started"], false);
    assert_eq!(inventory(root.path()), before);

    invoke(
        root.path(),
        &[
            "server",
            "content",
            "acquire",
            "apply",
            &id,
            "--game-space",
            GAME,
            "--expected-plan-fingerprint",
            fingerprint,
        ],
        2,
    );
    invoke(
        root.path(),
        &[
            "--wipe-db",
            "--yes",
            "server",
            "content",
            "acquire",
            "plan",
            &id,
            "--game-space",
            GAME,
        ],
        2,
    );
    invoke(
        root.path(),
        &[
            "server",
            "content",
            "acquire",
            "apply",
            &id,
            "--game-space",
            GAME,
            "--yes",
            "--expected-plan-fingerprint",
            "0000000000000000000000000000000000000000000000000000000000000000",
        ],
        2,
    );
    assert_eq!(inventory(root.path()), before);
}

struct HttpOrigin {
    address: String,
    stop: Arc<AtomicBool>,
    corrupt_all_ranges: Arc<AtomicBool>,
    fail_payload: Arc<AtomicBool>,
    failed_payload_requests: Arc<AtomicU64>,
    range_requests: Arc<AtomicU64>,
    range_bytes: Arc<AtomicU64>,
    full_bytes: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl HttpOrigin {
    fn start(root: PathBuf, control_root: PathBuf) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = format!(
            "http://127.0.0.1:{}/repo/",
            listener.local_addr().unwrap().port()
        );
        let stop = Arc::new(AtomicBool::new(false));
        let corrupt_all_ranges = Arc::new(AtomicBool::new(false));
        let fail_payload = Arc::new(AtomicBool::new(false));
        let failed_payload_requests = Arc::new(AtomicU64::new(0));
        let range_requests = Arc::new(AtomicU64::new(0));
        let range_bytes = Arc::new(AtomicU64::new(0));
        let full_bytes = Arc::new(AtomicU64::new(0));
        let thread_stop = Arc::clone(&stop);
        let thread_corrupt_all = Arc::clone(&corrupt_all_ranges);
        let thread_fail_payload = Arc::clone(&fail_payload);
        let thread_failed_payload_requests = Arc::clone(&failed_payload_requests);
        let thread_ranges = Arc::clone(&range_requests);
        let thread_range_bytes = Arc::clone(&range_bytes);
        let thread_full_bytes = Arc::clone(&full_bytes);
        let mode_path = control_root.join("origin-mode.txt");
        let stop_path = control_root.join("stop-server");
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) && !stop_path.exists() {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let context = HttpRequestContext {
                            root: &root,
                            mode_path: &mode_path,
                            stop_path: &stop_path,
                            corrupt_all_ranges: &thread_corrupt_all,
                            fail_payload: &thread_fail_payload,
                            failed_payload_requests: &thread_failed_payload_requests,
                            range_requests: &thread_ranges,
                            range_bytes: &thread_range_bytes,
                            full_bytes: &thread_full_bytes,
                        };
                        handle_request(stream, &context);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            address,
            stop,
            corrupt_all_ranges,
            fail_payload,
            failed_payload_requests,
            range_requests,
            range_bytes,
            full_bytes,
            thread: Some(thread),
        }
    }

    fn corrupt_all_ranges(&self, enabled: bool) {
        self.corrupt_all_ranges.store(enabled, Ordering::Release);
    }

    fn fail_payload(&self, enabled: bool) {
        self.fail_payload.store(enabled, Ordering::Release);
    }
}

impl Drop for HttpOrigin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct HttpRequestContext<'a> {
    root: &'a Path,
    mode_path: &'a Path,
    stop_path: &'a Path,
    corrupt_all_ranges: &'a AtomicBool,
    fail_payload: &'a AtomicBool,
    failed_payload_requests: &'a AtomicU64,
    range_requests: &'a AtomicU64,
    range_bytes: &'a AtomicU64,
    full_bytes: &'a AtomicU64,
}

fn handle_request(mut stream: TcpStream, context: &HttpRequestContext<'_>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut request = Vec::new();
    let mut buffer = [0; 1024];
    while !request.windows(4).any(|part| part == b"\r\n\r\n") {
        let Ok(count) = stream.read(&mut buffer) else {
            return;
        };
        if count == 0 || request.len() + count > 16 * 1024 {
            return;
        }
        request.extend_from_slice(&buffer[..count]);
    }
    let request = String::from_utf8_lossy(&request);
    let mut lines = request.lines();
    let Some(request_line) = lines.next() else {
        return;
    };
    let head = request_line.split_whitespace().next() == Some("HEAD");
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let mut range_header = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("range")
        {
            range_header = Some(value.trim().to_owned());
        }
    }
    let relative = path
        .strip_prefix("/repo/")
        .unwrap_or(path.trim_start_matches('/'));
    let relative = percent_decode(relative);
    let relative_path = Path::new(&relative);
    if relative_path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    let is_payload = relative_path
        .file_name()
        .is_some_and(|name| name == "payload.bin");
    let mode = fs::read_to_string(context.mode_path).unwrap_or_default();
    if is_payload && (context.fail_payload.load(Ordering::Acquire) || mode.trim() == "fail") {
        if !head {
            context
                .failed_payload_requests
                .fetch_add(1, Ordering::Relaxed);
        }
        if !head && range_header.is_some() {
            context.range_requests.fetch_add(1, Ordering::Relaxed);
        }
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    let body_path = context.root.join(relative_path);
    let Ok(body) = fs::read(body_path) else {
        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    };
    let mut start = 0usize;
    let mut end = body.len().saturating_sub(1);
    let partial = if let Some(range) = range_header.as_deref().and_then(parse_range) {
        start = range.0.min(body.len());
        end = range.1.min(body.len().saturating_sub(1));
        if start > end || start >= body.len() {
            let _ = stream.write_all(b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return;
        }
        if !head {
            context.range_requests.fetch_add(1, Ordering::Relaxed);
        }
        true
    } else {
        false
    };
    let mut response_body = body[start..=end].to_vec();
    let content_len = response_body.len();
    if partial && context.corrupt_all_ranges.load(Ordering::Acquire) && !response_body.is_empty() {
        response_body[0] ^= 0xff;
    }
    let status = if partial {
        "206 Partial Content"
    } else {
        "200 OK"
    };
    let mut header =
        format!("HTTP/1.1 {status}\r\nContent-Length: {content_len}\r\nConnection: close\r\n");
    if partial {
        header.push_str(&format!(
            "Content-Range: bytes {start}-{end}/{}\r\n",
            body.len()
        ));
    }
    header.push_str("Accept-Ranges: bytes\r\n\r\n");
    if stream.write_all(header.as_bytes()).is_err() {
        return;
    }
    if head {
        return;
    }
    if partial {
        context
            .range_bytes
            .fetch_add(response_body.len() as u64, Ordering::Relaxed);
    } else {
        context
            .full_bytes
            .fetch_add(response_body.len() as u64, Ordering::Relaxed);
    }
    if is_payload && mode.trim() == "slow" {
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        for chunk in response_body.chunks(64 * 1024) {
            if context.stop_path.exists() || stream.write_all(chunk).is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(75));
        }
    } else {
        let _ = stream.write_all(&response_body);
    }
}

fn parse_range(value: &str) -> Option<(usize, usize)> {
    let bounds = value.strip_prefix("bytes=")?.split_once('-')?;
    Some((bounds.0.parse().ok()?, bounds.1.parse().ok()?))
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2]))
        {
            decoded.push(high * 16 + low);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn publish(publisher: &Path, config: &Path, output: &Path) {
    let result = Command::new(publisher)
        .args(["--json", "create"])
        .arg(config)
        .arg(output)
        .arg("--no-progress")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "publisher failed: {}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn pseudo_random_bytes(length: usize) -> Vec<u8> {
    let mut value = 0x9e37_79b9_u32;
    (0..length)
        .map(|_| {
            value ^= value << 13;
            value ^= value >> 17;
            value ^= value << 5;
            value as u8
        })
        .collect()
}

fn run_acquisition(
    root: &Path,
    instance_id: &str,
    seed: Option<&str>,
    apply: bool,
    fingerprint: Option<&str>,
) -> Value {
    let mut args = vec!["server", "content", "acquire"];
    args.push(if apply { "apply" } else { "plan" });
    args.push(instance_id);
    args.extend(["--game-space", GAME]);
    if let Some(seed) = seed {
        args.extend(["--seed-stage", seed]);
    }
    if apply {
        args.push("--yes");
        args.extend(["--expected-plan-fingerprint", fingerprint.unwrap()]);
    }
    invoke(root, &args, 0)
}

#[test]
#[ignore = "requires FOXY_REPOSITORY_PUBLISHER and isolated native Windows or Linux containment"]
fn native_repository_acquisition_patch_fallback_and_failed_generation_preservation() {
    let publisher =
        PathBuf::from(std::env::var_os("FOXY_REPOSITORY_PUBLISHER").expect(
            "set FOXY_REPOSITORY_PUBLISHER to the built foxy-server-backend-cli executable",
        ));
    assert!(
        publisher.is_file(),
        "repository publisher executable is missing"
    );
    let scratch_root = std::env::var_os("FOXY_ACQUISITION_SCRATCH").map(PathBuf::from);
    if let Some(path) = &scratch_root {
        fs::create_dir_all(path).unwrap();
    }
    let scratch = match scratch_root {
        Some(path) => tempfile::tempdir_in(path).unwrap(),
        None => tempfile::tempdir().unwrap(),
    };
    let scratch = scratch.keep();
    let root = scratch.join("foxy");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("games.json"),
        include_str!("../examples/json/appdata/games.json"),
    )
    .unwrap();
    let source = scratch.join("source");
    let addon_dir = source.join(ADDON);
    fs::create_dir_all(&addon_dir).unwrap();
    let original = pseudo_random_bytes(8 << 20);
    fs::write(addon_dir.join("payload.bin"), &original).unwrap();
    let config = scratch.join("publisher.json");
    fs::write(
        &config,
        serde_json::to_vec(&json!({
            "repoName":"RepoAlpha", "basePath":source, "chunking":"content",
            "requiredMods":[{"modName":ADDON}]
        }))
        .unwrap(),
    )
    .unwrap();
    let output = scratch.join("published");
    publish(&publisher, &config, &output);
    let origin = HttpOrigin::start(output.clone(), scratch.clone());
    let instance_id = add_instance(&root);
    register_download_profile(&root, &origin.address);
    save_download_binding(&root, &instance_id);
    let profile_path = root
        .join("repository-profile")
        .join(ADDON)
        .join("payload.bin");
    let profile_bytes = fs::read(&profile_path).unwrap();

    let plan_a = run_acquisition(&root, &instance_id, None, false, None);
    let fingerprint_a = plan_a["data"]["plan"]["fingerprint"].as_str().unwrap();
    let acquired_a = run_acquisition(&root, &instance_id, None, true, Some(fingerprint_a));
    let stage_a_report = &acquired_a["data"]["outcome"]["stage"];
    let stage_a = stage_a_report["stage_id"].as_str().unwrap().to_owned();
    assert_eq!(acquired_a["data"]["outcome"]["status"], "complete");
    assert_eq!(
        acquired_a["data"]["outcome"]["client_database_opened"],
        false
    );
    assert!(
        acquired_a["data"]["outcome"]["scratch_database_opened"]
            .as_bool()
            .unwrap()
    );

    let mut changed = original.clone();
    changed[5 * 1024 * 1024..5 * 1024 * 1024 + 100].fill(0xa5);
    fs::write(addon_dir.join("payload.bin"), &changed).unwrap();
    publish(&publisher, &config, &output);
    let b_ranges_before = origin.range_requests.load(Ordering::Relaxed);
    let b_range_bytes_before = origin.range_bytes.load(Ordering::Relaxed);
    let plan_b = run_acquisition(&root, &instance_id, Some(&stage_a), false, None);
    let fingerprint_b = plan_b["data"]["plan"]["fingerprint"].as_str().unwrap();
    let acquired_b = run_acquisition(
        &root,
        &instance_id,
        Some(&stage_a),
        true,
        Some(fingerprint_b),
    );
    assert_eq!(acquired_b["data"]["outcome"]["status"], "complete");
    let item_b = &acquired_b["data"]["outcome"]["items"][0];
    assert!(item_b["patch_files_planned"].as_u64().unwrap() > 0);
    assert!(
        item_b["planned_transfer_bytes"].as_u64().unwrap()
            < item_b["full_download_bytes"].as_u64().unwrap()
    );
    let b_range_bytes = origin.range_bytes.load(Ordering::Relaxed) - b_range_bytes_before;
    assert!(origin.range_requests.load(Ordering::Relaxed) > b_ranges_before);
    assert!(b_range_bytes < changed.len() as u64);
    println!(
        "B delta planned={} full={} ranged={}",
        item_b["planned_transfer_bytes"].as_u64().unwrap(),
        item_b["full_download_bytes"].as_u64().unwrap(),
        b_range_bytes
    );
    let stage_b_report = &acquired_b["data"]["outcome"]["stage"];
    let stage_b = stage_b_report["stage_id"].as_str().unwrap();
    let stage_b_dir = PathBuf::from(stage_b_report["directory"].as_str().unwrap());
    let stage_manifest = stage_b_dir.join("addons").join(ADDON).join("payload.bin");
    assert_eq!(fs::read(stage_manifest).unwrap(), changed);

    let mut fallback_content = changed.clone();
    fallback_content[6 * 1024 * 1024..6 * 1024 * 1024 + 100].fill(0x5a);
    fs::write(addon_dir.join("payload.bin"), &fallback_content).unwrap();
    publish(&publisher, &config, &output);
    origin.corrupt_all_ranges(true);
    let c_ranges_before = origin.range_requests.load(Ordering::Relaxed);
    let c_range_bytes_before = origin.range_bytes.load(Ordering::Relaxed);
    let c_full_bytes_before = origin.full_bytes.load(Ordering::Relaxed);
    let plan_c = run_acquisition(&root, &instance_id, Some(stage_b), false, None);
    let fingerprint_c = plan_c["data"]["plan"]["fingerprint"].as_str().unwrap();
    let acquired_c = run_acquisition(
        &root,
        &instance_id,
        Some(stage_b),
        true,
        Some(fingerprint_c),
    );
    assert_eq!(acquired_c["data"]["outcome"]["status"], "complete");
    let item_c = &acquired_c["data"]["outcome"]["items"][0];
    assert!(item_c["patch_files_planned"].as_u64().unwrap() > 0);
    assert!(
        item_c["planned_transfer_bytes"].as_u64().unwrap()
            < item_c["full_download_bytes"].as_u64().unwrap()
    );
    let c_range_bytes = origin.range_bytes.load(Ordering::Relaxed) - c_range_bytes_before;
    let c_full_bytes = origin.full_bytes.load(Ordering::Relaxed) - c_full_bytes_before;
    assert!(origin.range_requests.load(Ordering::Relaxed) > c_ranges_before);
    assert!(c_range_bytes < fallback_content.len() as u64);
    assert!(c_full_bytes >= fallback_content.len() as u64);
    println!(
        "C corrupt-range fallback ranged={} full={}",
        c_range_bytes, c_full_bytes
    );
    let stage_c_report = &acquired_c["data"]["outcome"]["stage"];
    let stage_c = stage_c_report["stage_id"].as_str().unwrap();
    let stage_c_dir = PathBuf::from(stage_c_report["directory"].as_str().unwrap());
    assert_eq!(
        fs::read(stage_c_dir.join("addons").join(ADDON).join("payload.bin")).unwrap(),
        fallback_content
    );

    origin.corrupt_all_ranges(false);
    let mut failed_content = fallback_content.clone();
    failed_content[7 * 1024 * 1024..7 * 1024 * 1024 + 100].fill(0x3c);
    fs::write(addon_dir.join("payload.bin"), &failed_content).unwrap();
    publish(&publisher, &config, &output);
    let mut before_failed = inventory(&root);
    let profile_before_failed = fs::read(&profile_path).unwrap();
    origin.fail_payload(true);
    let failed_plan = run_acquisition(&root, &instance_id, Some(stage_c), false, None);
    let failed_fingerprint = failed_plan["data"]["plan"]["fingerprint"].as_str().unwrap();
    let failed_requests_before = origin.failed_payload_requests.load(Ordering::Relaxed);
    let mut failed_command = Command::new(env!("CARGO_BIN_EXE_Foxy"));
    failed_command.arg("--config-dir").arg(&root).args([
        "--json",
        "server",
        "content",
        "acquire",
        "apply",
        &instance_id,
        "--game-space",
        GAME,
        "--seed-stage",
        stage_c,
        "--yes",
        "--expected-plan-fingerprint",
        failed_fingerprint,
    ]);
    failed_command.env("FOXY_CONFIG_DIR", &root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        failed_command.creation_flags(0x08000000);
    }
    let failed_output = failed_command.output().unwrap();
    assert_eq!(failed_output.status.code(), Some(4));
    let failed_value: Value = serde_json::from_slice(&failed_output.stdout).unwrap();
    fs::write(scratch.join("failed-outcome.json"), &failed_output.stdout).unwrap();
    assert_eq!(failed_value["ok"], true);
    assert_eq!(failed_value["data"]["outcome"]["status"], "failed");
    assert!(failed_value["data"]["outcome"]["stage"].is_null());
    assert!(
        origin.failed_payload_requests.load(Ordering::Relaxed) > failed_requests_before,
        "changed publication failed before an HTTP payload rejection: {}",
        failed_value["data"]["outcome"]
    );
    let mut after_failed = inventory(&root);
    let owner_path = PathBuf::from("games").join(GAME).join("database.owner");
    for files in [&mut before_failed, &mut after_failed] {
        let owner = files.remove(&owner_path).unwrap();
        assert!(
            std::str::from_utf8(&owner)
                .unwrap()
                .parse::<u32>()
                .is_ok_and(|pid| pid > 0)
        );
    }
    assert_eq!(after_failed, before_failed);
    println!("D permanent payload failure retained prior inventory=true");
    assert_eq!(fs::read(&profile_path).unwrap(), profile_before_failed);
    assert_eq!(fs::read(&profile_path).unwrap(), profile_bytes);
    assert_eq!(
        fs::read(addon_dir.join("payload.bin")).unwrap(),
        failed_content
    );
    println!(
        "Native acquisition evidence retained for case {}",
        scratch.file_name().unwrap().to_string_lossy()
    );
}

#[test]
#[ignore = "manual GUI cancellation fixture; requires FOXY_ACQUISITION_GUI_ROOT and FOXY_REPOSITORY_PUBLISHER"]
fn serve_acquisition_fixture() {
    let fixture_root = PathBuf::from(
        std::env::var_os("FOXY_ACQUISITION_GUI_ROOT")
            .expect("set FOXY_ACQUISITION_GUI_ROOT to an isolated short absolute directory"),
    );
    assert!(fixture_root.is_absolute(), "fixture root must be absolute");
    fs::create_dir_all(&fixture_root).unwrap();
    assert!(
        fs::read_dir(&fixture_root).unwrap().next().is_none(),
        "fixture root must be empty"
    );
    let publisher =
        PathBuf::from(std::env::var_os("FOXY_REPOSITORY_PUBLISHER").expect(
            "set FOXY_REPOSITORY_PUBLISHER to the built foxy-server-backend-cli executable",
        ));
    assert!(
        publisher.is_file(),
        "repository publisher executable is missing"
    );

    let config_dir = fixture_root.join("foxy");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("games.json"),
        include_str!("../examples/json/appdata/games.json"),
    )
    .unwrap();
    let source = fixture_root.join("source");
    let addon_dir = source.join(ADDON);
    fs::create_dir_all(&addon_dir).unwrap();
    fs::write(addon_dir.join("payload.bin"), pseudo_random_bytes(8 << 20)).unwrap();
    let config = fixture_root.join("publisher.json");
    fs::write(
        &config,
        serde_json::to_vec(&json!({
            "repoName":"RepoAlpha", "basePath":source, "chunking":"content",
            "requiredMods":[{"modName":ADDON}]
        }))
        .unwrap(),
    )
    .unwrap();
    let published = fixture_root.join("published");
    publish(&publisher, &config, &published);
    let origin = HttpOrigin::start(published, fixture_root.clone());
    let instance_id = add_instance(&config_dir);
    register_download_profile(&config_dir, &origin.address);
    save_download_binding(&config_dir, &instance_id);
    fs::write(fixture_root.join("origin-mode.txt"), "normal").unwrap();
    let initial_plan = run_acquisition(&config_dir, &instance_id, None, false, None);
    let initial_fingerprint = initial_plan["data"]["plan"]["fingerprint"]
        .as_str()
        .unwrap();
    let initial = run_acquisition(
        &config_dir,
        &instance_id,
        None,
        true,
        Some(initial_fingerprint),
    );
    assert_eq!(initial["data"]["outcome"]["status"], "complete");
    let seed_stage = initial["data"]["outcome"]["stage"]["stage_id"]
        .as_str()
        .unwrap();
    fs::write(
        fixture_root.join("gui-info.json"),
        serde_json::to_vec_pretty(&json!({
            "config_dir": config_dir,
            "instance_id": instance_id,
            "seed_stage": seed_stage,
            "repository_url": origin.address,
            "source_path": addon_dir.join("payload.bin"),
            "profile_path": config_dir.join("repository-profile").join(ADDON).join("payload.bin")
        }))
        .unwrap(),
    )
    .unwrap();
    println!("Acquisition GUI fixture ready");
    while !fixture_root.join("stop-server").exists() {
        thread::sleep(Duration::from_millis(100));
    }
    drop(origin);
}
