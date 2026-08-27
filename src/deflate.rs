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
/// Trial remainder after GPBF hint. Pin this order; first-match-wins on collision.
const ZLIB_LEVELS: [u32; 4] = [1, 3, 6, 9];

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
        if !matches!(level, 1 | 3 | 6 | 9) {
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

/// First matching codec, or `None`. Order: zlib GPBF∩{1,3,6,9}, remaining zlib
/// {1,3,6,9}, flate2 trial_levels, then stored. First-match-wins.
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
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use zlib_rs::Strategy;

    use crate::exact::{slice_from_archive, ExactLocal};

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
        assert_eq!(inflate_raw_capped(&c, u64::MAX).unwrap(), plain);
        assert_eq!(
            match_deflate(&plain, &c, 0).unwrap(),
            Some(CdataCodec::Flate2(6))
        );
    }

    #[test]
    fn stored_block_is_stored_codec_hit() {
        let plain = vec![b'a'; 256];
        let stored = stored_deflate(&plain);
        assert_eq!(inflate_raw_capped(&stored, u64::MAX).unwrap(), plain);
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
        assert_eq!(inflate_raw_capped(&stored, u64::MAX).unwrap(), plain);
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
        assert_eq!(
            inflate_raw_capped(&cdata, u64::MAX).unwrap(),
            plain.as_slice()
        );
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
        assert_eq!(
            inflate_raw_capped(&cdata, u64::MAX).unwrap(),
            plain.as_slice()
        );
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
    fn zlib_raw_level_3_is_flate2_miss_and_zlib_hit() {
        let plain = b"zlib-rs gpbf-fast level-3 fixture payload".repeat(8);
        let c3 = zlib_raw_deflate(&plain, 3).unwrap();
        let c1 = zlib_raw_deflate(&plain, 1).unwrap();
        let c6 = zlib_raw_deflate(&plain, 6).unwrap();
        let c9 = zlib_raw_deflate(&plain, 9).unwrap();
        assert_ne!(c3, c1);
        assert_ne!(c3, c6);
        assert_ne!(c3, c9);
        assert_eq!(inflate_raw_capped(&c3, u64::MAX).unwrap(), plain.as_slice());
        assert_eq!(
            match_flate2(&plain, &c3, 1 << 2).unwrap(),
            None,
            "baked zlib-3 bitstream must not be a miniz collision"
        );
        // flags=0 tries zlib-6 first; distinguishing payload still matches 3.
        assert_eq!(
            match_deflate(&plain, &c3, 0).unwrap(),
            Some(CdataCodec::Zlib(3))
        );
        assert_eq!(
            match_deflate(&plain, &c3, 1 << 2).unwrap(),
            Some(CdataCodec::Zlib(3))
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
        assert_eq!(
            parse_codec("deflate-raw:zlib:3").unwrap(),
            CdataCodec::Zlib(3)
        );
        assert_eq!(parse_codec(STORED_CODEC).unwrap(), CdataCodec::Stored);
        assert!(parse_codec("deflate-raw:flate2:2").is_err());
        assert!(parse_codec("deflate-raw:zlib:2").is_err());
        assert!(parse_codec("zlib:6").is_err());
    }

    #[test]
    fn inflate_raw_capped_rejects_oversize() {
        let plain = vec![b'x'; 64];
        let c = deflate_raw(&plain, 6).unwrap();
        assert!(inflate_raw_capped(&c, 8).is_err());
        assert_eq!(inflate_raw_capped(&c, 64).unwrap(), plain);
    }

    /// Extra zlib-rs levels that fit `deflate-raw:zlib:<n>`. Not on the dehydrate path.
    const EXTRA_ZLIB_LEVELS: [u32; 5] = [2, 4, 5, 7, 8];
    const MIX_PINNED_DESTS: &[&str] = &["failureaccess-1.0.2.jar", "slf4j-api-2.0.16.jar"];
    const LOCK_JSON: &str =
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/ci/corpus.lock.json"));
    const CORPUS_YML: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.github/workflows/corpus.yml"
    ));

    #[test]
    fn codec_probe_env_is_not_in_corpus_workflow() {
        assert!(
            !CORPUS_YML.contains("AYZENPACK_CODEC_PROBE"),
            "do not run extra-level probe in the 25-minute corpus.yml job"
        );
    }

    fn lockfile_probe_dests() -> Vec<String> {
        let root: serde_json::Value =
            serde_json::from_str(LOCK_JSON).expect("ci/corpus.lock.json must be valid JSON");
        let mut names: Vec<String> = root["artifacts"]
            .as_array()
            .expect("lockfile artifacts array")
            .iter()
            .filter_map(|a| a.get("dest")?.as_str().map(str::to_string))
            .filter(|n| {
                n.starts_with("lucene-")
                    || n.starts_with("jackson-")
                    || MIX_PINNED_DESTS.contains(&n.as_str())
            })
            .collect();
        names.sort();
        names.dedup();
        names
    }

    fn method8_file_flags(local: &ExactLocal) -> Option<u16> {
        if local.header.len() < 30 {
            return None;
        }
        let method = u16::from_le_bytes([local.header[8], local.header[9]]);
        if method != 8 {
            return None;
        }
        let name_len = u16::from_le_bytes([local.header[26], local.header[27]]) as usize;
        if local.header.len() < 30 + name_len {
            return None;
        }
        if local.header[30..30 + name_len].ends_with(b"/") {
            return None;
        }
        Some(u16::from_le_bytes([local.header[6], local.header[7]]))
    }

    fn inflate_method8(local: &ExactLocal) -> Option<Vec<u8>> {
        let uncomp = u32::from_le_bytes(local.header.get(22..26)?.try_into().ok()?) as u64;
        let cap = if uncomp == 0 { u64::MAX } else { uncomp.max(1) };
        match inflate_raw_capped(&local.cdata, cap) {
            Ok(p) => Some(p),
            Err(_) if cap != u64::MAX => inflate_raw_capped(&local.cdata, u64::MAX).ok(),
            Err(_) => None,
        }
    }

    fn zlib_raw_cfg(
        plain: &[u8],
        level: i32,
        strategy: Strategy,
        mem_level: i32,
    ) -> Option<Vec<u8>> {
        let mut cfg = DeflateConfig::new(level);
        cfg.window_bits = -15;
        cfg.strategy = strategy;
        cfg.mem_level = mem_level;
        let bound = compress_bound(plain.len()).max(16);
        let mut buf = vec![0u8; bound];
        let (out, rc) = compress_slice(&mut buf, plain, cfg);
        (rc == ReturnCode::Ok).then(|| out.to_vec())
    }

    fn strategy_name(s: Strategy) -> &'static str {
        match s {
            Strategy::Default => "Default",
            Strategy::Filtered => "Filtered",
            Strategy::HuffmanOnly => "HuffmanOnly",
            Strategy::Rle => "Rle",
            Strategy::Fixed => "Fixed",
        }
    }

    /// Re-open source JARs: packs have no original method-8 cdata to trial.
    /// Skip unless `AYZENPACK_CORPUS_DIR` is set and `AYZENPACK_CODEC_PROBE=1`.
    #[test]
    fn probe_zlib_rs_extra_levels_on_lockfile_source_jars() {
        match std::env::var("AYZENPACK_CODEC_PROBE") {
            Ok(v) if v == "1" => {}
            _ => {
                eprintln!(
                    "skipping probe_zlib_rs_extra_levels_on_lockfile_source_jars: AYZENPACK_CODEC_PROBE!=1"
                );
                return;
            }
        }
        let Some(corpus) = std::env::var_os("AYZENPACK_CORPUS_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
        else {
            eprintln!(
                "skipping probe_zlib_rs_extra_levels_on_lockfile_source_jars: AYZENPACK_CORPUS_DIR not set"
            );
            return;
        };
        if !corpus.is_dir() {
            eprintln!(
                "skipping probe_zlib_rs_extra_levels_on_lockfile_source_jars: {} is not a directory",
                corpus.display()
            );
            return;
        }

        let mut dests = lockfile_probe_dests();
        assert!(
            !dests.is_empty(),
            "lockfile must list lucene/jackson or mix pinned dests"
        );
        dests.sort_by_key(|d| {
            std::fs::metadata(corpus.join(d))
                .map(|m| m.len())
                .unwrap_or(u64::MAX)
        });

        let mut jars_seen = 0u64;
        let mut jars_miss0_closed = 0u64;
        let mut jars_miss0_if_extra = 0u64;
        let mut tot_method8 = 0u64;
        let mut tot_closed_hit = 0u64;
        let mut tot_closed_miss = 0u64;
        let mut tot_extra_level = [0u64; 5];
        let mut tot_extra_any = 0u64;
        let mut tot_residual = 0u64;
        let mut strategy_hist: BTreeMap<String, u64> = BTreeMap::new();
        let mut tot_strategy_hits = 0u64;

        for dest in &dests {
            let path = corpus.join(dest);
            if !path.is_file() {
                eprintln!("codec-probe skip missing {}", path.display());
                continue;
            }
            let slice = match slice_from_archive(&path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("codec-probe skip slice {}: {e}", path.display());
                    continue;
                }
            };
            jars_seen += 1;
            let mut method8 = 0u64;
            let mut closed_hit = 0u64;
            let mut extra_level = [0u64; 5];
            let mut extra_any = 0u64;
            let mut residual = 0u64;
            for local in &slice.locals {
                let Some(flags) = method8_file_flags(local) else {
                    continue;
                };
                method8 += 1;
                let Some(plain) = inflate_method8(local) else {
                    residual += 1;
                    continue;
                };
                match match_deflate(&plain, &local.cdata, flags) {
                    Ok(Some(_)) => {
                        closed_hit += 1;
                        continue;
                    }
                    Ok(None) => {}
                    Err(_) => {
                        residual += 1;
                        continue;
                    }
                }
                let mut hit_extra = false;
                for (i, level) in EXTRA_ZLIB_LEVELS.iter().enumerate() {
                    if zlib_raw_deflate(&plain, *level).ok().as_deref()
                        == Some(local.cdata.as_slice())
                    {
                        extra_level[i] += 1;
                        hit_extra = true;
                    }
                }
                if hit_extra {
                    extra_any += 1;
                    continue;
                }
                residual += 1;
                if let Some(key) = first_strategy_or_mem_hit(&plain, &local.cdata) {
                    *strategy_hist.entry(key).or_insert(0) += 1;
                    tot_strategy_hits += 1;
                }
            }
            let closed_miss = method8.saturating_sub(closed_hit);
            let miss0_closed = closed_miss == 0;
            let miss0_if_extra = residual == 0;
            if miss0_closed {
                jars_miss0_closed += 1;
            }
            if miss0_if_extra {
                jars_miss0_if_extra += 1;
            }
            tot_method8 += method8;
            tot_closed_hit += closed_hit;
            tot_closed_miss += closed_miss;
            for (tot, n) in tot_extra_level.iter_mut().zip(extra_level) {
                *tot += n;
            }
            tot_extra_any += extra_any;
            tot_residual += residual;
            println!(
                "codec-probe extra-zlib-rs-levels (promotable) {} method8={} closed_hit={} closed_miss={} extra2={} extra4={} extra5={} extra7={} extra8={} extra_any={} residual={} miss0_closed={} miss0_if_extra={}",
                dest,
                method8,
                closed_hit,
                closed_miss,
                extra_level[0],
                extra_level[1],
                extra_level[2],
                extra_level[3],
                extra_level[4],
                extra_any,
                residual,
                miss0_closed,
                miss0_if_extra
            );
        }

        if jars_seen == 0 {
            eprintln!(
                "skipping probe_zlib_rs_extra_levels_on_lockfile_source_jars: no lockfile JARs on disk under {}",
                corpus.display()
            );
            return;
        }

        println!(
            "codec-probe extra-zlib-rs-levels totals jars={} method8={} closed_hit={} closed_miss={} extra2={} extra4={} extra5={} extra7={} extra8={} extra_any_slots={} residual_slots={} jars_miss0_closed={} jars_miss0_if_extra_levels={}",
            jars_seen,
            tot_method8,
            tot_closed_hit,
            tot_closed_miss,
            tot_extra_level[0],
            tot_extra_level[1],
            tot_extra_level[2],
            tot_extra_level[3],
            tot_extra_level[4],
            tot_extra_any,
            tot_residual,
            jars_miss0_closed,
            jars_miss0_if_extra
        );
        println!(
            "codec-probe strategy/mem_level (NOT promotable; PR 4 must not consume) residual_after_extra={} hits={} still_unexplained={}",
            tot_residual,
            tot_strategy_hits,
            tot_residual.saturating_sub(tot_strategy_hits)
        );
        for (k, n) in &strategy_hist {
            println!(
                "codec-probe strategy/mem_level (NOT promotable; PR 4 must not consume) {k} slots={n}"
            );
        }
    }

    fn first_strategy_or_mem_hit(plain: &[u8], want: &[u8]) -> Option<String> {
        const STRATS: [Strategy; 4] = [
            Strategy::Filtered,
            Strategy::HuffmanOnly,
            Strategy::Rle,
            Strategy::Fixed,
        ];
        for strategy in STRATS {
            for level in 1..=9i32 {
                if zlib_raw_cfg(plain, level, strategy, 8).as_deref() == Some(want) {
                    return Some(format!(
                        "strategy={},level={level},mem=8",
                        strategy_name(strategy)
                    ));
                }
            }
        }
        // Default zlib-rs mem_level is already 8 (same as JDK Deflater). Spot-check
        // extremes only; a full 1..=9 grid is not promotable and blows debug runtime.
        for mem in [1i32, 9] {
            for level in [1i32, 6, 9] {
                if zlib_raw_cfg(plain, level, Strategy::Default, mem).as_deref() == Some(want) {
                    return Some(format!(
                        "mem_level={mem},level={level},strategy={}",
                        strategy_name(Strategy::Default)
                    ));
                }
            }
        }
        None
    }
}
