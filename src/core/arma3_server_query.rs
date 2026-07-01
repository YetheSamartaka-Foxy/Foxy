use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Error, ErrorKind};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

const A2S_INFO_HEADER: u8 = 0x49;
const A2S_RULES_HEADER: u8 = 0x45;
const S2C_CHALLENGE_HEADER: u8 = 0x41;
const SINGLE_PACKET_PREFIX: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];
const SPLIT_PACKET_PREFIX: [u8; 4] = [0xFE, 0xFF, 0xFF, 0xFF];
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerAddonRequirement {
    pub display_name: String,
    pub required: bool,
    pub raw_identity: Option<String>,
    #[serde(default)]
    pub workshop_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerAddonQueryResult {
    pub address: String,
    pub game_port: u16,
    pub query_port: u16,
    pub requirements: Vec<ServerAddonRequirement>,
    pub rules: BTreeMap<String, String>,
    pub server_browser_protocol: Option<ServerBrowserProtocol3>,
    pub info_keywords: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerBrowserProtocol3 {
    pub version: u32,
    pub difficulty: Option<u16>,
    pub ai_level: Option<u16>,
    pub dlc_flags: Option<u16>,
    pub mods: Vec<ServerBrowserProtocol3Mod>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerBrowserProtocol3Mod {
    pub display_name: String,
    #[serde(default)]
    pub workshop_id: Option<String>,
}

pub fn query_server_addon_requirements(
    address: &str,
    game_port: u16,
) -> Result<ServerAddonQueryResult, Error> {
    let query_port = game_port.checked_add(1).ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("Game port {} cannot be converted to query port", game_port),
        )
    })?;
    let endpoint = resolve_endpoint(address, query_port)?;
    let info = query_a2s_info(endpoint, DEFAULT_TIMEOUT).ok();
    let rules = query_a2s_rules(endpoint, DEFAULT_TIMEOUT)?;
    let server_browser_protocol = info
        .as_ref()
        .and_then(|info| parse_server_browser_protocol3(&info.keywords_raw).ok())
        .or_else(|| extract_server_browser_protocol3_from_raw_rules(&rules.raw_rules))
        .or_else(|| extract_server_browser_protocol3_from_rules(&rules.text_rules));
    let mut requirements =
        merge_requirement_sources(&rules.text_rules, server_browser_protocol.as_ref());
    requirements.extend(extract_binary_chunk_addon_requirements(&rules.raw_rules));
    deduplicate_requirements(&mut requirements);

    Ok(ServerAddonQueryResult {
        address: address.to_string(),
        game_port,
        query_port,
        requirements,
        rules: rules.text_rules,
        server_browser_protocol,
        info_keywords: info.and_then(|info| info.keywords),
    })
}

fn resolve_endpoint(address: &str, port: u16) -> Result<SocketAddr, Error> {
    format!("{}:{}", address.trim(), port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| {
            Error::new(
                ErrorKind::AddrNotAvailable,
                "Unable to resolve server address",
            )
        })
}

struct A2sRules {
    text_rules: BTreeMap<String, String>,
    raw_rules: Vec<(Vec<u8>, Vec<u8>)>,
}

fn query_a2s_rules(endpoint: SocketAddr, timeout: Duration) -> Result<A2sRules, Error> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_read_timeout(Some(timeout))?;
    let mut request = a2s_rules_request(-1);
    let mut response = send_and_collect_response(&socket, endpoint, &request, timeout)?;

    if response.len() >= 9
        && response.starts_with(&SINGLE_PACKET_PREFIX)
        && response[4] == S2C_CHALLENGE_HEADER
    {
        let challenge = i32::from_le_bytes([response[5], response[6], response[7], response[8]]);
        request = a2s_rules_request(challenge);
        response = send_and_collect_response(&socket, endpoint, &request, timeout)?;
    }

    parse_a2s_rules_response_full(&response)
}

fn a2s_rules_request(challenge: i32) -> Vec<u8> {
    let mut request = Vec::with_capacity(9);
    request.extend_from_slice(&SINGLE_PACKET_PREFIX);
    request.push(0x56);
    request.extend_from_slice(&challenge.to_le_bytes());
    request
}

#[derive(Debug, Clone)]
struct A2sInfo {
    keywords: Option<String>,
    keywords_raw: Vec<u8>,
}

fn query_a2s_info(endpoint: SocketAddr, timeout: Duration) -> Result<A2sInfo, Error> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_read_timeout(Some(timeout))?;
    let response = send_and_collect_response(&socket, endpoint, &a2s_info_request(), timeout)?;
    parse_a2s_info_response(&response)
}

