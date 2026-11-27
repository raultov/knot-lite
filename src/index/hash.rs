//! SHA-256 hex helpers shared by the inventory and the shard text hashes,
//! and the file change time the cheap "did it change?" checks rely on.

use std::time::SystemTime;

use sha2::{Digest, Sha256};

/// When the file's inode last changed (content, or metadata such as the
/// mtime itself), where the platform records it. A write always moves it
/// forward and no tool can set it back, unlike the mtime (`cp -p`,
/// `rsync -t`, `tar x`, `touch -d`). `None` where there is no such time.
#[cfg(unix)]
pub(crate) fn inode_changed_at(meta: &std::fs::Metadata) -> Option<SystemTime> {
    use std::os::unix::fs::MetadataExt as _;

    let secs = u64::try_from(meta.ctime()).ok()?;
    let nanos = u32::try_from(meta.ctime_nsec()).ok()?;
    SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::new(secs, nanos))
}

/// When the file's inode last changed: not recorded on this platform.
#[cfg(not(unix))]
pub(crate) fn inode_changed_at(_meta: &std::fs::Metadata) -> Option<SystemTime> {
    None
}

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
