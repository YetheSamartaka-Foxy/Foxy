use std::net::{Ipv4Addr, UdpSocket};
use std::time::Duration;

const MAX_PACKET: usize = 128 * 1024;
const MAX_FRAGMENTS: usize = 32;
const DATAGRAM_CAPACITY: usize = 65_536;
const MAX_SPLIT_SIZE: usize = 4096;

pub(super) fn query_keys(port: u16, hostname: &str) -> Option<Vec<String>> {
    use super::report::query_info;

    if !query_info(port, hostname) {
        return None;
    }
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).ok()?;
    socket.connect((Ipv4Addr::LOCALHOST, port)).ok()?;
    socket
        .set_read_timeout(Some(Duration::from_millis(200)))
        .ok()?;
    socket
        .set_write_timeout(Some(Duration::from_millis(200)))
        .ok()?;
    let mut request = b"\xff\xff\xff\xff\x56\xff\xff\xff\xff".to_vec();
    let mut buffer = [0u8; DATAGRAM_CAPACITY];
    for _ in 0..3 {
        socket.send(&request).ok()?;
        let length = socket.recv(&mut buffer).ok()?;
        let response = &buffer[..length];
        if response.starts_with(b"\xff\xff\xff\xffA") && response.len() == 9 {
            request.truncate(5);
            request.extend_from_slice(&response[5..9]);
            continue;
        }
        let packet = reassemble(response, &socket)?;
        return parse_rules(&packet);
    }
    None
}

fn reassemble(first: &[u8], socket: &UdpSocket) -> Option<Vec<u8>> {
    let mut buffer = [0u8; DATAGRAM_CAPACITY];
    if first.len() < 4 {
        return None;
    }
    if first[..4] == [0xff; 4] {
        return (first.len() <= MAX_PACKET).then(|| first.to_vec());
    }
    let first_fragment = parse_split_fragment(first)?;
    let id = first_fragment.id;
    let total = first_fragment.total;
    let split_size = first_fragment.split_size;
    let fragment = first_fragment.index;
    if total == 0 || total > MAX_FRAGMENTS || fragment >= total {
        return None;
    }
    let mut fragments: Vec<Option<Vec<u8>>> = vec![None; total];
    if !store_fragment(&mut fragments, fragment, first_fragment.payload) {
        return None;
    }
    let mut size = first_fragment.payload.len();
    let mut packets_read = 1;
    while fragments.iter().any(Option::is_none) {
        if packets_read >= total {
            return None;
        }
        let length = socket.recv(&mut buffer).ok()?;
        let packet = &buffer[..length];
        packets_read += 1;
        let fragment = parse_split_fragment(packet)?;
        if fragment.id != id
            || fragment.total != total
            || fragment.split_size != split_size
            || fragment.index >= total
        {
            return None;
        }
        let index = fragment.index;
        if let Some(previous) = &fragments[index] {
            if previous != fragment.payload {
                return None;
            }
        } else {
            size = size.checked_add(fragment.payload.len())?;
            if size > MAX_PACKET {
                return None;
            }
            store_fragment(&mut fragments, index, fragment.payload);
        }
    }
    assemble_fragments(fragments, size)
}

fn assemble_fragments(fragments: Vec<Option<Vec<u8>>>, size: usize) -> Option<Vec<u8>> {
    if size > MAX_PACKET || fragments.iter().any(Option::is_none) {
        return None;
    }
    let mut assembled = Vec::with_capacity(size);
    for fragment in fragments.into_iter().flatten() {
        assembled.extend_from_slice(&fragment);
    }
    (assembled.len() == size).then_some(assembled)
}

struct SplitFragment<'a> {
    id: u32,
    total: usize,
    index: usize,
    split_size: usize,
    payload: &'a [u8],
}

fn parse_split_fragment(packet: &[u8]) -> Option<SplitFragment<'_>> {
    if packet.len() < 12 || packet[..4] != [0xfe, 0xff, 0xff, 0xff] {
        return None;
    }
    let id = u32::from_le_bytes(packet[4..8].try_into().ok()?);
    let total = usize::from(packet[8]);
    let index = usize::from(packet[9]);
    let split_size = usize::from(u16::from_le_bytes([packet[10], packet[11]]));
    if id & 0x8000_0000 != 0 || total == 0 || total > MAX_FRAGMENTS || index >= total {
        return None;
    }
    if split_size == 0 || split_size > MAX_SPLIT_SIZE || packet.len() - 12 > split_size {
        return None;
    }
    Some(SplitFragment {
        id,
        total,
        index,
        split_size,
        payload: &packet[12..],
    })
}