fn a2s_info_request() -> Vec<u8> {
    let mut request = Vec::with_capacity(25);
    request.extend_from_slice(&SINGLE_PACKET_PREFIX);
    request.push(0x54);
    request.extend_from_slice(b"Source Engine Query\0");
    request
}

fn send_and_collect_response(
    socket: &UdpSocket,
    endpoint: SocketAddr,
    request: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, Error> {
    socket.send_to(request, endpoint)?;
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 4096];
    let mut split_packets: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
    let mut expected_split_count: Option<u8>;

    loop {
        let (len, _) = socket.recv_from(&mut buf)?;
        let packet = &buf[..len];
        if packet.starts_with(&SINGLE_PACKET_PREFIX) {
            return Ok(packet.to_vec());
        }

        if !packet.starts_with(&SPLIT_PACKET_PREFIX) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "Unexpected server query packet prefix",
            ));
        }

        let split = parse_split_packet(packet)?;
        if split.compressed {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "Compressed A2S split packets are not supported",
            ));
        }
        expected_split_count = Some(split.total);
        split_packets.insert(split.number, split.payload.to_vec());

        if split_packets.len() == usize::from(split.total) {
            let mut joined = Vec::new();
            for payload in split_packets.values() {
                joined.extend_from_slice(payload);
            }
            return Ok(joined);
        }

        let now = Instant::now();
        if now >= deadline {
            return Err(Error::new(
                ErrorKind::TimedOut,
                format!(
                    "Timed out while reading split A2S response ({}/{})",
                    split_packets.len(),
                    expected_split_count.unwrap_or_default()
                ),
            ));
        }
        socket.set_read_timeout(Some(deadline.saturating_duration_since(now)))?;
    }
}

struct SplitPacket<'a> {
    total: u8,
    number: u8,
    compressed: bool,
    payload: &'a [u8],
}

fn parse_split_packet(packet: &[u8]) -> Result<SplitPacket<'_>, Error> {
    if packet.len() < 12 || !packet.starts_with(&SPLIT_PACKET_PREFIX) {
        return Err(Error::new(ErrorKind::InvalidData, "Split packet too short"));
    }

    let request_id = i32::from_le_bytes([packet[4], packet[5], packet[6], packet[7]]);
    let compressed = request_id < 0;
    let total = packet[8];
    let number = packet[9];
    if total == 0 || number >= total {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Invalid split packet index",
        ));
    }

    Ok(SplitPacket {
        total,
        number,
        compressed,
        payload: &packet[12..],
    })
}

fn parse_a2s_rules_response_full(data: &[u8]) -> Result<A2sRules, Error> {
    if data.len() < 7 || !data.starts_with(&SINGLE_PACKET_PREFIX) {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "A2S_RULES response is too short",
        ));
    }
    if data[4] != A2S_RULES_HEADER {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("Unexpected A2S_RULES response header 0x{:X}", data[4]),
        ));
    }

    let rule_count = u16::from_le_bytes([data[5], data[6]]) as usize;
    let mut cursor = 7;
    let mut text_rules = BTreeMap::new();
    let mut raw_rules = Vec::with_capacity(rule_count);
    for _ in 0..rule_count {
        let key = read_cstring_bytes(data, &mut cursor)?;
        let value = read_cstring_bytes(data, &mut cursor)?;
        text_rules.insert(
            String::from_utf8_lossy(&key).to_string(),
            String::from_utf8_lossy(&value).to_string(),
        );
        raw_rules.push((key, value));
    }
    Ok(A2sRules {
        text_rules,
        raw_rules,
    })
}

fn parse_a2s_info_response(data: &[u8]) -> Result<A2sInfo, Error> {
    if data.len() < 6 || !data.starts_with(&SINGLE_PACKET_PREFIX) {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "A2S_INFO response is too short",
        ));
    }
    if data[4] != A2S_INFO_HEADER {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("Unexpected A2S_INFO response header 0x{:X}", data[4]),
        ));
    }

    let mut cursor = 6;
    let _name = read_cstring(data, &mut cursor)?;
    let _map = read_cstring(data, &mut cursor)?;
    let _folder = read_cstring(data, &mut cursor)?;
    let _game = read_cstring(data, &mut cursor)?;
    skip_bytes(data, &mut cursor, 2 + 1 + 1 + 1 + 1 + 1 + 1 + 1)?;
    let _version = read_cstring(data, &mut cursor)?;

    if cursor >= data.len() {
        return Ok(A2sInfo {
            keywords: None,
            keywords_raw: Vec::new(),
        });
    }

    let edf = data[cursor];
    cursor += 1;

    if edf & 0x80 != 0 {
        skip_bytes(data, &mut cursor, 2)?;
    }
    if edf & 0x10 != 0 {
        skip_bytes(data, &mut cursor, 8)?;
    }
    if edf & 0x40 != 0 {
        let _spectator_port = read_u16_le(data, &mut cursor)?;
        let _spectator_name = read_cstring(data, &mut cursor)?;
    }
    let (keywords, keywords_raw) = if edf & 0x20 != 0 {
        let raw = read_cstring_bytes(data, &mut cursor)?;
        let text = String::from_utf8(raw.clone()).ok();
        (text, raw)
    } else {
        (None, Vec::new())
    };

    Ok(A2sInfo {
        keywords,
        keywords_raw,
    })
}

