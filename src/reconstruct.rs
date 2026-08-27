//! Rebuild ZIP bytes from a stencil + CAS callback.

use std::io::{Cursor, Seek, SeekFrom, Write};

use crate::deflate;
use crate::error::{AyzenpackError, Result};
use crate::hashutil::parse_hex;
use crate::manifest::{Entry, NestedIndex};

pub fn reconstruct_child_zip(
    index: &NestedIndex,
    expected_len: u64,
    mut get_blob: impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    if expected_len > usize::MAX as u64 {
        return Err(AyzenpackError::Format("child zip too large"));
    }
    let mut buf = vec![0u8; expected_len as usize];
    let mut cur = Cursor::new(&mut buf);

    let prefix = if let Some(hex) = &index.prefix_blob {
        get_blob(hex)?
    } else {
        Vec::new()
    };
    if let Some(sz) = index.prefix_size {
        if prefix.len() as u64 != sz {
            return Err(AyzenpackError::HashMismatch(format!(
                "child prefix size: recorded {sz} computed {}",
                prefix.len()
            )));
        }
    }
    let prefix_len = prefix.len() as u64;
    cur.write_all(&prefix).map_err(io_err)?;

    if let Some(hex) = &index.leading_pad_blob {
        let pad = get_blob(hex)?;
        if let Some(sz) = index.leading_pad_size {
            if pad.len() as u64 != sz {
                return Err(AyzenpackError::HashMismatch(format!(
                    "child leading_pad size: recorded {sz} computed {}",
                    pad.len()
                )));
            }
        }
        cur.write_all(&pad).map_err(io_err)?;
    }

    for e in &index.entries {
        let cdata = resolve_slot_cdata(e, &mut get_blob, false)?;
        write_slot(&mut cur, e, prefix_len, &mut get_blob, &cdata)?;
    }

    let tail = match (&index.tail_blob, index.tail_size) {
        (Some(hex), Some(sz)) => {
            let bytes = get_blob(hex)?;
            if bytes.len() as u64 != sz {
                return Err(AyzenpackError::HashMismatch(format!(
                    "child tail size: recorded {sz} computed {}",
                    bytes.len()
                )));
            }
            bytes
        }
        _ => {
            return Err(AyzenpackError::Format(
                "child zip_index missing tail_blob/tail_size",
            ));
        }
    };
    if expected_len < tail.len() as u64 {
        return Err(AyzenpackError::HashMismatch(
            "child source_size smaller than tail".into(),
        ));
    }
    let tail_pos = expected_len - tail.len() as u64;
    cur.seek(SeekFrom::Start(tail_pos)).map_err(io_err)?;
    cur.write_all(&tail).map_err(io_err)?;
    if buf.len() as u64 != expected_len {
        return Err(AyzenpackError::HashMismatch(format!(
            "child zip size: recorded {expected_len} computed {}",
            buf.len()
        )));
    }
    Ok(buf)
}

/// Seek `offsetheader` (else `prefix_len + local_header_offset`); header; cdata; descriptor; pad.
pub(crate) fn write_slot<W: Write + Seek>(
    w: &mut W,
    e: &Entry,
    prefix_len: u64,
    get_blob: &mut impl FnMut(&str) -> Result<Vec<u8>>,
    cdata: &[u8],
) -> Result<()> {
    let header = load_header(e, get_blob)?;
    let desc = match &e.data_descriptor_hex {
        Some(h) => parse_hex(h)?,
        None => Vec::new(),
    };
    let pos = if let Some(oh) = e.offsetheader {
        oh
    } else {
        let off = e.local_header_offset.ok_or_else(|| {
            AyzenpackError::FormatOwned(format!("missing local_header_offset for {}", e.name))
        })?;
        prefix_len
            .checked_add(off)
            .ok_or_else(|| AyzenpackError::FormatOwned(format!("{} seek overflow", e.name)))?
    };
    w.seek(SeekFrom::Start(pos)).map_err(io_err)?;
    w.write_all(&header).map_err(io_err)?;
    w.write_all(cdata).map_err(io_err)?;
    w.write_all(&desc).map_err(io_err)?;
    write_pad(w, e, get_blob)?;
    Ok(())
}

pub(crate) fn load_header(
    e: &Entry,
    get_blob: &mut impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    match (&e.local_header_hex, &e.local_header_blob) {
        (Some(hex), None) => parse_hex(hex),
        (None, Some(blob)) => get_blob(blob),
        (None, None) => Err(AyzenpackError::FormatOwned(format!(
            "missing local header for {}",
            e.name
        ))),
        (Some(_), Some(_)) => Err(AyzenpackError::FormatOwned(format!(
            "local_header_hex and local_header_blob both set on {}",
            e.name
        ))),
    }
}

