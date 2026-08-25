use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use super::{io_error, map_truncated, FILE_MAGIC, FORMAT_VERSION};
use crate::error::{AyzenpackError, Result};

/// Uncompressed file header: magic[8] + header_len:u32le + UTF-8 JSON.
/// Unknown JSON keys are ignored (no `deny_unknown_fields`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileHeader {
    pub format: String,
    pub version: u32,
    pub hash: String,
    pub sha256: bool,
    pub mode: String,
    pub zstd_level: i32,
    pub created_unix: u64,
    pub tool: String,
    pub tool_version: String,
}

impl FileHeader {
    pub fn new(zstd_level: i32, created_unix: u64) -> Self {
        Self {
            format: "ayzenpack".into(),
            version: u32::from(FORMAT_VERSION),
            hash: "blake3".into(),
            sha256: true,
            mode: "content".into(),
            zstd_level,
            created_unix,
            tool: "ayzenpack".into(),
            tool_version: env!("CARGO_PKG_VERSION").into(),
        }
    }

    pub fn write<W: Write>(&self, w: &mut W) -> Result<u32> {
        write_header(w, self)
    }

    pub fn read<R: Read>(r: &mut R) -> Result<Self> {
        read_header(r)
    }
}

pub fn write_header<W: Write>(w: &mut W, header: &FileHeader) -> Result<u32> {
    if header.format != "ayzenpack" {
        return Err(AyzenpackError::Format("header format must be ayzenpack"));
    }
    if header.version != u32::from(FORMAT_VERSION) {
        return Err(AyzenpackError::UnsupportedVersion(header.version as u8));
    }
    let json = serde_json::to_vec(header)?;
    let header_len = u32::try_from(json.len())
        .map_err(|_| AyzenpackError::Format("header JSON exceeds u32 length"))?;
    w.write_all(&FILE_MAGIC).map_err(io_error)?;
    w.write_all(&header_len.to_le_bytes()).map_err(io_error)?;
    w.write_all(&json).map_err(io_error)?;
    Ok(header_len)
}

pub fn read_header<R: Read>(r: &mut R) -> Result<FileHeader> {
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic)
        .map_err(|e| map_truncated(e, "truncated header"))?;
    if magic[..4] != *b"AYZP" {
        return Err(AyzenpackError::NotAyzenpack);
    }
    let ver = magic[4];
    if ver == 0 {
        return Err(AyzenpackError::Format("invalid format version"));
    }
    if ver > FORMAT_VERSION {
        return Err(AyzenpackError::UnsupportedVersion(ver));
    }
    if magic != FILE_MAGIC {
        return Err(AyzenpackError::NotAyzenpack);
    }

    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)
        .map_err(|e| map_truncated(e, "truncated header"))?;
    let header_len = u32::from_le_bytes(len_buf);
    let mut json = vec![0u8; header_len as usize];
    r.read_exact(&mut json)
        .map_err(|e| map_truncated(e, "truncated header"))?;

    let header: FileHeader = serde_json::from_slice(&json)?;
    if header.format != "ayzenpack" {
        return Err(AyzenpackError::Format("header format must be ayzenpack"));
    }
    if header.version != u32::from(FORMAT_VERSION) {
        return Err(AyzenpackError::UnsupportedVersion(header.version as u8));
    }
    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::FILE_MAGIC;
    use std::io::Cursor;

    #[test]
    fn wrong_file_magic_is_not_ayzenpack() {
        // Guards silent keep of JDED magic (or any non-AYZP prefix).
        let mut buf = Vec::from(*b"JDED\x01\x00\x00\x00");
        buf.extend_from_slice(&4u32.to_le_bytes());
        buf.extend_from_slice(b"{}  ");
        let err = read_header(&mut Cursor::new(buf)).unwrap_err();
        assert!(
            matches!(err, AyzenpackError::NotAyzenpack),
            "JDED prefix must be NotAyzenpack, got {err:?}"
        );
        assert_eq!(err.to_string(), "not an ayzenpack file");
        assert!(!err.to_string().contains("jded"));

        let err = read_header(&mut Cursor::new(*b"XXXX\x01\x00\x00\x00")).unwrap_err();
        assert!(matches!(err, AyzenpackError::NotAyzenpack));
    }

    #[test]
    fn unsupported_version_byte_errors() {
        // Guards treating version >1 as NotAyzenpack or accepting it as v1.
        let mut magic = FILE_MAGIC;
        magic[4] = 2;
        let err = read_header(&mut Cursor::new(magic.to_vec())).unwrap_err();
        assert!(
            matches!(err, AyzenpackError::UnsupportedVersion(2)),
            "version byte 2 must be UnsupportedVersion(2), got {err:?}"
        );
        assert_eq!(err.to_string(), "unsupported ayzenpack version 2");

        let mut v0 = FILE_MAGIC;
        v0[4] = 0;
        let err = read_header(&mut Cursor::new(v0.to_vec())).unwrap_err();
        assert!(
            matches!(err, AyzenpackError::Format(_)),
            "version 0 is invalid, got {err:?}"
        );
    }

    #[test]
    fn created_unix_zero_roundtrips() {
        // Guards skip_serializing_if / Option dropping created_unix=0 (--sort-inputs).
        let header = FileHeader::new(3, 0);
        let mut cur = Cursor::new(Vec::new());
        let header_len = write_header(&mut cur, &header).unwrap();
        let json = &cur.get_ref()[12..12 + header_len as usize];
        let s = std::str::from_utf8(json).unwrap();
        assert!(
            s.contains("\"created_unix\":0"),
            "created_unix=0 must be written: {s}"
        );
        cur.set_position(0);
        let got = read_header(&mut cur).unwrap();
        assert_eq!(got.created_unix, 0);
        assert_eq!(got, header);
    }
}
