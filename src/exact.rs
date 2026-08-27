//! Slice a ZIP (after any executable prefix) into bit-exact reconstruction parts.
//!
//! Dehydrate builds locals from the same `ZipArchive::by_index` listing as scan
//! (`slice_from_archive`). Homemade CD parse is a store-tail count gate only.
//! A count mismatch or a listed-jar slice failure is skip-exact, not `raw_zip`.
//! `capture_zip_exact` / `ZipExact::Raw` remain for unit tests of the homemade
//! walker; dehydrate must not consume `Raw`.

use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use zip::ZipArchive;

use crate::error::{AyzenpackError, Result};
#[cfg(test)]
use crate::scan::find_cd_bounds_v0_2_1;
use crate::scan::{detect_zip_layout, find_cd_bounds, io_at, ZipLayout, ZipView};

pub(crate) const LOCAL_FILE_MAGIC: [u8; 4] = *b"PK\x03\x04";
pub(crate) const CD_MAGIC: [u8; 4] = *b"PK\x01\x02";
pub(crate) const DATA_DESC_MAGIC: [u8; 4] = *b"PK\x07\x08";
pub(crate) const EOCD_MAGIC: [u8; 4] = *b"PK\x05\x06";
pub(crate) const ZIP64_LOCATOR_MAGIC: [u8; 4] = *b"PK\x06\x07";
pub(crate) const ZIP64_EOCD_MAGIC: [u8; 4] = *b"PK\x06\x06";
pub(crate) const ZIP64_EXTRA_ID: u16 = 0x0001;
const GPBF_ENCRYPTED: u16 = 0x0001;
pub(crate) const GPBF_DATA_DESC: u16 = 0x0008;

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
    /// Present only when homemade CD parse succeeded (`homemade_ok`).
    pub tail: Option<Vec<u8>>,
    pub homemade_ok: bool,
    /// Bytes `[0, first zip-rel local)` when `prefix_len == 0` and first local ≠ 0.
    pub leading_pad: Vec<u8>,
    /// Executable / decoy prefix (`layout.prefix_len`), if any.
    pub prefix: Vec<u8>,
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) enum ZipExact {
    Sliced(ExactSlice),
    /// Homemade `slice_zip` fallback for unit tests only. Dehydrate must not consume this.
    #[allow(dead_code)]
    Raw(Vec<u8>),
}

pub(crate) struct CdRecord {
    crc: u32,
    compressed_size: u64,
    uncompressed_size: u64,
    /// ZIP-relative local header offset.
    zip_rel_offset: u64,
}

#[cfg(test)]
pub(crate) fn capture_zip_exact(path: &Path) -> Result<ZipExact> {
    capture_zip_exact_using(path, slice_zip)
}

/// 0.2.1 homemade walker: `find_cd_bounds` ignored a present Zip64 locator unless
/// classic EOCD fields were sentinels. `Err` became `ZipExact::Raw` (whole zip).
#[cfg(test)]
pub(crate) fn capture_zip_exact_v0_2_1(path: &Path) -> Result<ZipExact> {
    capture_zip_exact_using(path, slice_zip_v0_2_1)
}

