use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use super::session::{
    CommandOutcome, LoginFailure, RconPassword, Retransmit, Session, UnknownReason,
};
use super::wire::{self, Part, Reassembly, ServerPacket, WireError};

pub(crate) fn server_packet(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut covered = vec![0xFF, kind];
    covered.extend_from_slice(payload);
    let mut packet = b"BE".to_vec();
    packet.extend_from_slice(&wire::crc32(&covered).to_le_bytes());
    packet.extend_from_slice(&covered);
    packet
}

fn client_packet(datagram: &[u8]) -> (u8, Vec<u8>) {
    assert_eq!(&datagram[..2], b"BE");
    let crc = u32::from_le_bytes(datagram[2..6].try_into().unwrap());
    assert_eq!(wire::crc32(&datagram[6..]), crc);
    assert_eq!(datagram[6], 0xFF);
    (datagram[7], datagram[8..].to_vec())
}

type Handler = Box<dyn FnMut(&UdpSocket, SocketAddr, u8, &[u8]) + Send>;
type Received = Arc<Mutex<Vec<(u8, Vec<u8>)>>>;

pub(crate) struct Fixture {
    pub(crate) address: SocketAddr,
    received: Received,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Fixture {
    fn new(mut handler: Handler) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let address = socket.local_addr().unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let received = received.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut buffer = [0u8; 70_000];
                while !stop.load(Ordering::Acquire) {
                    let Ok((length, peer)) = socket.recv_from(&mut buffer) else {
                        continue;
                    };
                    let (kind, payload) = client_packet(&buffer[..length]);
                    received.lock().unwrap().push((kind, payload.clone()));
                    handler(&socket, peer, kind, &payload);
                }
            })
        };
        Self {
            address,
            received,
            stop,
            handle: Some(handle),
        }
    }

    pub(crate) fn received(&self) -> Vec<(u8, Vec<u8>)> {
        self.received.lock().unwrap().clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(crate) fn password() -> RconPassword {
    RconPassword::new("synthetic-pass".into()).unwrap()
}

pub(crate) fn accepting(
    mut reply: impl FnMut(&UdpSocket, SocketAddr, u8, &[u8]) + Send + 'static,
) -> Fixture {
    Fixture::new(Box::new(move |socket, peer, kind, payload| {
        if kind == 0x00 {
            let accepted = payload == b"synthetic-pass";
            socket
                .send_to(&server_packet(0x00, &[u8::from(accepted)]), peer)
                .unwrap();
        } else {
            reply(socket, peer, kind, payload);
        }
    }))
}

pub(crate) fn echo(socket: &UdpSocket, peer: SocketAddr, kind: u8, payload: &[u8]) {
    if kind == 0x01 {
        let mut body = vec![payload[0]];
        body.extend_from_slice(&payload[1..]);
        socket.send_to(&server_packet(0x01, &body), peer).unwrap();
    }
}

#[test]
fn checksum_matches_the_crc32_check_value_and_a_known_keepalive_packet() {
    assert_eq!(wire::crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(
        wire::command(0, b"").as_slice(),
        [0x42, 0x45, 0xBE, 0xDC, 0xC2, 0x58, 0xFF, 0x01, 0x00]
    );
}

#[test]
fn decoding_rejects_bad_magic_checksum_marker_type_and_part_headers() {
    let valid = server_packet(0x01, b"\x05players");
    assert_eq!(
        wire::decode(&valid).unwrap(),
        ServerPacket::Reply {
            sequence: 5,
            part: None,
            body: b"players".to_vec()
        }
    );
    assert_eq!(wire::decode(b"BE\0\0\0"), Err(WireError::Short));
    let mut magic = valid.clone();
    magic[1] = b'X';
    assert_eq!(wire::decode(&magic), Err(WireError::Magic));
    let mut checksum = valid.clone();
    *checksum.last_mut().unwrap() ^= 1;
    assert_eq!(wire::decode(&checksum), Err(WireError::Checksum));
    let mut marker = b"BE\0\0\0\0\xFE\x01\x05".to_vec();
    let crc = wire::crc32(&marker[6..]);
    marker[2..6].copy_from_slice(&crc.to_le_bytes());
    assert_eq!(wire::decode(&marker), Err(WireError::Marker));
    assert_eq!(
        wire::decode(&server_packet(0x07, b"")),
        Err(WireError::PacketType)
    );
    assert_eq!(
        wire::decode(&server_packet(0x00, b"\x01\x01")),
        Err(WireError::Length)
    );
    assert_eq!(
        wire::decode(&server_packet(0x00, b"\x02")),
        Err(WireError::Length)
    );
    for bad in [
        b"\x01\x00\x00\x00".as_slice(),
        b"\x01\x00\x02\x02",
        b"\x01\x00\x41\x00",
        b"\x01\x00",
        b"\x01\x00\x03",
    ] {
        assert_eq!(
            wire::decode(&server_packet(0x01, bad)),
            Err(WireError::Parts)
        );
    }
    assert_eq!(
        wire::decode(&server_packet(0x01, b"\x01\x00\x03\x02tail")).unwrap(),
        ServerPacket::Reply {
            sequence: 1,
            part: Some(Part { count: 3, index: 2 }),
            body: b"tail".to_vec()
        }
    );
    assert_eq!(
        wire::decode(&server_packet(0x02, b"\x09hello")).unwrap(),
        ServerPacket::Message {
            sequence: 9,
            body: b"hello".to_vec()
        }
    );
    assert_eq!(
        wire::decode(&server_packet(0x00, b"\x00")).unwrap(),
        ServerPacket::Login { accepted: false }
    );
}

#[test]
fn reassembly_orders_parts_ignores_exact_duplicates_and_rejects_conflicts() {
    let mut assembly = Reassembly::new(3).unwrap();
    let part = |index| Part { count: 3, index };
    assert_eq!(assembly.accept(part(2), b"c".to_vec()), Ok(None));
    assert_eq!(assembly.accept(part(0), b"a".to_vec()), Ok(None));
    assert_eq!(assembly.accept(part(0), b"a".to_vec()), Ok(None));
    assert_eq!(
        assembly.accept(part(1), b"b".to_vec()),
        Ok(Some(b"abc".to_vec()))
    );
    let mut conflict = Reassembly::new(2).unwrap();
    conflict
        .accept(Part { count: 2, index: 0 }, b"x".to_vec())
        .unwrap();
    assert_eq!(
        conflict.accept(Part { count: 2, index: 0 }, b"y".to_vec()),
        Err(WireError::Conflict)
    );
    assert_eq!(
        conflict.accept(Part { count: 3, index: 1 }, b"z".to_vec()),
        Err(WireError::Parts)
    );
    assert!(Reassembly::new(0).is_err());
    assert!(Reassembly::new(wire::MAX_PARTS + 1).is_err());
    let mut large = Reassembly::new(2).unwrap();
    assert_eq!(
        large.accept(
            Part { count: 2, index: 0 },
            vec![0; wire::MAX_RESPONSE_BYTES + 1]
        ),
        Err(WireError::Length)
    );
}

#[test]
fn passwords_are_validated_and_never_rendered() {
    let secret = "synthetic-pass";
    let password = RconPassword::new(secret.into()).unwrap();
    assert_eq!(format!("{password:?}"), "RconPassword([redacted])");
    for bad in [
        "ab",
        "has space",
        "tab\tchar",
        "\u{e9}t\u{e9}",
        &"x".repeat(65),
    ] {
        assert!(RconPassword::new(bad.into()).is_err(), "{bad:?}");
    }
    let packet = wire::login(secret.as_bytes());
    assert_eq!(client_packet(&packet), (0x00, secret.as_bytes().to_vec()));
}

#[test]
fn login_distinguishes_accepted_rejected_silent_and_closed_endpoints() {
    let server = accepting(echo);
    assert!(Session::login(server.address, &password(), Duration::from_secs(3)).is_ok());
    let wrong = RconPassword::new("other-pass".into()).unwrap();
    assert_eq!(
        Session::login(server.address, &wrong, Duration::from_secs(3)).unwrap_err(),
        LoginFailure::Rejected
    );
    let silent = Fixture::new(Box::new(|_, _, _, _| {}));
    assert_eq!(
        Session::login(silent.address, &password(), Duration::from_millis(2300)).unwrap_err(),
        LoginFailure::NoReply
    );
    let login_attempts = silent
        .received()
        .iter()
        .filter(|(kind, _)| *kind == 0x00)
        .count();
    assert!(login_attempts >= 2, "login is resent: {login_attempts}");
    let closed = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    assert_eq!(
        Session::login(closed, &password(), Duration::from_secs(3)).unwrap_err(),
        LoginFailure::Unreachable
    );
}

#[test]
fn commands_reassemble_out_of_order_parts_and_skip_noise_and_stale_replies() {
    let server = accepting(|socket, peer, kind, payload| {
        if kind != 0x01 || payload.len() < 2 {
            if kind == 0x01 {
                socket
                    .send_to(&server_packet(0x01, &payload[..1]), peer)
                    .unwrap();
            }
            return;
        }
        let sequence = payload[0];
        let stale = server_packet(0x01, &[sequence.wrapping_sub(1), b's']);
        socket.send_to(&stale, peer).unwrap();
        let mut corrupt = server_packet(0x01, &[sequence, b'x']);
        corrupt[3] ^= 0xFF;
        socket.send_to(&corrupt, peer).unwrap();
        for index in [1u8, 0, 1, 2] {
            let mut body = vec![sequence, 0x00, 3, index];
            body.extend_from_slice(format!("part{index};").as_bytes());
            socket.send_to(&server_packet(0x01, &body), peer).unwrap();
        }
    });
    let mut session = Session::login(server.address, &password(), Duration::from_secs(3)).unwrap();
    assert_eq!(
        session.command(b"", Duration::from_secs(2), Retransmit::Safe),
        CommandOutcome::Replied(Vec::new())
    );
    assert_eq!(
        session.command(b"players", Duration::from_secs(2), Retransmit::Never),
        CommandOutcome::Replied(b"part0;part1;part2;".to_vec())
    );
    let counters = session.counters();
    assert_eq!(counters.stale_replies, 1);
    assert_eq!(counters.invalid_datagrams, 1);
    assert_eq!((counters.multipart_replies, counters.reply_parts), (1, 3));
}

#[test]
fn a_lost_reply_is_unknown_and_an_effectful_command_is_sent_exactly_once() {
    let dropped = Arc::new(AtomicBool::new(false));
    let server = {
        let dropped = dropped.clone();
        accepting(move |socket, peer, kind, payload| {
            if kind != 0x01 {
                return;
            }
            if payload[1..] == *b"say -1 hello" || !dropped.swap(true, Ordering::AcqRel) {
                return;
            }
            echo(socket, peer, kind, payload);
        })
    };
    let mut session = Session::login(server.address, &password(), Duration::from_secs(3)).unwrap();
    assert_eq!(
        session.command(b"", Duration::from_secs(3), Retransmit::Safe),
        CommandOutcome::Replied(Vec::new()),
        "a safe keepalive is resent after the first reply is dropped"
    );
    assert_eq!(
        session.command(
            b"say -1 hello",
            Duration::from_millis(2500),
            Retransmit::Never
        ),
        CommandOutcome::Unknown(UnknownReason::NoReply)
    );
    let received = server.received();
    let commands: Vec<_> = received.iter().filter(|(kind, _)| *kind == 0x01).collect();
    assert_eq!(
        commands
            .iter()
            .filter(|(_, payload)| payload[1..] == *b"say -1 hello")
            .count(),
        1
    );
    assert!(
        commands
            .iter()
            .filter(|(_, payload)| payload.len() == 1)
            .count()
            >= 2
    );
    assert_eq!(
        session.command(b"next", Duration::from_secs(2), Retransmit::Never),
        CommandOutcome::Replied(b"next".to_vec()),
        "the next command uses a fresh sequence"
    );
}

#[test]
fn server_messages_are_acknowledged_deduplicated_and_bounded() {
    let server = accepting(|socket, peer, kind, payload| {
        if kind == 0x01 {
            for _ in 0..2 {
                socket
                    .send_to(&server_packet(0x02, b"\x07RCon admin #1 logged in"), peer)
                    .unwrap();
            }
            let mut long = vec![0x08];
            long.extend(std::iter::repeat_n(b'z', 5000));
            socket.send_to(&server_packet(0x02, &long), peer).unwrap();
            echo(socket, peer, kind, payload);
        }
    });
    let mut session = Session::login(server.address, &password(), Duration::from_secs(3)).unwrap();
    assert_eq!(
        session.command(b"", Duration::from_secs(2), Retransmit::Safe),
        CommandOutcome::Replied(Vec::new())
    );
    session.pump(Duration::from_millis(300)).unwrap();
    let messages = session.take_messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].text, "RCon admin #1 logged in");
    assert!(messages[1].truncated && messages[1].text.len() == super::session::MAX_MESSAGE_BYTES);
    let counters = session.counters();
    assert_eq!(counters.messages_acknowledged, 3);
    assert_eq!(counters.duplicate_messages, 1);
    std::thread::sleep(Duration::from_millis(100));
    let acks: Vec<_> = server
        .received()
        .into_iter()
        .filter(|(kind, _)| *kind == 0x02)
        .map(|(_, payload)| payload)
        .collect();
    assert_eq!(acks, vec![vec![7], vec![7], vec![8]]);
}

#[test]
fn sequence_numbers_wrap_after_255() {
    let server = accepting(echo);
    let mut session = Session::login(server.address, &password(), Duration::from_secs(3)).unwrap();
    for _ in 0..258 {
        assert_eq!(
            session.command(b"", Duration::from_secs(2), Retransmit::Safe),
            CommandOutcome::Replied(Vec::new())
        );
    }
    let sequences: Vec<u8> = server
        .received()
        .into_iter()
        .filter(|(kind, _)| *kind == 0x01)
        .map(|(_, payload)| payload[0])
        .collect();
    assert_eq!(&sequences[254..258], &[254, 255, 0, 1]);
}
