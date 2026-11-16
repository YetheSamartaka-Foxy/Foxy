use super::{
    Capability, ClientMessage, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, Scope, ServerMessage, invalid,
    transport::{Connection, IoBudget},
    valid_id,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Signed<T> {
    body: T,
    mac: [u8; 32],
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientHello {
    version: u16,
    scope: Scope,
    client_id: String,
    nonce: [u8; 32],
    resume_after: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerHello {
    version: u16,
    scope: Scope,
    client_id: String,
    client_nonce: [u8; 32],
    server_nonce: [u8; 32],
    resume_after: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    mac: [u8; 32],
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseReceipt {
    received: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    scope: Scope,
    sequence: u64,
    body: T,
}

fn mac<T: Serialize>(key: &[u8; 32], domain: &str, body: &T) -> io::Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new_keyed(key);
    hasher.update(domain.as_bytes());
    hasher.update(&[0]);
    hasher.update(&serde_json::to_vec(body).map_err(|_| invalid("control encoding"))?);
    Ok(*hasher.finalize().as_bytes())
}

fn equal_mac(left: [u8; 32], right: [u8; 32]) -> bool {
    blake3::Hash::from(left) == blake3::Hash::from(right)
}

fn send<T: Serialize>(
    connection: &mut Connection<'_>,
    value: &T,
    budget: IoBudget<'_>,
) -> io::Result<()> {
    budget.check()?;
    let bytes = serde_json::to_vec(value).map_err(|_| invalid("control encoding"))?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(invalid("control message too large"));
    }
    connection.write_all(&(bytes.len() as u32).to_le_bytes(), budget)?;
    connection.write_all(&bytes, budget)
}

fn receive<T: DeserializeOwned>(
    connection: &mut Connection<'_>,
    budget: IoBudget<'_>,
) -> io::Result<T> {
    let mut prefix = [0; 4];
    connection.read_exact(&mut prefix, budget)?;
    let length = u32::from_le_bytes(prefix) as usize;
    if length == 0 || length > MAX_MESSAGE_BYTES {
        return Err(invalid("control message length"));
    }
    let mut bytes = vec![0; length];
    connection.read_exact(&mut bytes, budget)?;
    serde_json::from_slice(&bytes).map_err(|_| invalid("control decoding"))
}

struct Channel<'a> {
    connection: Connection<'a>,
    scope: Scope,
    key: [u8; 32],
    outgoing: u64,
    incoming: u64,
    usable: bool,
}

impl Channel<'_> {
    fn send<T: Serialize>(
        &mut self,
        body: &T,
        direction: &str,
        budget: IoBudget<'_>,
    ) -> io::Result<()> {
        if !self.usable {
            return Err(invalid("control session closed"));
        }
        self.usable = false;
        let sequence = self
            .outgoing
            .checked_add(1)
            .ok_or_else(|| invalid("control sequence exhausted"))?;
        let body = Envelope {
            scope: self.scope.clone(),
            sequence,
            body,
        };
        let signed = Signed {
            mac: mac(&self.key, direction, &body)?,
            body,
        };
        send(&mut self.connection, &signed, budget)?;
        self.outgoing = sequence;
        self.usable = true;
        Ok(())
    }

    fn receive<T: DeserializeOwned + Serialize>(
        &mut self,
        direction: &str,
        budget: IoBudget<'_>,
    ) -> io::Result<T> {
        if !self.usable {
            return Err(invalid("control session closed"));
        }
        self.usable = false;
        let signed: Signed<Envelope<T>> = receive(&mut self.connection, budget)?;
        let sequence = self
            .incoming
            .checked_add(1)
            .ok_or_else(|| invalid("control sequence exhausted"))?;
        if signed.body.scope != self.scope
            || signed.body.sequence != sequence
            || !equal_mac(signed.mac, mac(&self.key, direction, &signed.body)?)
        {
            return Err(invalid("unauthenticated or replayed control message"));
        }
        self.incoming = sequence;
        self.usable = true;
        Ok(signed.body.body)
    }
}

pub struct ClientSession<'a> {
    channel: Channel<'a>,
    pending: Option<ClientMessage>,
    event_after: u64,
}

pub struct ServerSession<'a> {
    channel: Channel<'a>,
    client_id: String,
    resume_after: u64,
}

