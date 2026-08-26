use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};

use super::toc::{read_toc, Toc, TOC_OVERHEAD};
use super::writer::AyzWriter;
use super::{
    io_error, map_truncated, read_header, read_trailer, write_header, write_trailer, FileHeader,
    Trailer, REC_BLOB, REC_END, REC_MANIFEST, TRAILER_LEN,
};
use crate::error::{AyzenpackError, Result};

/// Capacity of the `BufWriter` under the zstd encoder. Trailer is written on this same writer.
pub const BUF_WRITER_CAP: usize = 256 * 1024;

/// Uncompressed BLOB record size: type + blake3 + size + payload.
pub fn blob_record_len(data_len: u64) -> u64 {
    1 + 32 + 8 + data_len
}

/// One record in the decompressed zstd payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    Blob { hash: [u8; 32], data: Vec<u8> },
    Manifest { json: Vec<u8> },
    End { digest: [u8; 32] },
}

/// Header + trailer + measured layout after version / toc_len checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AyzLayout {
    pub header: FileHeader,
    pub trailer: Trailer,
    pub header_total: u64,
    pub file_len: u64,
}

pub fn write_record<W: Write>(w: &mut W, r: &Record) -> Result<()> {
    match r {
        Record::Blob { hash, data } => {
            w.write_all(&[REC_BLOB]).map_err(io_error)?;
            w.write_all(hash).map_err(io_error)?;
            w.write_all(&(data.len() as u64).to_le_bytes())
                .map_err(io_error)?;
            w.write_all(data).map_err(io_error)?;
        }
        Record::Manifest { json } => {
            w.write_all(&[REC_MANIFEST]).map_err(io_error)?;
            w.write_all(&(json.len() as u64).to_le_bytes())
                .map_err(io_error)?;
            w.write_all(json).map_err(io_error)?;
        }
        Record::End { digest } => {
            w.write_all(&[REC_END]).map_err(io_error)?;
            w.write_all(digest).map_err(io_error)?;
        }
    }
    Ok(())
}

pub fn read_record<R: Read>(r: &mut R) -> Result<Record> {
    let mut ty = [0u8; 1];
    r.read_exact(&mut ty)
        .map_err(|e| map_truncated(e, "truncated record"))?;
    match ty[0] {
        REC_BLOB => {
            let mut hash = [0u8; 32];
            r.read_exact(&mut hash)
                .map_err(|e| map_truncated(e, "truncated blob header"))?;
            let size = read_u64le(r, "truncated blob header")?;
            let data = read_payload(r, size, "truncated blob payload")?;
            Ok(Record::Blob { hash, data })
        }
        REC_MANIFEST => {
            let size = read_u64le(r, "truncated manifest header")?;
            let json = read_payload(r, size, "truncated manifest payload")?;
            Ok(Record::Manifest { json })
        }
        REC_END => {
            let mut digest = [0u8; 32];
            r.read_exact(&mut digest)
                .map_err(|e| map_truncated(e, "truncated END record"))?;
            Ok(Record::End { digest })
        }
        0x00 => Err(AyzenpackError::Format("reserved record type")),
        _ => Err(AyzenpackError::Format("unknown record type")),
    }
}

/// Read records until END (inclusive). Errors if END is missing or stream order is invalid.
pub fn read_records<R: Read>(r: &mut R) -> Result<Vec<Record>> {
    let mut records = Vec::new();
    let mut seen_manifest = false;
    loop {
        let rec = match read_record(r) {
            Ok(rec) => rec,
            Err(AyzenpackError::Format("truncated record")) => {
                return Err(AyzenpackError::Format("missing END record"));
            }
            Err(e) => return Err(e),
        };
        match &rec {
            Record::Blob { .. } => {
                if seen_manifest {
                    return Err(AyzenpackError::Format("BLOB after MANIFEST"));
                }
            }
            Record::Manifest { .. } => {
                if seen_manifest {
                    return Err(AyzenpackError::Format("multiple MANIFEST records"));
                }
                seen_manifest = true;
            }
            Record::End { .. } => {
                if !seen_manifest {
                    return Err(AyzenpackError::Format("missing MANIFEST record"));
                }
                records.push(rec);
                return Ok(records);
            }
        }
        records.push(rec);
    }
}

