//! Slice a ZIP (after any executable prefix) into bit-exact reconstruction parts.
//!
//! Local records are read by seeking to central-directory offsets. Compressed
//! payloads are the original bytes — the zip crate's inflating reader is not used
//! for `cdata`. Parse failure, spanning, or a CD/entry-count mismatch yields the
//! whole zip portion as `Raw` so rehydrate can still copy it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::error::{AyzenpackError, Result};
use crate::scan::{detect_zip_layout, find_cd_bounds, find_eocd, io_at, ZipLayout};

const LOCAL_FILE_MAGIC: [u8; 4] = *b"PK\x03\x04";
const CD_MAGIC: [u8; 4] = *b"PK\x01\x02";
const DATA_DESC_MAGIC: [u8; 4] = *b"PK\x07\x08";
const ZIP64_EXTRA_ID: u16 = 0x0001;
const GPBF_ENCRYPTED: u16 = 0x0001;
const GPBF_DATA_DESC: u16 = 0x0008;

#[derive(Debug, Clone)]
pub(crate) struct ExactLocal {
    pub zip_rel_offset: u64,
    pub header: Vec<u8>,
    pub cdata: Vec<u8>,
    pub descriptor: Option<Vec<u8>>,
    pub pad: Vec<u8>,
}

#[derive(Debug, Clone)]
pub(crate) struct ExactSlice {
    pub locals: Vec<ExactLocal>,
    pub tail: Vec<u8>,
}

#[derive(Debug, Clone)]
pub(crate) enum ZipExact {
    Sliced(ExactSlice),
    Raw(Vec<u8>),
}

struct CdRecord {
    crc: u32,
    compressed_size: u64,
    uncompressed_size: u64,
    /// ZIP-relative local header offset.
    zip_rel_offset: u64,
}

pub(crate) fn capture_zip_exact(path: &Path) -> Result<ZipExact> {
    match slice_zip(path) {
        Ok(slice) => Ok(ZipExact::Sliced(slice)),
        Err(AyzenpackError::Encrypted { .. }) => Err(AyzenpackError::Encrypted {
            path: path.to_path_buf(),
        }),
        Err(_) => {
            let zip = read_zip_portion(path)?;
            Ok(ZipExact::Raw(zip))
        }
    }
}

fn read_zip_portion(path: &Path) -> Result<Vec<u8>> {
    let mut file = File::open(path).map_err(|source| io_at(source, path))?;
    let layout = detect_zip_layout(path, &mut file)?;
    let file_len = file
        .seek(SeekFrom::End(0))
        .map_err(|source| io_at(source, path))?;
    let zip_len = file_len.saturating_sub(layout.prefix_len);
    file.seek(SeekFrom::Start(layout.prefix_len))
        .map_err(|source| io_at(source, path))?;
    let mut zip = vec![0u8; usize_from_u64(zip_len, path)?];
    file.read_exact(&mut zip)
        .map_err(|source| io_at(source, path))?;
    Ok(zip)
}

fn slice_zip(path: &Path) -> Result<ExactSlice> {
    let mut file = File::open(path).map_err(|source| io_at(source, path))?;
    let layout = detect_zip_layout(path, &mut file)?;
    let file_len = file
        .seek(SeekFrom::End(0))
        .map_err(|source| io_at(source, path))?;
    if file_len < layout.prefix_len {
        return Err(slice_fail(path, "prefix longer than file"));
    }

    check_not_spanned(path, &mut file, file_len)?;

    let (cd_struct_off, cd_size, _recorded_cd, entry_count) =
        find_cd_bounds(path, &mut file, file_len)?;
    let phys_cd = cd_struct_off
        .checked_sub(cd_size)
        .ok_or_else(|| slice_fail(path, "central directory offset underflow"))?;
    if phys_cd < layout.prefix_len {
        return Err(slice_fail(path, "central directory starts inside prefix"));
    }
    let zip_rel_cd = phys_cd - layout.prefix_len;

    file.seek(SeekFrom::Start(phys_cd))
        .map_err(|source| io_at(source, path))?;
    let mut cd = vec![0u8; usize_from_u64(cd_size, path)?];
    file.read_exact(&mut cd)
        .map_err(|source| io_at(source, path))?;

    let records = parse_central_directory(&cd, &layout)
        .ok_or_else(|| slice_fail(path, "central directory parse failed"))?;
    if u64::try_from(records.len()).unwrap_or(u64::MAX) != entry_count {
        return Err(slice_fail(path, "central directory entry count mismatch"));
    }

    let mut tail = vec![0u8; usize_from_u64(file_len - phys_cd, path)?];
    file.seek(SeekFrom::Start(phys_cd))
        .map_err(|source| io_at(source, path))?;
    file.read_exact(&mut tail)
        .map_err(|source| io_at(source, path))?;

    if records.is_empty() {
        return Ok(ExactSlice {
            locals: Vec::new(),
            tail,
        });
    }

    let mut order: Vec<usize> = (0..records.len()).collect();
    order.sort_by_key(|&i| records[i].zip_rel_offset);
    if records[order[0]].zip_rel_offset != 0 {
        return Err(slice_fail(
            path,
            "first local header is not at zip offset 0",
        ));
    }
    for pair in order.windows(2) {
        if records[pair[0]].zip_rel_offset >= records[pair[1]].zip_rel_offset {
            return Err(slice_fail(path, "overlapping or unsorted local offsets"));
        }
    }

    let mut locals = Vec::with_capacity(records.len());
    for rec in &records {
        let next = order
            .iter()
            .copied()
            .find(|&j| records[j].zip_rel_offset > rec.zip_rel_offset)
            .map(|j| records[j].zip_rel_offset)
            .unwrap_or(zip_rel_cd);
        if next <= rec.zip_rel_offset {
            return Err(slice_fail(
                path,
                "local header offset at or past central directory",
            ));
        }
        locals.push(read_local(path, &mut file, &layout, rec, next)?);
    }

    Ok(ExactSlice { locals, tail })
}

