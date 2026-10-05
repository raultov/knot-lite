//! SHA-256 hex helpers shared by the inventory and the shard text hashes.

use sha2::{Digest, Sha256};

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Lower-case hex encoding of `bytes`.
pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// Finish a streaming hasher into 64 lower-case hex characters.
pub(crate) fn finish_hex(hasher: Sha256) -> String {
    hex_lower(hasher.finalize().as_slice())
}

/// SHA-256 of `bytes` as 64 lower-case hex characters.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    finish_hex(hasher)
}

/// First 16 hex characters (64 bits) of a full hex digest.
pub(crate) fn short16(mut full_hex: String) -> String {
    full_hex.truncate(16);
    full_hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_lower_encodes_every_nibble() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(hex_lower(&[]), "");
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn short16_keeps_the_first_sixteen_characters() {
        assert_eq!(short16(sha256_hex(b"abc")), "ba7816bf8f01cfea");
    }
}