fn store_fragment(fragments: &mut [Option<Vec<u8>>], index: usize, data: &[u8]) -> bool {
    if let Some(previous) = &fragments[index] {
        return previous == data;
    }
    fragments[index] = Some(data.to_vec());
    true
}

fn parse_rules(packet: &[u8]) -> Option<Vec<String>> {
    if packet.len() < 7 || packet[..4] != [0xff; 4] || packet[4] != b'E' {
        return None;
    }
    let count = usize::from(u16::from_le_bytes([packet[5], packet[6]]));
    let mut offset = 7;
    let mut pages = Vec::new();
    for _ in 0..count {
        let key = cstring(packet, &mut offset)?;
        let value = cstring(packet, &mut offset)?;
        if key.len() == 2 && key[0] != 0 && key[1] != 0 {
            pages.push((key[0], key[1], value));
        }
    }
    if offset != packet.len() {
        return None;
    }
    if pages.is_empty() {
        return None;
    }
    let total = pages.first()?.1;
    if pages.len() != usize::from(total) {
        return None;
    }
    let mut ordered: Vec<Option<&[u8]>> = vec![None; usize::from(total)];
    for (number, advertised, value) in pages {
        if advertised != total || number > total {
            return None;
        }
        let slot = &mut ordered[usize::from(number - 1)];
        if slot.is_some() {
            return None;
        }
        *slot = Some(value);
    }
    let mut encoded = Vec::new();
    for page in ordered {
        encoded.extend_from_slice(page?);
        if encoded.len() > MAX_PACKET {
            return None;
        }
    }
    let decoded = unescape(&encoded)?;
    parse_a3sb_v3(&decoded)
}

fn cstring<'a>(packet: &'a [u8], offset: &mut usize) -> Option<&'a [u8]> {
    let tail = packet.get(*offset..)?;
    let end = tail.iter().position(|byte| *byte == 0)?;
    *offset = offset.checked_add(end + 1)?;
    Some(&tail[..end])
}

fn unescape(input: &[u8]) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] != 1 {
            output.push(input[index]);
            index += 1;
            continue;
        }
        index += 1;
        let decoded = match *input.get(index)? {
            1 => 1,
            2 => 0,
            3 => 0xff,
            _ => return None,
        };
        output.push(decoded);
        index += 1;
    }
    Some(output)
}

fn parse_a3sb_v3(data: &[u8]) -> Option<Vec<String>> {
    let mut cursor = Cursor { data, offset: 0 };
    if cursor.byte()? != 3 {
        return None;
    }
    cursor.byte()?;
    let dlc_mask = cursor.u16()?;
    cursor.skip(
        usize::try_from(dlc_mask.count_ones())
            .ok()?
            .checked_mul(4)?,
    )?;
    cursor.skip(2)?;
    let mod_count = usize::from(cursor.byte()?);
    for _ in 0..mod_count {
        cursor.skip(4)?;
        let id_len = usize::from(cursor.byte()?);
        if id_len == 19 {
            cursor.skip(4)?;
            continue;
        }
        if !matches!(id_len, 1 | 4 | 8) {
            return None;
        }
        cursor.skip(id_len)?;
        let name_len = usize::from(cursor.byte()?);
        cursor.skip(name_len)?;
    }
    let signature_count = usize::from(cursor.byte()?);
    let mut signatures = Vec::with_capacity(signature_count);
    for _ in 0..signature_count {
        let length = usize::from(cursor.byte()?);
        let bytes = cursor.take(length)?;
        signatures.push(std::str::from_utf8(bytes).ok()?.to_owned());
    }
    (cursor.offset == data.len()).then_some(signatures)
}