fn check_not_spanned(path: &Path, file: &mut File, file_len: u64) -> Result<()> {
    let (eocd_off, _cd_size, _cd_off, _entries) = find_eocd(path, file, file_len)?;
    file.seek(SeekFrom::Start(eocd_off))
        .map_err(|source| io_at(source, path))?;
    let mut eocd = [0u8; 22];
    file.read_exact(&mut eocd)
        .map_err(|source| io_at(source, path))?;
    let this_disk = u16::from_le_bytes([eocd[4], eocd[5]]);
    let cd_disk = u16::from_le_bytes([eocd[6], eocd[7]]);
    if this_disk != 0 || cd_disk != 0 {
        return Err(slice_fail(path, "spanned zip"));
    }
    Ok(())
}

fn parse_central_directory(cd: &[u8], layout: &ZipLayout) -> Option<Vec<CdRecord>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < cd.len() {
        if i + 46 > cd.len() || cd[i..i + 4] != CD_MAGIC {
            return None;
        }
        let flags = u16::from_le_bytes(cd[i + 8..i + 10].try_into().ok()?);
        let crc = u32::from_le_bytes(cd[i + 16..i + 20].try_into().ok()?);
        let comp32 = u32::from_le_bytes(cd[i + 20..i + 24].try_into().ok()?);
        let uncomp32 = u32::from_le_bytes(cd[i + 24..i + 28].try_into().ok()?);
        let name_len = u16::from_le_bytes(cd[i + 28..i + 30].try_into().ok()?) as usize;
        let extra_len = u16::from_le_bytes(cd[i + 30..i + 32].try_into().ok()?) as usize;
        let comment_len = u16::from_le_bytes(cd[i + 32..i + 34].try_into().ok()?) as usize;
        let disk16 = u16::from_le_bytes(cd[i + 34..i + 36].try_into().ok()?);
        let off32 = u32::from_le_bytes(cd[i + 42..i + 46].try_into().ok()?);
        let rec_end = i
            .checked_add(46)?
            .checked_add(name_len)?
            .checked_add(extra_len)?
            .checked_add(comment_len)?;
        if rec_end > cd.len() {
            return None;
        }
        let extra = &cd[i + 46 + name_len..i + 46 + name_len + extra_len];
        let (uncomp, comp, local_off, disk) =
            resolve_cd_zip64(extra, uncomp32, comp32, off32, disk16)?;
        if disk != 0 {
            return None;
        }
        if flags & GPBF_ENCRYPTED != 0 {
            return None;
        }
        let zip_rel_offset = cd_offset_to_zip_rel(local_off, layout)?;
        out.push(CdRecord {
            crc,
            compressed_size: comp,
            uncompressed_size: uncomp,
            zip_rel_offset,
        });
        i = rec_end;
    }
    if i != cd.len() {
        return None;
    }
    Some(out)
}

fn cd_offset_to_zip_rel(recorded: u64, layout: &ZipLayout) -> Option<u64> {
    if layout.view_shift == 0 && layout.prefix_len > 0 {
        recorded.checked_sub(layout.prefix_len)
    } else {
        Some(recorded)
    }
}

