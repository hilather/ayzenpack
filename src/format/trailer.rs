use std::io::{Read, Seek, SeekFrom, Write};

use super::{io_error, map_truncated, TRAILER_LEN, TRAILER_MAGIC};
use crate::error::{AyzenpackError, Result};

const ZIP_LOCAL: &[u8] = b"PK\x03\x04";
const ZIP_EOCD: &[u8] = b"PK\x05\x06";
const ZIP_SPAN: &[u8] = b"PK\x07\x08";
const HEAD_AYZP: &[u8] = b"AYZP";
const HEAD_LEGACY: &[u8] = b"JDED";
const TRAILER_LEGACY: &[u8] = b"JDEDTLR1";

const MSG_JAR_ZIP: &str = "invalid trailer magic: this looks like a JAR/ZIP; rehydrate/list/verify need the .ayz from dehydrate, not the jar";
const MSG_EXEC_JAR: &str =
    "invalid trailer magic: this looks like a Spring/executable JAR (script prefix); pass the .ayz";
const MSG_TRUNCATED_AYZ: &str = "invalid trailer magic: the .ayz trailer is missing or truncated (file not finished writing, or extra bytes after the trailer)";
/// Must not contain the substring `jded` (see `wrong_trailer_magic_errors`).
const MSG_LEGACY_MAGIC: &str = "invalid trailer magic: legacy archive magic 4a444544544c5231";

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
            return Err(invalid_trailer_error(buf, &[]));
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
    if buf[0..8] == TRAILER_MAGIC {
        return Trailer::from_bytes(&buf);
    }
    r.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let mut head = [0u8; 8];
    let n = r.read(&mut head).map_err(io_error)?;
    Err(invalid_trailer_error(&buf, &head[..n]))
}

fn is_zip_prefix(head: &[u8]) -> bool {
    head.starts_with(ZIP_LOCAL) || head.starts_with(ZIP_EOCD) || head.starts_with(ZIP_SPAN)
}

fn trailer_has_zip_eocd(buf: &[u8; 64]) -> bool {
    buf.windows(4).any(|w| w == ZIP_EOCD)
}

fn format_seen_magic(magic: &[u8]) -> String {
    let hex = hex::encode(magic);
    let ascii: String = magic
        .iter()
        .map(|&b| if b.is_ascii_graphic() { b as char } else { '.' })
        .collect();
    format!("invalid trailer magic: saw {ascii} ({hex})")
}

/// Classify a non-`AYZPTLR1` trailer. `head` is the first bytes of the file when known.
///
/// Shebang is checked before ZIP EOCD so a `#!/bin/bash` + zip (Spring/executable JAR)
/// is not reported as a plain JAR/ZIP. Legacy magic stays `Format` so existing
/// `matches!(err, AyzenpackError::Format(_))` tests keep working, and the message
/// must not contain the substring `jded`.
fn invalid_trailer_error(buf: &[u8; 64], head: &[u8]) -> AyzenpackError {
    if head.starts_with(b"#!") {
        return AyzenpackError::FormatOwned(MSG_EXEC_JAR.into());
    }
    if is_zip_prefix(head) || trailer_has_zip_eocd(buf) {
        return AyzenpackError::FormatOwned(MSG_JAR_ZIP.into());
    }
    if head.starts_with(HEAD_AYZP) {
        return AyzenpackError::FormatOwned(MSG_TRUNCATED_AYZ.into());
    }
    if head.starts_with(HEAD_LEGACY) || buf.starts_with(TRAILER_LEGACY) {
        return AyzenpackError::Format(MSG_LEGACY_MAGIC);
    }
    AyzenpackError::FormatOwned(format_seen_magic(&buf[..8]))
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
        assert_eq!(err.to_string(), MSG_LEGACY_MAGIC);

        let err = Trailer::from_bytes(&buf).unwrap_err();
        assert!(matches!(err, AyzenpackError::Format(_)));
        assert!(!err.to_string().contains("jded"));
        assert_eq!(err.to_string(), MSG_LEGACY_MAGIC);
    }

    fn tiny_zip_bytes() -> Vec<u8> {
        use std::io::Write as _;
        use zip::write::SimpleFileOptions;
        use zip::{CompressionMethod, ZipWriter};

        let mut z = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        z.start_file("x.txt", opts).unwrap();
        z.write_all(b"hello").unwrap();
        let bytes = z.finish().unwrap().into_inner();
        assert!(
            bytes.len() >= 64,
            "fixture zip must be at least a trailer long, got {}",
            bytes.len()
        );
        assert_eq!(&bytes[..4], ZIP_LOCAL);
        assert!(
            bytes.windows(4).any(|w| w == ZIP_EOCD),
            "fixture zip must contain EOCD"
        );
        bytes
    }

    #[test]
    fn zip_jar_trailer_names_jar_zip() {
        let zip = tiny_zip_bytes();
        let err = read_trailer(&mut Cursor::new(zip.clone())).unwrap_err();
        assert!(
            matches!(err, AyzenpackError::FormatOwned(_)),
            "zip/jar must be FormatOwned, got {err:?}"
        );
        assert_eq!(err.to_string(), MSG_JAR_ZIP);
        assert!(err.to_string().contains("JAR/ZIP"));

        let mut last64 = [0u8; 64];
        last64.copy_from_slice(&zip[zip.len() - 64..]);
        let err = Trailer::from_bytes(&last64).unwrap_err();
        assert_eq!(err.to_string(), MSG_JAR_ZIP);
    }

    #[test]
    fn shebang_zip_trailer_names_executable_script() {
        let mut data = b"#!/bin/bash\n".to_vec();
        data.extend_from_slice(&tiny_zip_bytes());
        let err = read_trailer(&mut Cursor::new(data)).unwrap_err();
        assert_eq!(err.to_string(), MSG_EXEC_JAR);
        let msg = err.to_string();
        assert!(
            msg.contains("executable") || msg.contains("script"),
            "shebang+zip must mention executable/script, got {msg}"
        );
        assert!(
            !msg.contains("saw "),
            "shebang+zip must not be generic magic only, got {msg}"
        );
    }

    #[test]
    fn ayzp_header_garbage_trailer_names_missing_truncated() {
        let mut data = b"AYZP\x01\x00\x00\x00".to_vec();
        data.extend_from_slice(&[0u8; 64]);
        let err = read_trailer(&mut Cursor::new(data)).unwrap_err();
        assert_eq!(err.to_string(), MSG_TRUNCATED_AYZ);
        let msg = err.to_string();
        assert!(
            msg.contains("missing") || msg.contains("truncated"),
            "AYZP + garbage trailer must mention missing/truncated, got {msg}"
        );
    }

    #[test]
    fn unknown_trailer_magic_includes_seen_bytes() {
        let mut buf = [0u8; 64];
        buf[0..8].copy_from_slice(b"XXXXXXXX");
        let err = Trailer::from_bytes(&buf).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid trailer magic: saw XXXXXXXX (5858585858585858)"
        );
        let err = read_trailer(&mut Cursor::new(buf.to_vec())).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid trailer magic: saw XXXXXXXX (5858585858585858)"
        );
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