fn read_cstring(data: &[u8], cursor: &mut usize) -> Result<String, Error> {
    let bytes = read_cstring_bytes(data, cursor)?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

fn read_cstring_bytes(data: &[u8], cursor: &mut usize) -> Result<Vec<u8>, Error> {
    let start = *cursor;
    while *cursor < data.len() && data[*cursor] != 0 {
        *cursor += 1;
    }
    if *cursor >= data.len() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Missing null terminator in A2S_RULES response",
        ));
    }
    let value = data[start..*cursor].to_vec();
    *cursor += 1;
    Ok(value)
}

fn skip_bytes(data: &[u8], cursor: &mut usize, count: usize) -> Result<(), Error> {
    if data.len().saturating_sub(*cursor) < count {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "A2S response ended before expected field",
        ));
    }
    *cursor += count;
    Ok(())
}

fn read_u8(data: &[u8], cursor: &mut usize) -> Result<u8, Error> {
    if *cursor >= data.len() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Server Browser Protocol 3 payload ended before u8 field",
        ));
    }
    let value = data[*cursor];
    *cursor += 1;
    Ok(value)
}

fn read_u16_le(data: &[u8], cursor: &mut usize) -> Result<u16, Error> {
    if data.len().saturating_sub(*cursor) < 2 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Server Browser Protocol 3 payload ended before u16 field",
        ));
    }
    let value = u16::from_le_bytes([data[*cursor], data[*cursor + 1]]);
    *cursor += 2;
    Ok(value)
}

fn read_u32_le(data: &[u8], cursor: &mut usize) -> Result<u32, Error> {
    if data.len().saturating_sub(*cursor) < 4 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Server Browser Protocol 3 payload ended before u32 field",
        ));
    }
    let value = u32::from_le_bytes([
        data[*cursor],
        data[*cursor + 1],
        data[*cursor + 2],
        data[*cursor + 3],
    ]);
    *cursor += 4;
    Ok(value)
}

fn read_u64_le(data: &[u8], cursor: &mut usize) -> Result<u64, Error> {
    if data.len().saturating_sub(*cursor) < 8 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Server Browser Protocol 3 payload ended before u64 field",
        ));
    }
    let value = u64::from_le_bytes([
        data[*cursor],
        data[*cursor + 1],
        data[*cursor + 2],
        data[*cursor + 3],
        data[*cursor + 4],
        data[*cursor + 5],
        data[*cursor + 6],
        data[*cursor + 7],
    ]);
    *cursor += 8;
    Ok(value)
}

fn read_length_prefixed_string(
    data: &[u8],
    cursor: &mut usize,
    len: usize,
) -> Result<String, Error> {
    if data.len().saturating_sub(*cursor) < len {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Server Browser Protocol 3 payload ended before string field",
        ));
    }
    let text = String::from_utf8_lossy(&data[*cursor..*cursor + len])
        .trim_matches(char::from(0))
        .trim()
        .to_string();
    *cursor += len;
    Ok(text)
}

pub fn parse_server_browser_protocol3(data: &[u8]) -> Result<ServerBrowserProtocol3, Error> {
    let mut candidates = Vec::new();
    if let Ok(parsed) = parse_server_browser_protocol3_layout(data, true) {
        candidates.push(parsed);
    }
    if let Ok(parsed) = parse_server_browser_protocol3_layout(data, false) {
        candidates.push(parsed);
    }

    candidates
        .into_iter()
        .max_by_key(|parsed| parsed.mods.len())
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                "Server Browser Protocol 3 payload could not be decoded",
            )
        })
}

