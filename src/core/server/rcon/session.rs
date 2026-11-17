use std::collections::VecDeque;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::wire::{self, Reassembly, ServerPacket};

/// BattlEye drops a client that sends nothing for 45 seconds.
pub(crate) const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(25);
pub(crate) const MAX_COMMAND_BYTES: usize = 1024;
pub(crate) const MAX_MESSAGES: usize = 64;
pub(crate) const MAX_MESSAGE_BYTES: usize = 2048;
const RESEND_INTERVAL: Duration = Duration::from_millis(1000);
const RECEIVE_SLICE: Duration = Duration::from_millis(100);
const SEEN_MESSAGES: usize = 32;

#[derive(Clone)]
pub(crate) struct RconPassword(Zeroizing<String>);

impl RconPassword {
    pub(crate) fn new(value: String) -> Result<Self, &'static str> {
        let value = Zeroizing::new(value);
        if !(3..=64).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err("An RCon password needs 3 to 64 visible ASCII characters without spaces.");
        }
        Ok(Self(value))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for RconPassword {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RconPassword([redacted])")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LoginFailure {
    /// The endpoint answered and refused the password.
    Rejected,
    /// Nothing answered before the deadline; RCon may be disabled or the endpoint unreachable.
    NoReply,
    /// The host reported that nothing listens on the endpoint.
    Unreachable,
    LocalSocket,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Retransmit {
    /// Resend the same sequence while waiting; only for commands without side effects.
    Safe,
    /// Transmit once; a missing reply is an unknown outcome, never a retry.
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UnknownReason {
    NoReply,
    IncompleteReply,
    InvalidReply,
    ReceiveFailed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CommandOutcome {
    Replied(Vec<u8>),
    /// Transmitted without a complete reply before the deadline; the effect may have happened.
    Unknown(UnknownReason),
    /// Not transmitted, so no effect occurred.
    NotSent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerText {
    pub received_unix_ms: i64,
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionCounters {
    pub messages_acknowledged: u64,
    pub duplicate_messages: u64,
    pub stale_replies: u64,
    pub invalid_datagrams: u64,
    pub multipart_replies: u64,
    pub reply_parts: u64,
}

pub(crate) struct Session {
    socket: UdpSocket,
    next_sequence: u8,
    last_send: Instant,
    messages: VecDeque<ServerText>,
    seen: VecDeque<(u8, u32)>,
    counters: SessionCounters,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Session")
            .field("next_sequence", &self.next_sequence)
            .field("counters", &self.counters)
            .finish_non_exhaustive()
    }
}

pub(crate) fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn refused(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset
    )
}

fn timed_out(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

fn text(body: &[u8], limit: usize) -> (String, bool) {
    let body = body.strip_suffix(&[0]).unwrap_or(body);
    let mut text = String::from_utf8_lossy(body).into_owned();
    if text.len() <= limit {
        return (text, false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}

pub(crate) fn response_text(body: &[u8]) -> String {
    text(body, wire::MAX_RESPONSE_BYTES).0
}

impl Session {
    pub(crate) fn login(
        endpoint: SocketAddr,
        password: &RconPassword,
        timeout: Duration,
    ) -> Result<Self, LoginFailure> {
        let local = match endpoint {
            SocketAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
            SocketAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
        };
        let socket = UdpSocket::bind(local).map_err(|_| LoginFailure::LocalSocket)?;
        socket
            .connect(endpoint)
            .map_err(|_| LoginFailure::LocalSocket)?;
        let packet = wire::login(password.expose().as_bytes());
        let deadline = Instant::now() + timeout;
        let mut buffer = vec![0u8; wire::MAX_DATAGRAM + 1];
        let mut next_send = Instant::now();
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(LoginFailure::NoReply);
            }
            if now >= next_send {
                match socket.send(&packet) {
                    Ok(_) => {}
                    Err(error) if refused(&error) => return Err(LoginFailure::Unreachable),
                    Err(_) => return Err(LoginFailure::LocalSocket),
                }
                next_send = now + RESEND_INTERVAL;
            }
            let wait = deadline
                .min(next_send)
                .saturating_duration_since(Instant::now());
            socket
                .set_read_timeout(Some(wait.max(Duration::from_millis(1))))
                .map_err(|_| LoginFailure::LocalSocket)?;
            match socket.recv(&mut buffer) {
                Ok(length) => {
                    if let Ok(ServerPacket::Login { accepted }) = wire::decode(&buffer[..length]) {
                        return if accepted {
                            Ok(Self {
                                socket,
                                next_sequence: 0,
                                last_send: Instant::now(),
                                messages: VecDeque::new(),
                                seen: VecDeque::new(),
                                counters: SessionCounters::default(),
                            })
                        } else {
                            Err(LoginFailure::Rejected)
                        };
                    }
                }
                Err(error) if refused(&error) => return Err(LoginFailure::Unreachable),
                Err(error) if timed_out(&error) => {}
                Err(_) => return Err(LoginFailure::LocalSocket),
            }
        }
    }

    pub(crate) fn idle(&self) -> Duration {
        self.last_send.elapsed()
    }

    pub(crate) fn counters(&self) -> SessionCounters {
        self.counters
    }

    pub(crate) fn take_messages(&mut self) -> Vec<ServerText> {
        self.messages.drain(..).collect()
    }

    pub(crate) fn command(
        &mut self,
        text: &[u8],
        timeout: Duration,
        retransmit: Retransmit,
    ) -> CommandOutcome {
        if text.len() > MAX_COMMAND_BYTES {
            return CommandOutcome::NotSent;
        }
        let sequence = self.next_sequence;
        let packet = wire::command(sequence, text);
        if self.socket.send(&packet).is_err() {
            return CommandOutcome::NotSent;
        }
        self.next_sequence = sequence.wrapping_add(1);
        self.last_send = Instant::now();
        let deadline = Instant::now() + timeout;
        let mut next_send = Instant::now() + RESEND_INTERVAL;
        let mut assembly: Option<Reassembly> = None;
        let mut buffer = vec![0u8; wire::MAX_DATAGRAM + 1];
        loop {
            let now = Instant::now();
            if now >= deadline {
                return CommandOutcome::Unknown(if assembly.is_some() {
                    UnknownReason::IncompleteReply
                } else {
                    UnknownReason::NoReply
                });
            }
            if retransmit == Retransmit::Safe && assembly.is_none() && now >= next_send {
                let _ = self.socket.send(&packet);
                self.last_send = now;
                next_send = now + RESEND_INTERVAL;
            }
            let wait = deadline
                .min(next_send)
                .saturating_duration_since(Instant::now())
                .clamp(Duration::from_millis(1), RECEIVE_SLICE);
            if self.socket.set_read_timeout(Some(wait)).is_err() {
                return CommandOutcome::Unknown(UnknownReason::ReceiveFailed);
            }
            let length = match self.socket.recv(&mut buffer) {
                Ok(length) => length,
                Err(error) if timed_out(&error) => continue,
                Err(error) if refused(&error) => {
                    std::thread::sleep(wait);
                    continue;
                }
                Err(_) => return CommandOutcome::Unknown(UnknownReason::ReceiveFailed),
            };
            match wire::decode(&buffer[..length]) {
                Ok(ServerPacket::Message { sequence, body }) => self.message(sequence, &body),
                Ok(ServerPacket::Reply {
                    sequence: actual,
                    part,
                    body,
                }) if actual == sequence => match part {
                    None if assembly.is_none() => return CommandOutcome::Replied(body),
                    None => return CommandOutcome::Unknown(UnknownReason::InvalidReply),
                    Some(part) => {
                        let current = match assembly.take() {
                            Some(current) if current.count() == part.count => Ok(current),
                            Some(_) => Err(wire::WireError::Parts),
                            None => Reassembly::new(part.count),
                        };
                        let Ok(mut current) = current else {
                            return CommandOutcome::Unknown(UnknownReason::InvalidReply);
                        };
                        match current.accept(part, body) {
                            Ok(Some(joined)) => {
                                self.counters.multipart_replies += 1;
                                self.counters.reply_parts += u64::from(part.count);
                                return CommandOutcome::Replied(joined);
                            }
                            Ok(None) => assembly = Some(current),
                            Err(_) => return CommandOutcome::Unknown(UnknownReason::InvalidReply),
                        }
                    }
                },
                Ok(ServerPacket::Reply { .. }) => self.counters.stale_replies += 1,
                Ok(ServerPacket::Login { .. }) => {}
                Err(_) => self.counters.invalid_datagrams += 1,
            }
        }
    }

    /// Receives and acknowledges server messages until `wait` elapses or the socket fails.
    pub(crate) fn pump(&mut self, wait: Duration) -> io::Result<()> {
        let deadline = Instant::now() + wait;
        let mut buffer = vec![0u8; wire::MAX_DATAGRAM + 1];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            self.socket
                .set_read_timeout(Some(remaining.max(Duration::from_millis(1))))?;
            match self.socket.recv(&mut buffer) {
                Ok(length) => match wire::decode(&buffer[..length]) {
                    Ok(ServerPacket::Message { sequence, body }) => self.message(sequence, &body),
                    Ok(ServerPacket::Reply { .. }) => self.counters.stale_replies += 1,
                    Ok(ServerPacket::Login { .. }) => {}
                    Err(_) => self.counters.invalid_datagrams += 1,
                },
                Err(error) if timed_out(&error) => return Ok(()),
                Err(error) if refused(&error) => return Err(error),
                Err(error) => return Err(error),
            }
        }
    }

    fn message(&mut self, sequence: u8, body: &[u8]) {
        if self.socket.send(&wire::acknowledge(sequence)).is_ok() {
            self.counters.messages_acknowledged += 1;
        }
        let digest = wire::crc32(body);
        if self.seen.contains(&(sequence, digest)) {
            self.counters.duplicate_messages += 1;
            return;
        }
        if self.seen.len() == SEEN_MESSAGES {
            self.seen.pop_front();
        }
        self.seen.push_back((sequence, digest));
        let (text, truncated) = text(body, MAX_MESSAGE_BYTES);
        if self.messages.len() == MAX_MESSAGES {
            self.messages.pop_front();
        }
        self.messages.push_back(ServerText {
            received_unix_ms: unix_ms(),
            text,
            truncated,
        });
    }
}