fn resolve_cd_zip64(
    extra: &[u8],
    uncomp32: u32,
    comp32: u32,
    off32: u32,
    disk16: u16,
) -> Option<(u64, u64, u64, u32)> {
    let need =
        uncomp32 == u32::MAX || comp32 == u32::MAX || off32 == u32::MAX || disk16 == u16::MAX;
    if !need {
        return Some((
            u64::from(uncomp32),
            u64::from(comp32),
            u64::from(off32),
            u32::from(disk16),
        ));
    }
    let data = find_extra(extra, ZIP64_EXTRA_ID)?;
    let mut cur = data;
    let uncomp = if uncomp32 == u32::MAX {
        let (v, rest) = take_u64(cur)?;
        cur = rest;
        v
    } else {
        u64::from(uncomp32)
    };
    let comp = if comp32 == u32::MAX {
        let (v, rest) = take_u64(cur)?;
        cur = rest;
        v
    } else {
        u64::from(comp32)
    };
    let off = if off32 == u32::MAX {
        let (v, rest) = take_u64(cur)?;
        cur = rest;
        v
    } else {
        u64::from(off32)
    };
    let disk = if disk16 == u16::MAX {
        let (v, _rest) = take_u32(cur)?;
        v
    } else {
        u32::from(disk16)
    };
    Some((uncomp, comp, off, disk))
}

fn find_extra(extra: &[u8], id: u16) -> Option<&[u8]> {
    let mut i = 0usize;
    while i + 4 <= extra.len() {
        let tag = u16::from_le_bytes(extra[i..i + 2].try_into().ok()?);
        let size = u16::from_le_bytes(extra[i + 2..i + 4].try_into().ok()?) as usize;
        let start = i + 4;
        let end = start.checked_add(size)?;
        if end > extra.len() {
            return None;
        }
        if tag == id {
            return Some(&extra[start..end]);
        }
        i = end;
    }
    None
}

fn take_u64(data: &[u8]) -> Option<(u64, &[u8])> {
    if data.len() < 8 {
        return None;
    }
    let v = u64::from_le_bytes(data[..8].try_into().ok()?);
    Some((v, &data[8..]))
}

fn take_u32(data: &[u8]) -> Option<(u32, &[u8])> {
    if data.len() < 4 {
        return None;
    }
    let v = u32::from_le_bytes(data[..4].try_into().ok()?);
    Some((v, &data[4..]))
}

fn read_local(
    path: &Path,
    file: &mut File,
    layout: &ZipLayout,
    rec: &CdRecord,
    next_zip_rel: u64,
) -> Result<ExactLocal> {
    let phys = layout
        .prefix_len
        .checked_add(rec.zip_rel_offset)
        .ok_or_else(|| slice_fail(path, "local header seek overflow"))?;
    file.seek(SeekFrom::Start(phys))
        .map_err(|source| io_at(source, path))?;
    let mut fixed = [0u8; 30];
    file.read_exact(&mut fixed)
        .map_err(|source| io_at(source, path))?;
    if fixed[..4] != LOCAL_FILE_MAGIC {
        return Err(slice_fail(path, "local header magic"));
    }
    let flags = u16::from_le_bytes([fixed[6], fixed[7]]);
    if flags & GPBF_ENCRYPTED != 0 {
        return Err(AyzenpackError::Encrypted {
            path: path.to_path_buf(),
        });
    }
    let name_len = u16::from_le_bytes([fixed[26], fixed[27]]) as usize;
    let extra_len = u16::from_le_bytes([fixed[28], fixed[29]]) as usize;
    let mut name_extra = vec![0u8; name_len + extra_len];
    file.read_exact(&mut name_extra)
        .map_err(|source| io_at(source, path))?;
    let mut header = Vec::with_capacity(30 + name_extra.len());
    header.extend_from_slice(&fixed);
    header.extend_from_slice(&name_extra);

    let header_end = rec
        .zip_rel_offset
        .checked_add(header.len() as u64)
        .ok_or_else(|| slice_fail(path, "local header length overflow"))?;
    if header_end > next_zip_rel {
        return Err(slice_fail(path, "local header overruns next record"));
    }

    let csize = rec.compressed_size;
    let cdata_end = header_end
        .checked_add(csize)
        .ok_or_else(|| slice_fail(path, "compressed size overflow"))?;
    if cdata_end > next_zip_rel {
        return Err(slice_fail(path, "compressed data overruns next record"));
    }
    let mut cdata = vec![0u8; usize_from_u64(csize, path)?];
    file.read_exact(&mut cdata)
        .map_err(|source| io_at(source, path))?;

    let after_len = next_zip_rel - cdata_end;
    let mut after = vec![0u8; usize_from_u64(after_len, path)?];
    file.read_exact(&mut after)
        .map_err(|source| io_at(source, path))?;

    let (descriptor, pad) = if flags & GPBF_DATA_DESC != 0 {
        let zip64 = rec.compressed_size >= u64::from(u32::MAX)
            || rec.uncompressed_size >= u64::from(u32::MAX);
        split_descriptor(&after, rec.crc, zip64)
            .ok_or_else(|| slice_fail(path, "data descriptor"))?
    } else {
        (None, after)
    };

    Ok(ExactLocal {
        zip_rel_offset: rec.zip_rel_offset,
        header,
        cdata,
        descriptor,
        pad,
    })
}

