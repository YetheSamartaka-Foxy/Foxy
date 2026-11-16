use serde::{Deserialize, Serialize};

pub(super) const MAX_BIKEY_BYTES: usize = 16 * 1024;
const MAX_AUTHORITY_BYTES: usize = 128;
const PUBLIC_KEY_BLOB: u8 = 6;
const PRIVATE_KEY_BLOB: u8 = 7;
const RSA1_MAGIC: u32 = 0x3141_5352;
const CALG_RSA_SIGN: u32 = 0x0000_2400;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicKeyIdentity {
    pub authority: String,
    pub bits: u32,
    pub fingerprint: String,
}

pub(super) fn inspect(bytes: &[u8]) -> Result<PublicKeyIdentity, String> {
    if bytes.len() > MAX_BIKEY_BYTES {
        return Err("BIKEY exceeds the 16 KiB limit".to_owned());
    }

    let terminator = bytes
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| "BIKEY authority is not NUL-terminated".to_owned())?;
    if terminator == 0 || terminator > MAX_AUTHORITY_BYTES {
        return Err("BIKEY authority length is invalid".to_owned());
    }
    let authority_bytes = &bytes[..terminator];
    if !authority_bytes
        .iter()
        .all(|byte| (0x20..=0x7e).contains(byte))
    {
        return Err("BIKEY authority must contain printable ASCII".to_owned());
    }
    let authority = std::str::from_utf8(authority_bytes)
        .map_err(|_| "BIKEY authority is not valid ASCII".to_owned())?
        .to_owned();

    let length_start = terminator + 1;
    let prefix_end = length_start
        .checked_add(4)
        .ok_or_else(|| "BIKEY length prefix overflows".to_owned())?;
    let prefix = bytes
        .get(length_start..prefix_end)
        .ok_or_else(|| "BIKEY length prefix is truncated".to_owned())?;
    let blob_len = u32::from_le_bytes(prefix.try_into().expect("four-byte slice")) as usize;
    let blob_start = prefix_end;
    let blob_end = blob_start
        .checked_add(blob_len)
        .ok_or_else(|| "BIKEY public key length overflows".to_owned())?;
    if blob_end != bytes.len() {
        return Err("BIKEY public key length does not match file length".to_owned());
    }
    let blob = &bytes[blob_start..blob_end];
    if blob.len() < 20 {
        return Err("BIKEY public key blob is truncated".to_owned());
    }
    if blob[0] == PRIVATE_KEY_BLOB {
        return Err("BIKEY contains a private key blob".to_owned());
    }
    if blob[0] != PUBLIC_KEY_BLOB || blob[1] != 2 || blob[2..4] != [0, 0] {
        return Err("BIKEY header is not a supported public key blob".to_owned());
    }
    if read_u32(&blob[4..8]) != CALG_RSA_SIGN || read_u32(&blob[8..12]) != RSA1_MAGIC {
        return Err("BIKEY blob is not an RSA signing public key".to_owned());
    }

    let bits = read_u32(&blob[12..16]);
    if !(512..=8192).contains(&bits) || !bits.is_multiple_of(8) {
        return Err("BIKEY RSA bit length is invalid".to_owned());
    }
    let exponent = read_u32(&blob[16..20]);
    if exponent < 3 || exponent.is_multiple_of(2) {
        return Err("BIKEY RSA public exponent is invalid".to_owned());
    }
    let modulus_len = (bits / 8) as usize;
    if blob.len() != 20 + modulus_len || blob[20..].iter().all(|byte| *byte == 0) {
        return Err("BIKEY RSA modulus length or value is invalid".to_owned());
    }

    let fingerprint = blake3::hash(blob).to_hex().to_string();
    Ok(PublicKeyIdentity {
        authority,
        bits,
        fingerprint,
    })
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("four-byte slice"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public_bikey(authority: &[u8]) -> Vec<u8> {
        let mut blob = vec![
            PUBLIC_KEY_BLOB,
            2,
            0,
            0,
            0x00,
            0x24,
            0x00,
            0x00,
            b'R',
            b'S',
            b'A',
            b'1',
            0x00,
            0x08,
            0x00,
            0x00,
            0x01,
            0x00,
            0x01,
            0x00,
        ];
        blob.extend(std::iter::repeat_n(0x5a, 256));
        let mut bytes = authority.to_vec();
        bytes.push(0);
        bytes.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        bytes.extend(blob);
        bytes
    }

    #[test]
    fn accepts_rsa_public_blob_and_fingerprints_the_blob() {
        let bytes = public_bikey(b"FoxyNativeAlpha");
        let identity = inspect(&bytes).unwrap();
        assert_eq!(identity.authority, "FoxyNativeAlpha");
        assert_eq!(identity.bits, 2048);
        assert_eq!(
            identity.fingerprint,
            blake3::hash(&bytes[20..]).to_hex().as_str()
        );
    }

    #[test]
    fn rejects_private_blobs_even_with_a_public_key_filename() {
        let mut bytes = public_bikey(b"FoxyNativeAlpha");
        let blob_start = b"FoxyNativeAlpha".len() + 1 + 4;
        bytes[blob_start] = PRIVATE_KEY_BLOB;
        assert!(inspect(&bytes).unwrap_err().contains("private key"));
    }

    #[test]
    fn rejects_mismatched_lengths_and_invalid_authorities() {
        let bytes = public_bikey(b"FoxyNativeAlpha");
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(inspect(&trailing).is_err());

        let mut collision = public_bikey(b"Foxy\nNative");
        assert!(inspect(&collision).is_err());
        collision.truncate(128);
        assert!(inspect(&collision).is_err());
    }
}
