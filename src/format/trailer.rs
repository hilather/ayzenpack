use std::io::{Read, Seek, SeekFrom, Write};

use super::{io_error, map_truncated, TRAILER_LEN, TRAILER_MAGIC};
use crate::error::{AyzenpackError, Result};

/// Uncompressed 64-byte trailer at EOF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trailer {
    pub payload_bytes: u64,
    pub manifest_len: u64,
    pub blob_count: u64,
    pub blob_bytes: u64,
    pub jar_count: u64,
    pub header_len: u32,
    pub version: u32,
}

impl Trailer {
    pub fn to_bytes(&self) -> [u8; 64] {
        let mut buf = [0u8; 64];
        buf[0..8].copy_from_slice(&TRAILER_MAGIC);
        buf[8..16].copy_from_slice(&self.payload_bytes.to_le_bytes());
        buf[16..24].copy_from_slice(&self.manifest_len.to_le_bytes());
        buf[24..32].copy_from_slice(&self.blob_count.to_le_bytes());
        buf[32..40].copy_from_slice(&self.blob_bytes.to_le_bytes());
        buf[40..48].copy_from_slice(&self.jar_count.to_le_bytes());
        buf[48..52].copy_from_slice(&self.header_len.to_le_bytes());
        buf[52..56].copy_from_slice(&self.version.to_le_bytes());
        buf
    }

    pub fn from_bytes(buf: &[u8; 64]) -> Result<Self> {
        if buf[0..8] != TRAILER_MAGIC {
            return Err(AyzenpackError::Format("invalid trailer magic"));
        }
        let version = u32_le(buf, 52);
        if version != 1 {
            return Err(AyzenpackError::Format("unsupported trailer version"));
        }
        Ok(Self {
            payload_bytes: u64_le(buf, 8),
            manifest_len: u64_le(buf, 16),
            blob_count: u64_le(buf, 24),
            blob_bytes: u64_le(buf, 32),
            jar_count: u64_le(buf, 40),
            header_len: u32_le(buf, 48),
            version,
        })
    }

    pub fn write<W: Write>(&self, w: &mut W) -> Result<()> {
        write_trailer(w, self)
    }

    pub fn read<R: Read + Seek>(r: &mut R) -> Result<Self> {
        read_trailer(r)
    }
}

pub fn write_trailer<W: Write>(w: &mut W, trailer: &Trailer) -> Result<()> {
    w.write_all(&trailer.to_bytes()).map_err(io_error)
}

pub fn read_trailer<R: Read + Seek>(r: &mut R) -> Result<Trailer> {
    let len = r.seek(SeekFrom::End(0)).map_err(io_error)?;
    if len < TRAILER_LEN {
        return Err(AyzenpackError::Format("truncated trailer"));
    }
    r.seek(SeekFrom::Start(len - TRAILER_LEN))
        .map_err(io_error)?;
    let mut buf = [0u8; 64];
    r.read_exact(&mut buf)
        .map_err(|e| map_truncated(e, "truncated trailer"))?;
    Trailer::from_bytes(&buf)
}

fn u64_le(buf: &[u8; 64], offset: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&buf[offset..offset + 8]);
    u64::from_le_bytes(b)
}

fn u32_le(buf: &[u8; 64], offset: usize) -> u32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&buf[offset..offset + 4]);
    u32::from_le_bytes(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn wrong_trailer_magic_errors() {
        // Guards silent keep of JDEDTLR1 (or any non-AYZPTLR1) trailer magic.
        let mut buf = [0u8; 64];
        buf[0..8].copy_from_slice(b"JDEDTLR1");
        buf[52..56].copy_from_slice(&1u32.to_le_bytes());
        let err = read_trailer(&mut Cursor::new(buf.to_vec())).unwrap_err();
        assert!(
            matches!(err, AyzenpackError::Format(_)),
            "JDEDTLR1 must error, got {err:?}"
        );
        assert!(!err.to_string().contains("jded"));

        let err = Trailer::from_bytes(&buf).unwrap_err();
        assert!(matches!(err, AyzenpackError::Format(_)));
    }

    #[test]
    fn truncated_trailer_errors() {
        // Guards panic (or Io-only) on a file shorter than 64 bytes.
        for n in [0usize, 1, 63] {
            let err = read_trailer(&mut Cursor::new(vec![0u8; n])).unwrap_err();
            assert!(
                matches!(err, AyzenpackError::Format("truncated trailer")),
                "len={n} must be truncated trailer, got {err:?}"
            );
            assert_eq!(err.to_string(), "truncated trailer");
        }
    }
}