#[cfg(test)]
fn capture_zip_exact_using(
    path: &Path,
    slice: fn(&Path) -> Result<ExactSlice>,
) -> Result<ZipExact> {
    match slice(path) {
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

/// Locals from the same `ZipArchive` listing scan uses. Never returns a whole-zip `Raw`.
///
/// Store-tail is valid only when homemade `parse_central_directory` yields the same
/// count as `ZipArchive::len()`. Overlap or a homemade count mismatch is `Err`.
/// Homemade parse `None` is `Ok` with `homemade_ok == false` and `tail == None`.
/// A PK-start hole (`prefix_len == 0`, first local ≠ 0) is `leading_pad`.
pub(crate) fn slice_from_archive(path: &Path) -> Result<ExactSlice> {
    let mut file = File::open(path).map_err(|source| io_at(source, path))?;
    slice_from_reader(path, &mut file)
}

/// Slice an in-memory STORE ZIP (child probe). Encrypted / unlistable stay `Err`.
pub(crate) fn slice_from_bytes(bytes: &[u8]) -> Result<ExactSlice> {
    let path = Path::new("<bytes>");
    let mut cur = Cursor::new(bytes);
    slice_from_reader(path, &mut cur)
}

fn slice_from_reader<R: Read + Seek>(path: &Path, file: &mut R) -> Result<ExactSlice> {
    let layout = detect_zip_layout(path, file)?;
    let prefix = if layout.prefix_len > 0 {
        file.seek(SeekFrom::Start(0))
            .map_err(|source| io_at(source, path))?;
        let mut p = vec![0u8; usize_from_u64(layout.prefix_len, path)?];
        file.read_exact(&mut p)
            .map_err(|source| io_at(source, path))?;
        p
    } else {
        Vec::new()
    };
    let recs = archive_local_records(path, &layout, &mut *file)?;
    let mut leading_pad = Vec::new();
    if !recs.is_empty() {
        let min_off = recs.iter().map(|r| r.zip_rel_offset).min().unwrap();
        if min_off != 0 {
            if layout.prefix_len != 0 {
                return Err(slice_fail(
                    path,
                    "first local header is not at zip offset 0",
                ));
            }
            leading_pad = {
                file.seek(SeekFrom::Start(0))
                    .map_err(|source| io_at(source, path))?;
                let mut pad = vec![0u8; usize_from_u64(min_off, path)?];
                file.read_exact(&mut pad)
                    .map_err(|source| io_at(source, path))?;
                pad
            };
        }
        let mut ordered: Vec<u64> = recs.iter().map(|r| r.zip_rel_offset).collect();
        ordered.sort_unstable();
        for pair in ordered.windows(2) {
            if pair[0] >= pair[1] {
                return Err(slice_fail(path, "overlapping or unsorted local offsets"));
            }
        }
    }

    let file_len = file
        .seek(SeekFrom::End(0))
        .map_err(|source| io_at(source, path))?;
    if file_len < layout.prefix_len {
        return Err(slice_fail(path, "prefix longer than file"));
    }
    let (cd_struct_off, cd_size, _recorded_cd, _entry_count) =
        find_cd_bounds(path, file, file_len)?;
    let phys_cd = cd_struct_off
        .checked_sub(cd_size)
        .ok_or_else(|| slice_fail(path, "central directory offset underflow"))?;
    if phys_cd < layout.prefix_len {
        return Err(slice_fail(path, "central directory starts inside prefix"));
    }
    let zip_rel_cd = phys_cd - layout.prefix_len;
    let mut tail = vec![0u8; usize_from_u64(file_len - phys_cd, path)?];
    file.seek(SeekFrom::Start(phys_cd))
        .map_err(|source| io_at(source, path))?;
    file.read_exact(&mut tail)
        .map_err(|source| io_at(source, path))?;
    let cd_n = usize_from_u64(cd_size, path)?;
    if tail.len() < cd_n {
        return Err(slice_fail(path, "central directory truncated in tail"));
    }
    let homemade = parse_central_directory(&tail[..cd_n], &layout);
    let homemade_ok = match &homemade {
        Some(h) if h.len() == recs.len() => true,
        Some(_) => {
            return Err(slice_fail(path, "central directory entry count mismatch"));
        }
        None => false,
    };

    let mut locals = Vec::with_capacity(recs.len());
    for rec in &recs {
        let next = recs
            .iter()
            .map(|r| r.zip_rel_offset)
            .filter(|&off| off > rec.zip_rel_offset)
            .min()
            .unwrap_or(zip_rel_cd);
        if next <= rec.zip_rel_offset {
            return Err(slice_fail(
                path,
                "local header offset at or past central directory",
            ));
        }
        locals.push(read_local(path, file, &layout, rec, next)?);
    }
    Ok(ExactSlice {
        locals,
        tail: if homemade_ok { Some(tail) } else { None },
        homemade_ok,
        leading_pad,
        prefix,
    })
}

fn archive_local_records<R: Read + Seek>(
    path: &Path,
    layout: &ZipLayout,
    file: R,
) -> Result<Vec<CdRecord>> {
    let reader = BufReader::new(ZipView::new(file, layout.view_shift));
    let mut archive = ZipArchive::new(reader).map_err(|err| archive_open_err(err, path))?;
    let mut recs = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let zf = archive
            .by_index(i)
            .map_err(|err| archive_entry_err(err, path))?;
        if zf.encrypted() {
            return Err(AyzenpackError::Encrypted {
                path: path.to_path_buf(),
            });
        }
        let zip_rel_offset = cd_offset_to_zip_rel(zf.header_start(), layout)
            .ok_or_else(|| slice_fail(path, "local header offset underflow after prefix"))?;
        recs.push(CdRecord {
            crc: zf.crc32(),
            compressed_size: zf.compressed_size(),
            uncompressed_size: zf.size(),
            zip_rel_offset,
        });
    }
    Ok(recs)
}

/// Homemade CD record count vs `ZipArchive::len()` on the **source** file.
/// `None` homemade means parse failed. Used as the second-bug gate (not tautological).
#[cfg(test)]
pub(crate) fn homemade_cd_count_and_archive_len(path: &Path) -> Result<(Option<usize>, usize)> {
    let (layout, archive_len) = {
        let mut file = File::open(path).map_err(|source| io_at(source, path))?;
        let layout = detect_zip_layout(path, &mut file)?;
        file.seek(SeekFrom::Start(0))
            .map_err(|source| io_at(source, path))?;
        let archive = ZipArchive::new(BufReader::new(ZipView::new(file, layout.view_shift)))
            .map_err(|err| archive_open_err(err, path))?;
        (layout, archive.len())
    };
    let mut file = File::open(path).map_err(|source| io_at(source, path))?;
    let file_len = file
        .seek(SeekFrom::End(0))
        .map_err(|source| io_at(source, path))?;
    let (cd_struct_off, cd_size, _, _) = find_cd_bounds(path, &mut file, file_len)?;
    let Some(phys_cd) = cd_struct_off.checked_sub(cd_size) else {
        return Ok((None, archive_len));
    };
    if phys_cd < layout.prefix_len || file_len < phys_cd {
        return Ok((None, archive_len));
    }
    let mut tail = vec![0u8; usize_from_u64(file_len - phys_cd, path)?];
    file.seek(SeekFrom::Start(phys_cd))
        .map_err(|source| io_at(source, path))?;
    file.read_exact(&mut tail)
        .map_err(|source| io_at(source, path))?;
    let Ok(cd_n) = usize::try_from(cd_size) else {
        return Ok((None, archive_len));
    };
    if tail.len() < cd_n {
        return Ok((None, archive_len));
    }
    Ok((
        parse_central_directory(&tail[..cd_n], &layout).map(|r| r.len()),
        archive_len,
    ))
}

fn archive_open_err(err: zip::result::ZipError, path: &Path) -> AyzenpackError {
    match err {
        zip::result::ZipError::Io(source) => io_at(source, path),
        zip::result::ZipError::UnsupportedArchive(msg)
            if msg == zip::result::ZipError::PASSWORD_REQUIRED =>
        {
            AyzenpackError::Encrypted {
                path: path.to_path_buf(),
            }
        }
        _ => slice_fail(path, "zip archive open failed"),
    }
}

fn archive_entry_err(err: zip::result::ZipError, path: &Path) -> AyzenpackError {
    match err {
        zip::result::ZipError::Io(source) => io_at(source, path),
        zip::result::ZipError::UnsupportedArchive(msg)
            if msg == zip::result::ZipError::PASSWORD_REQUIRED =>
        {
            AyzenpackError::Encrypted {
                path: path.to_path_buf(),
            }
        }
        _ => slice_fail(path, "zip archive entry failed"),
    }
}

#[cfg(test)]
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

#[cfg(test)]
fn slice_zip(path: &Path) -> Result<ExactSlice> {
    slice_zip_using(path, find_cd_bounds)
}

#[cfg(test)]
fn slice_zip_v0_2_1(path: &Path) -> Result<ExactSlice> {
    slice_zip_using(path, find_cd_bounds_v0_2_1)
}

#[cfg(test)]
fn slice_zip_using(
    path: &Path,
    find_bounds: impl Fn(&Path, &mut File, u64) -> Result<(u64, u64, u64, u64)>,
) -> Result<ExactSlice> {
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
        find_bounds(path, &mut file, file_len)?;
    let phys_cd = cd_struct_off
        .checked_sub(cd_size)
        .ok_or_else(|| slice_fail(path, "central directory offset underflow"))?;
    if phys_cd < layout.prefix_len {
        return Err(slice_fail(path, "central directory starts inside prefix"));
    }
    let zip_rel_cd = phys_cd - layout.prefix_len;

    // Read CD+EOCD once as `tail`. Do not keep a second copy of the CD in RAM.
    let mut tail = vec![0u8; usize_from_u64(file_len - phys_cd, path)?];
    file.seek(SeekFrom::Start(phys_cd))
        .map_err(|source| io_at(source, path))?;
    file.read_exact(&mut tail)
        .map_err(|source| io_at(source, path))?;
    let cd_n = usize_from_u64(cd_size, path)?;
    if tail.len() < cd_n {
        return Err(slice_fail(path, "central directory truncated in tail"));
    }
    let records = parse_central_directory(&tail[..cd_n], &layout)
        .ok_or_else(|| slice_fail(path, "central directory parse failed"))?;
    if u64::try_from(records.len()).unwrap_or(u64::MAX) != entry_count {
        return Err(slice_fail(path, "central directory entry count mismatch"));
    }

    if records.is_empty() {
        return Ok(ExactSlice {
            locals: Vec::new(),
            tail: Some(tail),
            homemade_ok: true,
            leading_pad: Vec::new(),
            prefix: Vec::new(),
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

    Ok(ExactSlice {
        locals,
        tail: Some(tail),
        homemade_ok: true,
        leading_pad: Vec::new(),
        prefix: Vec::new(),
    })
}

#[cfg(test)]
fn check_not_spanned(path: &Path, file: &mut File, file_len: u64) -> Result<()> {
    let (eocd_off, _cd_size, _cd_off, _entries) = crate::scan::find_eocd(path, file, file_len)?;
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

/// Complete `PK\x01\x02` rows only. Trailing leftover after at least one row is
/// `Some` (junk lives in the phys CD→EOF tail). A truncated/malformed record
/// is `None` even if earlier rows looked fine. Empty `cd` is `Some([])`.
pub(crate) fn parse_central_directory(cd: &[u8], layout: &ZipLayout) -> Option<Vec<CdRecord>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < cd.len() {
        if i + 46 > cd.len() || cd[i..i + 4] != CD_MAGIC {
            break;
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
    if out.is_empty() && i != cd.len() {
        return None;
    }
    Some(out)
}

pub(crate) fn cd_offset_to_zip_rel(recorded: u64, layout: &ZipLayout) -> Option<u64> {
    if layout.view_shift == 0 && layout.prefix_len > 0 {
        recorded.checked_sub(layout.prefix_len)
    } else {
        Some(recorded)
    }
}

pub(crate) fn resolve_cd_zip64(
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

pub(crate) fn find_extra(extra: &[u8], id: u16) -> Option<&[u8]> {
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

pub(crate) fn read_local(
    path: &Path,
    file: &mut (impl Read + Seek),
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

pub(crate) fn split_descriptor(
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

/// First CD record's resolved local-header offset (ZIP- or file-relative as stored).
pub(crate) fn first_cd_local_offset(tail: &[u8]) -> Option<u64> {
    if tail.len() < 46 || tail[..4] != CD_MAGIC {
        return None;
    }
    let uncomp32 = u32::from_le_bytes(tail[24..28].try_into().ok()?);
    let comp32 = u32::from_le_bytes(tail[20..24].try_into().ok()?);
    let off32 = u32::from_le_bytes(tail[42..46].try_into().ok()?);
    let disk16 = u16::from_le_bytes(tail[34..36].try_into().ok()?);
    let name_len = u16::from_le_bytes(tail[28..30].try_into().ok()?) as usize;
    let extra_len = u16::from_le_bytes(tail[30..32].try_into().ok()?) as usize;
    if 46 + name_len + extra_len > tail.len() {
        return None;
    }
    let extra = &tail[46 + name_len..46 + name_len + extra_len];
    let (_u, _c, off, _d) = resolve_cd_zip64(extra, uncomp32, comp32, off32, disk16)?;
    Some(off)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OffsetMode {
    ZipRel,
    FileAbs,
}

pub(crate) fn detect_offset_mode(
    tail: &[u8],
    first_local: u64,
    prefix_len: u64,
    jar_name: &str,
) -> Result<OffsetMode> {
    let cd_off = first_cd_local_offset(tail).ok_or_else(|| {
        AyzenpackError::FormatOwned(format!("{jar_name}: cannot read first CD local offset"))
    })?;
    if cd_off == first_local {
        Ok(OffsetMode::ZipRel)
    } else if prefix_len > 0 && cd_off == prefix_len + first_local {
        Ok(OffsetMode::FileAbs)
    } else {
        Err(AyzenpackError::FormatOwned(format!(
            "{jar_name}: CD local offset {cd_off} matches neither zip-rel {first_local} nor file-abs {}",
            prefix_len + first_local
        )))
    }
}

pub(crate) fn encode_offset(mode: OffsetMode, zip_rel: u64, prefix_len: u64) -> u64 {
    match mode {
        OffsetMode::ZipRel => zip_rel,
        OffsetMode::FileAbs => prefix_len + zip_rel,
    }
}

/// Rebuild patch for one CD / local record. Beyond `(zip_rel, csize)`: method, crc, usize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RebuildPatch {
    pub zip_rel: u64,
    pub method: u16,
    pub crc: u32,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
}

/// Patch method, crc, sizes, and local offset on each CD record. Returns CD byte length.
pub(crate) fn patch_central_directory(
    tail: &mut [u8],
    updates: &[RebuildPatch],
    mode: OffsetMode,
    prefix_len: u64,
    jar_name: &str,
) -> Result<usize> {
    let mut i = 0usize;
    for (idx, update) in updates.iter().enumerate() {
        if i + 46 > tail.len() || tail[i..i + 4] != CD_MAGIC {
            return Err(AyzenpackError::FormatOwned(format!(
                "{jar_name}: CD record {idx} missing"
            )));
        }
        let name_len = u16::from_le_bytes([tail[i + 28], tail[i + 29]]) as usize;
        let extra_len = u16::from_le_bytes([tail[i + 30], tail[i + 31]]) as usize;
        let comment_len = u16::from_le_bytes([tail[i + 32], tail[i + 33]]) as usize;
        let rec_end = i
            .checked_add(46)
            .and_then(|n| n.checked_add(name_len))
            .and_then(|n| n.checked_add(extra_len))
            .and_then(|n| n.checked_add(comment_len))
            .ok_or_else(|| AyzenpackError::FormatOwned(format!("{jar_name}: CD overflow")))?;
        if rec_end > tail.len() {
            return Err(AyzenpackError::FormatOwned(format!(
                "{jar_name}: CD record {idx} truncated"
            )));
        }
        let extra_start = 46 + name_len;
        let rec = &mut tail[i..rec_end];
        rec[10..12].copy_from_slice(&update.method.to_le_bytes());
        rec[16..20].copy_from_slice(&update.crc.to_le_bytes());
        let comp32 = u32::from_le_bytes(rec[20..24].try_into().unwrap());
        let uncomp32 = u32::from_le_bytes(rec[24..28].try_into().unwrap());
        let off32 = u32::from_le_bytes(rec[42..46].try_into().unwrap());
        let new_off = encode_offset(mode, update.zip_rel, prefix_len);
        let (head, rest) = rec.split_at_mut(extra_start);
        let extra = &mut rest[..extra_len];
        patch_size32_or_zip64(
            &mut head[20..24],
            extra,
            uncomp32,
            comp32,
            update.compressed_size,
            SizeSlot::Compressed,
            jar_name,
        )?;
        patch_size32_or_zip64(
            &mut head[24..28],
            extra,
            uncomp32,
            comp32,
            update.uncompressed_size,
            SizeSlot::Uncompressed,
            jar_name,
        )?;
        patch_offset32_or_zip64(
            &mut head[42..46],
            extra,
            uncomp32,
            comp32,
            off32,
            new_off,
            jar_name,
        )?;
        i = rec_end;
    }
    Ok(i)
}

enum SizeSlot {
    Compressed,
    Uncompressed,
}

fn patch_size32_or_zip64(
    field32: &mut [u8],
    extra: &mut [u8],
    uncomp32: u32,
    comp32: u32,
    new_val: u64,
    slot: SizeSlot,
    jar_name: &str,
) -> Result<()> {
    let use_zip64 = match slot {
        SizeSlot::Compressed => comp32 == u32::MAX,
        SizeSlot::Uncompressed => uncomp32 == u32::MAX,
    };
    if use_zip64 {
        match slot {
            SizeSlot::Compressed => {
                patch_zip64_u64(extra, uncomp32 == u32::MAX, true, false, new_val, jar_name)?;
            }
            SizeSlot::Uncompressed => {
                patch_zip64_u64(extra, true, false, false, new_val, jar_name)?;
            }
        }
        return Ok(());
    }
    if new_val > u64::from(u32::MAX) {
        let which = match slot {
            SizeSlot::Compressed => "compressed",
            SizeSlot::Uncompressed => "uncompressed",
        };
        return Err(AyzenpackError::FormatOwned(format!(
            "{jar_name}: {which} size {new_val} needs Zip64 extra the source did not have"
        )));
    }
    field32.copy_from_slice(&(new_val as u32).to_le_bytes());
    Ok(())
}

fn patch_offset32_or_zip64(
    field32: &mut [u8],
    extra: &mut [u8],
    uncomp32: u32,
    comp32: u32,
    off32: u32,
    new_off: u64,
    jar_name: &str,
) -> Result<()> {
    if off32 == u32::MAX {
        patch_zip64_u64(
            extra,
            uncomp32 == u32::MAX,
            comp32 == u32::MAX,
            true,
            new_off,
            jar_name,
        )?;
        return Ok(());
    }
    if new_off > u64::from(u32::MAX) {
        return Err(AyzenpackError::FormatOwned(format!(
            "{jar_name}: local offset {new_off} needs Zip64 extra the source did not have"
        )));
    }
    field32.copy_from_slice(&(new_off as u32).to_le_bytes());
    Ok(())
}

/// Write `value` into the Zip64 extra slot selected by which 32-bit fields were sentinels.
fn patch_zip64_u64(
    extra: &mut [u8],
    has_uncomp: bool,
    has_comp: bool,
    has_off: bool,
    value: u64,
    jar_name: &str,
) -> Result<()> {
    let mut i = 0usize;
    while i + 4 <= extra.len() {
        let tag = u16::from_le_bytes(extra[i..i + 2].try_into().unwrap());
        let size = u16::from_le_bytes(extra[i + 2..i + 4].try_into().unwrap()) as usize;
        let start = i + 4;
        let end = start
            .checked_add(size)
            .ok_or_else(|| AyzenpackError::FormatOwned(format!("{jar_name}: Zip64 extra")))?;
        if end > extra.len() {
            return Err(AyzenpackError::FormatOwned(format!(
                "{jar_name}: truncated Zip64 extra"
            )));
        }
        if tag == ZIP64_EXTRA_ID {
            let mut off = 0usize;
            if has_uncomp && !has_comp && !has_off {
                if off + 8 > size {
                    return Err(AyzenpackError::FormatOwned(format!(
                        "{jar_name}: Zip64 extra missing uncompressed size"
                    )));
                }
                extra[start + off..start + off + 8].copy_from_slice(&value.to_le_bytes());
                return Ok(());
            }
            if has_uncomp {
                off += 8;
            }
            if has_comp && !has_off {
                if off + 8 > size {
                    return Err(AyzenpackError::FormatOwned(format!(
                        "{jar_name}: Zip64 extra missing compressed size"
                    )));
                }
                extra[start + off..start + off + 8].copy_from_slice(&value.to_le_bytes());
                return Ok(());
            }
            if has_comp {
                off += 8;
            }
            if has_off {
                if off + 8 > size {
                    return Err(AyzenpackError::FormatOwned(format!(
                        "{jar_name}: Zip64 extra missing offset"
                    )));
                }
                extra[start + off..start + off + 8].copy_from_slice(&value.to_le_bytes());
                return Ok(());
            }
            return Err(AyzenpackError::FormatOwned(format!(
                "{jar_name}: Zip64 extra has no matching slot"
            )));
        }
        i = end;
    }
    Err(AyzenpackError::FormatOwned(format!(
        "{jar_name}: missing Zip64 extra 0x0001"
    )))
}

/// Csize-only. GPBF bit 3 returns immediately — that early-return is csize-only.
pub(crate) fn patch_local_compressed_size(
    header: &mut [u8],
    new_csize: u64,
    jar_name: &str,
) -> Result<()> {
    if header.len() < 30 {
        return Err(AyzenpackError::FormatOwned(format!(
            "{jar_name}: local header too short"
        )));
    }
    let flags = u16::from_le_bytes([header[6], header[7]]);
    if flags & GPBF_DATA_DESC != 0 {
        return Ok(());
    }
    patch_local_size_fields(header, Some(new_csize), None, jar_name)
}

/// Class-4 / exotic-method rebuild: write method always, then crc/sizes unless bit 3.
/// Bit 3 still skips local csize/crc/uncomp (they live in the descriptor).
pub(crate) fn patch_local_rebuild_fields(
    header: &mut [u8],
    method: u16,
    crc: u32,
    new_csize: u64,
    new_uncomp: u64,
    jar_name: &str,
) -> Result<()> {
    if header.len() < 30 {
        return Err(AyzenpackError::FormatOwned(format!(
            "{jar_name}: local header too short"
        )));
    }
    header[8..10].copy_from_slice(&method.to_le_bytes());
    let flags = u16::from_le_bytes([header[6], header[7]]);
    if flags & GPBF_DATA_DESC != 0 {
        return Ok(());
    }
    header[14..18].copy_from_slice(&crc.to_le_bytes());
    patch_local_compressed_size(header, new_csize, jar_name)?;
    patch_local_size_fields(header, None, Some(new_uncomp), jar_name)
}

fn patch_local_size_fields(
    header: &mut [u8],
    new_csize: Option<u64>,
    new_uncomp: Option<u64>,
    jar_name: &str,
) -> Result<()> {
    let comp32 = u32::from_le_bytes(header[18..22].try_into().unwrap());
    let uncomp32 = u32::from_le_bytes(header[22..26].try_into().unwrap());
    let name_len = u16::from_le_bytes([header[26], header[27]]) as usize;
    let extra_len = u16::from_le_bytes([header[28], header[29]]) as usize;
    if 30 + name_len + extra_len != header.len() {
        return Err(AyzenpackError::FormatOwned(format!(
            "{jar_name}: local header extra length mismatch"
        )));
    }
    if let Some(new_csize) = new_csize {
        let mut field = [0u8; 4];
        field.copy_from_slice(&header[18..22]);
        {
            let extra = &mut header[30 + name_len..];
            patch_size32_or_zip64(
                &mut field,
                extra,
                uncomp32,
                comp32,
                new_csize,
                SizeSlot::Compressed,
                jar_name,
            )?;
        }
        header[18..22].copy_from_slice(&field);
    }
    if let Some(new_uncomp) = new_uncomp {
        let mut field = [0u8; 4];
        field.copy_from_slice(&header[22..26]);
        {
            let extra = &mut header[30 + name_len..];
            patch_size32_or_zip64(
                &mut field,
                extra,
                uncomp32,
                comp32,
                new_uncomp,
                SizeSlot::Uncompressed,
                jar_name,
            )?;
        }
        header[22..26].copy_from_slice(&field);
    }
    Ok(())
}

pub(crate) fn patch_data_descriptor(
    desc: &[u8],
    crc: u32,
    new_csize: u64,
    new_uncomp: u64,
    jar_name: &str,
) -> Result<Vec<u8>> {
    let mut d = desc.to_vec();
    let has_sig = d.starts_with(&DATA_DESC_MAGIC);
    let crc_off = if has_sig { 4 } else { 0 };
    let csize_off = crc_off + 4;
    d[crc_off..crc_off + 4].copy_from_slice(&crc.to_le_bytes());
    match d.len() {
        12 | 16 => {
            if new_csize > u64::from(u32::MAX) || new_uncomp > u64::from(u32::MAX) {
                return Err(AyzenpackError::FormatOwned(format!(
                    "{jar_name}: descriptor size does not fit u32"
                )));
            }
            d[csize_off..csize_off + 4].copy_from_slice(&(new_csize as u32).to_le_bytes());
            d[csize_off + 4..csize_off + 8].copy_from_slice(&(new_uncomp as u32).to_le_bytes());
        }
        20 | 24 => {
            d[csize_off..csize_off + 8].copy_from_slice(&new_csize.to_le_bytes());
            d[csize_off + 8..csize_off + 16].copy_from_slice(&new_uncomp.to_le_bytes());
        }
        _ => {
            return Err(AyzenpackError::FormatOwned(format!(
                "{jar_name}: unsupported data descriptor length {}",
                d.len()
            )));
        }
    }
    Ok(d)
}

/// Patch classic EOCD (and Zip64 EOCD + locator) CD start after locals change length.
pub(crate) fn patch_eocd_cd_start(
    tail: &mut [u8],
    cd_size: usize,
    new_cd_start_encoded: u64,
    jar_name: &str,
) -> Result<()> {
    let eocd = find_eocd_in(tail).ok_or_else(|| {
        AyzenpackError::FormatOwned(format!("{jar_name}: rebuild tail missing EOCD"))
    })?;
    let cd_off32 = u32::from_le_bytes(tail[eocd + 16..eocd + 20].try_into().unwrap());
    if cd_off32 != u32::MAX {
        if new_cd_start_encoded > u64::from(u32::MAX) {
            return Err(AyzenpackError::FormatOwned(format!(
                "{jar_name}: new CD offset does not fit classic EOCD"
            )));
        }
        tail[eocd + 16..eocd + 20].copy_from_slice(&(new_cd_start_encoded as u32).to_le_bytes());
    }
    if eocd >= 20 && tail[eocd - 20..eocd][..4] == ZIP64_LOCATOR_MAGIC {
        let loc = eocd - 20;
        let zip64_eocd_encoded = new_cd_start_encoded + cd_size as u64;
        tail[loc + 8..loc + 16].copy_from_slice(&zip64_eocd_encoded.to_le_bytes());
        // Zip64 EOCD immediately precedes the locator when the record size matches.
        if loc >= 56 && tail[loc - 56..loc - 52] == ZIP64_EOCD_MAGIC {
            let z64 = loc - 56;
            tail[z64 + 48..z64 + 56].copy_from_slice(&new_cd_start_encoded.to_le_bytes());
        } else {
            // Variable-length Zip64 EOCD: scan backward for magic whose record ends at loc.
            let mut found = false;
            let mut i = loc.saturating_sub(56);
            loop {
                if tail[i..i + 4] == ZIP64_EOCD_MAGIC {
                    let rec_size = u64::from_le_bytes(tail[i + 4..i + 12].try_into().unwrap());
                    let rec_len = 12u64.saturating_add(rec_size);
                    if i as u64 + rec_len == loc as u64 {
                        tail[i + 48..i + 56].copy_from_slice(&new_cd_start_encoded.to_le_bytes());
                        found = true;
                        break;
                    }
                }
                if i == 0 {
                    break;
                }
                i -= 1;
            }
            if !found {
                return Err(AyzenpackError::FormatOwned(format!(
                    "{jar_name}: Zip64 locator without Zip64 EOCD"
                )));
            }
        }
    }
    Ok(())
}

fn find_eocd_in(buf: &[u8]) -> Option<usize> {
    if buf.len() < 22 {
        return None;
    }
    let mut i = buf.len() - 22;
    loop {
        if buf[i..i + 4] == EOCD_MAGIC {
            let comment_len = u16::from_le_bytes([buf[i + 20], buf[i + 21]]) as usize;
            if i + 22 + comment_len == buf.len() {
                return Some(i);
            }
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use std::path::Path;
    use zip::write::SimpleFileOptions;
    use zip::{ZipArchive, ZipWriter};

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
                out.extend_from_slice(slice.tail.as_ref().expect("sliced tail"));
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
    fn patch_local_rebuild_writes_method_when_gpbf_bit3() {
        let mut header = vec![0u8; 30];
        header[0..4].copy_from_slice(&LOCAL_FILE_MAGIC);
        header[6..8].copy_from_slice(&GPBF_DATA_DESC.to_le_bytes());
        header[8..10].copy_from_slice(&8u16.to_le_bytes());
        header[14..18].copy_from_slice(&0x1111_1111u32.to_le_bytes());
        header[18..22].copy_from_slice(&4u32.to_le_bytes());
        header[22..26].copy_from_slice(&4u32.to_le_bytes());
        patch_local_rebuild_fields(&mut header, 0, 0, 0, 0, "bit3.jar").unwrap();
        assert_eq!(u16::from_le_bytes([header[8], header[9]]), 0);
        assert_eq!(
            u32::from_le_bytes(header[14..18].try_into().unwrap()),
            0x1111_1111,
            "bit 3 must not write crc in the local header"
        );
        assert_eq!(
            u32::from_le_bytes(header[18..22].try_into().unwrap()),
            4,
            "bit 3 early-return stays csize-only"
        );
        assert_eq!(
            u32::from_le_bytes(header[22..26].try_into().unwrap()),
            4,
            "bit 3 must not write uncomp in the local header"
        );
    }

    #[test]
    fn patch_data_descriptor_writes_crc_csize_uncomp() {
        let mut desc = vec![0u8; 16];
        desc[0..4].copy_from_slice(&DATA_DESC_MAGIC);
        desc[4..8].copy_from_slice(&0xaaaaaaaau32.to_le_bytes());
        desc[8..12].copy_from_slice(&4u32.to_le_bytes());
        desc[12..16].copy_from_slice(&4u32.to_le_bytes());
        let out = patch_data_descriptor(&desc, 0, 0, 0, "bit3.jar").unwrap();
        assert_eq!(&out[4..8], &0u32.to_le_bytes());
        assert_eq!(&out[8..12], &0u32.to_le_bytes());
        assert_eq!(&out[12..16], &0u32.to_le_bytes());
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
                assert_eq!(slice.tail.as_deref(), Some(zip.as_slice()));
            }
            ZipExact::Raw(_) => panic!("empty zip must slice as tail"),
        }
        let (home, arch) = homemade_cd_count_and_archive_len(&path).unwrap();
        assert_eq!(home, Some(0));
        assert_eq!(arch, 0);
        let sliced = slice_from_archive(&path).unwrap();
        assert!(sliced.locals.is_empty());
        assert!(!sliced.tail.as_ref().unwrap().is_empty());
    }

    fn write_stored_named(path: &Path, files: &[(&str, &[u8])]) {
        let mut local = Vec::new();
        let mut central = Vec::new();
        for (name, data) in files {
            let name_b = name.as_bytes();
            let crc = crc32fast::hash(data);
            let off = local.len() as u32;
            local.extend_from_slice(b"PK\x03\x04");
            local.extend_from_slice(&20u16.to_le_bytes());
            local.extend_from_slice(&0u16.to_le_bytes());
            local.extend_from_slice(&0u16.to_le_bytes());
            local.extend_from_slice(&0u16.to_le_bytes());
            local.extend_from_slice(&0u16.to_le_bytes());
            local.extend_from_slice(&crc.to_le_bytes());
            local.extend_from_slice(&(data.len() as u32).to_le_bytes());
            local.extend_from_slice(&(data.len() as u32).to_le_bytes());
            local.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
            local.extend_from_slice(&0u16.to_le_bytes());
            local.extend_from_slice(name_b);
            local.extend_from_slice(data);
            central.extend_from_slice(b"PK\x01\x02");
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&off.to_le_bytes());
            central.extend_from_slice(name_b);
        }
        let cd_off = local.len() as u32;
        let cd_len = central.len() as u32;
        let n = files.len() as u16;
        local.extend_from_slice(&central);
        local.extend_from_slice(b"PK\x05\x06");
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(&n.to_le_bytes());
        local.extend_from_slice(&n.to_le_bytes());
        local.extend_from_slice(&cd_len.to_le_bytes());
        local.extend_from_slice(&cd_off.to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        std::fs::write(path, local).unwrap();
    }

    #[test]
    fn zip64_large_file_homemade_cd_matches_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("z64.jar");
        let mut z = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        z.set_zip64_comment(Some(""));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true);
        z.start_file("a.txt", opts).unwrap();
        z.write_all(b"hello").unwrap();
        z.start_file("b.txt", opts).unwrap();
        z.write_all(b"world").unwrap();
        let zip = z.finish().unwrap().into_inner();
        let mut out = b"#!/bin/bash\n# stub\n".to_vec();
        out.extend_from_slice(&zip);
        std::fs::write(&path, &out).unwrap();
        let (home, arch) = homemade_cd_count_and_archive_len(&path).unwrap();
        assert_eq!(home, Some(2));
        assert_eq!(arch, 2);
        let sliced = slice_from_archive(&path).expect("Zip64 fat must slice from archive");
        assert_eq!(sliced.locals.len(), 2);
    }

    #[test]
    fn homemade_cd_matches_archive_on_plain_zipwriter() {
        let (_dir, path) = write_temp_zip(&[("a.txt", b"hello"), ("b.txt", b"world")], false);
        let (home, arch) = homemade_cd_count_and_archive_len(&path).unwrap();
        assert_eq!(home, Some(2));
        assert_eq!(arch, 2);
        let sliced = slice_from_archive(&path).unwrap();
        assert_eq!(sliced.locals.len(), 2);
    }

    #[test]
    fn dup_name_homemade_cd_is_two_archive_is_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dup.jar");
        write_stored_named(
            &path,
            &[
                ("dup.txt", b"first-payload"),
                ("dup.txt", b"second-payload"),
            ],
        );
        let (home, arch) = homemade_cd_count_and_archive_len(&path).unwrap();
        assert_eq!(home, Some(2), "homemade CD must see both records");
        assert_eq!(arch, 1, "ZipArchive IndexMap last-wins");
        assert!(
            slice_from_archive(&path).is_err(),
            "count mismatch must skip exact, not raw_zip"
        );
        match capture_zip_exact(&path).unwrap() {
            ZipExact::Sliced(s) => assert_eq!(s.locals.len(), 2),
            ZipExact::Raw(_) => panic!("dup names must homemade-slice; 0.2.1 used Sliced mismatch"),
        }
    }

    #[test]
    fn overlapping_locals_are_raw_on_homemade_slice_not_archive_slice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overlap.jar");
        write_stored_named(
            &path,
            &[("a.txt", b"SAME-payload"), ("b.txt", b"SAME-payload")],
        );
        let mut buf = std::fs::read(&path).unwrap();
        let eocd = find_eocd_in(&buf).unwrap();
        let cd_off = u32::from_le_bytes(buf[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
        let name_len =
            u16::from_le_bytes(buf[cd_off + 28..cd_off + 30].try_into().unwrap()) as usize;
        let rec2 = cd_off + 46 + name_len;
        buf[rec2 + 42..rec2 + 46].copy_from_slice(&0u32.to_le_bytes());
        std::fs::write(&path, &buf).unwrap();

        match capture_zip_exact(&path).unwrap() {
            ZipExact::Raw(_) => {}
            ZipExact::Sliced(_) => panic!("overlapping locals must be Raw on homemade slice_zip"),
        }
        assert!(
            slice_from_archive(&path).is_err(),
            "overlap must skip exact, not become Raw for dehydrate"
        );
        let (home, arch) = homemade_cd_count_and_archive_len(&path).unwrap();
        assert_eq!(home, Some(2));
        assert_eq!(arch, 2);
    }

    #[test]
    fn zip64_prefix_zipa_is_raw_on_v0_2_1_not_on_archive_slice() {
        // rust zip large_file + prefix + zip -A: classic EOCD is not sentinels,
        // 0.2.1 find_cd_bounds used eocd-cd_size (Zip64 footer) → Raw. Listed.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fat.jar");
        let mut z = ZipWriter::new(Cursor::new(Vec::new()));
        z.set_zip64_comment(Some(""));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(true);
        z.start_file("BOOT-INF/lib/a.jar", opts).unwrap();
        z.write_all(&[0xA5; 4096]).unwrap();
        z.start_file("BOOT-INF/lib/b.jar", opts).unwrap();
        z.write_all(&[0x5A; 4096]).unwrap();
        let zip = z.finish().unwrap().into_inner();
        let launcher = b"#!/bin/bash\n# :: Spring Boot ::\n# launcher\n";
        let mut wrapped = launcher.to_vec();
        wrapped.extend_from_slice(&zip);
        // zip -A: bump classic CD local offsets + EOCD CD offset.
        let eocd = find_eocd_in(&wrapped).unwrap();
        let cd_size = u32::from_le_bytes(wrapped[eocd + 12..eocd + 16].try_into().unwrap());
        let cd_off = u32::from_le_bytes(wrapped[eocd + 16..eocd + 20].try_into().unwrap());
        let delta = launcher.len() as u32;
        let phys_cd = cd_off as usize + delta as usize;
        let mut i = phys_cd;
        let cd_end = phys_cd + cd_size as usize;
        while i + 46 <= cd_end {
            let nl = u16::from_le_bytes(wrapped[i + 28..i + 30].try_into().unwrap()) as usize;
            let el = u16::from_le_bytes(wrapped[i + 30..i + 32].try_into().unwrap()) as usize;
            let cl = u16::from_le_bytes(wrapped[i + 32..i + 34].try_into().unwrap()) as usize;
            let old = u32::from_le_bytes(wrapped[i + 42..i + 46].try_into().unwrap());
            wrapped[i + 42..i + 46].copy_from_slice(&(old + delta).to_le_bytes());
            i += 46 + nl + el + cl;
        }
        wrapped[eocd + 16..eocd + 20].copy_from_slice(&(cd_off + delta).to_le_bytes());
        std::fs::write(&path, &wrapped).unwrap();

        let listed = ZipArchive::new(std::fs::File::open(&path).unwrap())
            .unwrap()
            .len();
        assert!(listed >= 2, "fixture must be listable, got {listed}");
        match capture_zip_exact_v0_2_1(&path).unwrap() {
            ZipExact::Raw(zip) => {
                assert!(
                    zip.len() > 8000,
                    "0.2.1 Raw must be the zip portion, got {}",
                    zip.len()
                );
            }
            ZipExact::Sliced(s) => panic!(
                "0.2.1 must Raw this Zip64+prefix+zip-A jar, sliced {} locals",
                s.locals.len()
            ),
        }
        let sliced = slice_from_archive(&path).expect("0.2.2 archive slice must succeed");
        assert_eq!(sliced.locals.len(), listed);
        assert!(
            capture_zip_exact(&path)
                .ok()
                .is_some_and(|z| matches!(z, ZipExact::Sliced(_))),
            "current homemade walker (fixed bounds) must slice"
        );
    }

    fn zip_rel_layout() -> ZipLayout {
        ZipLayout {
            prefix_len: 0,
            view_shift: 0,
        }
    }

    fn magic_but_short_cd_header() -> [u8; 46] {
        let mut stub = [0u8; 46];
        stub[..4].copy_from_slice(&CD_MAGIC);
        stub[28..30].copy_from_slice(&100u16.to_le_bytes());
        stub
    }

    fn splice_cd_trailing_junk(buf: &mut Vec<u8>, junk: &[u8]) {
        let eocd = find_eocd_in(buf).expect("EOCD");
        let cd_size = u32::from_le_bytes(buf[eocd + 12..eocd + 16].try_into().unwrap());
        buf.splice(eocd..eocd, junk.iter().copied());
        let new_eocd = eocd + junk.len();
        buf[new_eocd + 12..new_eocd + 16]
            .copy_from_slice(&(cd_size + junk.len() as u32).to_le_bytes());
    }

    fn stored_cd_bytes(path: &Path) -> Vec<u8> {
        let buf = std::fs::read(path).unwrap();
        let eocd = find_eocd_in(&buf).expect("EOCD");
        let cd_size = u32::from_le_bytes(buf[eocd + 12..eocd + 16].try_into().unwrap()) as usize;
        let cd_off = u32::from_le_bytes(buf[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
        buf[cd_off..cd_off + cd_size].to_vec()
    }

    #[test]
    fn parse_central_directory_leftover_vs_truncated() {
        let layout = zip_rel_layout();
        assert_eq!(
            parse_central_directory(&[], &layout).map(|r| r.len()),
            Some(0),
            "empty cd is Some([])"
        );
        assert!(
            parse_central_directory(&[0xAB, 0xCD, 0xEF, 0x01], &layout).is_none(),
            "junk-only is None"
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jar");
        write_stored_named(&path, &[("a.txt", b"hello")]);
        let cd = stored_cd_bytes(&path);
        assert_eq!(
            parse_central_directory(&cd, &layout).map(|r| r.len()),
            Some(1)
        );

        let mut junked = cd.clone();
        junked.extend_from_slice(&[0xAB, 0xCD, 0xEF, 0x01]);
        assert_eq!(
            parse_central_directory(&junked, &layout).map(|r| r.len()),
            Some(1),
            "complete rows + non-magic junk is Some(N)"
        );

        let mut short = cd;
        short.extend_from_slice(&magic_but_short_cd_header());
        assert!(
            parse_central_directory(&short, &layout).is_none(),
            "complete rows + magic-but-short record is None"
        );
    }

    #[test]
    fn leftover_junk_cd_homemade_ok_matches_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("leftover-junk.jar");
        write_stored_named(&path, &[("a.txt", b"hello")]);
        let mut buf = std::fs::read(&path).unwrap();
        splice_cd_trailing_junk(&mut buf, &[0xAB, 0xCD, 0xEF, 0x01]);
        std::fs::write(&path, &buf).unwrap();

        let listed = ZipArchive::new(std::fs::File::open(&path).unwrap())
            .unwrap()
            .len();
        assert_eq!(listed, 1, "fixture must stay listable");
        let (home, arch) = homemade_cd_count_and_archive_len(&path).unwrap();
        assert_eq!(home, Some(1), "N complete CD rows + leftover junk is Some");
        assert_eq!(arch, 1);
        let sliced = slice_from_archive(&path).expect("leftover-junk must slice");
        assert!(sliced.homemade_ok);
        assert!(
            sliced.tail.is_some(),
            "homemade_ok must attach phys CD→EOF tail"
        );
        assert_eq!(sliced.locals.len(), 1);
    }

    #[test]
    fn truncated_cd_homemade_none_has_no_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("truncated-cd.jar");
        write_stored_named(&path, &[("a.txt", b"hello")]);
        let mut buf = std::fs::read(&path).unwrap();
        splice_cd_trailing_junk(&mut buf, &magic_but_short_cd_header());
        std::fs::write(&path, &buf).unwrap();

        let listed = ZipArchive::new(std::fs::File::open(&path).unwrap())
            .unwrap()
            .len();
        assert_eq!(listed, 1, "fixture must stay listable");
        let (home, arch) = homemade_cd_count_and_archive_len(&path).unwrap();
        assert_eq!(
            home, None,
            "magic-but-short CD record must stay homemade None"
        );
        assert_eq!(arch, 1);
        let sliced = slice_from_archive(&path).expect("listed truncated CD is skip-exact, not Err");
        assert!(!sliced.homemade_ok);
        assert!(
            sliced.tail.is_none(),
            "homemade None must never attach tail"
        );
        assert_eq!(sliced.locals.len(), 1);
    }

    fn zip64_eocd_off_ending_at_locator(buf: &[u8], loc: usize) -> usize {
        if loc >= 56 && buf[loc - 56..loc - 52] == ZIP64_EOCD_MAGIC {
            let rec_size = u64::from_le_bytes(buf[loc - 52..loc - 44].try_into().unwrap());
            if 12u64.saturating_add(rec_size) == 56 {
                return loc - 56;
            }
        }
        let mut i = loc.saturating_sub(56);
        loop {
            if buf[i..i + 4] == ZIP64_EOCD_MAGIC {
                let rec_size = u64::from_le_bytes(buf[i + 4..i + 12].try_into().unwrap());
                let rec_len = 12u64.saturating_add(rec_size);
                if i as u64 + rec_len == loc as u64 {
                    return i;
                }
            }
            assert!(i > 0, "Zip64 EOCD must precede locator");
            i -= 1;
        }
    }

    /// Stub before Zip64 EOCD (not classic EOCD). Classic splice would leave
    /// Zip64 `cd_size` covering only N complete rows → homemade Some + tail.
    fn splice_truncated_cd_before_zip64_eocd(buf: &mut Vec<u8>) {
        let eocd = find_eocd_in(buf).expect("EOCD");
        assert!(eocd >= 20, "Zip64 locator + classic EOCD");
        let loc = eocd - 20;
        assert_eq!(&buf[loc..loc + 4], ZIP64_LOCATOR_MAGIC);
        let z64 = zip64_eocd_off_ending_at_locator(buf, loc);
        let stub = magic_but_short_cd_header();
        let stub_len = stub.len();
        buf.splice(z64..z64, stub.iter().copied());
        let new_z64 = z64 + stub_len;
        let new_loc = loc + stub_len;
        let new_eocd = eocd + stub_len;
        let z64_cd_size = u64::from_le_bytes(buf[new_z64 + 40..new_z64 + 48].try_into().unwrap());
        buf[new_z64 + 40..new_z64 + 48]
            .copy_from_slice(&(z64_cd_size + stub_len as u64).to_le_bytes());
        let loc_z64_off = u64::from_le_bytes(buf[new_loc + 8..new_loc + 16].try_into().unwrap());
        buf[new_loc + 8..new_loc + 16]
            .copy_from_slice(&(loc_z64_off + stub_len as u64).to_le_bytes());
        let classic_cd_size =
            u32::from_le_bytes(buf[new_eocd + 12..new_eocd + 16].try_into().unwrap());
        if classic_cd_size != u32::MAX {
            buf[new_eocd + 12..new_eocd + 16]
                .copy_from_slice(&(classic_cd_size + stub_len as u32).to_le_bytes());
        }
    }

    #[test]
    fn truncated_cd_zip64_homemade_none_has_no_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("truncated-cd-zip64.jar");
        let mut z = ZipWriter::new(Cursor::new(Vec::new()));
        z.set_zip64_comment(Some(""));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(true);
        z.start_file("a.txt", opts).unwrap();
        z.write_all(b"hello").unwrap();
        z.start_file("b.txt", opts).unwrap();
        z.write_all(b"world").unwrap();
        let mut buf = z.finish().unwrap().into_inner();
        splice_truncated_cd_before_zip64_eocd(&mut buf);
        std::fs::write(&path, &buf).unwrap();

        let listed = ZipArchive::new(std::fs::File::open(&path).unwrap())
            .unwrap()
            .len();
        assert_eq!(listed, 2, "fixture must stay listable");
        let (home, arch) = homemade_cd_count_and_archive_len(&path).unwrap();
        assert_eq!(
            home, None,
            "Zip64-aware truncated stub must stay homemade None"
        );
        assert_eq!(arch, 2);
        let sliced = slice_from_archive(&path).expect("listed truncated Zip64 CD is skip-exact");
        assert!(!sliced.homemade_ok);
        assert!(
            sliced.tail.is_none(),
            "homemade None must never attach tail"
        );
        assert_eq!(sliced.locals.len(), 2);
    }
}
