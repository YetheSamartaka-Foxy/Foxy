pub(super) struct Observation {
    pub version: String,
    pub ready: bool,
    pub build: Option<&'static str>,
    pub distribution: Option<&'static str>,
}

pub(super) fn query_info(port: u16, hostname: &str) -> bool {
    use std::net::{Ipv4Addr, UdpSocket};
    use std::time::Duration;

    let Ok(socket) = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)) else {
        return false;
    };
    if socket.connect((Ipv4Addr::LOCALHOST, port)).is_err()
        || socket
            .set_read_timeout(Some(Duration::from_millis(50)))
            .is_err()
        || socket
            .set_write_timeout(Some(Duration::from_millis(50)))
            .is_err()
    {
        return false;
    }
    let initial = b"\xff\xff\xff\xffTSource Engine Query\0";
    let mut request = initial.to_vec();
    let mut buffer = [0; 4096];
    for _ in 0..2 {
        if socket.send(&request).is_err() {
            return false;
        }
        let Ok(length) = socket.recv(&mut buffer) else {
            return false;
        };
        let reply = &buffer[..length];
        if length == 9 && reply.starts_with(b"\xff\xff\xff\xffA") {
            request = initial.to_vec();
            request.extend_from_slice(&reply[5..]);
        } else {
            return info_matches(reply, hostname.as_bytes());
        }
    }
    false
}

fn info_matches(mut reply: &[u8], hostname: &[u8]) -> bool {
    if !reply.starts_with(b"\xff\xff\xff\xffI") || reply.len() < 6 {
        return false;
    }
    reply = &reply[6..];
    for index in 0..4 {
        let Some(end) = reply.iter().position(|byte| *byte == 0) else {
            return false;
        };
        if end > 256 || (index == 0 && &reply[..end] != hostname) {
            return false;
        }
        reply = &reply[end + 1..];
    }
    if reply.len() < 10 || reply[5] != b'd' || !matches!(reply[6], b'w' | b'l') {
        return false;
    }
    reply[9..]
        .iter()
        .take(129)
        .position(|byte| *byte == 0)
        .is_some_and(|end| end > 0)
}

#[test]
fn query_identity_requires_complete_info_from_the_expected_server_name() {
    let mut packet = b"\xff\xff\xff\xffI\x11MainServer\0\0Arma3\0Arma 3\0".to_vec();
    packet.extend_from_slice(&[0, 0, 0, 1, 0, b'd', b'w', 0, 0]);
    packet.extend_from_slice(b"2.22.150000\0");
    assert!(info_matches(&packet, b"MainServer"));
    assert!(!info_matches(&packet, b"OtherServer"));
    assert!(!info_matches(&packet[..packet.len() - 1], b"MainServer"));
    assert!(!info_matches(b"\xff\xff\xff\xffA1234", b"MainServer"));
}

pub(super) fn parse_report(text: &str, port: u16) -> Observation {
    let mut observation = Observation {
        version: String::new(),
        ready: false,
        build: None,
        distribution: None,
    };
    for line in text.lines() {
        let line = line.trim();
        if let Some(console) = line.strip_prefix("Arma 3 Console version ")
            && let Some((version, tail)) = console.split_once(" x64 : port ")
            && tail.parse::<u16>() == Ok(port)
            && valid_version(version)
        {
            observation.version = version.into();
        }
        if let Some(version) = line.strip_prefix("Version: ")
            && valid_version(version)
        {
            observation.version = version.into();
        }
        if line == "Build: Stable" {
            observation.build = Some("stable");
        }
        if line == "Build: Development" {
            observation.build = Some("development");
        }
        if line == "Type: Public" {
            observation.distribution = Some("public");
        }
        if let Some((_, tail)) = line.split_once("Game Port: ") {
            let number = tail
                .split(|character: char| !character.is_ascii_digit())
                .next()
                .unwrap_or_default();
            observation.ready |= number.parse::<u16>() == Ok(port);
        }
    }
    observation.ready &= !observation.version.is_empty();
    observation
}

fn valid_version(version: &str) -> bool {
    let parts = version.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 10
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && part.parse::<u32>().is_ok()
        })
}

#[test]
fn native_linux_console_version_is_tied_to_the_exact_requested_port() {
    let console = "Arma 3 Console version 2.22.154089 x64 : port 2302\n";
    assert_eq!(parse_report(console, 2302).version, "2.22.154089");
    assert!(parse_report(console, 230).version.is_empty());
    assert!(
        parse_report(&console.replace("154089", "unknown"), 2302)
            .version
            .is_empty()
    );
    assert_eq!(parse_report(console, 2302).build, None);
}

#[test]
fn report_requires_native_version_and_the_exact_requested_game_port() {
    let text = "Type: Public\nBuild: Stable\nVersion: 2.22.150000\n12:00:00 Game Port: 2302, Steam Query Port: 2303\n";
    let observation = parse_report(text, 2302);
    assert!(observation.ready);
    assert_eq!(observation.version, "2.22.150000");
    assert_eq!(observation.build, Some("stable"));
    assert!(!parse_report(text, 230).ready);
    assert!(
        !parse_report(
            &text.replace("Version: 2.22.150000", "Version: unknown"),
            2302
        )
        .ready
    );
}