fn parse_server_browser_protocol3_layout(
    data: &[u8],
    has_dlc_flags: bool,
) -> Result<ServerBrowserProtocol3, Error> {
    let mut cursor = 0;
    let version = read_u32_le(data, &mut cursor)?;
    if version != 3 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("Unsupported Server Browser Protocol version {}", version),
        ));
    }

    let dlc_flags = if has_dlc_flags {
        Some(read_u16_le(data, &mut cursor)?)
    } else {
        None
    };
    let difficulty = Some(read_u16_le(data, &mut cursor)?);
    let ai_level = Some(read_u16_le(data, &mut cursor)?);
    let mods_count = read_u8(data, &mut cursor)? as usize;
    let mut mods = Vec::with_capacity(mods_count);

    for _ in 0..mods_count {
        let steam_id_len = read_u8(data, &mut cursor)? as usize;
        let workshop_id = read_protocol_mod_id(data, &mut cursor, steam_id_len)?;
        let name_len = read_u8(data, &mut cursor)? as usize;
        let display_name = read_length_prefixed_string(data, &mut cursor, name_len)?;
        if !display_name.is_empty() {
            mods.push(ServerBrowserProtocol3Mod {
                display_name,
                workshop_id,
            });
        }
    }

    Ok(ServerBrowserProtocol3 {
        version,
        difficulty,
        ai_level,
        dlc_flags,
        mods,
    })
}

fn read_protocol_mod_id(
    data: &[u8],
    cursor: &mut usize,
    len: usize,
) -> Result<Option<String>, Error> {
    if len == 0 {
        return Ok(None);
    }
    if len == 8 {
        let value = read_u64_le(data, cursor)?;
        return Ok((value > 0).then(|| value.to_string()));
    }
    if data.len().saturating_sub(*cursor) < len {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Server Browser Protocol 3 payload ended before Workshop ID field",
        ));
    }
    let raw = &data[*cursor..*cursor + len];
    *cursor += len;
    Ok(String::from_utf8(raw.to_vec())
        .ok()
        .and_then(|value| normalize_workshop_id(&value)))
}

fn extract_server_browser_protocol3_from_rules(
    rules: &BTreeMap<String, String>,
) -> Option<ServerBrowserProtocol3> {
    rules
        .iter()
        .filter(|(key, _)| is_server_browser_protocol_rule_key(key))
        .filter_map(|(_, value)| decode_server_browser_protocol3_rule_value(value))
        .find_map(|candidate| parse_server_browser_protocol3(&candidate).ok())
}

fn extract_server_browser_protocol3_from_raw_rules(
    raw_rules: &[(Vec<u8>, Vec<u8>)],
) -> Option<ServerBrowserProtocol3> {
    reassemble_binary_rule_chunks(raw_rules)
        .and_then(|payload| parse_server_browser_protocol3(&payload).ok())
}

fn extract_binary_chunk_addon_requirements(
    raw_rules: &[(Vec<u8>, Vec<u8>)],
) -> Vec<ServerAddonRequirement> {
    let Some(payload) = reassemble_binary_rule_chunks(raw_rules) else {
        return Vec::new();
    };

    extract_length_prefixed_display_names(&payload)
        .into_iter()
        .map(|display_name| ServerAddonRequirement {
            raw_identity: Some(display_name.clone()),
            display_name,
            required: true,
            workshop_ids: Vec::new(),
        })
        .collect()
}

