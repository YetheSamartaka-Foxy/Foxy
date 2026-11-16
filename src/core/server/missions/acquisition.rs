use std::fs::OpenOptions;
use std::io::Write;
use std::thread;
use std::time::Duration;

use reqwest::redirect::Policy;

use super::{ContentStageControl, ServerStoreError};

const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
const CANCELLATION_POLL: Duration = Duration::from_millis(100);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(300);

pub(super) fn validate_url(url: &str) -> Result<reqwest::Url, ServerStoreError> {
    let parsed =
        reqwest::Url::parse(url).map_err(|_| invalid("Mission download URL is invalid"))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(invalid(
            "Mission download URL must be an HTTP(S) URL without credentials, query, or fragment",
        ));
    }
    Ok(parsed)
}

pub(super) fn download(
    url: &str,
    template: &str,
    expected_blake3: &str,
    control: &ContentStageControl,
) -> Result<tempfile::TempDir, ServerStoreError> {
    let url = validate_url(url)?;
    validate_template(template)?;
    if expected_blake3.len() != 64
        || !expected_blake3
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid("Expected mission BLAKE3 digest is invalid"));
    }
    control.checkpoint()?;
    let control = control.clone();
    let template = template.to_owned();
    let expected_blake3 = expected_blake3.to_owned();
    thread::Builder::new()
        .name("foxy-mission-acquisition".into())
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| ServerStoreError::Io)?
                .block_on(download_async(url, template, expected_blake3, &control))
        })
        .map_err(|_| ServerStoreError::Io)?
        .join()
        .map_err(|_| ServerStoreError::Io)?
}

async fn download_async(
    url: reqwest::Url,
    template: String,
    expected_blake3: String,
    control: &ContentStageControl,
) -> Result<tempfile::TempDir, ServerStoreError> {
    control.checkpoint()?;
    control.start_phase(super::super::ContentStagePhase::Copying, 1, 0);
    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        .redirect(Policy::none())
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .build()
        .map_err(|_| ServerStoreError::Io)?;
    let request = client.get(url).send();
    tokio::pin!(request);
    let mut response = loop {
        control.checkpoint()?;
        tokio::select! {
            result = &mut request => break result.map_err(|_| ServerStoreError::Io)?,
            _ = tokio::time::sleep(CANCELLATION_POLL) => control.checkpoint()?,
        }
    };
    if !response.status().is_success() {
        return Err(invalid(
            "Mission download did not return a successful response",
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DOWNLOAD_BYTES)
    {
        return Err(invalid("Mission download exceeds 512 MiB"));
    }
    control.start_phase(
        super::super::ContentStagePhase::Copying,
        1,
        response.content_length().unwrap_or(0),
    );

    let directory = tempfile::Builder::new()
        .prefix("foxy-mission-acquisition-")
        .tempdir()
        .map_err(|_| ServerStoreError::Io)?;
    let path = directory.path().join(format!("{template}.pbo"));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| ServerStoreError::Io)?;
    let mut hasher = blake3::Hasher::new();
    let mut total = 0u64;
    loop {
        control.checkpoint()?;
        let chunk = tokio::select! {
            result = response.chunk() => result.map_err(|_| ServerStoreError::Io)?,
            _ = tokio::time::sleep(CANCELLATION_POLL) => {
                control.checkpoint()?;
                continue;
            }
        };
        let Some(chunk) = chunk else { break };
        let count = chunk.len();
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| invalid("Mission download size overflow"))?;
        if total > MAX_DOWNLOAD_BYTES {
            return Err(invalid("Mission download exceeds 512 MiB"));
        }
        output.write_all(&chunk).map_err(|_| ServerStoreError::Io)?;
        hasher.update(&chunk);
        control.record_bytes(count as u64);
    }
    control.checkpoint()?;
    output.sync_all().map_err(|_| ServerStoreError::Io)?;
    if hasher.finalize().to_hex().as_str() != expected_blake3 {
        return Err(invalid("Downloaded mission BLAKE3 digest does not match"));
    }
    control.record_file();
    Ok(directory)
}

fn validate_template(template: &str) -> Result<(), ServerStoreError> {
    let Some((name, world)) = template.split_once('.') else {
        return Err(invalid("Mission filename must use Name.World"));
    };
    let safe = |part: &str| {
        !part.is_empty()
            && part.len() <= 128
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    };
    if !safe(name) || !safe(world) || world.contains('.') {
        return Err(invalid(
            "Mission filename must use ASCII Name.World components",
        ));
    }
    Ok(())
}

