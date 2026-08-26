//! On-disk `.ayz` container codecs (header, records, TOC, trailer).

mod header;
mod record;
mod toc;
mod trailer;
mod writer;

pub use header::{file_magic, read_header, write_header, FileHeader};
pub use record::{
    blob_record_len, decode_payload, open_ayz_layout, read_ayz_file, read_manifest_records,
    read_record, read_records, read_toc_at, write_ayz_file, write_ayz_file_v1, write_record,
    AyzLayout, Record, BUF_WRITER_CAP,
};
pub use toc::{read_toc, write_toc, Toc, TocEntry, TOC_ENTRY_SIZE, TOC_MAGIC, TOC_OVERHEAD};
pub use trailer::{read_trailer, write_trailer, Trailer};
pub use writer::{verify_finished_ayz, AyzWriter, BLOB_FRAME_FLUSH};

use crate::error::AyzenpackError;

/// Current write magic (`AYZP` + `FORMAT_VERSION` + three zero pad bytes).
pub const FILE_MAGIC: [u8; 8] = *b"AYZP\x02\x00\x00\x00";
/// v1 write/read magic. Kept so tests and `write_ayz_file_v1` can name it.
pub const FILE_MAGIC_V1: [u8; 8] = *b"AYZP\x01\x00\x00\x00";
pub const TRAILER_MAGIC: [u8; 8] = *b"AYZPTLR1";
pub const FORMAT_VERSION: u8 = 2;
pub const FORMAT_VERSION_V1: u8 = 1;
pub const REC_BLOB: u8 = 0x01;
pub const REC_MANIFEST: u8 = 0x02;
pub const REC_END: u8 = 0x03;
pub const TRAILER_LEN: u64 = 64;

const _: () = assert!(FILE_MAGIC[4] == FORMAT_VERSION);
const _: () = assert!(FILE_MAGIC_V1[4] == FORMAT_VERSION_V1);
const _: () = assert!(TRAILER_LEN == 64);
const _: () = assert!(TOC_OVERHEAD + 0 == 28);
const _: () = assert!(TOC_ENTRY_SIZE == 56);

pub(crate) fn io_error(err: std::io::Error) -> AyzenpackError {
    AyzenpackError::Io {
        source: err,
        path: None,
    }
}

pub(crate) fn map_truncated(err: std::io::Error, msg: &'static str) -> AyzenpackError {
    if err.kind() == std::io::ErrorKind::UnexpectedEof {
        AyzenpackError::Format(msg)
    } else {
        io_error(err)
    }
}

pub(crate) fn supported_write_version(version: u32) -> bool {
    version == 1 || version == 2
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Seek, SeekFrom};

    fn sample_header() -> FileHeader {
        FileHeader::new(3, 1_710_000_000)
    }

    fn sample_trailer(header_len: u32) -> Trailer {
        Trailer {
            payload_bytes: 0x0102_0304_0506_0708,
            manifest_len: 0x1112_1314_1516_1718,
            blob_count: 9,
            blob_bytes: 0xA1B2_C3D4_E5F6_7788,
            jar_count: 2,
            header_len,
            version: 2,
            toc_len: 0,
        }
    }

    #[test]
    fn header_trailer_roundtrip_cursor() {
        // Guards endian bugs, 64-byte trailer length, and JDED identity drift.
        let header = sample_header();
        let mut cur = Cursor::new(Vec::new());
        let header_len = write_header(&mut cur, &header).unwrap();
        let trailer = sample_trailer(header_len);
        write_trailer(&mut cur, &trailer).unwrap();

        let bytes = cur.get_ref();
        assert_eq!(&bytes[..8], &FILE_MAGIC);
        assert_eq!(&bytes[8..12], &header_len.to_le_bytes());
        let json = std::str::from_utf8(&bytes[12..12 + header_len as usize]).unwrap();
        assert!(json.contains("\"format\":\"ayzenpack\""));
        assert!(json.contains("\"tool\":\"ayzenpack\""));
        assert!(json.contains("\"version\":2"));
        assert!(!json.contains("jded"));
        assert!(!json.contains('\n'), "header JSON must be compact: {json}");

        assert_eq!(TRAILER_LEN, 64);
        assert_eq!(bytes.len() as u64, 12 + u64::from(header_len) + TRAILER_LEN);
        let t_off = bytes.len() - 64;
        assert_eq!(&bytes[t_off..t_off + 8], &TRAILER_MAGIC);
        assert_eq!(
            &bytes[t_off + 8..t_off + 16],
            &trailer.payload_bytes.to_le_bytes()
        );
        assert_eq!(
            &bytes[t_off + 16..t_off + 24],
            &trailer.manifest_len.to_le_bytes()
        );
        assert_eq!(
            &bytes[t_off + 24..t_off + 32],
            &trailer.blob_count.to_le_bytes()
        );
        assert_eq!(
            &bytes[t_off + 32..t_off + 40],
            &trailer.blob_bytes.to_le_bytes()
        );
        assert_eq!(
            &bytes[t_off + 40..t_off + 48],
            &trailer.jar_count.to_le_bytes()
        );
        assert_eq!(&bytes[t_off + 48..t_off + 52], &header_len.to_le_bytes());
        assert_eq!(&bytes[t_off + 52..t_off + 56], &2u32.to_le_bytes());
        assert_eq!(&bytes[t_off + 56..t_off + 64], &[0u8; 8]);

        cur.seek(SeekFrom::Start(0)).unwrap();
        let got_trailer = read_trailer(&mut cur).unwrap();
        assert_eq!(got_trailer, trailer);
        cur.seek(SeekFrom::Start(0)).unwrap();
        let got_header = read_header(&mut cur).unwrap();
        assert_eq!(got_header, header);
        assert_eq!(got_header.format, "ayzenpack");
        assert_eq!(got_header.tool, "ayzenpack");
        assert_eq!(got_header.tool_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(got_header.version, 2);

        // Unknown keys ignored (do not deny_unknown_fields).
        let mut extra = serde_json::to_value(&header).unwrap();
        extra
            .as_object_mut()
            .unwrap()
            .insert("future_field".into(), serde_json::json!(true));
        let extra_json = serde_json::to_vec(&extra).unwrap();
        let mut extra_cur = Cursor::new(Vec::new());
        extra_cur.get_mut().extend_from_slice(&FILE_MAGIC);
        extra_cur
            .get_mut()
            .extend_from_slice(&(extra_json.len() as u32).to_le_bytes());
        extra_cur.get_mut().extend_from_slice(&extra_json);
        extra_cur.seek(SeekFrom::Start(0)).unwrap();
        let ignored = read_header(&mut extra_cur).unwrap();
        assert_eq!(ignored, header);
    }
}