fn reassemble_binary_rule_chunks(raw_rules: &[(Vec<u8>, Vec<u8>)]) -> Option<Vec<u8>> {
    let chunks = raw_rules
        .iter()
        .filter_map(|(key, value)| {
            if key.len() == 2 && key[0] > 0 && key[1] > 0 && key[0] <= key[1] {
                Some((key[0], key[1], value.as_slice()))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    if chunks.is_empty() {
        return None;
    }

    let total = chunks[0].1;
    if chunks
        .iter()
        .any(|(_, chunk_total, _)| *chunk_total != total)
    {
        return None;
    }

    let mut ordered = BTreeMap::new();
    for (idx, _, value) in chunks {
        ordered.insert(idx, value);
    }
    if ordered.len() != usize::from(total) {
        return None;
    }

    let mut payload = Vec::new();
    for idx in 1..=total {
        payload.extend_from_slice(ordered.get(&idx)?);
    }
    Some(payload)
}

fn extract_length_prefixed_display_names(payload: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = BTreeMap::<String, ()>::new();
    let mut cursor = 0;

    while cursor < payload.len() {
        let len = usize::from(payload[cursor]);
        if is_plausible_mod_name_len(len) && payload.len().saturating_sub(cursor + 1) >= len {
            let bytes = &payload[cursor + 1..cursor + 1 + len];
            if let Some(name) = decode_plausible_mod_name(bytes) {
                let normalized = normalize_extracted_name_key(&name);
                if seen.insert(normalized, ()).is_none() {
                    names.push(name);
                }
                cursor += len + 1;
                continue;
            }
        }
        cursor += 1;
    }

    names
}

fn is_plausible_mod_name_len(len: usize) -> bool {
    (3..=80).contains(&len)
}

fn decode_plausible_mod_name(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes).trim().to_string();
    if text.len() < 3 || text.contains(char::REPLACEMENT_CHARACTER) {
        return None;
    }
    if has_implausible_mod_name_boundary(&text) {
        return None;
    }

    let mut has_alpha = false;
    for ch in text.chars() {
        if ch.is_alphabetic() {
            has_alpha = true;
        }
        if ch.is_control() || !(ch.is_alphanumeric() || is_allowed_mod_name_punctuation(ch)) {
            return None;
        }
    }

    has_alpha.then_some(text)
}

fn has_implausible_mod_name_boundary(text: &str) -> bool {
    text.chars()
        .next()
        .is_some_and(|ch| matches!(ch, '"' | '\'' | '/' | '\\' | ')' | ']' | ',' | '.'))
        || text
            .chars()
            .next_back()
            .is_some_and(|ch| matches!(ch, '"' | '\'' | '/' | '\\' | '(' | '[' | ',' | '-'))
}

fn is_allowed_mod_name_punctuation(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '@' | '-'
                | '_'
                | '.'
                | ':'
                | '\''
                | '"'
                | '('
                | ')'
                | '['
                | ']'
                | '/'
                | '&'
                | '+'
                | ','
        )
}

fn normalize_extracted_name_key(name: &str) -> String {
    name.chars()
        .flat_map(char::to_lowercase)
        .filter(|ch| ch.is_alphanumeric())
        .collect()
}

fn deduplicate_requirements(requirements: &mut Vec<ServerAddonRequirement>) {
    let mut seen = BTreeMap::<String, ()>::new();
    let mut deduped = Vec::<ServerAddonRequirement>::new();
    for requirement in requirements.drain(..) {
        let key = normalize_extracted_name_key(&requirement.display_name);
        if key.is_empty() {
            continue;
        }
        if let Some(existing_idx) = deduped
            .iter()
            .position(|existing| normalize_extracted_name_key(&existing.display_name) == key)
        {
            merge_workshop_ids(
                &mut deduped[existing_idx].workshop_ids,
                &requirement.workshop_ids,
            );
        } else if seen.insert(key, ()).is_none() {
            deduped.push(requirement);
        }
    }
    *requirements = deduped;
}

fn is_server_browser_protocol_rule_key(key: &str) -> bool {
    let key = normalize_rule_key(key);
    matches!(
        key.as_str(),
        "serverbrowserprotocol3"
            | "serverbrowserprotocol"
            | "steambrowserprotocol"
            | "gamekeywords"
            | "keywords"
    )
}

fn decode_server_browser_protocol3_rule_value(value: &str) -> Option<Vec<u8>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("hex:"))
    {
        return decode_hex_bytes(hex);
    }
    Some(trimmed.as_bytes().to_vec())
}

fn decode_hex_bytes(value: &str) -> Option<Vec<u8>> {
    let compact = value
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != ':' && *ch != '-')
        .collect::<String>();
    if compact.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(compact.len() / 2);
    for idx in (0..compact.len()).step_by(2) {
        let byte = u8::from_str_radix(&compact[idx..idx + 2], 16).ok()?;
        bytes.push(byte);
    }
    Some(bytes)
}

fn merge_requirement_sources(
    rules: &BTreeMap<String, String>,
    server_browser_protocol: Option<&ServerBrowserProtocol3>,
) -> Vec<ServerAddonRequirement> {
    let mut requirements = extract_addon_requirements(rules);

    if let Some(protocol) = server_browser_protocol {
        for protocol_mod in &protocol.mods {
            if protocol_mod.display_name.trim().is_empty() {
                continue;
            }
            if let Some(requirement) = requirements.iter_mut().find(|requirement| {
                requirement
                    .display_name
                    .eq_ignore_ascii_case(&protocol_mod.display_name)
            }) {
                if let Some(workshop_id) = protocol_mod
                    .workshop_id
                    .as_deref()
                    .and_then(normalize_workshop_id)
                {
                    push_unique_workshop_id(&mut requirement.workshop_ids, workshop_id);
                }
                continue;
            }
            requirements.push(ServerAddonRequirement {
                display_name: protocol_mod.display_name.clone(),
                required: true,
                raw_identity: Some(protocol_mod.display_name.clone()),
                workshop_ids: protocol_mod
                    .workshop_id
                    .iter()
                    .filter_map(|id| normalize_workshop_id(id))
                    .collect(),
            });
        }
    }

    requirements
}