impl<'a> ClientSession<'a> {
    pub fn authenticate(
        mut connection: Connection<'a>,
        scope: &Scope,
        capability: &Capability,
        client_id: &str,
        resume_after: u64,
        budget: IoBudget<'_>,
    ) -> io::Result<Self> {
        scope.validate()?;
        if !valid_id(client_id) {
            return Err(invalid("invalid control client identity"));
        }
        let hello = ClientHello {
            version: PROTOCOL_VERSION,
            scope: scope.clone(),
            client_id: client_id.into(),
            nonce: rand::random(),
            resume_after,
        };
        send(
            &mut connection,
            &Signed {
                mac: mac(&capability.0, "client-hello-v2", &hello)?,
                body: &hello,
            },
            budget,
        )?;
        let response: Signed<ServerHello> = receive(&mut connection, budget)?;
        if response.body.version != PROTOCOL_VERSION
            || response.body.scope != *scope
            || response.body.client_id != client_id
            || response.body.client_nonce != hello.nonce
            || response.body.resume_after != resume_after
            || !equal_mac(
                response.mac,
                mac(&capability.0, "server-hello-v2", &response.body)?,
            )
        {
            return Err(invalid("control owner authentication failed"));
        }
        let key = mac(&capability.0, "connection-v2", &(&hello, &response.body))?;
        send(
            &mut connection,
            &Proof {
                mac: mac(&key, "client-finish-v2", &())?,
            },
            budget,
        )?;
        let ready: Proof = receive(&mut connection, budget)?;
        if !equal_mac(ready.mac, mac(&key, "server-ready-v2", &())?) {
            return Err(invalid("control owner confirmation failed"));
        }
        Ok(Self {
            channel: Channel {
                connection,
                scope: scope.clone(),
                key,
                outgoing: 0,
                incoming: 0,
                usable: true,
            },
            pending: None,
            event_after: resume_after,
        })
    }

    pub fn send(&mut self, message: &ClientMessage, budget: IoBudget<'_>) -> io::Result<()> {
        super::ledger::validate_client(message)?;
        if self.pending.is_some() {
            return Err(invalid("control request already pending"));
        }
        if matches!(message, ClientMessage::Events { after } if *after != self.event_after) {
            return Err(invalid("control event cursor mismatch"));
        }
        self.channel.send(message, "client-message-v2", budget)?;
        self.pending = Some(message.clone());
        Ok(())
    }

    pub fn receive(&mut self, budget: IoBudget<'_>) -> io::Result<ServerMessage> {
        let Some(pending) = &self.pending else {
            return Err(invalid("no pending control request"));
        };
        let message = self.channel.receive("server-message-v2", budget)?;
        if let Err(error) = super::ledger::validate_server(&message) {
            self.channel.usable = false;
            return Err(error);
        }
        let matches = match (pending, &message) {
            (ClientMessage::Submit { request }, ServerMessage::Request { reply }) => {
                request.request_id == reply.request_id()
            }
            (
                ClientMessage::Acknowledge { request_id },
                ServerMessage::Acknowledged { request_id: actual },
            ) => request_id == actual,
            (ClientMessage::Events { after }, ServerMessage::Events { batch }) => {
                *after == batch.after
            }
            (ClientMessage::Close, ServerMessage::Closed) => true,
            _ => false,
        };
        if !matches {
            self.channel.usable = false;
            return Err(invalid("control response correlation mismatch"));
        }
        if let ServerMessage::Events { batch } = &message {
            self.event_after = batch
                .events
                .last()
                .map_or(batch.after, |event| event.sequence);
        }
        if matches!(message, ServerMessage::Closed) {
            self.channel
                .send(&CloseReceipt { received: true }, "client-close-v2", budget)?;
            self.channel.usable = false;
        }
        self.pending = None;
        Ok(message)
    }
}

impl<'a> ServerSession<'a> {
    pub fn authenticate(
        mut connection: Connection<'a>,
        scope: &Scope,
        capability: &Capability,
        budget: IoBudget<'_>,
    ) -> io::Result<Self> {
        scope.validate()?;
        let hello: Signed<ClientHello> = receive(&mut connection, budget)?;
        if hello.body.version != PROTOCOL_VERSION
            || hello.body.scope != *scope
            || !valid_id(&hello.body.client_id)
            || !equal_mac(
                hello.mac,
                mac(&capability.0, "client-hello-v2", &hello.body)?,
            )
        {
            return Err(invalid("control client authentication failed"));
        }
        let response = ServerHello {
            version: PROTOCOL_VERSION,
            scope: scope.clone(),
            client_id: hello.body.client_id.clone(),
            client_nonce: hello.body.nonce,
            server_nonce: rand::random(),
            resume_after: hello.body.resume_after,
        };
        send(
            &mut connection,
            &Signed {
                mac: mac(&capability.0, "server-hello-v2", &response)?,
                body: &response,
            },
            budget,
        )?;
        let key = mac(&capability.0, "connection-v2", &(&hello.body, &response))?;
        let proof: Proof = receive(&mut connection, budget)?;
        if !equal_mac(proof.mac, mac(&key, "client-finish-v2", &())?) {
            return Err(invalid("control client confirmation failed"));
        }
        send(
            &mut connection,
            &Proof {
                mac: mac(&key, "server-ready-v2", &())?,
            },
            budget,
        )?;
        Ok(Self {
            channel: Channel {
                connection,
                scope: scope.clone(),
                key,
                outgoing: 0,
                incoming: 0,
                usable: true,
            },
            client_id: hello.body.client_id,
            resume_after: hello.body.resume_after,
        })
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }
    pub fn scope(&self) -> &Scope {
        &self.channel.scope
    }
    pub fn resume_after(&self) -> u64 {
        self.resume_after
    }