fn invalid(message: &str) -> ServerStoreError {
    ServerStoreError::Invalid(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn serve(response: &'static [u8]) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request);
            stream.write_all(response).unwrap();
        });
        (format!("http://{address}/mission"), server)
    }

    fn response(status: &str, headers: &str, body: &[u8]) -> &'static [u8] {
        let mut bytes =
            format!("HTTP/1.1 {status}\r\n{headers}Connection: close\r\n\r\n").into_bytes();
        bytes.extend_from_slice(body);
        Box::leak(bytes.into_boxed_slice())
    }

    #[test]
    fn validates_only_credential_free_http_urls_without_query_or_fragment() {
        assert!(validate_url("http://repo.example.invalid:8080/missions/a.pbo").is_ok());
        for value in [
            "ftp://repo.example.invalid/a.pbo",
            "http://user@repo.example.invalid/a.pbo",
            "http://repo.example.invalid/a.pbo?token=x",
            "http://repo.example.invalid/a.pbo#part",
        ] {
            assert!(validate_url(value).is_err());
        }
    }

    #[test]
    fn downloads_exact_bytes_to_a_temporary_pbo_after_hash_verification() {
        let body = b"ordinary PBO fixture bytes";
        let digest = blake3::hash(body).to_hex().to_string();
        let response = response(
            "200 OK",
            &format!("Content-Length: {}\r\n", body.len()),
            body,
        );
        let (url, server) = serve(response);
        let control = ContentStageControl::default();
        let directory = download(&url, "Mission.Altis", &digest, &control).unwrap();
        server.join().unwrap();
        assert_eq!(
            std::fs::read(directory.path().join("Mission.Altis.pbo")).unwrap(),
            body
        );
    }

    #[test]
    fn rejects_redirects_and_declared_oversize_before_writing() {
        let (redirect, redirect_server) = serve(response(
            "302 Found",
            "Location: http://127.0.0.1:1/elsewhere\r\n",
            b"",
        ));
        assert!(
            download(
                &redirect,
                "Mission.Altis",
                &"0".repeat(64),
                &ContentStageControl::default()
            )
            .is_err()
        );
        redirect_server.join().unwrap();

        let (oversized, oversized_server) = serve(response(
            "200 OK",
            &format!("Content-Length: {}\r\n", MAX_DOWNLOAD_BYTES + 1),
            b"",
        ));
        assert!(
            download(
                &oversized,
                "Mission.Altis",
                &"0".repeat(64),
                &ContentStageControl::default()
            )
            .is_err()
        );
        oversized_server.join().unwrap();
    }

    #[test]
    fn cancellation_is_checked_between_response_chunks() {
        let body = vec![b'x'; 128 * 1024];
        let leaked_body = Box::leak(body.into_boxed_slice());
        let response = response(
            "200 OK",
            &format!("Content-Length: {}\r\n", leaked_body.len()),
            leaked_body,
        );
        let (url, server) = serve(response);
        let control = ContentStageControl::default();
        control.cancel_after_copy_bytes_for_test(1);
        assert!(matches!(
            download(&url, "Mission.Altis", &"0".repeat(64), &control),
            Err(ServerStoreError::Cancelled)
        ));
        server.join().unwrap();
    }

    #[test]
    fn cancellation_interrupts_a_stalled_body_and_joins_the_transfer() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (headers_sent, headers_received) = std::sync::mpsc::channel();
        let (release_body, body_released) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\nConnection: close\r\n\r\n")
                .unwrap();
            stream.flush().unwrap();
            headers_sent.send(()).unwrap();
            body_released.recv().unwrap();
        });
        let url = format!("http://{address}/mission");
        let control = ContentStageControl::default();
        let transfer_control = control.clone();
        let transfer = thread::spawn(move || {
            download(&url, "Mission.Altis", &"0".repeat(64), &transfer_control)
        });
        headers_received.recv().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let started = std::time::Instant::now();
        control.cancel();
        assert!(matches!(
            transfer.join().unwrap(),
            Err(ServerStoreError::Cancelled)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        release_body.send(()).unwrap();
        server.join().unwrap();
    }
}
