//! The keyed per-image ID: the first 16 bytes of HMAC-SHA-256 (RFC 2104) keyed by the installation's
//! 32-byte image key over the photograph's catalog fingerprint (the SHA-256 of its original), sent
//! as 32 lowercase hexadecimal characters.
//!
//! It reads no file: the fingerprint is already held when a photograph is adopted, and the ID costs
//! one HMAC (four SHA-256 compressions). Without the key, which never leaves the machine, the ID
//! cannot be matched to a file, even by someone holding the photograph. Neither the key nor the
//! fingerprint is ever formatted: [`ImageKey`]'s `Debug` is redacted and nothing here logs.
use sha2::{Digest, Sha256};
use std::fmt;
use zeroize::Zeroize;

/// SHA-256's block size, the HMAC pad length.
const BLOCK: usize = 64;

/// The installation's image key: 32 random bytes created and removed with the installation ID,
/// never uploaded, logged or returned by a read. Zeroed on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct ImageKey([u8; 32]);

impl ImageKey {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The key's bytes, for the consent record that persists it.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The image ID of the photograph whose original's SHA-256 is `fingerprint`.
    pub fn image_id(&self, fingerprint: &[u8; 32]) -> ImageId {
        let mac = hmac_sha256(&self.0, fingerprint);
        let mut id = [0; 16];
        id.copy_from_slice(&mac[..16]);
        ImageId(id)
    }

    /// The image ID of a catalog fingerprint as the catalog stores it: 64 hexadecimal characters.
    /// `None` for anything else.
    pub fn image_id_of_hex(&self, fingerprint: &str) -> Option<ImageId> {
        let mut bytes = [0; 32];
        decode_hex(fingerprint.as_bytes(), &mut bytes)?;
        let id = self.image_id(&bytes);
        bytes.zeroize();
        Some(id)
    }
}

impl Drop for ImageKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for ImageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ImageKey(redacted)")
    }
}

/// A photograph's keyed image ID: 16 bytes, 32 lowercase hexadecimal characters on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ImageId([u8; 16]);

impl ImageId {
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// The wire form, written into a stack buffer.
    pub fn to_hex(&self) -> [u8; 32] {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut out = [0; 32];
        for (index, byte) in self.0.iter().enumerate() {
            out[2 * index] = DIGITS[usize::from(byte >> 4)];
            out[2 * index + 1] = DIGITS[usize::from(byte & 0xF)];
        }
        out
    }

    /// Exactly 32 lowercase hexadecimal characters; nothing else.
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 32 || !bytes.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let mut id = [0; 16];
        decode_hex(bytes, &mut id)?;
        Some(Self(id))
    }
}

impl fmt::Debug for ImageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = self.to_hex();
        write!(
            f,
            "ImageId({})",
            std::str::from_utf8(&hex).unwrap_or_default()
        )
    }
}