    pub fn receive(&mut self, budget: IoBudget<'_>) -> io::Result<ClientMessage> {
        let message = self.channel.receive("client-message-v2", budget)?;
        if let Err(error) = super::ledger::validate_client(&message) {
            self.channel.usable = false;
            return Err(error);
        }
        Ok(message)
    }

    pub fn send(&mut self, message: &ServerMessage, budget: IoBudget<'_>) -> io::Result<()> {
        super::ledger::validate_server(message)?;
        self.channel.send(message, "server-message-v2", budget)?;
        if matches!(message, ServerMessage::Closed) {
            let receipt: CloseReceipt = self.channel.receive("client-close-v2", budget)?;
            self.channel.usable = false;
            if !receipt.received {
                return Err(invalid("control close receipt rejected"));
            }
        }
        Ok(())
    }
}

#[cfg(all(test, any(windows, target_os = "linux")))]
mod tests {
    use super::super::{
        tests::scope,
        transport::{self, Listener, LocalEndpoint},
    };
    use super::*;
    use std::{
        sync::atomic::AtomicBool,
        time::{Duration, Instant},
    };

    fn wire_rejected(bytes: Vec<u8>) {
        let temp = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let endpoint = LocalEndpoint::NamedPipe(format!(
            r"\\.\pipe\foxy-control-{:032x}",
            rand::random::<u128>()
        ));
        #[cfg(target_os = "linux")]
        let endpoint = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            LocalEndpoint::UnixSocket(temp.path().join("control.sock"))
        };
        let mut listener = Listener::bind(&endpoint).unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let server = std::thread::spawn(move || {
            let cancelled = AtomicBool::new(false);
            let budget = IoBudget {
                deadline: Instant::now() + Duration::from_secs(3),
                cancelled: &cancelled,
            };
            let connection = listener.accept(budget).unwrap();
            ready_tx.send(()).unwrap();
            let mut channel = Channel {
                connection,
                scope: scope(),
                key: [7; 32],
                outgoing: 0,
                incoming: 0,
                usable: true,
            };
            assert!(
                channel
                    .receive::<ClientMessage>("client-message-v2", budget)
                    .is_err()
            );
            assert!(!channel.usable);
            assert!(
                channel
                    .receive::<ClientMessage>("client-message-v2", budget)
                    .is_err()
            );
        });
        let cancelled = AtomicBool::new(false);
        let budget = IoBudget {
            deadline: Instant::now() + Duration::from_secs(3),
            cancelled: &cancelled,
        };
        let mut connection = transport::connect(&endpoint, budget).unwrap();
        connection.write_all(&bytes, budget).unwrap();
        ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        drop(connection);
        server.join().unwrap();
        drop(temp);
    }

    fn framed<T: Serialize>(value: &T) -> Vec<u8> {
        let body = serde_json::to_vec(value).unwrap();
        let mut bytes = (body.len() as u32).to_le_bytes().to_vec();
        bytes.extend(body);
        bytes
    }

    #[test]
    fn malformed_oversized_partial_tampered_stale_and_replayed_frames_poison_connections() {
        wire_rejected(((MAX_MESSAGE_BYTES + 1) as u32).to_le_bytes().to_vec());
        wire_rejected([8u32.to_le_bytes().as_slice(), b"{"].concat());
        for mode in 0..3 {
            let mut body = Envelope {
                scope: scope(),
                sequence: 1,
                body: ClientMessage::Close,
            };
            if mode == 0 {
                body.sequence = 2;
            }
            if mode == 1 {
                body.scope.instance_id = "other_instance".into();
            }
            let mut signed = Signed {
                mac: mac(&[7; 32], "client-message-v2", &body).unwrap(),
                body,
            };
            if mode == 2 {
                signed.mac[0] ^= 1;
            }
            wire_rejected(framed(&signed));
        }
    }

    #[test]
    fn connection_proofs_are_bound_to_both_fresh_nonces_and_direction() {
        let key = [7; 32];
        let transcript = ([1u8; 32], [2u8; 32]);
        let first = mac(&key, "connection-v2", &transcript).unwrap();
        assert!(!equal_mac(
            first,
            mac(&key, "connection-v2", &([1u8; 32], [3u8; 32])).unwrap()
        ));
        assert!(!equal_mac(
            first,
            mac(&key, "connection-v2", &([3u8; 32], [2u8; 32])).unwrap()
        ));
        assert!(!equal_mac(
            mac(&first, "client-finish-v2", &()).unwrap(),
            mac(&first, "server-ready-v2", &()).unwrap()
        ));
    }
}