/// Write header + records. Dispatches on `header.version` (1 = one frame, 2 = grouped + TOC).
pub fn write_ayz_file(
    file: &mut File,
    header: &FileHeader,
    records: &[Record],
    jar_count: u64,
) -> Result<Trailer> {
    match header.version {
        1 => write_ayz_file_v1(file, header, records, jar_count),
        2 => write_ayz_file_v2(file, header, records, jar_count),
        other => Err(AyzenpackError::UnsupportedVersion(other as u8)),
    }
}

/// One zstd frame of all records, no TOC. `header.version` must be 1.
pub fn write_ayz_file_v1(
    file: &mut File,
    header: &FileHeader,
    records: &[Record],
    jar_count: u64,
) -> Result<Trailer> {
    if header.version != 1 {
        return Err(AyzenpackError::UnsupportedVersion(header.version as u8));
    }
    validate_record_stream(records)?;
    let header_len = write_header(file, header)?;
    let header_total = file.stream_position().map_err(io_error)?;

    let mut enc = zstd::stream::Encoder::new(
        BufWriter::with_capacity(BUF_WRITER_CAP, file),
        header.zstd_level,
    )
    .map_err(io_error)?;
    enc.include_checksum(false).map_err(io_error)?;
    for rec in records {
        write_record(&mut enc, rec)?;
    }
    let mut w = enc.finish().map_err(io_error)?;
    w.flush().map_err(io_error)?;
    let mid_len = w.get_ref().metadata().map_err(io_error)?.len();
    let payload_bytes = mid_len - header_total;

    let mut trailer = trailer_from_records(records, payload_bytes, header_len, jar_count);
    trailer.version = 1;
    trailer.toc_len = 0;
    write_trailer(&mut w, &trailer)?;
    w.flush().map_err(io_error)?;
    Ok(trailer)
}

fn write_ayz_file_v2(
    file: &mut File,
    header: &FileHeader,
    records: &[Record],
    jar_count: u64,
) -> Result<Trailer> {
    validate_record_stream(records)?;
    let header_len = write_header(file, header)?;
    let header_total = file.stream_position().map_err(io_error)?;
    let mut writer = AyzWriter::after_header(file, header_len, header_total, header.zstd_level);
    let mut manifest_json = None;
    let mut digest = None;
    let mut blob_count = 0u64;
    let mut blob_bytes = 0u64;
    for rec in records {
        match rec {
            Record::Blob { hash, data } => {
                writer.write_blob(hash, data)?;
                blob_count += 1;
                blob_bytes += data.len() as u64;
            }
            Record::Manifest { json } => manifest_json = Some(json.as_slice()),
            Record::End { digest: d } => digest = Some(*d),
        }
    }
    let json = manifest_json.ok_or(AyzenpackError::Format("missing MANIFEST record"))?;
    let digest = digest.ok_or(AyzenpackError::Format("missing END record"))?;
    let (trailer, _) = writer.finish(json, digest, blob_count, blob_bytes, jar_count, 2)?;
    Ok(trailer)
}