struct Cursor<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn byte(&mut self) -> Option<u8> {
        Some(*self.take(1)?.first()?)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn skip(&mut self, count: usize) -> Option<()> {
        self.take(count).map(|_| ())
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.offset.checked_add(count)?;
        let bytes = self.data.get(self.offset..end)?;
        self.offset = end;
        Some(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_FRAGMENTS, assemble_fragments, parse_a3sb_v3, parse_rules, parse_split_fragment,
        store_fragment,
    };

    fn v3(signatures: &[&[u8]]) -> Vec<u8> {
        let mut data = vec![3, 0, 0, 0, 0, 0, 0];
        data.push(signatures.len() as u8);
        for signature in signatures {
            data.push(signature.len() as u8);
            data.extend_from_slice(signature);
        }
        data
    }

    fn rules(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut packet = b"\xff\xff\xff\xffE".to_vec();
        packet.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for (key, value) in entries {
            packet.extend_from_slice(key);
            packet.push(0);
            packet.extend_from_slice(value);
            packet.push(0);
        }
        packet
    }

    fn split_packet(id: u32, total: u8, index: u8, payload: &[u8]) -> Vec<u8> {
        let mut packet = b"\xfe\xff\xff\xff".to_vec();
        packet.extend_from_slice(&id.to_le_bytes());
        packet.extend_from_slice(&[total, index]);
        packet.extend_from_slice(&4096u16.to_le_bytes());
        packet.extend_from_slice(payload);
        packet
    }

    fn escape(data: &[u8]) -> Vec<u8> {
        let mut encoded = Vec::new();
        for byte in data {
            match byte {
                1 => encoded.extend_from_slice(&[1, 1]),
                0 => encoded.extend_from_slice(&[1, 2]),
                0xff => encoded.extend_from_slice(&[1, 3]),
                byte => encoded.push(*byte),
            }
        }
        encoded
    }

    #[test]
    fn signatures_come_from_typed_signature_section_only() {
        let data = v3(&[b"server.bikey", b"peer.bikey"]);
        assert_eq!(
            parse_a3sb_v3(&data).unwrap(),
            ["server.bikey", "peer.bikey"]
        );
        assert_eq!(parse_a3sb_v3(&v3(&[])).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn server_and_peer_key_names_in_mod_records_are_not_signatures() {
        let mut data = vec![3, 0, 0, 0, 0, 0, 1];
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&[4, 1, 0, 0, 0, 12]);
        data.extend_from_slice(b"server.bikey");
        data.extend_from_slice(&[1, 10]);
        data.extend_from_slice(b"peer.bikey");
        assert_eq!(parse_a3sb_v3(&data).unwrap(), ["peer.bikey"]);
    }

    #[test]
    fn native_key_named_rules_are_not_reported_as_signatures_or_mods() {
        let packet = rules(&[(b"serverkey", b"server.bikey"), (b"mod", b"peer.bikey")]);
        assert!(parse_rules(&packet).is_none());
    }

    #[test]
    fn malformed_pages_versions_and_truncated_signatures_are_refused() {
        assert!(parse_rules(&rules(&[(b"\x01\x01", b"\x04\0")])).is_none());
        assert!(parse_rules(&rules(&[(b"\x02\x02", b"\x03\0\0")])).is_none());
        assert!(parse_a3sb_v3(&[4, 0, 0, 0, 0, 0, 0, 0, 0]).is_none());
        assert!(parse_a3sb_v3(&[3, 0, 0, 0, 0, 0, 0, 0, 1, 4, b'a']).is_none());
    }

    #[test]
    fn rules_pages_reassemble_out_of_order_and_unescape_typed_signatures() {
        let encoded = escape(&v3(&[b"server.bikey"]));
        let middle = encoded.len() / 2;
        let packet = rules(&[
            (b"\x02\x02", &encoded[middle..]),
            (b"\x01\x02", &encoded[..middle]),
        ]);
        assert_eq!(parse_rules(&packet).unwrap(), ["server.bikey"]);
    }

    #[test]
    fn source_split_headers_reassemble_and_reject_compression_or_bad_metadata() {
        let first = split_packet(7, 2, 1, b"rules");
        let second = split_packet(7, 2, 0, b"A2S_");
        let a = parse_split_fragment(&first).unwrap();
        let b = parse_split_fragment(&second).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(a.total, 2);
        let mut fragments = vec![None; a.total];
        store_fragment(&mut fragments, a.index, a.payload);
        store_fragment(&mut fragments, b.index, b.payload);
        assert_eq!(assemble_fragments(fragments, 9).unwrap(), b"A2S_rules");

        assert!(parse_split_fragment(&split_packet(0x8000_0007, 1, 0, b"x")).is_none());
        assert!(
            parse_split_fragment(&split_packet(7, (MAX_FRAGMENTS + 1) as u8, 0, b"x")).is_none()
        );
        assert!(parse_split_fragment(&b"\xff\xff\xff\xfe\x07\0\0\0\x01\0\0\x10x"[..]).is_none());
    }

    #[test]
    fn maximum_source_fragment_is_received_with_its_complete_header_and_payload() {
        let receiver = std::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let sender = std::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        receiver
            .set_read_timeout(Some(std::time::Duration::from_secs(1)))
            .unwrap();
        receiver.connect(sender.local_addr().unwrap()).unwrap();
        let first = split_packet(7, 2, 0, b"first");
        let payload = vec![b'x'; super::MAX_SPLIT_SIZE];
        let second = split_packet(7, 2, 1, &payload);
        sender
            .send_to(&second, receiver.local_addr().unwrap())
            .unwrap();
        let assembled = super::reassemble(&first, &receiver).unwrap();
        assert_eq!(&assembled[..5], b"first");
        assert_eq!(&assembled[5..], payload);
    }
}
