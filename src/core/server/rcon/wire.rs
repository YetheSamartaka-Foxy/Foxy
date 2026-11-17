//! BattlEye RCon protocol v2 framing shared by Arma BattlEye and Reforger native RCon.

use zeroize::Zeroizing;

pub(crate) const MAX_DATAGRAM: usize = 65_507;
pub(crate) const MAX_PARTS: u8 = 64;
pub(crate) const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const HEADER: usize = 7;
const LOGIN: u8 = 0x00;
const COMMAND: u8 = 0x01;
const MESSAGE: u8 = 0x02;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WireError {
    Short,
    Magic,
    Checksum,
    Marker,
    PacketType,
    Length,
    Parts,
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ServerPacket {
    Login {
        accepted: bool,
    },
    Reply {
        sequence: u8,
        part: Option<Part>,
        body: Vec<u8>,
    },
    Message {
        sequence: u8,
        body: Vec<u8>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Part {
    pub count: u8,
    pub index: u8,
}

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 != 0 {
                0xEDB8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

const CRC_TABLE: [u32; 256] = crc_table();

pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(u32::MAX, |crc, byte| {
        CRC_TABLE[((crc ^ u32::from(*byte)) & 0xFF) as usize] ^ (crc >> 8)
    })
}

fn frame(kind: u8, body: &[&[u8]]) -> Zeroizing<Vec<u8>> {
    let length = body.iter().map(|part| part.len()).sum::<usize>();
    let mut packet = Zeroizing::new(Vec::with_capacity(HEADER + 1 + length));
    packet.extend_from_slice(b"BE\0\0\0\0\xFF");
    packet.push(kind);
    for part in body {
        packet.extend_from_slice(part);
    }
    let checksum = crc32(&packet[6..]);
    packet[2..6].copy_from_slice(&checksum.to_le_bytes());
    packet
}

pub(crate) fn login(password: &[u8]) -> Zeroizing<Vec<u8>> {
    frame(LOGIN, &[password])
}

pub(crate) fn command(sequence: u8, text: &[u8]) -> Zeroizing<Vec<u8>> {
    frame(COMMAND, &[&[sequence], text])
}

pub(crate) fn acknowledge(sequence: u8) -> Zeroizing<Vec<u8>> {
    frame(MESSAGE, &[&[sequence]])
}

pub(crate) fn decode(datagram: &[u8]) -> Result<ServerPacket, WireError> {
    if datagram.len() > MAX_DATAGRAM {
        return Err(WireError::Length);
    }
    if datagram.len() < HEADER + 1 {
        return Err(WireError::Short);
    }
    if &datagram[..2] != b"BE" {
        return Err(WireError::Magic);
    }
    let expected = u32::from_le_bytes([datagram[2], datagram[3], datagram[4], datagram[5]]);
    if crc32(&datagram[6..]) != expected {
        return Err(WireError::Checksum);
    }
    if datagram[6] != 0xFF {
        return Err(WireError::Marker);
    }
    let payload = &datagram[HEADER + 1..];
    match datagram[HEADER] {
        LOGIN => match payload {
            [0x01] => Ok(ServerPacket::Login { accepted: true }),
            [0x00] => Ok(ServerPacket::Login { accepted: false }),
            _ => Err(WireError::Length),
        },
        COMMAND => {
            let (&sequence, rest) = payload.split_first().ok_or(WireError::Short)?;
            if let [0x00, count, index, body @ ..] = rest {
                if *count == 0 || *count > MAX_PARTS || index >= count {
                    return Err(WireError::Parts);
                }
                Ok(ServerPacket::Reply {
                    sequence,
                    part: Some(Part {
                        count: *count,
                        index: *index,
                    }),
                    body: body.to_vec(),
                })
            } else if rest.first() == Some(&0x00) {
                Err(WireError::Parts)
            } else {
                Ok(ServerPacket::Reply {
                    sequence,
                    part: None,
                    body: rest.to_vec(),
                })
            }
        }
        MESSAGE => {
            let (&sequence, body) = payload.split_first().ok_or(WireError::Short)?;
            Ok(ServerPacket::Message {
                sequence,
                body: body.to_vec(),
            })
        }
        _ => Err(WireError::PacketType),
    }
}

/// Bounded reassembly of one multi-packet command reply.
#[derive(Debug)]
pub(crate) struct Reassembly {
    count: u8,
    parts: Vec<Option<Vec<u8>>>,
    bytes: usize,
}

impl Reassembly {
    pub(crate) fn new(count: u8) -> Result<Self, WireError> {
        if count == 0 || count > MAX_PARTS {
            return Err(WireError::Parts);
        }
        Ok(Self {
            count,
            parts: vec![None; usize::from(count)],
            bytes: 0,
        })
    }

    pub(crate) fn count(&self) -> u8 {
        self.count
    }

    pub(crate) fn accept(
        &mut self,
        part: Part,
        body: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, WireError> {
        if part.count != self.count || part.index >= self.count {
            return Err(WireError::Parts);
        }
        let slot = &mut self.parts[usize::from(part.index)];
        match slot {
            Some(existing) if *existing == body => {}
            Some(_) => return Err(WireError::Conflict),
            None => {
                self.bytes = self
                    .bytes
                    .checked_add(body.len())
                    .filter(|total| *total <= MAX_RESPONSE_BYTES)
                    .ok_or(WireError::Length)?;
                *slot = Some(body);
            }
        }
        if self.parts.iter().all(Option::is_some) {
            let mut joined = Vec::with_capacity(self.bytes);
            for part in self.parts.iter_mut() {
                joined.extend(part.take().unwrap_or_default());
            }
            return Ok(Some(joined));
        }
        Ok(None)
    }
}