pub fn extract_addon_requirements(rules: &BTreeMap<String, String>) -> Vec<ServerAddonRequirement> {
    let names = collect_rule_values(rules, "modnames");
    let workshop_ids = collect_rule_values(rules, "workshopids")
        .into_iter()
        .filter_map(|id| normalize_workshop_id(&id))
        .collect::<Vec<_>>();

    let mut requirements = names
        .into_iter()
        .enumerate()
        .filter_map(|display_name| {
            let (idx, display_name) = display_name;
            let display_name = display_name.trim();
            if display_name.is_empty() {
                return None;
            }
            Some(ServerAddonRequirement {
                display_name: display_name.to_string(),
                required: true,
                raw_identity: Some(display_name.to_string()),
                workshop_ids: workshop_ids.get(idx).cloned().into_iter().collect(),
            })
        })
        .collect::<Vec<_>>();

    if requirements.len() < workshop_ids.len() {
        requirements.extend(workshop_ids.iter().skip(requirements.len()).map(|id| {
            ServerAddonRequirement {
                display_name: format!("Steam Workshop {}", id),
                required: true,
                raw_identity: Some(id.clone()),
                workshop_ids: vec![id.clone()],
            }
        }));
    }

    requirements
}

fn normalize_workshop_id(id: &str) -> Option<String> {
    let trimmed = id.trim();
    (!trimmed.is_empty() && trimmed.chars().all(|ch| ch.is_ascii_digit()))
        .then(|| trimmed.to_string())
}

fn push_unique_workshop_id(ids: &mut Vec<String>, id: String) {
    if !ids.iter().any(|existing| existing == &id) {
        ids.push(id);
    }
}

fn merge_workshop_ids(target: &mut Vec<String>, source: &[String]) {
    for id in source {
        if let Some(id) = normalize_workshop_id(id) {
            push_unique_workshop_id(target, id);
        }
    }
}

