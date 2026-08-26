use std::io::{Read, Write};

use super::{io_error, map_truncated};
use crate::error::{AyzenpackError, Result};

pub const TOC_MAGIC: [u8; 8] = *b"AYZPTOC2";
/// `blake3[32] + zstd_off + zstd_len + rec_off`.
pub const TOC_ENTRY_SIZE: u64 = 56;
/// `"AYZPTOC2" + n:u32le + manifest_zstd_off + manifest_zstd_len`.
pub const TOC_OVERHEAD: u64 = 28;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    pub blake3: [u8; 32],
    pub zstd_off: u64,
    pub zstd_len: u64,
    pub rec_off: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toc {
    pub entries: Vec<TocEntry>,
    pub manifest_zstd_off: u64,
    pub manifest_zstd_len: u64,
}

impl Toc {
    pub fn encoded_len(&self) -> u64 {
        TOC_OVERHEAD + TOC_ENTRY_SIZE * self.entries.len() as u64
    }
}

pub fn write_toc<W: Write>(w: &mut W, toc: &Toc) -> Result<u64> {
    let n = u32::try_from(toc.entries.len())
        .map_err(|_| AyzenpackError::Format("TOC entry count exceeds u32"))?;
    w.write_all(&TOC_MAGIC).map_err(io_error)?;
    w.write_all(&n.to_le_bytes()).map_err(io_error)?;
    for e in &toc.entries {
        w.write_all(&e.blake3).map_err(io_error)?;
        w.write_all(&e.zstd_off.to_le_bytes()).map_err(io_error)?;
        w.write_all(&e.zstd_len.to_le_bytes()).map_err(io_error)?;
        w.write_all(&e.rec_off.to_le_bytes()).map_err(io_error)?;
    }
    w.write_all(&toc.manifest_zstd_off.to_le_bytes())
        .map_err(io_error)?;
    w.write_all(&toc.manifest_zstd_len.to_le_bytes())
        .map_err(io_error)?;
    Ok(toc.encoded_len())
}

pub fn read_toc<R: Read>(r: &mut R, toc_len: u64) -> Result<Toc> {
    if toc_len < TOC_OVERHEAD {
        return Err(AyzenpackError::Format("truncated TOC"));
    }
    let rest = toc_len - TOC_OVERHEAD;
    if rest % TOC_ENTRY_SIZE != 0 {
        return Err(AyzenpackError::Format("v2 toc_len not 28+n*56"));
    }
    let n = (rest / TOC_ENTRY_SIZE) as usize;

    let mut magic = [0u8; 8];
    r.read_exact(&mut magic)
        .map_err(|e| map_truncated(e, "truncated TOC"))?;
    if magic != TOC_MAGIC {
        return Err(AyzenpackError::Format("invalid TOC magic"));
    }
    let mut nbuf = [0u8; 4];
    r.read_exact(&mut nbuf)
        .map_err(|e| map_truncated(e, "truncated TOC"))?;
    let counted = u32::from_le_bytes(nbuf) as usize;
    if counted != n {
        return Err(AyzenpackError::Format("TOC entry count mismatch"));
    }

    let mut entries = Vec::with_capacity(n);
    for _ in 0..n {
        let mut blake3 = [0u8; 32];
        r.read_exact(&mut blake3)
            .map_err(|e| map_truncated(e, "truncated TOC"))?;
        let zstd_off = read_u64le(r)?;
        let zstd_len = read_u64le(r)?;
        let rec_off = read_u64le(r)?;
        entries.push(TocEntry {
            blake3,
            zstd_off,
            zstd_len,
            rec_off,
        });
    }
    let manifest_zstd_off = read_u64le(r)?;
    let manifest_zstd_len = read_u64le(r)?;
    Ok(Toc {
        entries,
        manifest_zstd_off,
        manifest_zstd_len,
    })
}

fn read_u64le<R: Read>(r: &mut R) -> Result<u64> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)
        .map_err(|e| map_truncated(e, "truncated TOC"))?;
    Ok(u64::from_le_bytes(buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn toc_roundtrip_two_entries() {
        let toc = Toc {
            entries: vec![
                TocEntry {
                    blake3: [1u8; 32],
                    zstd_off: 0,
                    zstd_len: 40,
                    rec_off: 0,
                },
                TocEntry {
                    blake3: [2u8; 32],
                    zstd_off: 0,
                    zstd_len: 40,
                    rec_off: 41,
                },
            ],
            manifest_zstd_off: 40,
            manifest_zstd_len: 12,
        };
        assert_eq!(toc.encoded_len(), 28 + 2 * 56);
        let mut buf = Vec::new();
        write_toc(&mut buf, &toc).unwrap();
        assert_eq!(buf.len() as u64, toc.encoded_len());
        assert_eq!(&buf[..8], &TOC_MAGIC);
        let got = read_toc(&mut Cursor::new(&buf), toc.encoded_len()).unwrap();
        assert_eq!(got, toc);
    }
}
