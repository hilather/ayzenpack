//! Raw DEFLATE (ZIP method 8) via flate2 / miniz_oxide.
//!
//! Not zlib, not gzip. Codec strings are `deflate-raw:flate2:<level>`.

use std::io::{self, Write};

use flate2::write::{DeflateDecoder, DeflateEncoder};
use flate2::Compression;

use crate::error::{AyzenpackError, Result};

pub const CODEC_PREFIX: &str = "deflate-raw:flate2:";
const DEFAULT_REBUILD_LEVEL: u32 = 6;

/// APPNOTE 4.4.4 bits 1–2 → Info-ZIP-style level hint.
pub fn gpbf_deflate_hint(flags: u16) -> u32 {
    match (flags >> 1) & 0b11 {
        0 => 6,
        1 => 9,
        2 => 3,
        _ => 1,
    }
}

/// Trial order: GPBF hint first, then 6, 9, 1 (deduped).
pub fn trial_levels(flags: u16) -> Vec<u32> {
    let mut out = vec![gpbf_deflate_hint(flags), 6, 9, 1];
    let mut seen = [false; 10];
    out.retain(|&lvl| {
        let i = lvl as usize;
        if i >= seen.len() || seen[i] {
            return false;
        }
        seen[i] = true;
        true
    });
    out
}

pub fn deflate_raw(data: &[u8], level: u32) -> Result<Vec<u8>> {
    let mut enc = DeflateEncoder::new(Vec::new(), Compression::new(level));
    enc.write_all(data).map_err(io_err)?;
    enc.finish().map_err(io_err)
}

pub fn inflate_raw(data: &[u8]) -> Result<Vec<u8>> {
    let mut dec = DeflateDecoder::new(Vec::new());
    dec.write_all(data).map_err(io_err)?;
    dec.finish().map_err(io_err)
}

pub fn codec_string(level: u32) -> String {
    format!("{CODEC_PREFIX}{level}")
}

pub fn parse_codec(codec: &str) -> Result<u32> {
    let rest = codec.strip_prefix(CODEC_PREFIX).ok_or_else(|| {
        AyzenpackError::FormatOwned(format!("unrecognized cdata_codec {codec}"))
    })?;
    let level: u32 = rest.parse().map_err(|_| {
        AyzenpackError::FormatOwned(format!("unrecognized cdata_codec {codec}"))
    })?;
    if !matches!(level, 1 | 3 | 6 | 9) {
        return Err(AyzenpackError::FormatOwned(format!(
            "unrecognized cdata_codec {codec}"
        )));
    }
    Ok(level)
}

pub fn rebuild_level() -> u32 {
    DEFAULT_REBUILD_LEVEL
}

/// First matching level, or `None` if miniz_oxide cannot reproduce `want`.
pub fn match_deflate(plain: &[u8], want: &[u8], flags: u16) -> Result<Option<u32>> {
    for level in trial_levels(flags) {
        if deflate_raw(plain, level)? == want {
            return Ok(Some(level));
        }
    }
    Ok(None)
}

fn io_err(source: io::Error) -> AyzenpackError {
    AyzenpackError::Io {
        source,
        path: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpbf_hint_appnote_mapping() {
        assert_eq!(gpbf_deflate_hint(0), 6);
        assert_eq!(gpbf_deflate_hint(1 << 1), 9);
        assert_eq!(gpbf_deflate_hint(1 << 2), 3);
        assert_eq!(gpbf_deflate_hint((1 << 1) | (1 << 2)), 1);
    }

    #[test]
    fn trial_levels_dedup_hint() {
        assert_eq!(trial_levels(0), vec![6, 9, 1]);
        assert_eq!(trial_levels(1 << 1), vec![9, 6, 1]);
        assert_eq!(trial_levels(1 << 2), vec![3, 6, 9, 1]);
    }

    #[test]
    fn roundtrip_raw_deflate_level_6() {
        let plain = vec![b'a'; 64];
        let c = deflate_raw(&plain, 6).unwrap();
        assert_ne!(c, plain);
        assert_eq!(inflate_raw(&c).unwrap(), plain);
        assert_eq!(match_deflate(&plain, &c, 0).unwrap(), Some(6));
    }

    #[test]
    fn stored_block_is_a_miss() {
        let plain = vec![b'a'; 256];
        let stored = raw_stored_block(&plain);
        assert_eq!(inflate_raw(&stored).unwrap(), plain);
        assert_eq!(match_deflate(&plain, &stored, 0).unwrap(), None);
    }

    fn raw_stored_block(plain: &[u8]) -> Vec<u8> {
        let len = u16::try_from(plain.len()).unwrap();
        let nlen = !len;
        let mut out = vec![0x01];
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&nlen.to_le_bytes());
        out.extend_from_slice(plain);
        out
    }

    #[test]
    fn parse_codec_rejects_unknown() {
        assert_eq!(parse_codec("deflate-raw:flate2:6").unwrap(), 6);
        assert!(parse_codec("deflate-raw:flate2:2").is_err());
        assert!(parse_codec("zlib:6").is_err());
    }
}
