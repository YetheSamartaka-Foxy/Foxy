use anyhow::{Context, Result, ensure};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose,
};
use rustls::{
    ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection, StreamOwned,
    pki_types::{PrivatePkcs8KeyDer, ServerName},
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\n{}";

struct Fixture {
    port: u16,
    ca_der: rustls::pki_types::CertificateDer<'static>,
    running: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Fixture {
    fn start() -> Result<Self> {
        let ca_key = KeyPair::generate()?;
        let mut ca_params = CertificateParams::default();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        ca_params.distinguished_name = DistinguishedName::new();
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "Foxy testkit loopback CA");
        let ca = ca_params.self_signed(&ca_key)?;
        let issuer = Issuer::new(ca_params, ca_key);

        let leaf_key = KeyPair::generate()?;
        let mut leaf_params = CertificateParams::new(vec!["127.0.0.1".to_owned()])?;
        leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let leaf = leaf_params.signed_by(&leaf_key, &issuer)?;
        let config = Arc::new(
            ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![leaf.der().clone()],
                    PrivatePkcs8KeyDer::from(leaf_key.serialize_der()).into(),
                )?,
        );

        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let running = Arc::new(AtomicBool::new(true));
        let thread_running = Arc::clone(&running);
        let thread = thread::spawn(move || {
            while thread_running.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((socket, _)) => {
                        let _ = socket.set_nonblocking(false);
                        let _ = socket.set_nodelay(true);
                        let _ = serve(socket, Arc::clone(&config));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            port,
            ca_der: ca.der().clone(),
            running,
            thread: Some(thread),
        })
    }

    fn url(&self) -> String {
        format!("https://127.0.0.1:{}/repo.json", self.port)
    }

    fn client(&self) -> Result<reqwest::blocking::Client> {
        Ok(reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .tcp_nodelay(true)
            .no_proxy()
            .tls_certs_only([reqwest::Certificate::from_der(&self.ca_der)?])
            .build()?)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(socket: TcpStream, config: Arc<ServerConfig>) -> Result<()> {
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut stream = StreamOwned::new(ServerConnection::new(config)?, socket);
    loop {
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            if stream.read(&mut byte)? == 0 {
                return Ok(());
            }
            request.push(byte[0]);
            ensure!(
                request.len() <= 16 * 1024,
                "HTTPS fixture request too large"
            );
        }
        ensure!(
            request.starts_with(b"GET /repo.json HTTP/1.1\r\n"),
            "Unexpected HTTPS fixture request"
        );
        stream.write_all(RESPONSE)?;
        stream.flush()?;
    }
}

pub fn measure(samples: usize) -> Result<Value> {
    ensure!(
        samples >= 10,
        "HTTPS calibration needs at least ten requests"
    );
    let fixture = Fixture::start()?;
    let mut roots = RootCertStore::empty();
    roots.add(fixture.ca_der.clone())?;
    let config = Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    let mut handshake = Vec::with_capacity(samples);
    for _ in 0..samples {
        let mut socket = TcpStream::connect(("127.0.0.1", fixture.port))?;
        socket.set_read_timeout(Some(Duration::from_secs(10)))?;
        socket.set_nodelay(true)?;
        let mut connection = ClientConnection::new(
            Arc::clone(&config),
            ServerName::try_from("127.0.0.1")?.to_owned(),
        )?;
        let started = Instant::now();
        while connection.is_handshaking() {
            connection.complete_io(&mut socket)?;
        }
        handshake.push(started.elapsed().as_secs_f64());
    }

    let url = fixture.url();
    let mut fresh = Vec::with_capacity(samples);
    for _ in 0..samples {
        let client = fixture.client()?;
        let started = Instant::now();
        let body = client.get(&url).send()?.error_for_status()?.bytes()?;
        ensure!(body.as_ref() == b"{}", "HTTPS fixture body changed");
        fresh.push(started.elapsed().as_secs_f64());
    }
    let client = fixture.client()?;
    client.get(&url).send()?.error_for_status()?.bytes()?;
    let mut reused = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = Instant::now();
        let body = client.get(&url).send()?.error_for_status()?.bytes()?;
        ensure!(body.as_ref() == b"{}", "HTTPS fixture body changed");
        reused.push(started.elapsed().as_secs_f64());
    }
    drop(client);
    let round = |values: &[f64]| {
        crate::calibrate::median(values)
            .map(|value| (value * 1e6).round() / 1e6)
            .context("HTTPS calibration has no samples")
    };
    Ok(json!({
        "scheme": "https",
        "host": "127.0.0.1",
        "port": fixture.port,
        "certificate_validation": "per_run_ca",
        "samples": samples,
        "handshake_s": round(&handshake)?,
        "fresh_request_s": round(&fresh)?,
        "reused_request_s": round(&reused)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_tls_requires_its_ca_and_reuses_a_connection() -> Result<()> {
        let fixture = Fixture::start()?;
        assert!(
            reqwest::blocking::Client::new()
                .get(fixture.url())
                .send()
                .is_err()
        );
        let client = fixture.client()?;
        for _ in 0..2 {
            assert_eq!(client.get(fixture.url()).send()?.bytes()?.as_ref(), b"{}");
        }
        Ok(())
    }

    #[test]
    fn calibration_records_distinct_verified_phases() -> Result<()> {
        let value = measure(10)?;
        assert_eq!(value["certificate_validation"], "per_run_ca");
        assert_eq!(value["samples"], 10);
        for field in ["handshake_s", "fresh_request_s", "reused_request_s"] {
            assert!(value[field].as_f64().is_some_and(|time| time > 0.0));
        }
        Ok(())
    }
}