fn split_descriptor(
    after: &[u8],
    crc: u32,
    zip64_likely: bool,
) -> Option<(Option<Vec<u8>>, Vec<u8>)> {
    let candidates: &[usize] = if zip64_likely {
        &[24, 20, 16, 12]
    } else {
        &[16, 12, 24, 20]
    };
    for &len in candidates {
        if after.len() < len {
            continue;
        }
        let desc = &after[..len];
        let has_sig = desc.starts_with(&DATA_DESC_MAGIC);
        let expect_sig = len == 16 || len == 24;
        if has_sig != expect_sig {
            continue;
        }
        let crc_off = if has_sig { 4 } else { 0 };
        let got = u32::from_le_bytes(desc[crc_off..crc_off + 4].try_into().ok()?);
        if got != crc {
            continue;
        }
        return Some((Some(desc.to_vec()), after[len..].to_vec()));
    }
    None
}

fn usize_from_u64(n: u64, path: &Path) -> Result<usize> {
    usize::try_from(n).map_err(|_| {
        AyzenpackError::FormatOwned(format!("size {n} exceeds usize on {}", path.display()))
    })
}

fn slice_fail(path: &Path, why: &str) -> AyzenpackError {
    AyzenpackError::FormatOwned(format!(
        "exact zip slice failed for {}: {why}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    fn write_temp_zip(
        files: &[(&str, &[u8])],
        stored: bool,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jar");
        let mut z = ZipWriter::new(File::create(&path).unwrap());
        let method = if stored {
            zip::CompressionMethod::Stored
        } else {
            zip::CompressionMethod::Deflated
        };
        let opts = SimpleFileOptions::default().compression_method(method);
        for (name, data) in files {
            z.start_file(*name, opts).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
        (dir, path)
    }

    #[test]
    fn slice_roundtrip_bytes_eq_source_zip() {
        let (_dir, path) = write_temp_zip(&[("a.txt", b"hello"), ("b.txt", b"world")], false);
        let src = std::fs::read(&path).unwrap();
        match capture_zip_exact(&path).unwrap() {
            ZipExact::Sliced(slice) => {
                let mut out = Vec::new();
                for loc in &slice.locals {
                    assert_eq!(out.len() as u64, loc.zip_rel_offset);
                    out.extend_from_slice(&loc.header);
                    out.extend_from_slice(&loc.cdata);
                    if let Some(d) = &loc.descriptor {
                        out.extend_from_slice(d);
                    }
                    out.extend_from_slice(&loc.pad);
                }
                out.extend_from_slice(&slice.tail);
                assert_eq!(out, src);
            }
            ZipExact::Raw(_) => panic!("ZipWriter jar must slice cleanly"),
        }
    }

    #[test]
    fn stored_cdata_equals_payload() {
        let payload = b"same-bytes";
        let (_dir, path) = write_temp_zip(&[("x.bin", payload)], true);
        match capture_zip_exact(&path).unwrap() {
            ZipExact::Sliced(slice) => {
                assert_eq!(slice.locals.len(), 1);
                assert_eq!(slice.locals[0].cdata, payload);
            }
            ZipExact::Raw(_) => panic!("stored zip must slice"),
        }
    }

    #[test]
    fn empty_zip_is_tail_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.jar");
        let z = ZipWriter::new(Cursor::new(Vec::new()));
        let zip = z.finish().unwrap().into_inner();
        std::fs::write(&path, &zip).unwrap();
        match capture_zip_exact(&path).unwrap() {
            ZipExact::Sliced(slice) => {
                assert!(slice.locals.is_empty());
                assert_eq!(slice.tail, zip);
            }
            ZipExact::Raw(_) => panic!("empty zip must slice as tail"),
        }
    }
}
