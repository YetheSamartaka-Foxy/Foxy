use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const SINGLE: [u8; 4] = [0xff; 4];
const SPLIT: [u8; 4] = [0xfe, 0xff, 0xff, 0xff];
const MAX_DATAGRAM: usize = 4096;
const MAX_FRAGMENTS: u8 = 64;
const MAX_REPLY: usize = 256 * 1024;
const MAX_STRING: usize = 4096;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerQueryInfo {
    pub protocol: u8,
    pub name: String,
    pub map: String,
    pub folder: String,
    pub game: String,
    pub app_id: u16,
    pub players: u8,
    pub max_players: u8,
    pub bots: u8,
    pub server_type: u8,
    pub environment: u8,
    pub visibility: u8,
    pub vac: u8,
    pub version: String,
    pub port: Option<u16>,
    pub steam_id: Option<u64>,
    pub keywords: Option<String>,
    pub game_id: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryError {
    Cancelled,
    Timeout,
    Protocol(String),
    Network(String),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("Server query cancelled"),
            Self::Timeout => f.write_str("Server query timed out"),
            Self::Protocol(message) | Self::Network(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for QueryError {}

pub fn query_info(
    target: SocketAddr,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<ServerQueryInfo, QueryError> {
    let socket = UdpSocket::bind(if target.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })
    .map_err(network_error)?;
    socket.connect(target).map_err(network_error)?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(QueryError::Timeout)?;
    let mut request = Vec::from(SINGLE);
    request.push(0x54);
    request.extend_from_slice(b"Source Engine Query\0");
    let mut challenged = false;
    loop {
        let response = exchange_connected(&socket, &request, deadline, cancelled)?;
        if response.starts_with(&SINGLE) && response.get(4) == Some(&0x41) {
            if challenged || response.len() != 9 {
                return Err(protocol("Invalid or repeated A2S challenge"));
            }
            challenged = true;
            request = Vec::from(SINGLE);
            request.push(0x54);
            request.extend_from_slice(b"Source Engine Query\0");
            request.extend_from_slice(&response[5..9]);
            continue;
        }
        return parse_info_reply(&response);
    }
}

pub fn parse_info_reply(data: &[u8]) -> Result<ServerQueryInfo, QueryError> {
    if data.len() < 6 || !data.starts_with(&SINGLE) || data[4] != 0x49 {
        return Err(protocol("Invalid A2S_INFO response header"));
    }
    let mut cursor = 5;
    let protocol_version = take_u8(data, &mut cursor)?;
    let name = take_string(data, &mut cursor)?;
    let map = take_string(data, &mut cursor)?;
    let folder = take_string(data, &mut cursor)?;
    let game = take_string(data, &mut cursor)?;
    let app_id = take_u16(data, &mut cursor)?;
    let players = take_u8(data, &mut cursor)?;
    let max_players = take_u8(data, &mut cursor)?;
    let bots = take_u8(data, &mut cursor)?;
    let server_type = take_u8(data, &mut cursor)?;
    let environment = take_u8(data, &mut cursor)?;
    let visibility = take_u8(data, &mut cursor)?;
    let vac = take_u8(data, &mut cursor)?;
    let version = take_string(data, &mut cursor)?;
    let mut result = ServerQueryInfo {
        protocol: protocol_version,
        name,
        map,
        folder,
        game,
        app_id,
        players,
        max_players,
        bots,
        server_type,
        environment,
        visibility,
        vac,
        version,
        port: None,
        steam_id: None,
        keywords: None,
        game_id: None,
    };
    if cursor < data.len() {
        let edf = take_u8(data, &mut cursor)?;
        if edf & 0x80 != 0 {
            result.port = Some(take_u16(data, &mut cursor)?);
        }
        if edf & 0x10 != 0 {
            result.steam_id = Some(take_u64(data, &mut cursor)?);
        }
        if edf & 0x40 != 0 {
            let _ = take_u16(data, &mut cursor)?;
            let _ = take_string(data, &mut cursor)?;
        }
        if edf & 0x20 != 0 {
            result.keywords = Some(take_string(data, &mut cursor)?);
        }
        if edf & 0x01 != 0 {
            result.game_id = Some(take_u64(data, &mut cursor)?);
        }
        if edf & !0xf1 != 0 {
            return Err(protocol("Unsupported A2S_INFO EDF flags"));
        }
    }
    if cursor != data.len() {
        return Err(protocol("Trailing bytes in A2S_INFO response"));
    }
    Ok(result)
}

pub(super) fn exchange_for_rules(
    socket: &UdpSocket,
    request: &[u8],
    timeout: Duration,
) -> io::Result<Vec<u8>> {
    let cancelled = AtomicBool::new(false);
    exchange_connected(socket, request, Instant::now() + timeout, &cancelled).map_err(|error| {
        io::Error::new(
            match &error {
                QueryError::Timeout => io::ErrorKind::TimedOut,
                QueryError::Protocol(_) => io::ErrorKind::InvalidData,
                _ => io::ErrorKind::Other,
            },
            error.to_string(),
        )
    })
}

fn exchange_connected(
    socket: &UdpSocket,
    request: &[u8],
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, QueryError> {
    let mut fragments: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
    let mut fragment_id = None;
    let mut fragment_total = None;
    let mut fragment_size = None;
    let mut joined_size = 0usize;
    check_cancel(cancelled, deadline)?;
    socket.send(request).map_err(network_error)?;
    let mut buf = [0u8; MAX_DATAGRAM + 1];
    loop {
        check_cancel(cancelled, deadline)?;
        socket
            .set_read_timeout(Some(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(100))
                    .max(Duration::from_millis(1)),
            ))
            .map_err(network_error)?;
        match socket.recv(&mut buf) {
            Ok(len) if len > 0 => {
                if len > MAX_DATAGRAM {
                    return Err(protocol("A2S datagram exceeds size limit"));
                }
                let packet = &buf[..len];
                if packet.starts_with(&SINGLE) {
                    return Ok(packet.to_vec());
                }
                if !packet.starts_with(&SPLIT) {
                    continue;
                }
                if len < 12 {
                    return Err(protocol("Malformed A2S fragment"));
                }
                let id = u32::from_le_bytes(packet[4..8].try_into().unwrap());
                if id & 0x8000_0000 != 0 {
                    return Err(protocol("Compressed A2S fragments are unsupported"));
                }
                let total = packet[8];
                let number = packet[9];
                let size = u16::from_le_bytes([packet[10], packet[11]]);
                if total == 0 || total > MAX_FRAGMENTS || number >= total || size == 0 {
                    return Err(protocol("Invalid A2S fragment metadata"));
                }
                if fragment_id.is_some_and(|value| value != id)
                    || fragment_total.is_some_and(|value| value != total)
                    || fragment_size.is_some_and(|value| value != size)
                {
                    return Err(protocol("Inconsistent A2S fragments"));
                }
                fragment_id = Some(id);
                fragment_total = Some(total);
                fragment_size = Some(size);
                if fragments.contains_key(&number) {
                    return Err(protocol("Repeated A2S fragment"));
                }
                let payload = packet[12..].to_vec();
                joined_size = joined_size.saturating_add(payload.len());
                if joined_size > MAX_REPLY {
                    return Err(protocol("A2S response exceeds size limit"));
                }
                fragments.insert(number, payload);
                if fragments.len() == usize::from(total) {
                    let mut joined = Vec::with_capacity(joined_size);
                    for index in 0..total {
                        joined.extend_from_slice(
                            fragments
                                .get(&index)
                                .ok_or_else(|| protocol("Missing A2S fragment"))?,
                        );
                    }
                    return Ok(joined);
                }
                continue;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                if Instant::now() >= deadline {
                    return Err(QueryError::Timeout);
                }
            }
            Err(error) => return Err(network_error(error)),
            _ => return Err(protocol("Empty A2S datagram")),
        }
    }
}

fn check_cancel(cancelled: &AtomicBool, deadline: Instant) -> Result<(), QueryError> {
    if cancelled.load(Ordering::Relaxed) {
        Err(QueryError::Cancelled)
    } else if Instant::now() >= deadline {
        Err(QueryError::Timeout)
    } else {
        Ok(())
    }
}
fn take_u8(data: &[u8], cursor: &mut usize) -> Result<u8, QueryError> {
    let value = *data
        .get(*cursor)
        .ok_or_else(|| protocol("Truncated A2S_INFO field"))?;
    *cursor += 1;
    Ok(value)
}
fn take_u16(data: &[u8], cursor: &mut usize) -> Result<u16, QueryError> {
    let end = (*cursor).saturating_add(2);
    let bytes = data
        .get(*cursor..end)
        .ok_or_else(|| protocol("Truncated A2S_INFO field"))?;
    *cursor = end;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}
fn take_u64(data: &[u8], cursor: &mut usize) -> Result<u64, QueryError> {
    let end = (*cursor).saturating_add(8);
    let bytes = data
        .get(*cursor..end)
        .ok_or_else(|| protocol("Truncated A2S_INFO field"))?;
    *cursor = end;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}
fn take_string(data: &[u8], cursor: &mut usize) -> Result<String, QueryError> {
    let tail = data
        .get(*cursor..)
        .ok_or_else(|| protocol("Truncated A2S_INFO string"))?;
    let end = tail
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| protocol("Unterminated A2S_INFO string"))?;
    if end > MAX_STRING {
        return Err(protocol("A2S_INFO string exceeds size limit"));
    }
    let value = std::str::from_utf8(&tail[..end])
        .map_err(|_| protocol("Invalid UTF-8 in A2S_INFO string"))?
        .to_owned();
    *cursor += end + 1;
    Ok(value)
}
fn protocol(message: &str) -> QueryError {
    QueryError::Protocol(message.to_string())
}
fn network_error(error: io::Error) -> QueryError {
    if error.kind() == io::ErrorKind::TimedOut || error.kind() == io::ErrorKind::WouldBlock {
        QueryError::Timeout
    } else {
        QueryError::Network(format!("Server query network error: {}", error.kind()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket;
    use std::thread;

    fn info_packet() -> Vec<u8> {
        let mut packet = vec![0xff; 4];
        packet.extend_from_slice(&[0x49, 17]);
        packet.extend_from_slice(b"Server\0Map\0folder\0Game\0");
        packet.extend_from_slice(&1234u16.to_le_bytes());
        packet.extend_from_slice(&[3, 16, 1, b'd', b'l', 0, 1]);
        packet.extend_from_slice(b"1.2.3\0");
        packet.push(0xB1);
        packet.extend_from_slice(&27015u16.to_le_bytes());
        packet.extend_from_slice(&99u64.to_le_bytes());
        packet.extend_from_slice(b"key=value\0");
        packet.extend_from_slice(&0x1234u64.to_le_bytes());
        packet
    }

    #[test]
    fn parses_full_info_and_edf_fields_in_order() {
        let parsed = parse_info_reply(&info_packet()).expect("valid INFO");
        assert_eq!(parsed.protocol, 17);
        assert_eq!(parsed.name, "Server");
        assert_eq!(parsed.app_id, 1234);
        assert_eq!(parsed.players, 3);
        assert_eq!(parsed.server_type, b'd');
        assert_eq!(parsed.visibility, 0);
        assert_eq!(parsed.port, Some(27015));
        assert_eq!(parsed.steam_id, Some(99));
        assert_eq!(parsed.keywords.as_deref(), Some("key=value"));
        assert_eq!(parsed.game_id, Some(0x1234));
    }

    #[test]
    fn rejects_truncated_invalid_utf8_and_trailing_info() {
        let packet = info_packet();
        assert!(matches!(
            parse_info_reply(&packet[..packet.len() - 1]),
            Err(QueryError::Protocol(_))
        ));
        let mut invalid = packet.clone();
        invalid[6] = 0xff;
        assert!(matches!(
            parse_info_reply(&invalid),
            Err(QueryError::Protocol(_))
        ));
        let mut trailing = packet;
        trailing.push(1);
        assert!(matches!(
            parse_info_reply(&trailing),
            Err(QueryError::Protocol(_))
        ));
    }

    #[test]
    fn info_query_answers_one_challenge() {
        let server = UdpSocket::bind("127.0.0.1:0").expect("bind fixture");
        let target = server.local_addr().expect("fixture address");
        let worker = thread::spawn(move || {
            let mut buf = [0u8; 128];
            let (_, peer) = server.recv_from(&mut buf).expect("initial query");
            let mut challenge = vec![0xff; 4];
            challenge.push(0x41);
            challenge.extend_from_slice(&[1, 2, 3, 4]);
            server.send_to(&challenge, peer).expect("send challenge");
            let (len, peer) = server.recv_from(&mut buf).expect("challenged query");
            assert!(buf[..len].ends_with(&[1, 2, 3, 4]));
            server.send_to(&info_packet(), peer).expect("send info");
        });
        let info = query_info(target, Duration::from_secs(1), &AtomicBool::new(false))
            .expect("INFO query");
        assert_eq!(info.name, "Server");
        worker.join().expect("fixture worker");
    }

    #[test]
    fn connected_query_ignores_datagrams_from_other_peers() {
        let server = UdpSocket::bind("127.0.0.1:0").expect("bind fixture");
        let target = server.local_addr().expect("fixture address");
        let worker = thread::spawn(move || {
            let mut buf = [0u8; 128];
            let (_, peer) = server.recv_from(&mut buf).expect("query");
            let noise = UdpSocket::bind("127.0.0.1:0").expect("bind noise peer");
            noise
                .send_to(&info_packet(), peer)
                .expect("send wrong peer");
            server.send_to(&info_packet(), peer).expect("send response");
        });
        let info = query_info(target, Duration::from_secs(1), &AtomicBool::new(false))
            .expect("INFO query");
        assert_eq!(info.name, "Server");
        worker.join().expect("fixture worker");
    }

    #[test]
    fn rejects_repeated_challenge_and_oversized_datagram() {
        let server = UdpSocket::bind("127.0.0.1:0").expect("bind fixture");
        let target = server.local_addr().expect("fixture address");
        let worker = thread::spawn(move || {
            let mut buf = [0u8; 128];
            let (_, peer) = server.recv_from(&mut buf).expect("initial query");
            let mut challenge = vec![0xff; 4];
            challenge.push(0x41);
            challenge.extend_from_slice(&[1, 2, 3, 4]);
            server.send_to(&challenge, peer).expect("first challenge");
            let (_, peer) = server.recv_from(&mut buf).expect("challenged query");
            server
                .send_to(&challenge, peer)
                .expect("repeated challenge");
        });
        assert!(matches!(
            query_info(target, Duration::from_secs(1), &AtomicBool::new(false)),
            Err(QueryError::Protocol(_))
        ));
        worker.join().expect("fixture worker");

        let server = UdpSocket::bind("127.0.0.1:0").expect("bind fixture");
        let target = server.local_addr().expect("fixture address");
        let worker = thread::spawn(move || {
            let mut buf = [0u8; 128];
            let (_, peer) = server.recv_from(&mut buf).expect("query");
            let oversized = vec![0xff; MAX_DATAGRAM + 1];
            server
                .send_to(&oversized, peer)
                .expect("oversized datagram");
        });
        assert!(matches!(
            query_info(target, Duration::from_secs(1), &AtomicBool::new(false)),
            Err(QueryError::Protocol(_))
        ));
        worker.join().expect("fixture worker");
    }

    #[test]
    fn connected_exchange_ignores_noise_and_reassembles_ordered_fragments() {
        let server = UdpSocket::bind("127.0.0.1:0").expect("bind fixture");
        let target = server.local_addr().expect("fixture address");
        let worker = thread::spawn(move || {
            let mut buf = [0u8; 128];
            let (_, peer) = server.recv_from(&mut buf).expect("request");
            server.send_to(b"noise", peer).expect("send noise");
            for (number, payload) in [(1u8, b"cd".as_slice()), (0u8, b"ab".as_slice())] {
                let mut fragment = vec![0xfe, 0xff, 0xff, 0xff];
                fragment.extend_from_slice(&7u32.to_le_bytes());
                fragment.extend_from_slice(&[2, number, 140, 0]);
                fragment.extend_from_slice(payload);
                server.send_to(&fragment, peer).expect("send fragment");
            }
        });
        let socket = UdpSocket::bind("127.0.0.1:0").expect("bind client");
        socket.connect(target).expect("connect client");
        let result = exchange_connected(
            &socket,
            b"request",
            Instant::now() + Duration::from_secs(1),
            &AtomicBool::new(false),
        )
        .expect("assembled response");
        assert_eq!(result, b"abcd");
        worker.join().expect("fixture worker");
    }

    #[test]
    fn cancellation_and_timeout_are_bounded() {
        let server = UdpSocket::bind("127.0.0.1:0").expect("bind fixture");
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            query_info(
                server.local_addr().unwrap(),
                Duration::from_secs(1),
                &cancelled
            ),
            Err(QueryError::Cancelled)
        );
        let started = Instant::now();
        assert_eq!(
            query_info(
                server.local_addr().unwrap(),
                Duration::from_millis(25),
                &AtomicBool::new(false)
            ),
            Err(QueryError::Timeout)
        );
        assert!(started.elapsed() < Duration::from_millis(500));
    }
}