/// Decodes `2 * out.len()` hexadecimal digits of either case into `out`.
fn decode_hex(text: &[u8], out: &mut [u8]) -> Option<()> {
    if text.len() != 2 * out.len() {
        return None;
    }
    fn nibble(digit: u8) -> Option<u8> {
        match digit {
            b'0'..=b'9' => Some(digit - b'0'),
            b'a'..=b'f' => Some(digit - b'a' + 10),
            b'A'..=b'F' => Some(digit - b'A' + 10),
            _ => None,
        }
    }
    for (byte, pair) in out.iter_mut().zip(text.chunks_exact(2)) {
        *byte = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(())
}

/// HMAC-SHA-256 (RFC 2104) of `message` under `key`, over the workspace's locked `sha2`.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut pad = [0u8; BLOCK];
    for (pad, key) in pad.iter_mut().zip(&block) {
        *pad = key ^ 0x36;
    }
    let inner = Sha256::new()
        .chain_update(pad)
        .chain_update(message)
        .finalize();
    for (pad, key) in pad.iter_mut().zip(&block) {
        *pad = key ^ 0x5c;
    }
    let outer = Sha256::new()
        .chain_update(pad)
        .chain_update(inner)
        .finalize();
    block.zeroize();
    pad.zeroize();
    outer.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    /// RFC 4231 section 4, the HMAC-SHA-256 results of test cases 1 to 7 (case 5's truncated output
    /// compared on its first 16 bytes, as the RFC does).
    #[test]
    fn hmac_sha256_matches_rfc_4231() {
        let long_key = vec![0xaa; 131];
        let cases: [(Vec<u8>, Vec<u8>, &str); 7] = [
            (
                vec![0x0b; 20],
                b"Hi There".to_vec(),
                "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
            ),
            (
                b"Jefe".to_vec(),
                b"what do ya want for nothing?".to_vec(),
                "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
            ),
            (
                vec![0xaa; 20],
                vec![0xdd; 50],
                "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe",
            ),
            (
                unhex("0102030405060708090a0b0c0d0e0f10111213141516171819"),
                vec![0xcd; 50],
                "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b",
            ),
            (
                vec![0x0c; 20],
                b"Test With Truncation".to_vec(),
                "a3b6167473100ee06e0c796c2955552b",
            ),
            (
                long_key.clone(),
                b"Test Using Larger Than Block-Size Key - Hash Key First".to_vec(),
                "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
            ),
            (
                long_key,
                b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm."
                    .to_vec(),
                "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2",
            ),
        ];
        for (index, (key, message, expected)) in cases.iter().enumerate() {
            let mac = hex(&hmac_sha256(key, message));
            assert_eq!(
                &mac[..expected.len()],
                *expected,
                "RFC 4231 case {}",
                index + 1
            );
        }
    }

    #[test]
    fn the_image_id_is_the_first_16_bytes_of_the_hmac_in_lowercase_hex() {
        let key = ImageKey::from_bytes([7; 32]);
        let fingerprint: [u8; 32] = Sha256::digest(b"an original's bytes").into();
        let full = hmac_sha256(&[7; 32], &fingerprint);
        let id = key.image_id(&fingerprint);
        assert_eq!(id.as_bytes()[..], full[..16]);
        let wire = String::from_utf8(id.to_hex().to_vec()).unwrap();
        assert_eq!(wire, hex(&full[..16]));
        assert_eq!(ImageId::parse(&wire), Some(id));
        // The catalog's stored form gives the same ID, in either case.
        let stored = hex(&fingerprint);
        assert_eq!(key.image_id_of_hex(&stored), Some(id));
        assert_eq!(key.image_id_of_hex(&stored.to_uppercase()), Some(id));
        assert_eq!(key.image_id_of_hex(&stored[..63]), None);
        assert_eq!(key.image_id_of_hex(&format!("{}g", &stored[..63])), None);
    }

    #[test]
    fn another_key_gives_an_unrelated_id_and_neither_key_nor_fingerprint_appears() {
        let fingerprint: [u8; 32] = Sha256::digest(b"the same photograph").into();
        let first = ImageKey::from_bytes([1; 32]);
        let second = ImageKey::from_bytes([2; 32]);
        assert_ne!(first.image_id(&fingerprint), second.image_id(&fingerprint));
        let id = String::from_utf8(first.image_id(&fingerprint).to_hex().to_vec()).unwrap();
        assert!(!hex(&fingerprint).contains(&id) && !id.contains(&hex(&fingerprint)[..32]));
        assert!(!id.contains(&hex(&[1; 16])));
        assert_eq!(format!("{first:?}"), "ImageKey(redacted)");
    }

    #[test]
    fn only_32_lowercase_hex_characters_parse_as_an_image_id() {
        for bad in [
            "",
            "3f9c2a7e5b1d48c09e6f1a2b3c4d5e6",
            "3f9c2a7e5b1d48c09e6f1a2b3c4d5e6f0",
            "3F9C2A7E5B1D48C09E6F1A2B3C4D5E6F",
            "3f9c2a7e5b1d48c09e6f1a2b3c4d5e6g",
            "3f9c2a7e-5b1d-48c0-9e6f-1a2b3c4d",
        ] {
            assert_eq!(ImageId::parse(bad), None, "{bad:?}");
        }
        assert!(ImageId::parse("3f9c2a7e5b1d48c09e6f1a2b3c4d5e6f").is_some());
    }
}
