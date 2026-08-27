//! Synthetic central directory (APPNOTE CD + Zip64 extra/EOCD/locator).
//!
//! Encoder writes caller-supplied `cd_start` and `local_offset` verbatim.
//! Zip64 extra is synthesized; local extra is never copied.

use crate::error::{AyzenpackError, Result};
use crate::exact::{
    CD_MAGIC, EOCD_MAGIC, LOCAL_FILE_MAGIC, ZIP64_EOCD_MAGIC, ZIP64_EXTRA_ID, ZIP64_LOCATOR_MAGIC,
};

const ZIP64_U32: u64 = u32::MAX as u64;
const VER_MADE_DOS: u16 = 0x0014;
const VER_MADE_UNIX: u16 = 0x031E;
const VER_NEED_ZIP: u16 = 20;
const VER_NEED_ZIP64: u16 = 45;
const VER_MADE_ZIP64_EOCD: u16 = 0x002D;
const ZIP64_EOCD_REMAINING: u64 = 44;

#[derive(Debug)]
pub(crate) struct SyntheticCd {
    pub bytes: Vec<u8>,
    pub cd_size: u64,
}

#[derive(Debug)]
pub(crate) struct SyntheticCdEntry {
    pub name: Vec<u8>,
    pub method: u16,
    pub gpbf: u16,
    pub dos_time: u16,
    pub dos_date: u16,
    pub crc: u32,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    pub local_offset: u64,
    pub unix_mode: Option<u32>,
}

pub(crate) fn write_synthetic_cd(
    entries: &[SyntheticCdEntry],
    cd_start: u64,
    eocd_comment: &[u8],
) -> Result<SyntheticCd> {
    if eocd_comment.len() > u16::MAX as usize {
        return Err(AyzenpackError::Format(
            "synthetic CD: EOCD comment exceeds 65535",
        ));
    }

    let mut bytes = Vec::new();
    let mut any_record_zip64 = false;
    for e in entries {
        if name_has_dotdot(&e.name) {
            return Err(AyzenpackError::Format("synthetic CD: name contains .."));
        }
        let extra = zip64_extra(e.uncompressed_size, e.compressed_size, e.local_offset);
        if !extra.is_empty() {
            any_record_zip64 = true;
        }
        write_cd_record(&mut bytes, e, &extra)?;
    }

    let cd_size = bytes.len() as u64;
    let need_zip64_eocd = (entries.len() as u64) > u64::from(u16::MAX)
        || needs_u32_zip64(cd_size)
        || needs_u32_zip64(cd_start)
        || any_record_zip64;

    if need_zip64_eocd {
        let zip64_eocd_off = cd_start.checked_add(cd_size).ok_or(AyzenpackError::Format(
            "synthetic CD: Zip64 EOCD offset overflow",
        ))?;
        write_zip64_eocd(&mut bytes, entries.len() as u64, cd_size, cd_start);
        write_zip64_locator(&mut bytes, zip64_eocd_off);
        write_classic_eocd(
            &mut bytes,
            true,
            entries.len() as u64,
            cd_size,
            cd_start,
            eocd_comment,
        );
    } else {
        write_classic_eocd(
            &mut bytes,
            false,
            entries.len() as u64,
            cd_size,
            cd_start,
            eocd_comment,
        );
    }

    Ok(SyntheticCd { bytes, cd_size })
}

pub(crate) fn name_and_gpbf_from_local_header(header: &[u8]) -> Result<(Vec<u8>, u16)> {
    if header.len() < 30 || header[..4] != LOCAL_FILE_MAGIC {
        return Err(AyzenpackError::Format("synthetic CD: local header magic"));
    }
    let gpbf = u16::from_le_bytes([header[6], header[7]]);
    let name_len = u16::from_le_bytes([header[26], header[27]]) as usize;
    let extra_len = u16::from_le_bytes([header[28], header[29]]) as usize;
    if 30 + name_len + extra_len != header.len() {
        return Err(AyzenpackError::Format("synthetic CD: local header length"));
    }
    Ok((header[30..30 + name_len].to_vec(), gpbf))
}

fn needs_u32_zip64(v: u64) -> bool {
    v >= ZIP64_U32
}

fn name_has_dotdot(name: &[u8]) -> bool {
    name.split(|&b| b == b'/' || b == b'\\').any(|c| c == b"..")
}