fn collect_rule_values(rules: &BTreeMap<String, String>, base_key: &str) -> Vec<String> {
    let mut keyed_values = rules
        .iter()
        .filter_map(|(key, value)| {
            let normalized = normalize_rule_key(key);
            if normalized == base_key || numbered_rule_suffix(&normalized, base_key).is_some() {
                Some((
                    numbered_rule_suffix(&normalized, base_key).unwrap_or(0),
                    value,
                ))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    keyed_values.sort_by_key(|(idx, _)| *idx);

    keyed_values
        .into_iter()
        .flat_map(|(_, value)| split_rule_list(value))
        .collect()
}

fn normalize_rule_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn numbered_rule_suffix(key: &str, base_key: &str) -> Option<usize> {
    key.strip_prefix(base_key)
        .filter(|suffix| !suffix.is_empty())
        .and_then(|suffix| suffix.parse::<usize>().ok())
}

fn split_rule_list(value: &str) -> Vec<String> {
    value
        .split([';', ',', '|', '\n', '\r', '\t'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(ToString::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rules_response_pairs() {
        let mut response = Vec::new();
        response.extend_from_slice(&SINGLE_PACKET_PREFIX);
        response.push(A2S_RULES_HEADER);
        response.extend_from_slice(&2u16.to_le_bytes());
        response.extend_from_slice(b"modNames\0@cba_a3;@ace\0");
        response.extend_from_slice(b"modHashes\0HASH1;HASH2\0");

        let rules = parse_a2s_rules_response_full(&response)
            .expect("rules should parse")
            .text_rules;

        assert_eq!(
            rules.get("modNames").map(String::as_str),
            Some("@cba_a3;@ace")
        );
        assert_eq!(
            rules.get("modHashes").map(String::as_str),
            Some("HASH1;HASH2")
        );
    }

    #[test]
    fn rejects_truncated_cstring() {
        let mut response = Vec::new();
        response.extend_from_slice(&SINGLE_PACKET_PREFIX);
        response.push(A2S_RULES_HEADER);
        response.extend_from_slice(&1u16.to_le_bytes());
        response.extend_from_slice(b"modNames\0@ace");

        let err = parse_a2s_rules_response_full(&response)
            .err()
            .expect("response should fail");

        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn parses_info_response_with_lossy_server_strings() {
        let mut response = Vec::new();
        response.extend_from_slice(&SINGLE_PACKET_PREFIX);
        response.push(A2S_INFO_HEADER);
        response.push(17);
        response.extend_from_slice(&[b'S', 0xFF, b'v', 0]);
        response.extend_from_slice(b"map\0arma3\0Arma 3\0");
        response.extend_from_slice(&123u16.to_le_bytes());
        response.extend_from_slice(&[0, 64, 0, b'd', b'w', 0, 1]);
        response.extend_from_slice(b"2.16\0");

        let info = parse_a2s_info_response(&response).expect("info should parse");

        assert!(info.keywords.is_none());
    }

    #[test]
    fn extracts_numbered_mod_names() {
        let rules = BTreeMap::from([
            ("modNames0".to_string(), "@cba_a3; @ace".to_string()),
            ("modNames1".to_string(), "@task_force_radio".to_string()),
            ("modHashes".to_string(), "HASH1;HASH2;HASH3".to_string()),
            (
                "workshopIds".to_string(),
                "450814997;463939057;620019431".to_string(),
            ),
        ]);

        let requirements = extract_addon_requirements(&rules);

        assert_eq!(requirements.len(), 3);
        assert_eq!(requirements[1].display_name, "@ace");
        assert_eq!(requirements[1].workshop_ids, vec!["463939057"]);
    }

    #[test]
    fn extracts_numbered_mod_names_in_numeric_suffix_order() {
        let rules = BTreeMap::from([
            ("modNames10".to_string(), "@ten".to_string()),
            ("modNames2".to_string(), "@two".to_string()),
            ("modNames1".to_string(), "@one".to_string()),
        ]);

        let display_names = extract_addon_requirements(&rules)
            .into_iter()
            .map(|requirement| requirement.display_name)
            .collect::<Vec<_>>();

        assert_eq!(display_names, vec!["@one", "@two", "@ten"]);
    }

    #[test]
    fn extracts_binary_chunked_mod_names_from_rules() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[3, 1, 2, 1, 0xAA, 0xBB, 0xCC, 0xDD]);
        payload.push(34);
        payload.extend_from_slice(b"Advanced Combat Environment 3.21.0");
        payload.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        payload.push(8);
        payload.extend_from_slice(b"TFR Core");

        let raw_rules = vec![
            (vec![1, 2], payload[..24].to_vec()),
            (vec![2, 2], payload[24..].to_vec()),
        ];

        let requirements = extract_binary_chunk_addon_requirements(&raw_rules);

        assert!(requirements
            .iter()
            .any(|requirement| requirement.display_name == "Advanced Combat Environment 3.21.0"));
        assert!(
            requirements
                .iter()
                .any(|requirement| requirement.display_name == "TFR Core")
        );
    }

    #[test]
    fn binary_chunk_scanner_rejects_false_prefix_before_real_name_length() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&[0x04, 0xAA, 0x25, 0xBB, 0x1B]);
        payload.push(34);
        payload.extend_from_slice(b"Advanced Combat Environment 3.21.0");

        let raw_rules = vec![(vec![1, 1], payload)];

        let requirements = extract_binary_chunk_addon_requirements(&raw_rules);

        assert_eq!(requirements.len(), 1);
        assert_eq!(
            requirements[0].display_name,
            "Advanced Combat Environment 3.21.0"
        );
    }

    #[test]
    fn deduplicates_requirements_across_server_sources() {
        let mut requirements = vec![
            ServerAddonRequirement {
                display_name: "@ACE".to_string(),
                required: true,
                raw_identity: Some("@ACE".to_string()),
                workshop_ids: vec!["463939057".to_string()],
            },
            ServerAddonRequirement {
                display_name: " ace ".to_string(),
                required: true,
                raw_identity: Some("ace".to_string()),
                workshop_ids: vec!["463939057".to_string()],
            },
            ServerAddonRequirement {
                display_name: String::new(),
                required: true,
                raw_identity: None,
                workshop_ids: Vec::new(),
            },
            ServerAddonRequirement {
                display_name: "@cba_a3".to_string(),
                required: true,
                raw_identity: Some("@cba_a3".to_string()),
                workshop_ids: Vec::new(),
            },
        ];

        deduplicate_requirements(&mut requirements);

        assert_eq!(
            requirements
                .iter()
                .map(|requirement| requirement.display_name.as_str())
                .collect::<Vec<_>>(),
            vec!["@ACE", "@cba_a3"]
        );
        assert_eq!(requirements[0].workshop_ids, vec!["463939057"]);
    }

    #[test]
    fn split_packet_payload_is_extracted_after_source_header() {
        let packet = [0xFE, 0xFF, 0xFF, 0xFF, 1, 0, 0, 0, 2, 1, 140, 0, b'a', b'b'];

        let split = parse_split_packet(&packet).expect("split packet should parse");

        assert_eq!(split.total, 2);
        assert_eq!(split.number, 1);
        assert_eq!(split.payload, b"ab");
    }

    #[test]
    fn parses_server_browser_protocol3_with_dlc_flags() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&3u32.to_le_bytes());
        payload.extend_from_slice(&0x0010u16.to_le_bytes());
        payload.extend_from_slice(&2u16.to_le_bytes());
        payload.extend_from_slice(&1u16.to_le_bytes());
        payload.push(2);
        payload.push(8);
        payload.extend_from_slice(&450814997u64.to_le_bytes());
        payload.push(7);
        payload.extend_from_slice(b"@cba_a3");
        payload.push(8);
        payload.extend_from_slice(&463939057u64.to_le_bytes());
        payload.push(4);
        payload.extend_from_slice(b"@ace");

        let parsed = parse_server_browser_protocol3(&payload).expect("SBP3 should parse");

        assert_eq!(parsed.version, 3);
        assert_eq!(parsed.dlc_flags, Some(0x0010));
        assert_eq!(parsed.difficulty, Some(2));
        assert_eq!(parsed.ai_level, Some(1));
        assert_eq!(parsed.mods.len(), 2);
        assert_eq!(parsed.mods[0].display_name, "@cba_a3");
        assert_eq!(parsed.mods[0].workshop_id.as_deref(), Some("450814997"));
        assert_eq!(parsed.mods[1].display_name, "@ace");
        assert_eq!(parsed.mods[1].workshop_id.as_deref(), Some("463939057"));
    }

    #[test]
    fn parses_server_browser_protocol3_with_ascii_mod_id_field() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&3u32.to_le_bytes());
        payload.extend_from_slice(&0u16.to_le_bytes());
        payload.extend_from_slice(&0u16.to_le_bytes());
        payload.push(1);
        payload.push(9);
        payload.extend_from_slice(b"463939057");
        payload.push(4);
        payload.extend_from_slice(b"@ace");

        let parsed = parse_server_browser_protocol3(&payload).expect("SBP3 should parse");

        assert_eq!(parsed.dlc_flags, None);
        assert_eq!(parsed.mods.len(), 1);
        assert_eq!(parsed.mods[0].display_name, "@ace");
        assert_eq!(parsed.mods[0].workshop_id.as_deref(), Some("463939057"));
    }

    #[test]
    fn merges_server_browser_protocol3_names_into_rule_requirements() {
        let rules = BTreeMap::from([
            ("modNames".to_string(), "@cba_a3;@ace".to_string()),
            ("modHashes".to_string(), "HASH1;HASH2".to_string()),
        ]);
        let protocol = ServerBrowserProtocol3 {
            version: 3,
            difficulty: Some(0),
            ai_level: Some(0),
            dlc_flags: None,
            mods: vec![
                ServerBrowserProtocol3Mod {
                    display_name: "@ace".to_string(),
                    workshop_id: Some("463939057".to_string()),
                },
                ServerBrowserProtocol3Mod {
                    display_name: "@task_force_radio".to_string(),
                    workshop_id: Some("620019431".to_string()),
                },
            ],
        };

        let requirements = merge_requirement_sources(&rules, Some(&protocol));

        assert_eq!(requirements.len(), 3);
        let ace = requirements
            .iter()
            .find(|requirement| requirement.display_name == "@ace")
            .expect("ACE requirement should exist");
        assert_eq!(ace.raw_identity.as_deref(), Some("@ace"));
        assert_eq!(ace.workshop_ids, vec!["463939057"]);
        assert!(
            requirements
                .iter()
                .any(|requirement| requirement.display_name == "@task_force_radio")
        );
    }

    #[test]
    fn extracts_server_browser_protocol3_from_hex_rule_value() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&3u32.to_le_bytes());
        payload.extend_from_slice(&0u16.to_le_bytes());
        payload.extend_from_slice(&0u16.to_le_bytes());
        payload.push(1);
        payload.push(0);
        payload.push(4);
        payload.extend_from_slice(b"@ace");
        let encoded = format!(
            "hex:{}",
            payload
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let rules = BTreeMap::from([("serverBrowserProtocol3".to_string(), encoded)]);

        let parsed = extract_server_browser_protocol3_from_rules(&rules)
            .expect("hex-encoded SBP3 rule should parse");

        assert_eq!(parsed.mods.len(), 1);
        assert_eq!(parsed.mods[0].display_name, "@ace");
    }
}
