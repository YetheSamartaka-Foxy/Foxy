use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[path = "../../src/core/server_query.rs"]
#[allow(dead_code)]
mod product_query;

#[derive(serde::Serialize)]
pub(super) struct LostReplyProof {
    target: String,
    requests_relayed: usize,
    engine_replies_received: usize,
    challenge_replies_forwarded: usize,
    info_replies_deliberately_dropped: usize,
    dropped_info_name: String,
    dropped_info_bytes: usize,
    dropped_info_blake3: String,
    query_result: &'static str,
    timeout_ms: u64,
}

pub(super) fn prove_lost_reply_timeout(
    target: SocketAddr,
    expected_name: &str,
) -> io::Result<LostReplyProof> {
    if !target.ip().is_loopback() {
        return Err(io::Error::other(
            "Native A2S loss probe target must be loopback",
        ));
    }

    let proxy = UdpSocket::bind(if target.is_ipv4() {
        "127.0.0.1:0"
    } else {
        "[::1]:0"
    })?;
    proxy.set_read_timeout(Some(Duration::from_millis(100)))?;
    let proxy_addr = proxy.local_addr()?;
    let stop = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(AtomicUsize::new(0));
    let replies = Arc::new(AtomicUsize::new(0));
    let challenges = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let dropped_info = Arc::new(Mutex::new(None::<Vec<u8>>));

    let worker_stop = Arc::clone(&stop);
    let worker_requests = Arc::clone(&requests);
    let worker_replies = Arc::clone(&replies);
    let worker_challenges = Arc::clone(&challenges);
    let worker_dropped = Arc::clone(&dropped);
    let worker_info = Arc::clone(&dropped_info);
    let worker = thread::Builder::new()
        .name("native-a2s-loss-proxy".into())
        .spawn(move || -> io::Result<()> {
            let upstream = UdpSocket::bind(if target.is_ipv4() {
                "127.0.0.1:0"
            } else {
                "[::1]:0"
            })?;
            upstream.set_read_timeout(Some(Duration::from_millis(100)))?;
            let mut buffer = [0u8; 4097];
            let mut upstream_buffer = [0u8; 4097];
            while !worker_stop.load(Ordering::Acquire) {
                match proxy.recv_from(&mut buffer) {
                    Ok((length, source)) => {
                        if length > 4096 {
                            return Err(io::Error::other("A2S proxy request exceeded bound"));
                        }
                        worker_requests.fetch_add(1, Ordering::Relaxed);
                        upstream.send_to(&buffer[..length], target)?;
                        let (reply_len, reply_source) = loop {
                            if worker_stop.load(Ordering::Acquire) {
                                return Ok(());
                            }
                            match upstream.recv_from(&mut upstream_buffer) {
                                Ok(reply) => break reply,
                                Err(error)
                                    if matches!(
                                        error.kind(),
                                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                                    ) =>
                                {
                                    continue;
                                }
                                Err(error) => return Err(error),
                            }
                        };
                        if reply_source != target || reply_len > 4096 {
                            return Err(io::Error::other(
                                "Unexpected native A2S reply source or size",
                            ));
                        }
                        worker_replies.fetch_add(1, Ordering::Relaxed);
                        let packet = &upstream_buffer[..reply_len];
                        if packet.len() >= 5 && packet[..4] == [0xff; 4] && packet[4] == 0x41 {
                            worker_challenges.fetch_add(1, Ordering::Relaxed);
                        }
                        if packet.len() >= 5 && packet[..4] == [0xff; 4] && packet[4] == 0x49 {
                            product_query::parse_info_reply(packet).map_err(|error| {
                                io::Error::other(format!(
                                    "Actual engine A2S_INFO was invalid: {error}"
                                ))
                            })?;
                            *worker_info
                                .lock()
                                .unwrap_or_else(|error| error.into_inner()) = Some(packet.to_vec());
                            worker_dropped.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        proxy.send_to(packet, source)?;
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        })?;

    const QUERY_TIMEOUT: Duration = Duration::from_millis(500);
    let cancelled = AtomicBool::new(false);
    let began = Instant::now();
    let result = product_query::query_info(proxy_addr, QUERY_TIMEOUT, &cancelled);
    let elapsed = began.elapsed();
    stop.store(true, Ordering::Release);
    let worker_result = worker
        .join()
        .map_err(|_| io::Error::other("A2S loss proxy thread panicked"))?;
    worker_result?;

    if !matches!(result, Err(product_query::QueryError::Timeout)) {
        return Err(io::Error::other(format!(
            "Product A2S query did not time out after reply loss: {result:?}"
        )));
    }
    if elapsed > Duration::from_secs(2) {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Product A2S query exceeded the bounded loss-probe deadline",
        ));
    }
    let info = dropped_info
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "Proxy received no INFO: relayed {}, replies {}, challenges {}, dropped {}",
                    requests.load(Ordering::Relaxed),
                    replies.load(Ordering::Relaxed),
                    challenges.load(Ordering::Relaxed),
                    dropped.load(Ordering::Relaxed)
                ),
            )
        })?;
    let parsed = product_query::parse_info_reply(&info).map_err(|error| {
        io::Error::other(format!("Actual engine A2S_INFO was invalid: {error}"))
    })?;
    let request_count = requests.load(Ordering::Relaxed);
    let reply_count = replies.load(Ordering::Relaxed);
    let challenge_count = challenges.load(Ordering::Relaxed);
    let dropped_count = dropped.load(Ordering::Relaxed);
    if parsed.name != expected_name || request_count == 0 || reply_count == 0 || dropped_count != 1
    {
        return Err(io::Error::other(
            "A2S loss proof lacked a genuine matching INFO reply",
        ));
    }
    let timeout_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    eprintln!(
        "Native A2S transport loss proved: requests={request_count}, engine_replies={reply_count}, challenge={challenge_count}, dropped_info={dropped_count}, query_timeout_ms={timeout_ms}"
    );
    Ok(LostReplyProof {
        target: target.to_string(),
        requests_relayed: request_count,
        engine_replies_received: reply_count,
        challenge_replies_forwarded: challenge_count,
        info_replies_deliberately_dropped: dropped_count,
        dropped_info_name: parsed.name,
        dropped_info_bytes: info.len(),
        dropped_info_blake3: blake3::hash(&info).to_hex().to_string(),
        query_result: "timeout after genuine reply deliberately dropped",
        timeout_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timeout_without_genuine_engine_info_cannot_satisfy_the_loss_proof() {
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        let error = prove_lost_reply_timeout(silent.local_addr().unwrap(), "Native fixture")
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    }

    #[test]
    fn delayed_reply_is_received_across_read_slices_before_deliberate_loss() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let target = server.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let mut request = [0; 128];
            let (_, peer) = server.recv_from(&mut request).unwrap();
            thread::sleep(Duration::from_millis(250));
            let mut reply = b"\xff\xff\xff\xffI\x11Native fixture\0Map\0folder\0Game\0".to_vec();
            reply.extend_from_slice(&1234u16.to_le_bytes());
            reply.extend_from_slice(&[0, 4, 0, b'd', b'l', 0, 0]);
            reply.extend_from_slice(b"1.0\0");
            server.send_to(&reply, peer).unwrap();
        });
        let proof = prove_lost_reply_timeout(target, "Native fixture");
        worker.join().unwrap();
        let proof = proof.unwrap();
        assert_eq!(proof.engine_replies_received, 1);
        assert_eq!(proof.info_replies_deliberately_dropped, 1);
        assert!(proof.timeout_ms >= 500);
    }
}