/// APPNOTE 4.5.3: only the overflowing u64s, in uncomp → comp → offset order.
fn zip64_extra(uncomp: u64, comp: u64, offset: u64) -> Vec<u8> {
    let need_uncomp = needs_u32_zip64(uncomp);
    let need_comp = needs_u32_zip64(comp);
    let need_off = needs_u32_zip64(offset);
    let k = u16::from(need_uncomp) + u16::from(need_comp) + u16::from(need_off);
    if k == 0 {
        return Vec::new();
    }
    let mut extra = Vec::with_capacity(4 + 8 * usize::from(k));
    extra.extend_from_slice(&ZIP64_EXTRA_ID.to_le_bytes());
    extra.extend_from_slice(&(8 * k).to_le_bytes());
    if need_uncomp {
        extra.extend_from_slice(&uncomp.to_le_bytes());
    }
    if need_comp {
        extra.extend_from_slice(&comp.to_le_bytes());
    }
    if need_off {
        extra.extend_from_slice(&offset.to_le_bytes());
    }
    extra
}

fn write_cd_record(out: &mut Vec<u8>, e: &SyntheticCdEntry, extra: &[u8]) -> Result<()> {
    let name_len = u16::try_from(e.name.len())
        .map_err(|_| AyzenpackError::Format("synthetic CD: filename exceeds 65535"))?;
    let extra_len = extra.len() as u16;
    let ver_made = if e.unix_mode.is_some() {
        VER_MADE_UNIX
    } else {
        VER_MADE_DOS
    };
    let ver_need = if extra.is_empty() {
        VER_NEED_ZIP
    } else {
        VER_NEED_ZIP64
    };
    let csize32 = if needs_u32_zip64(e.compressed_size) {
        u32::MAX
    } else {
        e.compressed_size as u32
    };
    let usize32 = if needs_u32_zip64(e.uncompressed_size) {
        u32::MAX
    } else {
        e.uncompressed_size as u32
    };
    let off32 = if needs_u32_zip64(e.local_offset) {
        u32::MAX
    } else {
        e.local_offset as u32
    };
    let ext_attr = e.unix_mode.unwrap_or(0) << 16;

    out.extend_from_slice(&CD_MAGIC);
    out.extend_from_slice(&ver_made.to_le_bytes());
    out.extend_from_slice(&ver_need.to_le_bytes());
    out.extend_from_slice(&e.gpbf.to_le_bytes());
    out.extend_from_slice(&e.method.to_le_bytes());
    out.extend_from_slice(&e.dos_time.to_le_bytes());
    out.extend_from_slice(&e.dos_date.to_le_bytes());
    out.extend_from_slice(&e.crc.to_le_bytes());
    out.extend_from_slice(&csize32.to_le_bytes());
    out.extend_from_slice(&usize32.to_le_bytes());
    out.extend_from_slice(&name_len.to_le_bytes());
    out.extend_from_slice(&extra_len.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&ext_attr.to_le_bytes());
    out.extend_from_slice(&off32.to_le_bytes());
    out.extend_from_slice(&e.name);
    out.extend_from_slice(extra);
    Ok(())
}

fn write_zip64_eocd(out: &mut Vec<u8>, count: u64, cd_size: u64, cd_start: u64) {
    out.extend_from_slice(&ZIP64_EOCD_MAGIC);
    out.extend_from_slice(&ZIP64_EOCD_REMAINING.to_le_bytes());
    out.extend_from_slice(&VER_MADE_ZIP64_EOCD.to_le_bytes());
    out.extend_from_slice(&VER_NEED_ZIP64.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_start.to_le_bytes());
}

fn write_zip64_locator(out: &mut Vec<u8>, zip64_eocd_offset: u64) {
    out.extend_from_slice(&ZIP64_LOCATOR_MAGIC);
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&zip64_eocd_offset.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
}

