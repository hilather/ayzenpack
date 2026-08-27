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
        write_slot(
            &mut cur,
            e,
            expected_len,
            prefix.len() as u64,
            &mut get_blob,
        )?;
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
    Ok(buf)
}

fn write_slot(
    cur: &mut Cursor<&mut Vec<u8>>,
    e: &Entry,
    expected_len: u64,
    prefix_len: u64,
    get_blob: &mut impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<()> {
    if e.zip_index.is_some() {
        return Err(AyzenpackError::Format(
            "nested zip_index past depth 1 is not supported",
        ));
    }
    let header = load_header(e, get_blob)?;
    let cdata = resolve_slot_cdata(e, get_blob)?;
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
    if pos >= expected_len {
        return Err(AyzenpackError::FormatOwned(format!(
            "{} offsetheader past child size",
            e.name
        )));
    }
    cur.seek(SeekFrom::Start(pos)).map_err(io_err)?;
    cur.write_all(&header).map_err(io_err)?;
    cur.write_all(&cdata).map_err(io_err)?;
    cur.write_all(&desc).map_err(io_err)?;
    write_pad(cur, e, get_blob)?;
    Ok(())
}

fn load_header(e: &Entry, get_blob: &mut impl FnMut(&str) -> Result<Vec<u8>>) -> Result<Vec<u8>> {
    if let Some(hex) = &e.local_header_hex {
        return parse_hex(hex);
    }
    if let Some(hex) = &e.local_header_blob {
        return get_blob(hex);
    }
    Err(AyzenpackError::FormatOwned(format!(
        "missing local header for {}",
        e.name
    )))
}

fn resolve_slot_cdata(
    e: &Entry,
    get_blob: &mut impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    if let Some(hex) = &e.cdata_blob {
        return get_blob(hex);
    }
    if let Some(codec) = &e.cdata_codec {
        let spec = deflate::parse_codec(codec)?;
        let bytes = if e.is_dir && e.blob.is_none() {
            Vec::new()
        } else {
            let hex = e.blob.as_deref().ok_or_else(|| {
                AyzenpackError::FormatOwned(format!("missing blob for {}", e.name))
            })?;
            get_blob(hex)?
        };
        return deflate::encode_codec(spec, &bytes);
    }
    if e.is_dir {
        return Ok(Vec::new());
    }
    let hex = e
        .blob
        .as_deref()
        .ok_or_else(|| AyzenpackError::FormatOwned(format!("missing blob for {}", e.name)))?;
    get_blob(hex)
}

fn write_pad(
    cur: &mut Cursor<&mut Vec<u8>>,
    e: &Entry,
    get_blob: &mut impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<()> {
    if let Some(n) = e.pad_zeros {
        let zeros = vec![0u8; usize::try_from(n).unwrap_or(0)];
        cur.write_all(&zeros).map_err(io_err)?;
        return Ok(());
    }
    if let Some(hex) = &e.pad_blob {
        let pad = get_blob(hex)?;
        cur.write_all(&pad).map_err(io_err)?;
    }
    Ok(())
}

fn io_err(source: std::io::Error) -> AyzenpackError {
    AyzenpackError::Io { source, path: None }
}
