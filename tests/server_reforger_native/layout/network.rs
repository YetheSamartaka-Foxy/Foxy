use std::net::UdpSocket;
use std::time::Duration;

const QUERY: &[u8] = b"\xff\xff\xff\xff\x54Source Engine Query\0";

pub(crate) fn query_a2s_hostname(port: u16) -> Option<String> {
    let socket = UdpSocket::bind(("127.0.0.1", 0)).ok()?;
    socket.connect(("127.0.0.1", port)).ok()?;
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .ok()?;
    socket
        .set_write_timeout(Some(Duration::from_millis(200)))
        .ok()?;
    let mut request = QUERY.to_vec();
    let mut response = [0_u8; 4096];
    for _ in 0..3 {
        socket.send(&request).ok()?;
        let length = socket.recv(&mut response).ok()?;
        let packet = &response[..length];
        if packet.len() == 9 && packet.starts_with(b"\xff\xff\xff\xffA") {
            request.truncate(QUERY.len());
            request.extend_from_slice(&packet[5..9]);
            continue;
        }
        return parse_a2s_name(packet);
    }
    None
}

fn parse_a2s_name(packet: &[u8]) -> Option<String> {
    if packet.len() < 7 || !packet.starts_with(b"\xff\xff\xff\xffI") {
        return None;
    }
    let name = &packet[6..];
    let end = name.iter().position(|byte| *byte == 0)?;
    String::from_utf8(name[..end].to_vec()).ok()
}

#[cfg(test)]
mod tests {
    use super::{QUERY, parse_a2s_name};

    #[test]
    fn parses_only_a2s_info_responses() {
        let mut packet = b"\xff\xff\xff\xffI\x11".to_vec();
        packet.extend_from_slice(b"FoxyNative\0Arland\0folder\0game\0");
        assert_eq!(parse_a2s_name(&packet).as_deref(), Some("FoxyNative"));
        assert!(parse_a2s_name(b"\xff\xff\xff\xffA\x01\x02\x03\x04").is_none());
        assert!(parse_a2s_name(b"\xff\xff\xff\xffI\x11missing-terminator").is_none());
        assert!(QUERY.starts_with(b"\xff\xff\xff\xff\x54"));
    }
}