fn write_classic_eocd(
    out: &mut Vec<u8>,
    zip64: bool,
    count: u64,
    cd_size: u64,
    cd_start: u64,
    comment: &[u8],
) {
    let disk = if zip64 { u16::MAX } else { 0 };
    let count16 = if zip64 { u16::MAX } else { count as u16 };
    let size32 = if zip64 { u32::MAX } else { cd_size as u32 };
    let off32 = if zip64 { u32::MAX } else { cd_start as u32 };
    out.extend_from_slice(&EOCD_MAGIC);
    out.extend_from_slice(&disk.to_le_bytes());
    out.extend_from_slice(&disk.to_le_bytes());
    out.extend_from_slice(&count16.to_le_bytes());
    out.extend_from_slice(&count16.to_le_bytes());
    out.extend_from_slice(&size32.to_le_bytes());
    out.extend_from_slice(&off32.to_le_bytes());
    out.extend_from_slice(&(comment.len() as u16).to_le_bytes());
    out.extend_from_slice(comment);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exact::GPBF_DATA_DESC;

    const STORE_NAME: &[u8] = b"a.txt";

    fn store(name: &[u8]) -> SyntheticCdEntry {
        SyntheticCdEntry {
            name: name.to_vec(),
            method: 0,
            gpbf: 0,
            dos_time: 0,
            dos_date: 0,
            crc: 0,
            compressed_size: 0,
            uncompressed_size: 0,
            local_offset: 0,
            unix_mode: None,
        }
    }

    fn u16_at(b: &[u8], i: usize) -> u16 {
        u16::from_le_bytes(b[i..i + 2].try_into().unwrap())
    }

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
    }

    fn u64_at(b: &[u8], i: usize) -> u64 {
        u64::from_le_bytes(b[i..i + 8].try_into().unwrap())
    }

    fn local_header(name: &[u8], gpbf: u16, extra: &[u8]) -> Vec<u8> {
        let mut h = vec![0u8; 30];
        h[..4].copy_from_slice(&LOCAL_FILE_MAGIC);
        h[6..8].copy_from_slice(&gpbf.to_le_bytes());
        h[26..28].copy_from_slice(&(name.len() as u16).to_le_bytes());
        h[28..30].copy_from_slice(&(extra.len() as u16).to_le_bytes());
        h.extend_from_slice(name);
        h.extend_from_slice(extra);
        h
    }

    fn assert_classic_eocd_no_zip64(eocd: &[u8], count: u16, cd_size: u32, cd_start: u32) {
        assert_eq!(&eocd[..4], &EOCD_MAGIC);
        assert_eq!(u16_at(eocd, 4), 0);
        assert_eq!(u16_at(eocd, 6), 0);
        assert_eq!(u16_at(eocd, 8), count);
        assert_eq!(u16_at(eocd, 10), count);
        assert_eq!(u32_at(eocd, 12), cd_size);
        assert_eq!(u32_at(eocd, 16), cd_start);
    }

    fn assert_zip64_eocd_locator_classic(
        tail: &[u8],
        count: u64,
        cd_size: u64,
        cd_start: u64,
        comment: &[u8],
    ) {
        assert!(
            tail.len() >= 56 + 20 + 22 + comment.len(),
            "Zip64 EOCD+locator+classic too short: {}",
            tail.len()
        );
        assert_eq!(&tail[..4], &ZIP64_EOCD_MAGIC);
        assert_eq!(u64_at(tail, 4), ZIP64_EOCD_REMAINING);
        assert_eq!(u16_at(tail, 12), VER_MADE_ZIP64_EOCD);
        assert_eq!(u16_at(tail, 14), VER_NEED_ZIP64);
        assert_eq!(u32_at(tail, 16), 0);
        assert_eq!(u32_at(tail, 20), 0);
        assert_eq!(u64_at(tail, 24), count);
        assert_eq!(u64_at(tail, 32), count);
        assert_eq!(u64_at(tail, 40), cd_size);
        assert_eq!(u64_at(tail, 48), cd_start);

        let loc = &tail[56..];
        assert_eq!(&loc[..4], &ZIP64_LOCATOR_MAGIC);
        assert_eq!(u32_at(loc, 4), 0);
        assert_eq!(u64_at(loc, 8), cd_start + cd_size);
        assert_eq!(u32_at(loc, 16), 1);

        let eocd = &loc[20..];
        assert_eq!(&eocd[..4], &EOCD_MAGIC);
        assert_eq!(u16_at(eocd, 4), u16::MAX);
        assert_eq!(u16_at(eocd, 6), u16::MAX);
        assert_eq!(u16_at(eocd, 8), u16::MAX);
        assert_eq!(u16_at(eocd, 10), u16::MAX);
        assert_eq!(u32_at(eocd, 12), u32::MAX);
        assert_eq!(u32_at(eocd, 16), u32::MAX);
        assert_eq!(u16_at(eocd, 20), comment.len() as u16);
        assert_eq!(&eocd[22..22 + comment.len()], comment);
    }

    #[test]
    fn empty_archive_writes_classic_eocd() {
        let cd = write_synthetic_cd(&[], 42, b"hi").unwrap();
        assert_eq!(cd.cd_size, 0);
        assert_eq!(cd.bytes.len(), 22 + 2);
        assert_classic_eocd_no_zip64(&cd.bytes, 0, 0, 42);
        assert_eq!(u16_at(&cd.bytes, 20), 2);
        assert_eq!(&cd.bytes[22..], b"hi");
        assert!(
            !cd.bytes.windows(4).any(|w| w == ZIP64_EOCD_MAGIC),
            "empty archive must not emit Zip64 EOCD"
        );
    }

    #[test]
    fn one_store_name_classic_cd_and_eocd() {
        let cd = write_synthetic_cd(&[store(STORE_NAME)], 100, &[]).unwrap();
        let rec_len = 46 + STORE_NAME.len();
        assert_eq!(cd.cd_size, rec_len as u64);
        assert_eq!(&cd.bytes[..4], &CD_MAGIC);
        assert_eq!(u16_at(&cd.bytes, 4), VER_MADE_DOS);
        assert_eq!(u16_at(&cd.bytes, 6), VER_NEED_ZIP);
        assert_eq!(u16_at(&cd.bytes, 8), 0);
        assert_eq!(u16_at(&cd.bytes, 10), 0);
        assert_eq!(u32_at(&cd.bytes, 20), 0);
        assert_eq!(u32_at(&cd.bytes, 24), 0);
        assert_eq!(u16_at(&cd.bytes, 28), STORE_NAME.len() as u16);
        assert_eq!(u16_at(&cd.bytes, 30), 0);
        assert_eq!(u16_at(&cd.bytes, 32), 0);
        assert_eq!(u32_at(&cd.bytes, 38), 0);
        assert_eq!(u32_at(&cd.bytes, 42), 0);
        assert_eq!(&cd.bytes[46..46 + STORE_NAME.len()], STORE_NAME);
        assert_classic_eocd_no_zip64(&cd.bytes[rec_len..], 1, rec_len as u32, 100);
    }

    #[test]
    fn gpbf_bit3_copied_into_cd_record() {
        let mut e = store(STORE_NAME);
        e.gpbf = GPBF_DATA_DESC;
        let cd = write_synthetic_cd(&[e], 0, &[]).unwrap();
        assert_eq!(u16_at(&cd.bytes, 8), GPBF_DATA_DESC);
    }

    #[test]
    fn unix_mode_sets_version_made_by_and_external_attrs() {
        let mut e = store(STORE_NAME);
        e.unix_mode = Some(0o100644);
        let cd = write_synthetic_cd(&[e], 0, &[]).unwrap();
        assert_eq!(u16_at(&cd.bytes, 4), VER_MADE_UNIX);
        assert_eq!(u32_at(&cd.bytes, 38), 0o100644 << 16);
    }

    #[test]
    fn zip64_4gib_sizes_and_offset_extra_order_and_eocd_layout() {
        let mut e = store(STORE_NAME);
        e.uncompressed_size = 0x1_0000_0002;
        e.compressed_size = 0x1_0000_0000;
        e.local_offset = 0x1_0000_0000;
        let cd_start = 0x100;
        let cd = write_synthetic_cd(&[e], cd_start, &[]).unwrap();
        let extra_len = u16_at(&cd.bytes, 30);
        assert_eq!(extra_len, 4 + 24);
        assert_eq!(u16_at(&cd.bytes, 6), VER_NEED_ZIP64);
        assert_eq!(u32_at(&cd.bytes, 20), u32::MAX);
        assert_eq!(u32_at(&cd.bytes, 24), u32::MAX);
        assert_eq!(u32_at(&cd.bytes, 42), u32::MAX);
        let extra = &cd.bytes[46 + STORE_NAME.len()..46 + STORE_NAME.len() + extra_len as usize];
        assert_eq!(u16_at(extra, 0), ZIP64_EXTRA_ID);
        assert_eq!(u16_at(extra, 2), 24);
        assert_eq!(u64_at(extra, 4), 0x1_0000_0002);
        assert_eq!(u64_at(extra, 12), 0x1_0000_0000);
        assert_eq!(u64_at(extra, 20), 0x1_0000_0000);
        assert_eq!(
            cd.cd_size,
            (46 + STORE_NAME.len() + extra_len as usize) as u64
        );
        assert_zip64_eocd_locator_classic(
            &cd.bytes[cd.cd_size as usize..],
            1,
            cd.cd_size,
            cd_start,
            &[],
        );
    }

    #[test]
    fn zip64_65536_entries_emits_eocd_without_per_record_extra() {
        let entries: Vec<SyntheticCdEntry> = (0..65_536).map(|_| store(b"")).collect();
        let cd_start = 7;
        let cd = write_synthetic_cd(&entries, cd_start, &[]).unwrap();
        assert_eq!(cd.cd_size, 65_536 * 46);
        assert_eq!(u16_at(&cd.bytes, 6), VER_NEED_ZIP);
        assert_eq!(u16_at(&cd.bytes, 30), 0);
        assert_zip64_eocd_locator_classic(
            &cd.bytes[cd.cd_size as usize..],
            65_536,
            cd.cd_size,
            cd_start,
            &[],
        );
    }

    #[test]
    fn local_zip64_and_ut_extra_not_copied_into_cd() {
        let mut local_extra = Vec::new();
        local_extra.extend_from_slice(&ZIP64_EXTRA_ID.to_le_bytes());
        local_extra.extend_from_slice(&16u16.to_le_bytes());
        local_extra.extend_from_slice(&5u64.to_le_bytes());
        local_extra.extend_from_slice(&5u64.to_le_bytes());
        local_extra.extend_from_slice(&0x5455u16.to_le_bytes());
        local_extra.extend_from_slice(&5u16.to_le_bytes());
        local_extra.push(1);
        local_extra.extend_from_slice(&0x1234_5678u32.to_le_bytes());

        let header = local_header(STORE_NAME, 0, &local_extra);
        let (name, gpbf) = name_and_gpbf_from_local_header(&header).unwrap();
        let mut e = store(&name);
        e.gpbf = gpbf;
        e.uncompressed_size = 5;
        e.compressed_size = 5;
        e.local_offset = 0x1_0000_0000;
        let cd = write_synthetic_cd(&[e], 0, &[]).unwrap();

        let extra_len = u16_at(&cd.bytes, 30);
        assert_eq!(extra_len, 12);
        let extra = &cd.bytes[46 + STORE_NAME.len()..46 + STORE_NAME.len() + extra_len as usize];
        assert_eq!(u16_at(extra, 0), ZIP64_EXTRA_ID);
        assert_eq!(u16_at(extra, 2), 8);
        assert_eq!(u64_at(extra, 4), 0x1_0000_0000);
        assert_eq!(
            extra.len(),
            12,
            "CD extra must be synthesized 0x0001 only, not local 0x0001+UT"
        );
    }

    #[test]
    fn eocd_comment_over_65535_is_format() {
        let comment = vec![b'x'; 65_536];
        let err = write_synthetic_cd(&[], 0, &comment).unwrap_err();
        assert_eq!(err.to_string(), "synthetic CD: EOCD comment exceeds 65535");
    }

    #[test]
    fn name_with_dotdot_is_format() {
        for name in [b".." as &[u8], b"a/../b", b"a\\..\\b", b"foo/bar/.."] {
            let err = write_synthetic_cd(&[store(name)], 0, &[]).unwrap_err();
            assert_eq!(err.to_string(), "synthetic CD: name contains ..");
        }
        write_synthetic_cd(&[store(b"foo..bar")], 0, &[]).unwrap();
    }

    #[test]
    fn filename_over_65535_is_format() {
        let err = write_synthetic_cd(&[store(&vec![b'a'; 65_536])], 0, &[]).unwrap_err();
        assert_eq!(err.to_string(), "synthetic CD: filename exceeds 65535");
    }

    #[test]
    fn name_and_gpbf_from_local_header_errors() {
        let err = name_and_gpbf_from_local_header(&[]).unwrap_err();
        assert_eq!(err.to_string(), "synthetic CD: local header magic");
        let mut bad_magic = local_header(STORE_NAME, 0, &[]);
        bad_magic[0] = b'X';
        let err = name_and_gpbf_from_local_header(&bad_magic).unwrap_err();
        assert_eq!(err.to_string(), "synthetic CD: local header magic");
        let mut short = local_header(STORE_NAME, GPBF_DATA_DESC, &[]);
        short.pop();
        let err = name_and_gpbf_from_local_header(&short).unwrap_err();
        assert_eq!(err.to_string(), "synthetic CD: local header length");
        let (name, gpbf) =
            name_and_gpbf_from_local_header(&local_header(STORE_NAME, GPBF_DATA_DESC, &[]))
                .unwrap();
        assert_eq!(name, STORE_NAME);
        assert_eq!(gpbf, GPBF_DATA_DESC);
    }
}
