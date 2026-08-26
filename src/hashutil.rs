use std::io::Read;

use sha2::{Digest, Sha256};

use crate::error::{AyzenpackError, Result};

const HASH_CHUNK: usize = 16 * 1024;

pub fn blake3_bytes(data: &[u8]) -> [u8; 32] {
    *blake3::hash(data).as_bytes()
}

pub fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// One RAM pass: both hashers see each chunk before advancing. Dehydrate must call this,
/// not `blake3_bytes` then `sha256_bytes`. Test oracle may still compare against the singles.
pub fn hash_both(data: &[u8]) -> ([u8; 32], [u8; 32]) {
    let mut b3 = blake3::Hasher::new();
    let mut sha = Sha256::new();
    for chunk in data.chunks(HASH_CHUNK) {
        b3.update(chunk);
        sha.update(chunk);
    }
    (*b3.finalize().as_bytes(), sha.finalize().into())
}

pub fn hex_lower(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

/// Decode mixed-case hex of any even length (local headers, descriptors).
pub fn parse_hex(s: &str) -> Result<Vec<u8>> {
    if s.len() % 2 != 0 {
        return Err(AyzenpackError::Format("hex length must be even"));
    }
    hex::decode(s).map_err(|_| AyzenpackError::Format("invalid hex"))
}

/// Stream both digests from a reader (restored-file identity checks).
pub fn hash_reader<R: Read>(mut reader: R) -> std::io::Result<([u8; 32], [u8; 32])> {
    let mut b3 = blake3::Hasher::new();
    let mut sha = Sha256::new();
    let mut buf = [0u8; HASH_CHUNK];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        b3.update(&buf[..n]);
        sha.update(&buf[..n]);
    }
    Ok((*b3.finalize().as_bytes(), sha.finalize().into()))
}

pub fn parse_blake3_hex(s: &str) -> Result<[u8; 32]> {
    if s.len() != 64 {
        return Err(AyzenpackError::Format(
            "blake3 hex must be exactly 64 hex characters",
        ));
    }
    let mut out = [0u8; 32];
    hex::decode_to_slice(s, &mut out).map_err(|_| AyzenpackError::Format("invalid blake3 hex"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY_BLAKE3: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const ONE_BYTE_BLAKE3: &str =
        "2d3adedff11b61f14c886e35afa036736dcd87a74d27b5c1510225d0f592e213";
    const ONE_BYTE_SHA256: &str =
        "6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d";

    fn parse_hex32(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        hex::decode_to_slice(s, &mut out).expect("test vector is valid hex");
        out
    }

    #[test]
    fn empty_blake3_matches_known_vector() {
        // Guards wiring SHA-256 (or another digest) into blake3_bytes.
        assert_eq!(blake3_bytes(b""), parse_hex32(EMPTY_BLAKE3));
        assert_eq!(hex_lower(&blake3_bytes(b"")), EMPTY_BLAKE3);
    }

    #[test]
    fn empty_sha256_matches_known_vector() {
        // Guards wiring BLAKE3 into sha256_bytes or dropping the empty-input IV.
        assert_eq!(sha256_bytes(b""), parse_hex32(EMPTY_SHA256));
        assert_eq!(hex_lower(&sha256_bytes(b"")), EMPTY_SHA256);
    }

    #[test]
    fn hash_both_matches_singles_on_empty_and_64kib() {
        // Guards a two-pass hash_both (or mismatched chunking) against the single-hasher oracle.
        let big = vec![0x5a; 64 * 1024];
        for data in [&[][..], big.as_slice()] {
            let (b3, sha) = hash_both(data);
            assert_eq!(b3, blake3_bytes(data));
            assert_eq!(sha, sha256_bytes(data));
        }
    }

    #[test]
    fn hex_roundtrip_32_bytes() {
        // Guards uppercase encode or a parser that drops a nibble.
        let bytes: [u8; 32] = blake3_bytes(b"ayzenpack");
        let hex = hex_lower(&bytes);
        assert_eq!(hex.len(), 64);
        assert!(
            hex.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "hex_lower must emit lowercase: {hex}"
        );
        assert_eq!(parse_blake3_hex(&hex).unwrap(), bytes);
    }

    #[test]
    fn odd_length_hex_fails() {
        // Guards a parser that silently pads odd-length hex.
        for s in ["a", "abc", "af1", EMPTY_BLAKE3.trim_end_matches('2')] {
            let err = parse_blake3_hex(s).unwrap_err();
            assert!(
                matches!(
                    err,
                    AyzenpackError::Format(_) | AyzenpackError::FormatOwned(_)
                ),
                "odd/wrong length {s:?} must be Format/FormatOwned, got {err:?}"
            );
        }
    }

    #[test]
    fn mixed_case_hex_parses_to_same_bytes() {
        // Guards an uppercase-only (or lowercase-only) hex parser.
        let mixed: String = EMPTY_BLAKE3
            .chars()
            .enumerate()
            .map(|(i, c)| {
                if i % 2 == 0 {
                    c.to_ascii_uppercase()
                } else {
                    c
                }
            })
            .collect();
        assert_ne!(mixed, EMPTY_BLAKE3);
        assert_eq!(
            parse_blake3_hex(&mixed).unwrap(),
            parse_blake3_hex(EMPTY_BLAKE3).unwrap()
        );
        assert_eq!(
            parse_blake3_hex(&EMPTY_BLAKE3.to_ascii_uppercase()).unwrap(),
            parse_hex32(EMPTY_BLAKE3)
        );
    }

    #[test]
    fn parse_hex_accepts_mixed_case_and_rejects_odd() {
        assert_eq!(parse_hex("504B0304").unwrap(), b"PK\x03\x04");
        assert_eq!(parse_hex("504b0304").unwrap(), b"PK\x03\x04");
        assert!(parse_hex("abc").is_err());
    }

    #[test]
    fn one_byte_payload_hashes_are_stable() {
        // Guards swapping hashers or changing empty-vs-one-byte IVs.
        let data = [0u8];
        assert_eq!(blake3_bytes(&data), parse_hex32(ONE_BYTE_BLAKE3));
        assert_eq!(sha256_bytes(&data), parse_hex32(ONE_BYTE_SHA256));
        let (b3, sha) = hash_both(&data);
        assert_eq!(b3, parse_hex32(ONE_BYTE_BLAKE3));
        assert_eq!(sha, parse_hex32(ONE_BYTE_SHA256));
    }
}
