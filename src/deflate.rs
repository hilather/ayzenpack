//! Raw DEFLATE (ZIP method 8): flate2/miniz, zlib-rs, and BTYPE-00 stored.
//!
//! Codec strings: `deflate-raw:flate2:<level>`, `deflate-raw:zlib:<level>`,
//! `deflate-raw:stored`. Not a zlib/gzip container.

use std::io::{self, Write};

use flate2::write::{DeflateDecoder, DeflateEncoder};
use flate2::Compression;
use zlib_rs::{compress_bound, compress_slice, DeflateConfig, ReturnCode};

use crate::error::{AyzenpackError, Result};

pub const FLATE2_PREFIX: &str = "deflate-raw:flate2:";
pub const ZLIB_PREFIX: &str = "deflate-raw:zlib:";
pub const STORED_CODEC: &str = "deflate-raw:stored";
const DEFAULT_REBUILD_LEVEL: u32 = 6;
const ZLIB_LEVELS: [u32; 3] = [1, 6, 9];

/// Closed codec set recorded on a hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CdataCodec {
    Flate2(u32),
    Zlib(u32),
    Stored,
}

impl CdataCodec {
    pub fn as_str(self) -> String {
        match self {
            CdataCodec::Flate2(level) => format!("{FLATE2_PREFIX}{level}"),
            CdataCodec::Zlib(level) => format!("{ZLIB_PREFIX}{level}"),
            CdataCodec::Stored => STORED_CODEC.to_string(),
        }
    }
}

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
    inflate_raw_capped(data, u64::MAX)
}

/// Inflate raw DEFLATE, refusing to grow the output past `max` bytes.
pub fn inflate_raw_capped(data: &[u8], max: u64) -> Result<Vec<u8>> {
    let cap = usize::try_from(max).unwrap_or(usize::MAX);
    let mut dec = DeflateDecoder::new(CapWriter {
        inner: Vec::new(),
        max: cap,
    });
    dec.write_all(data).map_err(io_err)?;
    let w = dec.finish().map_err(io_err)?;
    Ok(w.inner)
}

struct CapWriter {
    inner: Vec<u8>,
    max: usize,
}

impl Write for CapWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let next = self.inner.len().saturating_add(buf.len());
        if next > self.max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "inflate exceeded cap",
            ));
        }
        self.inner.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn zlib_raw_deflate(data: &[u8], level: u32) -> Result<Vec<u8>> {
    let mut cfg = DeflateConfig::new(level as i32);
    cfg.window_bits = -15;
    let bound = compress_bound(data.len()).max(16);
    let mut buf = vec![0u8; bound];
    let (out, rc) = compress_slice(&mut buf, data, cfg);
    if rc != ReturnCode::Ok {
        return Err(AyzenpackError::FormatOwned(format!(
            "zlib-rs raw deflate level {level} failed: {rc:?}"
        )));
    }
    Ok(out.to_vec())
}

