pub(super) const MAX_RESOURCE_DATABASE_BYTES: usize = 32 * 1024 * 1024;
const MAX_RECORDS: usize = 200_000;

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or("Invalid resource database length")?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or("Truncated resource database")?;
        self.offset = end;
        Ok(value)
    }

    fn word(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| "Invalid resource database word")?,
        ))
    }

    fn string(&mut self) -> Result<&'a str, String> {
        let length = self.word()? as usize;
        if length == 0 || length > 4097 {
            return Err("Invalid resource database string length".into());
        }
        let value = self.take(length)?;
        if value.last() != Some(&0) {
            return Err("Unterminated resource database string".into());
        }
        let text = std::str::from_utf8(&value[..length - 1])
            .map_err(|_| "Invalid resource database encoding")?;
        if text.chars().any(char::is_control) {
            return Err("Invalid resource database string".into());
        }
        Ok(text)
    }
}

pub(super) fn contains_resource(bytes: &[u8], resource: &str) -> Result<bool, String> {
    let (guid_text, path) = resource
        .strip_prefix('{')
        .and_then(|value| value.split_once('}'))
        .filter(|(guid, path)| {
            guid.len() == 16
                && guid.bytes().all(|byte| byte.is_ascii_hexdigit())
                && !path.is_empty()
                && !path.starts_with('/')
                && !path.contains(['\\', ':'])
                && !path.chars().any(char::is_control)
                && !path
                    .split('/')
                    .any(|part| part.is_empty() || matches!(part, "." | ".."))
        })
        .ok_or_else(|| "Invalid resource reference".to_string())?;
    if bytes.len() < 32
        || bytes.len() > MAX_RESOURCE_DATABASE_BYTES
        || &bytes[..4] != b"FORM"
        || &bytes[8..12] != b"RDBC"
        || u32::from_be_bytes(
            bytes[4..8]
                .try_into()
                .map_err(|_| "Invalid resource database header")?,
        ) as usize
            != bytes.len() - 8
    {
        return Err("Invalid resource database or scenario resource".into());
    }
    let guid = u64::from_str_radix(guid_text, 16).map_err(|_| "Invalid scenario GUID")?;
    let mut reader = Reader { bytes, offset: 12 };
    if reader.word()? != 7 {
        return Err("This resource database version has not been accepted.".into());
    }
    if reader.word()? as usize != bytes.len() || reader.word()? != 0 {
        return Err("Invalid resource database header".into());
    }
    let names = reader.word()? as usize;
    if names > 1024 {
        return Err("Resource database name table exceeds its limit".into());
    }
    for _ in 0..names {
        reader.string()?;
    }
    let records = reader.word()? as usize;
    if records == 0 || records > MAX_RECORDS {
        return Err("Resource database record count exceeds its limit".into());
    }
    let mut found = false;
    for index in 0..records {
        let name = reader.string()?;
        let kind = reader.word()?;
        let flags = reader.take(2)?;
        let id = u64::from_le_bytes(
            reader
                .take(8)?
                .try_into()
                .map_err(|_| "Invalid resource GUID")?,
        );
        let base_kind = kind & 15;
        if !matches!(base_kind, 4..=7) || kind & !0x3001f != 0 || flags != [0, 0] {
            return Err(format!(
                "Unknown resource database record at index {index}, offset {}: kind {kind:#x}, flags {flags:?}",
                reader.offset
            ));
        }
        if index == 0 {
            if !name.is_empty() || kind != 5 || id != 0 {
                return Err("Invalid resource database root".into());
            }
        } else if name.is_empty()
            || name.starts_with('/')
            || name.contains('\\')
            || name
                .split('/')
                .any(|part| part.is_empty() || matches!(part, "." | ".."))
        {
            return Err("Invalid resource database path".into());
        }
        if matches!(base_kind, 6 | 7) {
            reader.take(8)?;
            let same_name = name.eq_ignore_ascii_case(path);
            if id == guid && !same_name || same_name && id != guid {
                return Err("Scenario GUID and path conflict with the resource database".into());
            }

            if id == guid && same_name && kind & 0x10 == 0 {
                found = true;
            }
        } else if base_kind == 4 && id != 0 {
            return Err("Invalid script resource GUID".into());
        }
    }
    if reader.offset != bytes.len() {
        return Err("Resource database has trailing or unparsed records".into());
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(body: &mut Vec<u8>, path: &str, kind: u32, guid: u64) {
        body.extend_from_slice(&(path.len() as u32 + 1).to_le_bytes());
        body.extend_from_slice(path.as_bytes());
        body.push(0);
        body.extend_from_slice(&kind.to_le_bytes());
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&guid.to_le_bytes());
        if matches!(kind & 15, 6 | 7) {
            body.extend_from_slice(&[0; 8]);
        }
    }

    fn fixture(kind: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&2u32.to_le_bytes());
        record(&mut body, "", 5, 0);
        record(&mut body, "Missions/Main.conf", kind, 0x0123456789abcdef);
        let length = 20 + body.len();
        let mut bytes = b"FORM".to_vec();
        bytes.extend_from_slice(&(length as u32 - 8).to_be_bytes());
        bytes.extend_from_slice(b"RDBC");
        bytes.extend_from_slice(&7u32.to_le_bytes());
        bytes.extend_from_slice(&(length as u32).to_le_bytes());
        bytes.extend_from_slice(&body);
        bytes
    }

    #[test]
    fn requires_both_guid_and_path_and_excludes_unshipped_resources() {
        let resource = "{0123456789ABCDEF}missions/main.conf";
        assert_eq!(contains_resource(&fixture(6), resource), Ok(true));
        assert_eq!(contains_resource(&fixture(0x20006), resource), Ok(true));
        assert_eq!(contains_resource(&fixture(0x16), resource), Ok(false));
        assert!(contains_resource(&fixture(0x40006), resource).is_err());
        assert!(contains_resource(&fixture(6), "{0123456789ABCDEF}Missions/Other.conf").is_err());
        assert!(contains_resource(&fixture(6), "{FEDCBA9876543210}Missions/Main.conf").is_err());
    }

    #[test]
    fn rejects_truncation_unknown_versions_and_incomplete_record_counts() {
        let resource = "{0123456789ABCDEF}Missions/Main.conf";
        let mut bytes = fixture(6);
        assert!(contains_resource(&bytes[..bytes.len() - 1], resource).is_err());
        bytes[12] = 8;
        assert!(contains_resource(&bytes, resource).is_err());
        bytes[12] = 7;
        bytes[28] = 1;
        assert!(contains_resource(&bytes, resource).is_err());
    }

    #[test]
    #[ignore = "Requires explicit read-only native base and addon resource databases"]
    fn native_reforger_resource_indexes_match_the_selected_resource_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let source = std::path::PathBuf::from(std::env::var("FOXY_REFORGER_NATIVE_SOURCE")?);
        let addon = std::path::PathBuf::from(std::env::var("FOXY_REFORGER_NATIVE_ADDON")?);
        let scenario = std::env::var("FOXY_REFORGER_NATIVE_SCENARIO")?;
        let control = crate::core::server::ContentStageControl::default();
        let base = crate::core::server::content_staging::read_bounded_controlled(
            &source.join("addons/data/resourceDatabase.rdb"),
            MAX_RESOURCE_DATABASE_BYTES,
            &control,
        )
        .map_err(|error| std::io::Error::other(error.to_string()))?;
        assert!(
            contains_resource(&base, "{2BBBE828037C6F4B}Missions/22_GM_Arland.conf")
                .map_err(std::io::Error::other)?
        );
        let addon = crate::core::server::content_staging::read_bounded_controlled(
            &addon.join("resourceDatabase.rdb"),
            MAX_RESOURCE_DATABASE_BYTES,
            &control,
        )
        .map_err(|error| std::io::Error::other(error.to_string()))?;
        assert!(contains_resource(&addon, &scenario).map_err(std::io::Error::other)?);
        assert!(!contains_resource(&base, &scenario).map_err(std::io::Error::other)?);
        Ok(())
    }
}