/// Read trailer + header, check version agreement and `toc_len`, leave `r` after the header.
pub fn open_ayz_layout<R: Read + Seek>(r: &mut R) -> Result<AyzLayout> {
    let file_len = r.seek(SeekFrom::End(0)).map_err(io_error)?;
    let trailer = read_trailer(r)?;
    r.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let header = read_header(r)?;
    let header_total = r.stream_position().map_err(io_error)?;
    let from_lens = 12u64
        .checked_add(u64::from(trailer.header_len))
        .ok_or(AyzenpackError::Format("header_len overflow"))?;
    if header_total != from_lens {
        return Err(AyzenpackError::Format("header_total != 12+header_len"));
    }
    if header.version != trailer.version {
        return Err(AyzenpackError::VersionSkew {
            magic: header.version as u8,
            header: header.version,
            trailer: trailer.version,
        });
    }
    let expected_toc = file_len
        .checked_sub(TRAILER_LEN)
        .and_then(|x| x.checked_sub(header_total))
        .and_then(|x| x.checked_sub(trailer.payload_bytes))
        .ok_or(AyzenpackError::Format("toc_len overflow"))?;
    if trailer.toc_len != expected_toc {
        return Err(AyzenpackError::Format("toc_len mismatch"));
    }
    match header.version {
        1 => {
            if trailer.toc_len != 0 {
                return Err(AyzenpackError::Format("v1 toc_len must be 0"));
            }
        }
        2 => {
            if trailer.toc_len == 0 {
                return Err(AyzenpackError::Format("v2 toc_len must not be 0"));
            }
            if trailer.toc_len < TOC_OVERHEAD {
                return Err(AyzenpackError::Format("truncated TOC"));
            }
            if (trailer.toc_len - TOC_OVERHEAD) % crate::format::TOC_ENTRY_SIZE != 0 {
                return Err(AyzenpackError::Format("v2 toc_len not 28+n*56"));
            }
        }
        other => return Err(AyzenpackError::UnsupportedVersion(other as u8)),
    }
    Ok(AyzLayout {
        header,
        trailer,
        header_total,
        file_len,
    })
}

/// Decode every zstd frame in `payload_bytes` (v1: one frame; v2: blob groups + manifest).
pub fn decode_payload<R: Read>(
    r: R,
    version: u32,
) -> Result<zstd::stream::read::Decoder<'static, std::io::BufReader<R>>> {
    let dec = zstd::stream::Decoder::new(r).map_err(io_error)?;
    if version == 1 {
        Ok(dec.single_frame())
    } else {
        Ok(dec)
    }
}

/// Read trailer, header, then decode all record frames (stop after END).
pub fn read_ayz_file<R: Read + Seek>(r: &mut R) -> Result<(FileHeader, Trailer, Vec<Record>)> {
    let layout = open_ayz_layout(r)?;
    let limited = Read::take(r, layout.trailer.payload_bytes);
    let mut decoder = decode_payload(limited, layout.header.version)?;
    let records = read_records(&mut decoder)?;
    Ok((layout.header, layout.trailer, records))
}

/// Seek the last (MANIFEST+END) v2 frame via the TOC. v1 falls back to a full decode.
pub fn read_manifest_records<R: Read + Seek>(r: &mut R) -> Result<(AyzLayout, Vec<Record>)> {
    let layout = open_ayz_layout(r)?;
    if layout.header.version == 1 {
        let limited = Read::take(r, layout.trailer.payload_bytes);
        let mut decoder = decode_payload(limited, 1)?;
        let records = read_records(&mut decoder)?;
        return Ok((layout, records));
    }
    r.seek(SeekFrom::Start(
        layout.header_total + layout.trailer.payload_bytes,
    ))
    .map_err(io_error)?;
    let toc = read_toc(r, layout.trailer.toc_len)?;
    let frame_abs = layout
        .header_total
        .checked_add(toc.manifest_zstd_off)
        .ok_or(AyzenpackError::Format("manifest_zstd_off overflow"))?;
    r.seek(SeekFrom::Start(frame_abs)).map_err(io_error)?;
    let limited = Read::take(r, toc.manifest_zstd_len);
    let mut decoder = zstd::stream::Decoder::new(limited).map_err(io_error)?;
    let records = read_records(&mut decoder)?;
    Ok((layout, records))
}

pub fn read_toc_at<R: Read + Seek>(r: &mut R, layout: &AyzLayout) -> Result<Toc> {
    r.seek(SeekFrom::Start(
        layout.header_total + layout.trailer.payload_bytes,
    ))
    .map_err(io_error)?;
    read_toc(r, layout.trailer.toc_len)
}

fn read_u64le<R: Read>(r: &mut R, msg: &'static str) -> Result<u64> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf).map_err(|e| map_truncated(e, msg))?;
    Ok(u64::from_le_bytes(buf))
}