/// Raw DEFLATE BTYPE 00 stored blocks (final bit on the last block).
pub fn stored_deflate(plain: &[u8]) -> Vec<u8> {
    if plain.is_empty() {
        return vec![0x01, 0x00, 0x00, 0xff, 0xff];
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < plain.len() {
        let n = (plain.len() - i).min(65535);
        let last = i + n == plain.len();
        out.push(if last { 0x01 } else { 0x00 });
        let len = n as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(&plain[i..i + n]);
        i += n;
    }
    out
}

pub fn encode_codec(codec: CdataCodec, plain: &[u8]) -> Result<Vec<u8>> {
    match codec {
        CdataCodec::Flate2(level) => deflate_raw(plain, level),
        CdataCodec::Zlib(level) => zlib_raw_deflate(plain, level),
        CdataCodec::Stored => Ok(stored_deflate(plain)),
    }
}

pub fn parse_codec(codec: &str) -> Result<CdataCodec> {
    if codec == STORED_CODEC {
        return Ok(CdataCodec::Stored);
    }
    if let Some(rest) = codec.strip_prefix(FLATE2_PREFIX) {
        let level: u32 = rest.parse().map_err(|_| {
            AyzenpackError::FormatOwned(format!("unrecognized cdata_codec {codec}"))
        })?;
        if !matches!(level, 1 | 3 | 6 | 9) {
            return Err(AyzenpackError::FormatOwned(format!(
                "unrecognized cdata_codec {codec}"
            )));
        }
        return Ok(CdataCodec::Flate2(level));
    }
    if let Some(rest) = codec.strip_prefix(ZLIB_PREFIX) {
        let level: u32 = rest.parse().map_err(|_| {
            AyzenpackError::FormatOwned(format!("unrecognized cdata_codec {codec}"))
        })?;
        if !matches!(level, 1 | 6 | 9) {
            return Err(AyzenpackError::FormatOwned(format!(
                "unrecognized cdata_codec {codec}"
            )));
        }
        return Ok(CdataCodec::Zlib(level));
    }
    Err(AyzenpackError::FormatOwned(format!(
        "unrecognized cdata_codec {codec}"
    )))
}

pub fn rebuild_level() -> u32 {
    DEFAULT_REBUILD_LEVEL
}

/// Flate2/miniz only. Used to prove a bitstream is not a miniz collision.
pub fn match_flate2(plain: &[u8], want: &[u8], flags: u16) -> Result<Option<u32>> {
    for level in trial_levels(flags) {
        if deflate_raw(plain, level)? == want {
            return Ok(Some(level));
        }
    }
    Ok(None)
}

/// First matching codec, or `None`. Order: zlib GPBF∩{1,6,9}, remaining zlib
/// {1,6,9}, flate2 trial_levels, then stored.
pub fn match_deflate(plain: &[u8], want: &[u8], flags: u16) -> Result<Option<CdataCodec>> {
    let hint = gpbf_deflate_hint(flags);
    let mut zlib_order = Vec::new();
    if ZLIB_LEVELS.contains(&hint) {
        zlib_order.push(hint);
    }
    for lvl in ZLIB_LEVELS {
        if !zlib_order.contains(&lvl) {
            zlib_order.push(lvl);
        }
    }
    for level in zlib_order {
        if zlib_raw_deflate(plain, level)? == want {
            return Ok(Some(CdataCodec::Zlib(level)));
        }
    }
    if let Some(level) = match_flate2(plain, want, flags)? {
        return Ok(Some(CdataCodec::Flate2(level)));
    }
    if stored_deflate(plain) == want {
        return Ok(Some(CdataCodec::Stored));
    }
    Ok(None)
}

fn io_err(source: io::Error) -> AyzenpackError {
    AyzenpackError::Io { source, path: None }
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
        assert_eq!(
            match_deflate(&plain, &c, 0).unwrap(),
            Some(CdataCodec::Flate2(6))
        );
    }

    #[test]
    fn stored_block_is_stored_codec_hit() {
        let plain = vec![b'a'; 256];
        let stored = stored_deflate(&plain);
        assert_eq!(inflate_raw(&stored).unwrap(), plain);
        assert_eq!(match_flate2(&plain, &stored, 0).unwrap(), None);
        assert_eq!(
            match_deflate(&plain, &stored, 0).unwrap(),
            Some(CdataCodec::Stored)
        );
    }

    #[test]
    fn stored_deflate_multi_block_over_u16() {
        let plain = vec![b'b'; 70_000];
        let stored = stored_deflate(&plain);
        assert_eq!(inflate_raw(&stored).unwrap(), plain);
        assert_eq!(
            match_deflate(&plain, &stored, 0).unwrap(),
            Some(CdataCodec::Stored)
        );
    }

    /// Empty non-final stored block + a closed-set stream. Inflates, but is not
    /// a zlib/flate2/stored hit (Test 4 unknown-deflate).
    pub fn unknown_deflate_prefix(plain: &[u8]) -> Result<Vec<u8>> {
        let mut out = vec![0x00, 0x00, 0x00, 0xff, 0xff];
        out.extend_from_slice(&zlib_raw_deflate(plain, 6)?);
        Ok(out)
    }

    #[test]
    fn unknown_deflate_prefix_is_miss() {
        let plain = b"unknown-deflate sibling miss payload".repeat(8);
        let cdata = unknown_deflate_prefix(&plain).unwrap();
        assert_eq!(inflate_raw(&cdata).unwrap(), plain.as_slice());
        assert_eq!(match_flate2(&plain, &cdata, 0).unwrap(), None);
        assert_eq!(
            match_deflate(&plain, &cdata, 0).unwrap(),
            None,
            "prefixed bitstream must stay a miss"
        );
    }

    #[test]
    fn zlib_raw_is_flate2_miss_and_zlib_hit() {
        let plain = b"zlib-rs classic fixture payload for 0.2.4".repeat(8);
        let cdata = zlib_raw_deflate(&plain, 6).unwrap();
        assert_eq!(inflate_raw(&cdata).unwrap(), plain.as_slice());
        assert_eq!(
            match_flate2(&plain, &cdata, 0).unwrap(),
            None,
            "baked zlib bitstream must not be a miniz collision"
        );
        assert_eq!(
            match_deflate(&plain, &cdata, 0).unwrap(),
            Some(CdataCodec::Zlib(6))
        );
    }

    #[test]
    fn parse_codec_accepts_closed_set() {
        assert_eq!(
            parse_codec("deflate-raw:flate2:6").unwrap(),
            CdataCodec::Flate2(6)
        );
        assert_eq!(
            parse_codec("deflate-raw:zlib:9").unwrap(),
            CdataCodec::Zlib(9)
        );
        assert_eq!(parse_codec(STORED_CODEC).unwrap(), CdataCodec::Stored);
        assert!(parse_codec("deflate-raw:flate2:2").is_err());
        assert!(parse_codec("deflate-raw:zlib:3").is_err());
        assert!(parse_codec("zlib:6").is_err());
    }

    #[test]
    fn inflate_raw_capped_rejects_oversize() {
        let plain = vec![b'x'; 64];
        let c = deflate_raw(&plain, 6).unwrap();
        assert!(inflate_raw_capped(&c, 8).is_err());
        assert_eq!(inflate_raw_capped(&c, 64).unwrap(), plain);
    }
}