/// Exact splice and child reconstruct pass `allow_rebuild = false`.
pub(crate) fn resolve_slot_cdata(
    e: &Entry,
    get_blob: &mut impl FnMut(&str) -> Result<Vec<u8>>,
    allow_rebuild: bool,
) -> Result<Vec<u8>> {
    if e.blob.is_some() && e.zip_index.is_some() {
        return Err(AyzenpackError::FormatOwned(format!(
            "{} has both blob and zip_index",
            e.name
        )));
    }
    if e.zip_index.is_some() {
        return Err(AyzenpackError::Format(
            "nested zip_index past depth 1 is not supported",
        ));
    }
    if let Some(hex) = &e.cdata_blob {
        return get_blob(hex);
    }
    if let Some(codec) = &e.cdata_codec {
        if !(allow_rebuild && e.is_dir) {
            let spec = deflate::parse_codec(codec)?;
            let bytes = if e.is_dir && e.blob.is_none() {
                Vec::new()
            } else {
                let hex = e.blob.as_deref().ok_or_else(|| {
                    AyzenpackError::FormatOwned(format!("missing blob for {}", e.name))
                })?;
                get_blob(hex)?
            };
            let out = deflate::encode_codec(spec, &bytes)?;
            if out.len() as u64 != e.compressed_size {
                return Err(AyzenpackError::HashMismatch(format!(
                    "{} cdata_codec size: recorded {} computed {}",
                    e.name,
                    e.compressed_size,
                    out.len()
                )));
            }
            return Ok(out);
        }
        // Miss-jar rebuild may ignore dir codecs (hash may change).
    }
    if e.is_dir {
        if allow_rebuild && e.method_code == 8 && e.uncompressed_size == 0 {
            return deflate::deflate_raw(&[], deflate::rebuild_level());
        }
        return Ok(Vec::new());
    }
    if e.method_code == 0 {
        let hex = e
            .blob
            .as_deref()
            .ok_or_else(|| AyzenpackError::FormatOwned(format!("missing blob for {}", e.name)))?;
        return get_blob(hex);
    }
    if allow_rebuild {
        let hex = e
            .blob
            .as_deref()
            .ok_or_else(|| AyzenpackError::FormatOwned(format!("missing blob for {}", e.name)))?;
        let bytes = get_blob(hex)?;
        return deflate::deflate_raw(&bytes, deflate::rebuild_level());
    }
    Err(AyzenpackError::FormatOwned(format!(
        "missing cdata for {} (no cdata_blob/cdata_codec)",
        e.name
    )))
}

fn write_pad<W: Write>(
    w: &mut W,
    e: &Entry,
    get_blob: &mut impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<()> {
    match (e.pad_zeros, &e.pad_blob) {
        (Some(_), Some(_)) => Err(AyzenpackError::FormatOwned(format!(
            "pad_zeros and pad_blob both set on {}",
            e.name
        ))),
        (Some(n), None) => write_zeros(w, n),
        (None, Some(hex)) => {
            let bytes = get_blob(hex)?;
            w.write_all(&bytes).map_err(io_err)
        }
        (None, None) => Ok(()),
    }
}

fn write_zeros<W: Write>(w: &mut W, n: u64) -> Result<()> {
    let buf = [0u8; 4096];
    let mut left = n;
    while left > 0 {
        let chunk = left.min(4096) as usize;
        w.write_all(&buf[..chunk]).map_err(io_err)?;
        left -= chunk as u64;
    }
    Ok(())
}

fn io_err(source: std::io::Error) -> AyzenpackError {
    AyzenpackError::Io { source, path: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get_blob(hex: &str) -> Result<Vec<u8>> {
        match hex {
            "blob" => Ok(b"data".to_vec()),
            "tail" => Ok(b"TAIL".to_vec()),
            "hdr" => Ok(b"HDR!".to_vec()),
            "pad" => Ok(b"PAD!".to_vec()),
            other => Err(AyzenpackError::HashMismatch(format!("missing {other}"))),
        }
    }

    #[test]
    fn child_codec_encode_wrong_length_is_hash_mismatch() {
        let e = Entry {
            name: "a.class".into(),
            blob: Some("blob".into()),
            method: "deflated".into(),
            method_code: 8,
            uncompressed_size: 4,
            compressed_size: 1,
            cdata_codec: Some("deflate-raw:flate2:6".into()),
            local_header_hex: Some("504b0304".into()),
            local_header_offset: Some(0),
            offsetheader: Some(0),
            ..Entry::default()
        };
        let index = NestedIndex {
            tail_blob: Some("tail".into()),
            tail_size: Some(4),
            entries: vec![e],
            ..NestedIndex::default()
        };
        let err = reconstruct_child_zip(&index, 64, get_blob).unwrap_err();
        assert!(
            matches!(
                err,
                AyzenpackError::HashMismatch(ref s) if s.contains("cdata_codec size")
            ),
            "{err}"
        );
    }

    #[test]
    fn load_header_rejects_both_hex_and_blob() {
        let e = Entry {
            name: "a.txt".into(),
            local_header_hex: Some("504b0304".into()),
            local_header_blob: Some("hdr".into()),
            ..Entry::default()
        };
        let err = load_header(&e, &mut get_blob).unwrap_err();
        assert!(
            matches!(
                err,
                AyzenpackError::FormatOwned(ref s)
                    if s.contains("local_header_hex") && s.contains("local_header_blob")
            ),
            "{err}"
        );
    }

    #[test]
    fn write_slot_rejects_both_pad_zeros_and_pad_blob() {
        let e = Entry {
            name: "a.txt".into(),
            local_header_hex: Some("504b0304".into()),
            local_header_offset: Some(0),
            offsetheader: Some(0),
            pad_zeros: Some(4),
            pad_blob: Some("pad".into()),
            ..Entry::default()
        };
        let mut buf = vec![0u8; 32];
        let mut cur = Cursor::new(&mut buf);
        let err = write_slot(&mut cur, &e, 0, &mut get_blob, b"").unwrap_err();
        assert!(
            matches!(
                err,
                AyzenpackError::FormatOwned(ref s)
                    if s.contains("pad_zeros") && s.contains("pad_blob")
            ),
            "{err}"
        );
    }
}