fn read_payload<R: Read>(r: &mut R, size: u64, msg: &'static str) -> Result<Vec<u8>> {
    let n =
        usize::try_from(size).map_err(|_| AyzenpackError::Format("record payload too large"))?;
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf).map_err(|e| map_truncated(e, msg))?;
    Ok(buf)
}

fn validate_record_stream(records: &[Record]) -> Result<()> {
    let mut i = 0;
    while i < records.len() && matches!(records[i], Record::Blob { .. }) {
        i += 1;
    }
    if i >= records.len() || !matches!(records[i], Record::Manifest { .. }) {
        return Err(AyzenpackError::Format("missing MANIFEST record"));
    }
    i += 1;
    if i >= records.len() || !matches!(records[i], Record::End { .. }) {
        return Err(AyzenpackError::Format("missing END record"));
    }
    if i + 1 != records.len() {
        return Err(AyzenpackError::Format("records after END"));
    }
    Ok(())
}

fn trailer_from_records(
    records: &[Record],
    payload_bytes: u64,
    header_len: u32,
    jar_count: u64,
) -> Trailer {
    let mut blob_count = 0u64;
    let mut blob_bytes = 0u64;
    let mut manifest_len = 0u64;
    for rec in records {
        match rec {
            Record::Blob { data, .. } => {
                blob_count += 1;
                blob_bytes += data.len() as u64;
            }
            Record::Manifest { json } => manifest_len = json.len() as u64,
            Record::End { .. } => {}
        }
    }
    Trailer {
        payload_bytes,
        manifest_len,
        blob_count,
        blob_bytes,
        jar_count,
        header_len,
        version: 1,
        toc_len: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{FILE_MAGIC, TOC_ENTRY_SIZE, TOC_OVERHEAD, TRAILER_LEN};
    use crate::hashutil::blake3_bytes;
    use std::io::Cursor;

    fn order_digest(hashes: &[[u8; 32]]) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        for hash in hashes {
            h.update(hash);
        }
        *h.finalize().as_bytes()
    }

    fn zstd_roundtrip(records: &[Record]) -> Result<Vec<Record>> {
        let mut enc = zstd::stream::Encoder::new(Vec::new(), 3).map_err(io_error)?;
        enc.include_checksum(false).map_err(io_error)?;
        for rec in records {
            write_record(&mut enc, rec)?;
        }
        let compressed = enc.finish().map_err(io_error)?;
        let mut dec = zstd::stream::Decoder::new(compressed.as_slice())
            .map_err(io_error)?
            .single_frame();
        read_records(&mut dec)
    }

    fn fill_incompressible(len: usize) -> Vec<u8> {
        let mut data = vec![0u8; len];
        let mut x: u32 = 0x1234_5678;
        for b in &mut data {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *b = (x >> 16) as u8;
        }
        data
    }

    fn sample_records() -> (Vec<u8>, Vec<u8>, Vec<Record>) {
        let d0 = b"hello blob one".to_vec();
        let d1 = b"second blob payload".to_vec();
        let h0 = blake3_bytes(&d0);
        let h1 = blake3_bytes(&d1);
        let json = br#"{"format":"ayzenpack-manifest"}"#.to_vec();
        let records = vec![
            Record::Blob {
                hash: h0,
                data: d0.clone(),
            },
            Record::Blob {
                hash: h1,
                data: d1.clone(),
            },
            Record::Manifest { json: json.clone() },
            Record::End {
                digest: order_digest(&[h0, h1]),
            },
        ];
        (d0, d1, records)
    }

    #[test]
    fn zstd_record_roundtrip_two_blobs_manifest_end() {
        // Guards per-blob zstd (must be one blob frame) and length-prefix endian.
        let (d0, d1, records) = sample_records();
        let json = match &records[2] {
            Record::Manifest { json } => json.clone(),
            _ => panic!("manifest"),
        };

        let mut raw = Vec::new();
        for rec in &records {
            write_record(&mut raw, rec).unwrap();
        }
        assert_eq!(raw[0], REC_BLOB);
        assert_eq!(&raw[33..41], &(d0.len() as u64).to_le_bytes());
        let m_off = 1 + 32 + 8 + d0.len() + 1 + 32 + 8 + d1.len();
        assert_eq!(raw[m_off], REC_MANIFEST);
        assert_eq!(
            &raw[m_off + 1..m_off + 9],
            &(json.len() as u64).to_le_bytes()
        );

        let mut file = tempfile::tempfile().unwrap();
        let header = FileHeader::new(3, 1_710_000_000);
        let trailer = write_ayz_file(&mut file, &header, &records, 0).unwrap();
        assert_eq!(trailer.blob_count, 2);
        assert_eq!(trailer.blob_bytes, (d0.len() + d1.len()) as u64);
        assert_eq!(trailer.manifest_len, json.len() as u64);
        assert_eq!(trailer.version, 2);
        assert_eq!(trailer.toc_len, TOC_OVERHEAD + 2 * TOC_ENTRY_SIZE);

        let file_len = file.metadata().unwrap().len();
        let header_total = 12 + u64::from(trailer.header_len);
        assert_eq!(
            file_len,
            header_total + trailer.payload_bytes + trailer.toc_len + TRAILER_LEN
        );

        file.seek(SeekFrom::Start(0)).unwrap();
        let (got_header, got_trailer, got_records) = read_ayz_file(&mut file).unwrap();
        assert_eq!(got_header, header);
        assert_eq!(got_trailer, trailer);
        assert_eq!(got_records, records);

        file.seek(SeekFrom::Start(0)).unwrap();
        let layout = open_ayz_layout(&mut file).unwrap();
        let toc = read_toc_at(&mut file, &layout).unwrap();
        assert_eq!(toc.entries.len(), 2);
        assert_eq!(toc.entries[0].zstd_off, 0, "payload-relative origin");
        assert_eq!(
            toc.entries[0].zstd_off, toc.entries[1].zstd_off,
            "two small blobs must share one zstd frame (not per-blob)"
        );
        assert_eq!(toc.entries[0].zstd_len, toc.entries[1].zstd_len);
        assert_eq!(toc.entries[0].rec_off, 0);
        assert_eq!(toc.entries[1].rec_off, blob_record_len(d0.len() as u64));
        assert_ne!(
            toc.manifest_zstd_len, trailer.manifest_len,
            "TOC must not copy uncompressed Trailer.manifest_len"
        );
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut magic = [0u8; 8];
        file.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, &FILE_MAGIC);
    }

    #[test]
    fn empty_blob_roundtrips() {
        let hash = blake3_bytes(b"");
        let records = vec![
            Record::Blob {
                hash,
                data: Vec::new(),
            },
            Record::Manifest {
                json: b"{}".to_vec(),
            },
            Record::End {
                digest: order_digest(&[hash]),
            },
        ];

        let mut raw = Vec::new();
        write_record(&mut raw, &records[0]).unwrap();
        assert_eq!(raw[0], REC_BLOB);
        assert_eq!(&raw[1..33], &hash);
        assert_eq!(&raw[33..41], &0u64.to_le_bytes());
        assert_eq!(raw.len(), 41);

        let got = zstd_roundtrip(&records).unwrap();
        assert_eq!(got, records);
        match &got[0] {
            Record::Blob { data, .. } => assert!(data.is_empty()),
            other => panic!("expected empty blob, got {other:?}"),
        }
    }

    #[test]
    fn one_byte_and_64kib_blob_roundtrip() {
        for size in [1usize, 64 * 1024] {
            let data = vec![0x5a; size];
            let hash = blake3_bytes(&data);
            let records = vec![
                Record::Blob {
                    hash,
                    data: data.clone(),
                },
                Record::Manifest {
                    json: b"{}".to_vec(),
                },
                Record::End {
                    digest: order_digest(&[hash]),
                },
            ];

            let mut raw = Vec::new();
            write_record(&mut raw, &records[0]).unwrap();
            assert_eq!(raw[0], REC_BLOB);
            assert_eq!(&raw[33..41], &(size as u64).to_le_bytes());
            assert_eq!(raw.len(), 1 + 32 + 8 + size);

            let got = zstd_roundtrip(&records).unwrap();
            assert_eq!(got, records, "size={size}");
        }
    }

    #[test]
    fn reserved_type_zero_errors() {
        let err = read_record(&mut Cursor::new([0x00u8])).unwrap_err();
        assert!(
            matches!(err, AyzenpackError::Format("reserved record type")),
            "0x00 must error, got {err:?}"
        );
        assert_eq!(err.to_string(), "reserved record type");
    }

    #[test]
    fn unknown_type_errors() {
        for ty in [0x04u8, 0xFF] {
            let err = read_record(&mut Cursor::new([ty])).unwrap_err();
            assert!(
                matches!(err, AyzenpackError::Format("unknown record type")),
                "type {ty:#x} must error, got {err:?}"
            );
            assert_eq!(err.to_string(), "unknown record type");
        }
    }

    #[test]
    fn truncated_blob_payload_errors() {
        let mut buf = Vec::new();
        buf.push(REC_BLOB);
        buf.extend_from_slice(&[0x11u8; 32]);
        buf.extend_from_slice(&8u64.to_le_bytes());
        buf.extend_from_slice(&[1, 2, 3, 4]);
        let err = read_record(&mut Cursor::new(buf)).unwrap_err();
        assert!(
            matches!(err, AyzenpackError::Format("truncated blob payload")),
            "got {err:?}"
        );
        assert_eq!(err.to_string(), "truncated blob payload");
    }

    #[test]
    fn zstd_record_roundtrip_multi_megabyte_payload_bytes_filled_before_trailer() {
        // Guards trailer written to a raw File while BufWriter is dirty, and writing
        // trailer then deriving payload_bytes from file_len-64.
        let data = fill_incompressible(2 * 1024 * 1024);
        let hash = blake3_bytes(&data);
        let json = vec![b'x'; 1024];
        let records = vec![
            Record::Blob {
                hash,
                data: data.clone(),
            },
            Record::Manifest { json: json.clone() },
            Record::End {
                digest: order_digest(&[hash]),
            },
        ];

        let mut file = tempfile::tempfile().unwrap();
        let mut header = FileHeader::new(3, 0);
        header.version = 1;
        let header_len = write_header(&mut file, &header).unwrap();
        let header_total = file.stream_position().unwrap();
        assert_eq!(header_total, 12 + u64::from(header_len));

        let mut enc = zstd::stream::Encoder::new(
            BufWriter::with_capacity(BUF_WRITER_CAP, &mut file),
            header.zstd_level,
        )
        .unwrap();
        enc.include_checksum(false).unwrap();
        for rec in &records {
            write_record(&mut enc, rec).unwrap();
        }
        let mut w = enc.finish().unwrap();
        w.flush().unwrap();
        let mid_len = w.get_ref().metadata().unwrap().len();
        let payload_bytes = mid_len - header_total;
        assert!(
            payload_bytes > BUF_WRITER_CAP as u64,
            "zstd payload must exceed BufWriter cap, got {payload_bytes}"
        );

        let trailer = Trailer {
            payload_bytes,
            manifest_len: json.len() as u64,
            blob_count: 1,
            blob_bytes: data.len() as u64,
            jar_count: 0,
            header_len,
            version: 1,
            toc_len: 0,
        };
        write_trailer(&mut w, &trailer).unwrap();
        w.flush().unwrap();
        let file_len = w.get_ref().metadata().unwrap().len();
        assert_eq!(file_len, mid_len + 64);
        drop(w);

        let got = read_trailer(&mut file).unwrap();
        assert_eq!(got.payload_bytes, payload_bytes);
        assert_eq!(got, trailer);

        file.seek(SeekFrom::End(-64)).unwrap();
        let mut tbuf = [0u8; 64];
        file.read_exact(&mut tbuf).unwrap();
        assert_eq!(&tbuf[8..16], &payload_bytes.to_le_bytes());

        file.seek(SeekFrom::Start(0)).unwrap();
        let (got_header, got_trailer, got_records) = read_ayz_file(&mut file).unwrap();
        assert_eq!(got_header, header);
        assert_eq!(got_trailer, trailer);
        assert_eq!(got_records, records);
    }

    #[test]
    fn v2_last_frame_is_manifest_and_end_only() {
        let (_, _, records) = sample_records();
        let mut file = tempfile::tempfile().unwrap();
        let header = FileHeader::new(3, 0);
        write_ayz_file(&mut file, &header, &records, 0).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let (layout, last) = read_manifest_records(&mut file).unwrap();
        assert_eq!(layout.header.version, 2);
        assert!(matches!(last[0], Record::Manifest { .. }));
        assert!(matches!(last[1], Record::End { .. }));
        assert_eq!(last.len(), 2);
        assert!(!last.iter().any(|r| matches!(r, Record::Blob { .. })));
    }

    #[test]
    fn write_ayz_file_v1_is_one_frame_version_1() {
        let (_, _, records) = sample_records();
        let mut file = tempfile::tempfile().unwrap();
        let mut header = FileHeader::new(3, 0);
        header.version = 1;
        let trailer = write_ayz_file_v1(&mut file, &header, &records, 0).unwrap();
        assert_eq!(trailer.version, 1);
        assert_eq!(trailer.toc_len, 0);
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut magic = [0u8; 8];
        file.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, crate::format::FILE_MAGIC_V1.as_ref());
        file.seek(SeekFrom::Start(0)).unwrap();
        let (got_h, got_t, got_r) = read_ayz_file(&mut file).unwrap();
        assert_eq!(got_h.version, 1);
        assert_eq!(got_t.version, 1);
        assert_eq!(got_r, records);
    }

    #[test]
    fn synthetic_two_blob_frames() {
        let d0 = fill_incompressible(3 * 1024 * 1024);
        let d1 = fill_incompressible(3 * 1024 * 1024);
        let h0 = blake3_bytes(&d0);
        let h1 = blake3_bytes(&d1);
        let json = br#"{"format":"ayzenpack-manifest","two_frames":true}"#.to_vec();
        let records = vec![
            Record::Blob { hash: h0, data: d0 },
            Record::Blob { hash: h1, data: d1 },
            Record::Manifest { json },
            Record::End {
                digest: order_digest(&[h0, h1]),
            },
        ];
        let mut file = tempfile::tempfile().unwrap();
        let header = FileHeader::new(3, 0);
        let trailer = write_ayz_file(&mut file, &header, &records, 0).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        let layout = open_ayz_layout(&mut file).unwrap();
        let toc = read_toc_at(&mut file, &layout).unwrap();
        assert_eq!(toc.entries.len(), 2);
        assert_eq!(toc.entries[0].zstd_off, 0);
        assert_ne!(
            toc.entries[0].zstd_off, toc.entries[1].zstd_off,
            "3 MiB + 3 MiB must flush into two blob frames"
        );
        assert_ne!(toc.manifest_zstd_off, toc.entries[1].zstd_off);
        assert_ne!(toc.manifest_zstd_len, trailer.manifest_len);
        file.seek(SeekFrom::Start(0)).unwrap();
        let (_, _, got) = read_ayz_file(&mut file).unwrap();
        assert_eq!(got.len(), 4);
    }

    #[test]
    fn empty_v2_is_one_manifest_frame() {
        let json = b"{}".to_vec();
        let records = vec![
            Record::Manifest { json: json.clone() },
            Record::End { digest: [0u8; 32] },
        ];
        let mut file = tempfile::tempfile().unwrap();
        let header = FileHeader::new(3, 0);
        let trailer = write_ayz_file(&mut file, &header, &records, 0).unwrap();
        assert_eq!(trailer.toc_len, TOC_OVERHEAD);
        file.seek(SeekFrom::Start(0)).unwrap();
        let layout = open_ayz_layout(&mut file).unwrap();
        let toc = read_toc_at(&mut file, &layout).unwrap();
        assert!(toc.entries.is_empty());
        assert_eq!(toc.manifest_zstd_off, 0);
        assert_eq!(toc.manifest_zstd_len, trailer.payload_bytes);
    }
}
