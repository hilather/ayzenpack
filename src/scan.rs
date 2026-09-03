//! Stream ZIP/JAR entries in central-directory order without a class forest.
//!
//! `ScannedEntry` is metadata only. Payloads are yielded one at a time through
//! `for_each_jar_entry` and dropped before the next entry is inflated.

use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};
use zip::CompressionMethod;
use zip::HasZipMetadata;
use zip::ZipArchive;

use crate::error::{AyzenpackError, Result};
use crate::hashutil::hex_lower;

const HASH_CHUNK: usize = 16 * 1024;
const ZIP_BUF: usize = 64 * 1024;

const LOCAL_FILE_MAGIC: [u8; 4] = *b"PK\x03\x04";
const CD_MAGIC: [u8; 4] = *b"PK\x01\x02";
const EOCD_MAGIC: [u8; 4] = *b"PK\x05\x06";
const ZIP64_LOCATOR_MAGIC: [u8; 4] = *b"PK\x06\x07";
const ZIP64_EOCD_MAGIC: [u8; 4] = *b"PK\x06\x06";
const ZIP64_EXTRA_ID: u16 = 0x0001;

const EOCD_MIN: u64 = 22;
const EOCD_MAX_COMMENT: u64 = 65_535;
const ZIP64_LOCATOR_LEN: u64 = 20;
const ZIP64_EOCD_MIN: u64 = 56;
const ZIP64_SCAN: u64 = 64 * 1024;
/// Spring Boot launchers are a few KiB; reject absurd prepended junk.
const MAX_PREFIX: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScannedEntry {
    pub name: String,
    pub is_dir: bool,
    pub crc32: u32,
    pub method: String,
    pub method_code: u16,
    pub uncompressed_size: u64,
    pub compressed_size: u64,
    pub dos_date: u16,
    pub dos_time: u16,
    pub unix_mode: Option<u32>,
    pub utf8_flag: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name_raw_hex: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedJar {
    pub source_path: PathBuf,
    pub source_size: u64,
    pub source_blake3: [u8; 32],
    pub source_sha256: [u8; 32],
    pub comment: String,
    pub signed: bool,
    /// Prepended launcher bytes (Spring Boot fully-executable JAR). `None` if the file is a normal ZIP.
    pub prefix: Option<Vec<u8>>,
    pub entries: Vec<ScannedEntry>,
}

/// Seek/Read/Write view that hides a prepended launcher so ZIP offsets stay ZIP-relative.
pub(crate) struct ZipView<T> {
    inner: T,
    prefix_len: u64,
}

impl<T> ZipView<T> {
    pub(crate) fn new(inner: T, prefix_len: u64) -> Self {
        Self { inner, prefix_len }
    }
}

impl<T: Read> Read for ZipView<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<T: io::Write> io::Write for ZipView<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<T: Seek> Seek for ZipView<T> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let physical = match pos {
            SeekFrom::Start(n) => {
                let phys = self
                    .prefix_len
                    .checked_add(n)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek overflow"))?;
                SeekFrom::Start(phys)
            }
            SeekFrom::Current(n) => SeekFrom::Current(n),
            SeekFrom::End(n) => SeekFrom::End(n),
        };
        let new_phys = self.inner.seek(physical)?;
        if new_phys < self.prefix_len {
            self.inner.seek(SeekFrom::Start(self.prefix_len))?;
            return Ok(0);
        }
        Ok(new_phys - self.prefix_len)
    }
}

/// Metadata-only scan. Uncompressed payloads are not retained.
pub fn scan_jar(path: &Path, max_entry: u64) -> Result<ScannedJar> {
    for_each_jar_entry(path, max_entry, |_meta, _payload| Ok(()))
}

/// Inflate one entry at a time. `payload` is `None` for directories and
/// `Some` for files; the slice is invalid after `f` returns.
pub fn for_each_jar_entry<F>(path: &Path, max_entry: u64, mut f: F) -> Result<ScannedJar>
where
    F: FnMut(&ScannedEntry, Option<&[u8]>) -> Result<()>,
{
    for_each_jar_entry_with_len(
        path,
        max_entry,
        |_| {},
        |meta, payload| f(meta, payload.as_deref()),
    )
}

/// In-memory ZIP listing for depth-1 child probe. Encrypted / unlistable stay `Err`.
pub(crate) struct ScannedByteEntry {
    pub meta: ScannedEntry,
    pub payload: Option<Vec<u8>>,
}

pub(crate) fn scan_from_bytes(bytes: &[u8], max_entry: u64) -> Result<Vec<ScannedByteEntry>> {
    let path = Path::new("<bytes>");
    let mut cur = Cursor::new(bytes);
    let layout = detect_zip_layout(path, &mut cur)?;
    // Prefixed children already ran zip_archive_opens in detect_zip_layout.
    // PK-start returns prefix_len=0 immediately; still require homemade CD
    // count == ZipArchive::len() so a nested-EOCD latch is Err (opaque at probe).
    // Do not require header_start == 0: a PK-start hole is leading_pad, not latch.
    let homemade_n =
        pk_start_homemade_entry_count(path, &mut cur, bytes.len() as u64, layout.prefix_len)?;
    cur.seek(SeekFrom::Start(0))
        .map_err(|source| io_at(source, path))?;
    let reader = BufReader::with_capacity(ZIP_BUF, ZipView::new(cur, layout.view_shift));
    let mut archive = ZipArchive::new(reader).map_err(|err| map_archive_open_err(err, path))?;
    reject_pk_start_listing_latch(archive.len(), homemade_n, true)?;
    let mut out = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let mut zf = archive
            .by_index(i)
            .map_err(|err| map_entry_err(err, path))?;
        if zf.encrypted() {
            return Err(AyzenpackError::Encrypted {
                path: path.to_path_buf(),
            });
        }
        let name = zf.name().to_string();
        let (method, method_code) = method_label_and_code(zf.compression());
        let (dos_date, dos_time) = match zf.last_modified() {
            Some(dt) => (dt.datepart(), dt.timepart()),
            None => (0, 0),
        };
        let name_raw_hex = match std::str::from_utf8(zf.name_raw()) {
            Ok(_) => None,
            Err(_) => Some(hex_lower(zf.name_raw())),
        };
        let meta = ScannedEntry {
            is_dir: zf.is_dir(),
            crc32: zf.crc32(),
            method,
            method_code,
            uncompressed_size: zf.size(),
            compressed_size: zf.compressed_size(),
            dos_date,
            dos_time,
            unix_mode: zf.unix_mode(),
            utf8_flag: zf.get_metadata().is_utf8,
            name_raw_hex,
            name,
        };
        if meta.is_dir {
            drop(zf);
            out.push(ScannedByteEntry {
                meta,
                payload: None,
            });
            continue;
        }
        if meta.uncompressed_size > max_entry {
            return Err(AyzenpackError::EntryTooLarge {
                path: path.to_path_buf(),
                name: meta.name,
                size: meta.uncompressed_size,
                max: max_entry,
            });
        }
        let mut buf = Vec::with_capacity(meta.uncompressed_size as usize);
        io::copy(&mut zf, &mut buf).map_err(|source| io_at(source, path))?;
        drop(zf);
        if buf.len() as u64 != meta.uncompressed_size {
            return Err(AyzenpackError::FormatOwned(format!(
                "uncompressed size mismatch for {}!{}: header {}, read {}",
                path.display(),
                meta.name,
                meta.uncompressed_size,
                buf.len()
            )));
        }
        out.push(ScannedByteEntry {
            meta,
            payload: Some(buf),
        });
    }
    Ok(out)
}

/// Same as [`for_each_jar_entry`], then `on_len(archive.len())` before the first entry.
/// File payloads are owned so dehydrate can hand them to hash workers without a second copy.
pub(crate) fn for_each_jar_entry_with_len<L, F>(
    path: &Path,
    max_entry: u64,
    mut on_len: L,
    mut f: F,
) -> Result<ScannedJar>
where
    L: FnMut(u64),
    F: FnMut(&ScannedEntry, Option<Vec<u8>>) -> Result<()>,
{
    let mut file = File::open(path).map_err(|source| io_at(source, path))?;
    let source_size = file.metadata().map_err(|source| io_at(source, path))?.len();
    let layout = detect_zip_layout(path, &mut file)?;
    let (source_blake3, source_sha256) =
        hash_source(&mut file).map_err(|source| io_at(source, path))?;
    let homemade_n =
        pk_start_homemade_entry_count(path, &mut file, source_size, layout.prefix_len)?;
    let prefix = if layout.prefix_len > 0 {
        file.seek(SeekFrom::Start(0))
            .map_err(|source| io_at(source, path))?;
        let mut buf = vec![0u8; layout.prefix_len as usize];
        file.read_exact(&mut buf)
            .map_err(|source| io_at(source, path))?;
        Some(buf)
    } else {
        None
    };

    let reader = BufReader::with_capacity(ZIP_BUF, ZipView::new(file, layout.view_shift));
    let mut archive = ZipArchive::new(reader).map_err(|err| map_archive_open_err(err, path))?;
    if let Some(n) = homemade_n {
        if archive.len() as u64 != n {
            let mut probe = File::open(path).map_err(|source| io_at(source, path))?;
            if pk_start_is_nested_eocd_latch(path, &mut probe, source_size, archive.len(), n)? {
                return Err(AyzenpackError::FormatOwned(
                    "zip listing disagrees with homemade central directory count".into(),
                ));
            }
        }
    }
    let comment = String::from_utf8_lossy(archive.comment()).into_owned();
    on_len(archive.len() as u64);

    let mut entries = Vec::with_capacity(archive.len());
    let mut signed = false;

    for i in 0..archive.len() {
        let mut zf = archive
            .by_index(i)
            .map_err(|err| map_entry_err(err, path))?;

        if zf.encrypted() {
            return Err(AyzenpackError::Encrypted {
                path: path.to_path_buf(),
            });
        }

        let name = zf.name().to_string();
        // DESIGN: signed if any META-INF/*.SF or *.RSA/*.DSA/*.EC entry exists.
        if looks_signed(&name) {
            signed = true;
        }

        let (method, method_code) = method_label_and_code(zf.compression());
        let (dos_date, dos_time) = match zf.last_modified() {
            Some(dt) => (dt.datepart(), dt.timepart()),
            None => (0, 0),
        };
        let name_raw_hex = match std::str::from_utf8(zf.name_raw()) {
            Ok(_) => None,
            Err(_) => Some(hex_lower(zf.name_raw())),
        };

        let meta = ScannedEntry {
            is_dir: zf.is_dir(),
            crc32: zf.crc32(),
            method,
            method_code,
            uncompressed_size: zf.size(),
            compressed_size: zf.compressed_size(),
            dos_date,
            dos_time,
            unix_mode: zf.unix_mode(),
            utf8_flag: zf.get_metadata().is_utf8,
            name_raw_hex,
            name,
        };

        if meta.is_dir {
            f(&meta, None)?;
            entries.push(meta);
            continue;
        }

        if meta.uncompressed_size > max_entry {
            return Err(AyzenpackError::EntryTooLarge {
                path: path.to_path_buf(),
                name: meta.name,
                size: meta.uncompressed_size,
                max: max_entry,
            });
        }

        let mut buf = Vec::with_capacity(meta.uncompressed_size as usize);
        io::copy(&mut zf, &mut buf).map_err(|source| io_at(source, path))?;
        drop(zf);
        if buf.len() as u64 != meta.uncompressed_size {
            return Err(AyzenpackError::FormatOwned(format!(
                "uncompressed size mismatch for {}!{}: header {}, read {}",
                path.display(),
                meta.name,
                meta.uncompressed_size,
                buf.len()
            )));
        }

        f(&meta, Some(buf))?;
        entries.push(meta);
    }

    Ok(ScannedJar {
        source_path: path.to_path_buf(),
        source_size,
        source_blake3,
        source_sha256,
        comment,
        signed,
        prefix,
        entries,
    })
}

/// Length of a prepended executable launcher, or 0 if `path` is a normal ZIP.
pub fn zip_prefix_len(path: &Path) -> Result<u64> {
    let mut file = File::open(path).map_err(|source| io_at(source, path))?;
    Ok(detect_zip_layout(path, &mut file)?.prefix_len)
}

/// How a prepended launcher maps onto ZIP offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ZipLayout {
    /// Prefix bytes are `[0, prefix_len)`. Zero for a normal ZIP.
    pub(crate) prefix_len: u64,
    /// `ZipView` shift. Equal to `prefix_len` when offsets are ZIP-relative
    /// (Spring default); `0` when `zip -A` made offsets file-absolute.
    pub(crate) view_shift: u64,
}

fn looks_signed(name: &str) -> bool {
    let lower = name.replace('\\', "/").to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("meta-inf/") else {
        return false;
    };
    if rest.is_empty() || rest.contains('/') {
        return false;
    }
    rest.ends_with(".sf")
        || rest.ends_with(".rsa")
        || rest.ends_with(".dsa")
        || rest.ends_with(".ec")
}

fn method_label_and_code(method: CompressionMethod) -> (String, u16) {
    match method {
        CompressionMethod::Stored => ("stored".to_string(), 0),
        CompressionMethod::Deflated => ("deflated".to_string(), 8),
        other => {
            #[allow(deprecated)]
            let code = other.to_u16();
            ("other".to_string(), code)
        }
    }
}

/// Detect a prepended launcher. No CLI flag.
///
/// If the file does not start with ZIP magic, the prefix ends at the central
/// directory's first local header (CD min local offset, or that offset after
/// `zip -A` made it file-absolute) — not the first `PK\x03\x04` in the stub.
/// Prefix bytes are `[0, first_real_lh)`. Then:
/// 1. Try `ZipArchive` through `ZipView` shifted to that offset (unadjusted).
/// 2. If that fails, try the full file (`zip -A` file-absolute offsets).
///
/// A file with no local headers is `NotZip`, except an empty prefixed archive
/// (EOCD-only). Unadjusted empty archives use extra-data math. After `zip -A`,
/// extra is 0 and the recorded CD offset is the prefix.
pub(crate) fn detect_zip_layout(path: &Path, file: &mut (impl Read + Seek)) -> Result<ZipLayout> {
    let file_len = file
        .seek(SeekFrom::End(0))
        .map_err(|source| io_at(source, path))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_at(source, path))?;

    let mut magic = [0u8; 4];
    let n = file
        .read(&mut magic)
        .map_err(|source| io_at(source, path))?;
    if n == 4 && (magic == LOCAL_FILE_MAGIC || magic == EOCD_MAGIC) {
        return Ok(ZipLayout {
            prefix_len: 0,
            view_shift: 0,
        });
    }

    if let Some(layout) = layout_from_first_pk(path, file, file_len)? {
        return Ok(layout);
    }

    match layout_from_eocd_empty(path, file, file_len) {
        Ok(Some(layout)) => Ok(layout),
        Ok(None) => Err(not_zip(path)),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
fn detect_zip_prefix(path: &Path, file: &mut (impl Read + Seek)) -> Result<u64> {
    Ok(detect_zip_layout(path, file)?.prefix_len)
}

/// Real first local header (CD min offset) + try `ZipArchive`.
/// Does not use EOCD extra-data math.
fn layout_from_first_pk(
    path: &Path,
    file: &mut (impl Read + Seek),
    file_len: u64,
) -> Result<Option<ZipLayout>> {
    let Some(first) = find_cd_first_local(path, file, file_len)? else {
        return Ok(None);
    };
    if first == 0 || first > MAX_PREFIX {
        return Ok(None);
    }
    // Homemade outer count is find_cd_bounds(...).3 only. After zip -A the
    // recorded CD offset is file-absolute — do not treat it as prefix.
    let outer_entries = match find_cd_bounds(path, file, file_len) {
        Ok((_, _, _, n)) => n,
        Err(AyzenpackError::NotZip { .. }) => return Ok(None),
        Err(err) => return Err(err),
    };
    if zip_archive_opens(path, file, first, first, outer_entries)? {
        return Ok(Some(ZipLayout {
            prefix_len: first,
            view_shift: first,
        }));
    }
    if zip_archive_opens(path, file, 0, first, outer_entries)? {
        return Ok(Some(ZipLayout {
            prefix_len: first,
            view_shift: 0,
        }));
    }
    Ok(None)
}

/// Homemade EOCD/Zip64 entry count for a PK-start file (`prefix_len == 0`).
/// Prefixed files already ran [`zip_archive_opens`]; a PK-start hole is
/// `leading_pad`, not a prefix, so `detect_zip_layout` never applies that gate.
fn pk_start_homemade_entry_count(
    path: &Path,
    file: &mut (impl Read + Seek),
    file_len: u64,
    prefix_len: u64,
) -> Result<Option<u64>> {
    if prefix_len != 0 {
        return Ok(None);
    }
    let (_, _, _, n) = find_cd_bounds(path, file, file_len)?;
    Ok(Some(n))
}

/// Refuse a ZipArchive that bound a STORE nested EOCD instead of the outer CD.
/// Child probe maps this `Err` to opaque. Outer scan uses
/// [`pk_start_is_nested_eocd_latch`] so `dup.txt` last-wins stays listable.
fn reject_pk_start_listing_latch(
    archive_len: usize,
    homemade_n: Option<u64>,
    child: bool,
) -> Result<()> {
    let Some(n) = homemade_n else {
        return Ok(());
    };
    if archive_len as u64 == n {
        return Ok(());
    }
    let who = if child {
        "child zip listing"
    } else {
        "zip listing"
    };
    Err(AyzenpackError::FormatOwned(format!(
        "{who} disagrees with homemade central directory count"
    )))
}

/// Same-zip last-wins (`dup.txt`) has first local at 0. A PK-start hole whose
/// shifted view lists the homemade outer count is a nested-EOCD latch.
fn pk_start_is_nested_eocd_latch(
    path: &Path,
    file: &mut (impl Read + Seek),
    file_len: u64,
    archive_len: usize,
    homemade_n: u64,
) -> Result<bool> {
    if archive_len as u64 == homemade_n {
        return Ok(false);
    }
    let Some(first) = find_cd_first_local(path, file, file_len)? else {
        return Ok(false);
    };
    if first == 0 {
        return Ok(false);
    }
    zip_archive_opens(path, file, first, first, homemade_n)
}

/// `ZipArchive::new` success is not enough: rust zip may latch onto a STORE
/// nested EOCD when the view's CD offset is wrong (`zip -A` + prefix shift).
fn zip_archive_opens(
    path: &Path,
    file: &mut (impl Read + Seek),
    view_shift: u64,
    prefix_len: u64,
    outer_entries: u64,
) -> Result<bool> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_at(source, path))?;
    let mut archive = match ZipArchive::new(ZipView::new(file, view_shift)) {
        Ok(archive) => archive,
        Err(zip::result::ZipError::InvalidArchive(_)) => return Ok(false),
        Err(zip::result::ZipError::Io(source)) => return Err(io_at(source, path)),
        Err(_) => return Ok(false),
    };
    if archive.len() as u64 != outer_entries {
        return Ok(false);
    }
    if archive.is_empty() {
        return Ok(true);
    }
    let header_start = match archive.by_index(0) {
        Ok(zf) => zf.header_start(),
        Err(_) => return Ok(false),
    };
    // Do not use archive.offset(): on a correct zip -A open it is 0, which
    // would reject the good view. header_start + view_shift == prefix_len.
    Ok(header_start.checked_add(view_shift) == Some(prefix_len))
}

/// Empty prefixed ZIP (no local header): EOCD extra-data math, or the
/// file-absolute CD offset after `zip -A` (extra == 0).
fn layout_from_eocd_empty(
    path: &Path,
    file: &mut (impl Read + Seek),
    file_len: u64,
) -> Result<Option<ZipLayout>> {
    let (eocd_off, cd_size, cd_off, entries) = match find_cd_bounds(path, file, file_len) {
        Ok(bounds) => bounds,
        Err(AyzenpackError::NotZip { .. }) => return Ok(None),
        Err(err) => return Err(err),
    };
    let empty = entries == 0 && cd_size == 0;
    if !empty {
        return Ok(None);
    }
    let Some(cd_start) = eocd_off.checked_sub(cd_size) else {
        return Ok(None);
    };
    let Some(extra) = cd_start.checked_sub(cd_off) else {
        return Ok(None);
    };
    if extra > MAX_PREFIX {
        return Ok(None);
    }
    if extra == 0 {
        // zip -A: recorded CD offset is file-absolute and points at the EOCD.
        if cd_off == 0 || cd_off > MAX_PREFIX {
            return Ok(None);
        }
        return match confirm_zip_at(path, file, cd_off, true) {
            Ok(()) => Ok(Some(ZipLayout {
                prefix_len: cd_off,
                view_shift: 0,
            })),
            Err(AyzenpackError::NotZip { .. }) => Ok(None),
            Err(err) => Err(err),
        };
    }
    match confirm_zip_at(path, file, extra, true) {
        Ok(()) => Ok(Some(ZipLayout {
            prefix_len: extra,
            view_shift: extra,
        })),
        Err(AyzenpackError::NotZip { .. }) => Ok(None),
        Err(err) => Err(err),
    }
}

/// File offset of the CD's first local header, ignoring decoy `PK\x03\x04` in the stub.
fn find_cd_first_local(
    path: &Path,
    file: &mut (impl Read + Seek),
    file_len: u64,
) -> Result<Option<u64>> {
    // Prefer Zip64 EOCD when the locator is present, even if classic EOCD
    // fields are not sentinels (rust zip `large_file` writes sizes as MAX
    // but leaves a 32-bit CD offset). Classic `eocd - cd_size` then lands
    // in the Zip64 footer instead of the CD.
    let (eocd_off, cd_size32, _cd_off32, entries16) = match find_eocd(path, file, file_len) {
        Ok(bounds) => bounds,
        Err(AyzenpackError::NotZip { .. }) => return Ok(None),
        Err(err) => return Err(err),
    };
    let (struct_off, cd_size, entries) = match find_zip64_cd_bounds(path, file, eocd_off) {
        Ok((off, size, _, n)) => (off, size, n),
        Err(AyzenpackError::NotZip { .. }) => {
            (eocd_off, u64::from(cd_size32), u64::from(entries16))
        }
        Err(err) => return Err(err),
    };
    if entries == 0 || cd_size == 0 {
        return Ok(None);
    }
    let Some(phys_cd) = struct_off.checked_sub(cd_size) else {
        return Ok(None);
    };
    if !magic_at(path, file, phys_cd, &CD_MAGIC)? {
        return Ok(None);
    }
    let Some((min_off, name)) = read_cd_min_local(path, file, phys_cd, cd_size)? else {
        return Ok(None);
    };
    // zip -A: CD local offsets are already file-absolute.
    if local_name_eq(path, file, min_off, &name)? {
        return Ok(Some(min_off));
    }
    find_local_named(path, file, file_len, &name)
}

fn read_cd_min_local(
    path: &Path,
    file: &mut (impl Read + Seek),
    phys_cd: u64,
    cd_size: u64,
) -> Result<Option<(u64, Vec<u8>)>> {
    let Ok(cd_len) = usize::try_from(cd_size) else {
        return Ok(None);
    };
    file.seek(SeekFrom::Start(phys_cd))
        .map_err(|source| io_at(source, path))?;
    let mut cd = vec![0u8; cd_len];
    file.read_exact(&mut cd)
        .map_err(|source| io_at(source, path))?;

    let mut best: Option<(u64, Vec<u8>)> = None;
    let mut i = 0usize;
    while i < cd.len() {
        // Trailing leftover/truncated CD: prefix detect uses complete rows only.
        // homemade_ok (exact.rs) still returns None on truncated CD.
        if i + 46 > cd.len() || cd[i..i + 4] != CD_MAGIC {
            break;
        }
        let name_len = u16::from_le_bytes(cd[i + 28..i + 30].try_into().unwrap()) as usize;
        let extra_len = u16::from_le_bytes(cd[i + 30..i + 32].try_into().unwrap()) as usize;
        let comment_len = u16::from_le_bytes(cd[i + 32..i + 34].try_into().unwrap()) as usize;
        let uncomp32 = u32::from_le_bytes(cd[i + 24..i + 28].try_into().unwrap());
        let comp32 = u32::from_le_bytes(cd[i + 20..i + 24].try_into().unwrap());
        let off32 = u32::from_le_bytes(cd[i + 42..i + 46].try_into().unwrap());
        let Some(rec_end) = i
            .checked_add(46)
            .and_then(|n| n.checked_add(name_len))
            .and_then(|n| n.checked_add(extra_len))
            .and_then(|n| n.checked_add(comment_len))
        else {
            break;
        };
        if rec_end > cd.len() {
            break;
        }
        let name = cd[i + 46..i + 46 + name_len].to_vec();
        let extra = &cd[i + 46 + name_len..i + 46 + name_len + extra_len];
        let Some(local_off) = cd_local_offset(extra, uncomp32, comp32, off32) else {
            break;
        };
        match &best {
            Some((off, _)) if local_off >= *off => {}
            _ => best = Some((local_off, name)),
        }
        i = rec_end;
    }
    Ok(best)
}

fn cd_local_offset(extra: &[u8], uncomp32: u32, comp32: u32, off32: u32) -> Option<u64> {
    if off32 != u32::MAX {
        return Some(u64::from(off32));
    }
    let data = zip64_extra_payload(extra)?;
    let mut cur = data;
    if uncomp32 == u32::MAX {
        if cur.len() < 8 {
            return None;
        }
        cur = &cur[8..];
    }
    if comp32 == u32::MAX {
        if cur.len() < 8 {
            return None;
        }
        cur = &cur[8..];
    }
    if cur.len() < 8 {
        return None;
    }
    Some(u64::from_le_bytes(cur[..8].try_into().ok()?))
}

fn zip64_extra_payload(extra: &[u8]) -> Option<&[u8]> {
    let mut i = 0usize;
    while i + 4 <= extra.len() {
        let tag = u16::from_le_bytes(extra[i..i + 2].try_into().ok()?);
        let size = u16::from_le_bytes(extra[i + 2..i + 4].try_into().ok()?) as usize;
        let start = i + 4;
        let end = start.checked_add(size)?;
        if end > extra.len() {
            return None;
        }
        if tag == ZIP64_EXTRA_ID {
            return Some(&extra[start..end]);
        }
        i = end;
    }
    None
}

fn find_local_named(
    path: &Path,
    file: &mut (impl Read + Seek),
    file_len: u64,
    name: &[u8],
) -> Result<Option<u64>> {
    let scan = file_len.min(MAX_PREFIX.saturating_add(4));
    if scan < 4 {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_at(source, path))?;
    let mut buf = vec![0u8; scan as usize];
    file.read_exact(&mut buf)
        .map_err(|source| io_at(source, path))?;
    let end = buf.len() - 3;
    for i in 0..end {
        if buf[i..i + 4] != LOCAL_FILE_MAGIC {
            continue;
        }
        let off = i as u64;
        if off == 0 || off > MAX_PREFIX {
            continue;
        }
        if local_name_eq(path, file, off, name)? {
            return Ok(Some(off));
        }
    }
    Ok(None)
}

fn local_name_eq(
    path: &Path,
    file: &mut (impl Read + Seek),
    offset: u64,
    name: &[u8],
) -> Result<bool> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|source| io_at(source, path))?;
    let mut fixed = [0u8; 30];
    match file.read_exact(&mut fixed) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
        Err(source) => return Err(io_at(source, path)),
    }
    if fixed[..4] != LOCAL_FILE_MAGIC {
        return Ok(false);
    }
    let name_len = u16::from_le_bytes([fixed[26], fixed[27]]) as usize;
    if name_len != name.len() {
        return Ok(false);
    }
    let mut got = vec![0u8; name_len];
    match file.read_exact(&mut got) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
        Err(source) => return Err(io_at(source, path)),
    }
    Ok(got.as_slice() == name)
}

fn magic_at(
    path: &Path,
    file: &mut (impl Read + Seek),
    offset: u64,
    magic: &[u8; 4],
) -> Result<bool> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|source| io_at(source, path))?;
    let mut buf = [0u8; 4];
    let n = file.read(&mut buf).map_err(|source| io_at(source, path))?;
    Ok(n == 4 && buf == *magic)
}

fn not_zip(path: &Path) -> AyzenpackError {
    AyzenpackError::NotZip {
        path: path.to_path_buf(),
    }
}

/// `(eocd_or_zip64_eocd_file_offset, cd_size, recorded_cd_offset, entry_count)`.
/// The first offset is the structure that immediately follows the central directory.
pub(crate) fn find_cd_bounds(
    path: &Path,
    file: &mut (impl Read + Seek),
    file_len: u64,
) -> Result<(u64, u64, u64, u64)> {
    let (eocd_off, cd_size32, cd_off32, entries16) = find_eocd(path, file, file_len)?;
    // Prefer Zip64 EOCD when the locator is present, even if classic EOCD
    // fields are not sentinels (rust zip `large_file` writes a Zip64 footer
    // while leaving a 32-bit CD offset / entry count). Classic
    // `eocd - cd_size` then lands in the Zip64 footer instead of the CD.
    match find_zip64_cd_bounds(path, file, eocd_off) {
        Ok(bounds) => Ok(bounds),
        Err(AyzenpackError::NotZip { .. }) => Ok((
            eocd_off,
            u64::from(cd_size32),
            u64::from(cd_off32),
            u64::from(entries16),
        )),
        Err(err) => Err(err),
    }
}

/// 0.2.1 `find_cd_bounds`: Zip64 only when classic EOCD fields are sentinels.
/// rust zip `large_file` writes a Zip64 footer while leaving a 32-bit CD offset
/// / entry count, so `eocd - cd_size` lands in that footer. That is the listed-jar
/// `ZipExact::Raw` path this crate must not take anymore.
#[cfg(test)]
pub(crate) fn find_cd_bounds_v0_2_1(
    path: &Path,
    file: &mut (impl Read + Seek),
    file_len: u64,
) -> Result<(u64, u64, u64, u64)> {
    let (eocd_off, cd_size32, cd_off32, entries16) = find_eocd(path, file, file_len)?;
    let zip64 = cd_size32 == u32::MAX || cd_off32 == u32::MAX || entries16 == u16::MAX;
    if !zip64 {
        return Ok((
            eocd_off,
            u64::from(cd_size32),
            u64::from(cd_off32),
            u64::from(entries16),
        ));
    }
    find_zip64_cd_bounds(path, file, eocd_off)
}

pub(crate) fn find_eocd(
    path: &Path,
    file: &mut (impl Read + Seek),
    file_len: u64,
) -> Result<(u64, u32, u32, u16)> {
    if file_len < EOCD_MIN {
        return Err(not_zip(path));
    }
    let scan_len = file_len.min(EOCD_MIN + EOCD_MAX_COMMENT);
    let start = file_len - scan_len;
    file.seek(SeekFrom::Start(start))
        .map_err(|source| io_at(source, path))?;
    let mut buf = vec![0u8; scan_len as usize];
    file.read_exact(&mut buf)
        .map_err(|source| io_at(source, path))?;

    let mut i = buf.len() - EOCD_MIN as usize;
    loop {
        if buf[i..i + 4] == EOCD_MAGIC {
            let comment_len = u16::from_le_bytes([buf[i + 20], buf[i + 21]]) as usize;
            if i + 22 + comment_len == buf.len() {
                let cd_size = u32::from_le_bytes(buf[i + 12..i + 16].try_into().unwrap());
                let cd_off = u32::from_le_bytes(buf[i + 16..i + 20].try_into().unwrap());
                let entries = u16::from_le_bytes([buf[i + 10], buf[i + 11]]);
                return Ok((start + i as u64, cd_size, cd_off, entries));
            }
        }
        if i == 0 {
            break;
        }
        i -= 1;
    }
    Err(not_zip(path))
}

fn find_zip64_cd_bounds(
    path: &Path,
    file: &mut (impl Read + Seek),
    eocd_off: u64,
) -> Result<(u64, u64, u64, u64)> {
    if eocd_off < ZIP64_LOCATOR_LEN {
        return Err(not_zip(path));
    }
    let loc_off = eocd_off - ZIP64_LOCATOR_LEN;
    file.seek(SeekFrom::Start(loc_off))
        .map_err(|source| io_at(source, path))?;
    let mut loc = [0u8; ZIP64_LOCATOR_LEN as usize];
    file.read_exact(&mut loc)
        .map_err(|source| io_at(source, path))?;
    if loc[..4] != ZIP64_LOCATOR_MAGIC {
        return Err(not_zip(path));
    }

    if loc_off < ZIP64_EOCD_MIN {
        return Err(not_zip(path));
    }
    let scan = loc_off.min(ZIP64_SCAN);
    let start = loc_off - scan;
    file.seek(SeekFrom::Start(start))
        .map_err(|source| io_at(source, path))?;
    let mut buf = vec![0u8; scan as usize];
    file.read_exact(&mut buf)
        .map_err(|source| io_at(source, path))?;

    if buf.len() < ZIP64_EOCD_MIN as usize {
        return Err(not_zip(path));
    }
    let mut i = buf.len() - ZIP64_EOCD_MIN as usize;
    loop {
        if buf[i..i + 4] == ZIP64_EOCD_MAGIC {
            let rec_size = u64::from_le_bytes(buf[i + 4..i + 12].try_into().unwrap());
            let rec_len = 12u64.saturating_add(rec_size);
            if start + i as u64 + rec_len == loc_off && i + ZIP64_EOCD_MIN as usize <= buf.len() {
                let cd_size = u64::from_le_bytes(buf[i + 40..i + 48].try_into().unwrap());
                let cd_off = u64::from_le_bytes(buf[i + 48..i + 56].try_into().unwrap());
                let entries = u64::from_le_bytes(buf[i + 32..i + 40].try_into().unwrap());
                return Ok((start + i as u64, cd_size, cd_off, entries));
            }
        }
        if i == 0 {
            break;
        }
        i -= 1;
    }
    Err(not_zip(path))
}

fn confirm_zip_at(
    path: &Path,
    file: &mut (impl Read + Seek),
    prefix_len: u64,
    empty: bool,
) -> Result<()> {
    file.seek(SeekFrom::Start(prefix_len))
        .map_err(|source| io_at(source, path))?;
    let mut magic = [0u8; 4];
    let n = file
        .read(&mut magic)
        .map_err(|source| io_at(source, path))?;
    if n == 4 && magic == LOCAL_FILE_MAGIC {
        return Ok(());
    }
    if n == 4 && magic == EOCD_MAGIC && empty {
        return Ok(());
    }
    Err(not_zip(path))
}

/// Same dual-hasher chunk loop as `hash_both`, streamed so the JAR is not loaded whole.
fn hash_source(file: &mut File) -> io::Result<([u8; 32], [u8; 32])> {
    file.seek(SeekFrom::Start(0))?;
    let mut b3 = blake3::Hasher::new();
    let mut sha = Sha256::new();
    let mut buf = [0u8; HASH_CHUNK];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        b3.update(&buf[..n]);
        sha.update(&buf[..n]);
    }
    Ok((*b3.finalize().as_bytes(), sha.finalize().into()))
}

pub(crate) fn io_at(source: io::Error, path: &Path) -> AyzenpackError {
    AyzenpackError::Io {
        source,
        path: Some(path.to_path_buf()),
    }
}

fn map_archive_open_err(err: zip::result::ZipError, path: &Path) -> AyzenpackError {
    match err {
        zip::result::ZipError::InvalidArchive(_) => AyzenpackError::NotZip {
            path: path.to_path_buf(),
        },
        zip::result::ZipError::UnsupportedArchive(msg)
            if msg == zip::result::ZipError::PASSWORD_REQUIRED =>
        {
            AyzenpackError::Encrypted {
                path: path.to_path_buf(),
            }
        }
        zip::result::ZipError::Io(source) => io_at(source, path),
        source => AyzenpackError::Zip {
            source,
            path: path.to_path_buf(),
        },
    }
}

fn map_entry_err(err: zip::result::ZipError, path: &Path) -> AyzenpackError {
    match err {
        zip::result::ZipError::UnsupportedArchive(msg)
            if msg == zip::result::ZipError::PASSWORD_REQUIRED =>
        {
            AyzenpackError::Encrypted {
                path: path.to_path_buf(),
            }
        }
        zip::result::ZipError::Io(source) => io_at(source, path),
        source => AyzenpackError::Zip {
            source,
            path: path.to_path_buf(),
        },
    }
}

#[cfg(test)]
#[path = "../tests/spring_launch.rs"]
mod spring_launch;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read, Seek, Write};
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    use super::spring_launch::spring_boot_launch_script;

    fn make_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut z = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, data) in files {
            z.start_file(*name, opts).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap().into_inner()
    }

    #[test]
    fn prefix_len_zero_for_normal_zip() {
        let zip = make_zip(&[("a.txt", b"hello")]);
        assert_eq!(&zip[..4], &LOCAL_FILE_MAGIC);
        let mut cur = Cursor::new(zip);
        assert_eq!(detect_zip_prefix(Path::new("t.jar"), &mut cur).unwrap(), 0);
    }

    #[test]
    fn prefix_len_from_eocd_math_on_script_plus_zip() {
        // Guards scanning for the first PK\x03\x04 instead of EOCD extra-data math.
        let zip = make_zip(&[("a.txt", b"hello")]);
        let launcher = b"#!/bin/bash\n# :: Spring Boot ::\n# launcher\n";
        let mut wrapped = launcher.to_vec();
        wrapped.extend_from_slice(&zip);
        let expect = launcher.len() as u64;
        let mut cur = Cursor::new(wrapped);
        assert_eq!(
            detect_zip_prefix(Path::new("app.jar"), &mut cur).unwrap(),
            expect
        );
    }

    #[test]
    fn shebang_without_zip_is_not_zip() {
        let mut cur = Cursor::new(b"#!/bin/bash\necho no zip here\n".to_vec());
        let err = detect_zip_prefix(Path::new("script.sh"), &mut cur).unwrap_err();
        assert!(matches!(err, AyzenpackError::NotZip { .. }));
    }

    fn adjust_self_extracting_offsets(buf: &mut [u8], delta: u32) {
        let eocd = {
            let mut i = buf.len() - EOCD_MIN as usize;
            loop {
                if buf[i..i + 4] == EOCD_MAGIC {
                    let comment_len = u16::from_le_bytes([buf[i + 20], buf[i + 21]]) as usize;
                    if i + 22 + comment_len == buf.len() {
                        break i;
                    }
                }
                assert!(i > 0, "test zip must have EOCD");
                i -= 1;
            }
        };
        let cd_size = u32::from_le_bytes(buf[eocd + 12..eocd + 16].try_into().unwrap()) as usize;
        let cd_off = u32::from_le_bytes(buf[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
        let phys_cd = cd_off + delta as usize;
        let mut i = phys_cd;
        let cd_end = phys_cd + cd_size;
        while i + 46 <= cd_end {
            assert_eq!(&buf[i..i + 4], b"PK\x01\x02");
            let name_len = u16::from_le_bytes([buf[i + 28], buf[i + 29]]) as usize;
            let extra_len = u16::from_le_bytes([buf[i + 30], buf[i + 31]]) as usize;
            let comment_len = u16::from_le_bytes([buf[i + 32], buf[i + 33]]) as usize;
            let local_off = u32::from_le_bytes(buf[i + 42..i + 46].try_into().unwrap());
            buf[i + 42..i + 46].copy_from_slice(&(local_off + delta).to_le_bytes());
            i += 46 + name_len + extra_len + comment_len;
        }
        assert_eq!(i, cd_end);
        let new_cd = u32::try_from(cd_off).unwrap() + delta;
        buf[eocd + 16..eocd + 20].copy_from_slice(&new_cd.to_le_bytes());
    }

    /// 0.1.4 extra-data math: `(eocd_off - cd_size) - recorded_cd_off`.
    fn eocd_extra(file: &mut (impl Read + Seek)) -> Option<u64> {
        let file_len = file.seek(SeekFrom::End(0)).ok()?;
        let (eocd_off, cd_size, cd_off, _) = find_cd_bounds(Path::new("t"), file, file_len).ok()?;
        eocd_off.checked_sub(cd_size)?.checked_sub(cd_off)
    }

    fn make_zip64(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut z = ZipWriter::new(Cursor::new(Vec::new()));
        z.set_zip64_comment(Some(""));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(true);
        for (name, data) in files {
            z.start_file(*name, opts).unwrap();
            z.write_all(data).unwrap();
        }
        let zip = z.finish().unwrap().into_inner();
        assert!(zip.windows(4).any(|w| w == ZIP64_EOCD_MAGIC));
        zip
    }

    #[test]
    fn prefix_len_on_zip_a_adjusted_script_plus_zip() {
        // extra == 0 after zip -A; confirm_zip_at(0) would read #! and return NotZip.
        let zip = make_zip(&[("a.txt", b"hello")]);
        let launcher = b"#!/bin/bash\n# :: Spring Boot ::\n# launcher\n";
        let mut wrapped = launcher.to_vec();
        wrapped.extend_from_slice(&zip);
        adjust_self_extracting_offsets(&mut wrapped, launcher.len() as u32);

        let mut extra_cur = Cursor::new(wrapped.clone());
        assert_eq!(
            eocd_extra(&mut extra_cur),
            Some(0),
            "0.1.4 extra-data math is 0 after zip -A"
        );

        let mut cur = Cursor::new(wrapped.clone());
        let layout = detect_zip_layout(Path::new("app.jar"), &mut cur).unwrap();
        assert_eq!(layout.prefix_len, launcher.len() as u64);
        assert_eq!(layout.view_shift, 0, "adjusted offsets are file-absolute");
        assert_ne!(&wrapped[..4], &LOCAL_FILE_MAGIC);
        assert_eq!(
            &wrapped[launcher.len()..launcher.len() + 4],
            &LOCAL_FILE_MAGIC
        );
    }

    #[test]
    fn first_pk_succeeds_when_eocd_math_is_skipped() {
        // Official launch.script; 0.1.4 extra-data math is not called.
        let launcher = spring_boot_launch_script();
        assert!(
            launcher.len() > 200,
            "must be the official chkconfig/INIT INFO script, not a 2-line shebang"
        );
        assert!(
            !launcher.windows(4).any(|w| w == LOCAL_FILE_MAGIC),
            "launcher must not contain PK\\x03\\x04"
        );
        let zip = make_zip(&[("App.class", b"class-bytes")]);
        let mut wrapped = launcher.to_vec();
        wrapped.extend_from_slice(&zip);
        let file_len = wrapped.len() as u64;

        let mut extra_cur = Cursor::new(wrapped.clone());
        let extra = eocd_extra(&mut extra_cur).expect("well-formed zip has EOCD extra");
        assert_eq!(
            extra,
            launcher.len() as u64,
            "this fixture is unadjusted; extra happens to match first PK"
        );

        let mut cur = Cursor::new(wrapped);
        let layout = layout_from_first_pk(Path::new("app.jar"), &mut cur, file_len)
            .unwrap()
            .expect("first PK\\x03\\x04 + ZipArchive must open without EOCD math");
        assert_eq!(layout.prefix_len, launcher.len() as u64);
        assert_eq!(
            layout.view_shift,
            launcher.len() as u64,
            "unadjusted: ZipView shifts to first PK"
        );
    }

    #[test]
    fn first_pk_succeeds_when_eocd_extra_disagrees() {
        // zip -A: extra == 0, first PK is the launcher length. 0.1.4 would NotZip.
        let launcher = spring_boot_launch_script();
        let zip = make_zip(&[("App.class", b"class-bytes")]);
        let mut wrapped = launcher.to_vec();
        wrapped.extend_from_slice(&zip);
        adjust_self_extracting_offsets(&mut wrapped, u32::try_from(launcher.len()).unwrap());
        let file_len = wrapped.len() as u64;

        let mut extra_cur = Cursor::new(wrapped.clone());
        let extra = eocd_extra(&mut extra_cur).expect("EOCD still present");
        assert_eq!(extra, 0);
        assert_ne!(extra, launcher.len() as u64);

        let mut cur = Cursor::new(wrapped);
        let layout = layout_from_first_pk(Path::new("app.jar"), &mut cur, file_len)
            .unwrap()
            .expect("first PK path must work when extra disagrees");
        assert_eq!(layout.prefix_len, launcher.len() as u64);
        assert_eq!(layout.view_shift, 0);
    }

    #[test]
    fn first_pk_official_script_plus_zip64() {
        // Zip64 EOCD sits between CD and classic EOCD. find_cd_bounds must
        // prefer the locator so extra == first PK (not the inflated 0.1.4 math).
        let launcher = spring_boot_launch_script();
        let zip = make_zip64(&[("BOOT-INF/lib/dep.jar", b"nested-zip64-opaque")]);
        let mut wrapped = launcher.to_vec();
        wrapped.extend_from_slice(&zip);
        let file_len = wrapped.len() as u64;

        let mut extra_cur = Cursor::new(wrapped.clone());
        let extra = eocd_extra(&mut extra_cur).expect("classic EOCD still present");
        assert_eq!(
            extra,
            launcher.len() as u64,
            "Zip64 locator must yield CD start, not inflate extra through the footer"
        );

        let mut cur = Cursor::new(wrapped);
        let layout = layout_from_first_pk(Path::new("app.jar"), &mut cur, file_len)
            .unwrap()
            .expect("first PK + ZipArchive must open Zip64 after launch.script");
        assert_eq!(layout.prefix_len, launcher.len() as u64);
        assert_eq!(layout.view_shift, launcher.len() as u64);
    }

    #[test]
    fn empty_zip_with_prefix_confirms_eocd() {
        let zip = make_zip(&[]);
        assert_eq!(&zip[..4], &EOCD_MAGIC);
        let launcher = b"#!/bin/sh\n";
        let mut wrapped = launcher.to_vec();
        wrapped.extend_from_slice(&zip);
        let mut cur = Cursor::new(wrapped);
        assert_eq!(
            detect_zip_prefix(Path::new("empty.jar"), &mut cur).unwrap(),
            launcher.len() as u64
        );
    }

    /// Issue #24: first `PK\x03\x04` in the stub is a decoy, not the ZIP.
    const DECOY_LAUNCHER: &[u8] = b"#!/bin/bash\n# decoy PK\x03\x04 here\nexit 0\n";

    #[test]
    fn cd_first_local_skips_decoy_pk_in_stub() {
        assert_eq!(DECOY_LAUNCHER.len(), 37);
        assert_eq!(
            DECOY_LAUNCHER
                .windows(4)
                .position(|w| w == LOCAL_FILE_MAGIC),
            Some(20)
        );
        let zip = make_zip(&[("App.class", b"hello-app")]);
        let mut wrapped = DECOY_LAUNCHER.to_vec();
        wrapped.extend_from_slice(&zip);
        let mut cur = Cursor::new(wrapped);
        let layout = detect_zip_layout(Path::new("falsepk.jar"), &mut cur).unwrap();
        assert_eq!(layout.prefix_len, DECOY_LAUNCHER.len() as u64);
        assert_eq!(layout.view_shift, DECOY_LAUNCHER.len() as u64);
    }

    #[test]
    fn cd_first_local_skips_decoy_pk_after_zip_a() {
        let zip = make_zip(&[("App.class", b"hello-app")]);
        let mut wrapped = DECOY_LAUNCHER.to_vec();
        wrapped.extend_from_slice(&zip);
        adjust_self_extracting_offsets(&mut wrapped, DECOY_LAUNCHER.len() as u32);
        let mut extra_cur = Cursor::new(wrapped.clone());
        assert_eq!(eocd_extra(&mut extra_cur), Some(0));
        let mut cur = Cursor::new(wrapped);
        let layout = detect_zip_layout(Path::new("falsepkA.jar"), &mut cur).unwrap();
        assert_eq!(layout.prefix_len, DECOY_LAUNCHER.len() as u64);
        assert_eq!(layout.view_shift, 0);
    }

    #[test]
    fn cd_first_local_skips_decoy_pk_plus_zip64() {
        let zip = make_zip64(&[("BOOT-INF/lib/dep.jar", b"nested-zip64-opaque")]);
        let mut wrapped = DECOY_LAUNCHER.to_vec();
        wrapped.extend_from_slice(&zip);
        let mut extra_cur = Cursor::new(wrapped.clone());
        let extra = eocd_extra(&mut extra_cur).expect("classic EOCD still present");
        assert_eq!(extra, DECOY_LAUNCHER.len() as u64);
        let mut cur = Cursor::new(wrapped);
        let layout = detect_zip_layout(Path::new("falsepk64.jar"), &mut cur).unwrap();
        assert_eq!(layout.prefix_len, DECOY_LAUNCHER.len() as u64);
        assert_eq!(layout.view_shift, DECOY_LAUNCHER.len() as u64);
    }

    #[test]
    fn empty_zip_a_adjusted_prefix() {
        // Issue #25: empty ZIP has no PK\x03\x04; zip -A makes extra == 0.
        let zip = make_zip(&[]);
        let launcher = b"#!/bin/bash\nexit 0\n";
        let mut wrapped = launcher.to_vec();
        wrapped.extend_from_slice(&zip);
        adjust_self_extracting_offsets(&mut wrapped, launcher.len() as u32);
        assert!(!wrapped.windows(4).any(|w| w == LOCAL_FILE_MAGIC));
        let mut extra_cur = Cursor::new(wrapped.clone());
        assert_eq!(eocd_extra(&mut extra_cur), Some(0));
        let mut cur = Cursor::new(wrapped);
        let layout = detect_zip_layout(Path::new("empty_zipA.jar"), &mut cur).unwrap();
        assert_eq!(layout.prefix_len, launcher.len() as u64);
        assert_eq!(layout.view_shift, 0);
    }

    #[test]
    fn zip_view_start_seek_is_prefix_relative() {
        let mut buf = b"PREFIXpayload".to_vec();
        let mut view = ZipView::new(Cursor::new(&mut buf), 6);
        assert_eq!(view.seek(SeekFrom::Start(0)).unwrap(), 0);
        let mut got = [0u8; 7];
        view.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"payload");
        assert_eq!(view.seek(SeekFrom::Start(3)).unwrap(), 3);
    }

    /// Complete inner zip (own EOCD) stored as an opaque outer member.
    fn complete_inner_zip(i: u8) -> Vec<u8> {
        let mut z = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        z.start_file(format!("com/Lib{i}.class"), opts).unwrap();
        z.write_all(&[i; 2048]).unwrap();
        let bytes = z.finish().unwrap().into_inner();
        assert!(
            bytes.windows(4).any(|w| w == EOCD_MAGIC),
            "inner must be a complete zip"
        );
        bytes
    }

    fn store_nested_fat(zip_a: bool) -> (Vec<u8>, u64) {
        let launcher = spring_boot_launch_script();
        let lib0 = complete_inner_zip(0);
        let lib1 = complete_inner_zip(1);
        let mut z = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        z.start_file("App.class", opts).unwrap();
        z.write_all(b"class-bytes-outer").unwrap();
        z.start_file("BOOT-INF/lib/lib0.jar", opts).unwrap();
        z.write_all(&lib0).unwrap();
        z.start_file("BOOT-INF/lib/lib1.jar", opts).unwrap();
        z.write_all(&lib1).unwrap();
        let zip = z.finish().unwrap().into_inner();
        assert!(
            !zip.windows(4).any(|w| w == ZIP64_EOCD_MAGIC),
            "classic u32 CD only"
        );
        let mut out = launcher.to_vec();
        out.extend_from_slice(&zip);
        if zip_a {
            adjust_self_extracting_offsets(&mut out, launcher.len() as u32);
        }
        (out, launcher.len() as u64)
    }

    #[test]
    fn zip_a_store_nested_latches_on_prefix_shift_then_layout_uses_zero() {
        // Latch proof: ZipArchive::new(ZipView(prefix)) — not ZipArchive::new(File).
        let (bytes, prefix) = store_nested_fat(true);
        let file_len = bytes.len() as u64;
        let mut cur = Cursor::new(bytes.clone());
        let (_, _, _, outer) =
            find_cd_bounds(Path::new("fat-zipa.jar"), &mut cur, file_len).unwrap();
        assert_eq!(outer, 3, "App + 2 STORE BOOT-INF/lib");
        cur.seek(SeekFrom::Start(0)).unwrap();
        let mut latched = ZipArchive::new(ZipView::new(&mut cur, prefix)).expect("0.2.2 latch Ok");
        assert_ne!(
            latched.len() as u64,
            outer,
            "prefix shift must bind an inner EOCD, not the outer CD"
        );
        let names: Vec<String> = (0..latched.len())
            .map(|i| latched.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(
            !names
                .iter()
                .any(|n| n == "App.class" || n.starts_with("BOOT-INF/lib/")),
            "latched names must omit the outer listing, got {names:?}"
        );

        let mut cur = Cursor::new(bytes);
        let layout = detect_zip_layout(Path::new("fat-zipa.jar"), &mut cur).unwrap();
        assert_eq!(layout.prefix_len, prefix);
        assert_eq!(layout.view_shift, 0, "zip -A must use file-absolute view");
        cur.seek(SeekFrom::Start(0)).unwrap();
        let mut chosen = ZipArchive::new(ZipView::new(&mut cur, layout.view_shift)).unwrap();
        assert_eq!(chosen.len() as u64, outer);
        let chosen_names: Vec<String> = (0..chosen.len())
            .map(|i| chosen.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(chosen_names.iter().any(|n| n == "App.class"));
        assert_eq!(
            chosen_names
                .iter()
                .filter(|n| n.starts_with("BOOT-INF/lib/"))
                .count(),
            2
        );
    }

    #[test]
    fn unadjusted_store_nested_keeps_prefix_shift() {
        let (bytes, prefix) = store_nested_fat(false);
        let file_len = bytes.len() as u64;
        let mut cur = Cursor::new(bytes.clone());
        let (_, _, _, outer) = find_cd_bounds(Path::new("fat.jar"), &mut cur, file_len).unwrap();
        let mut cur = Cursor::new(bytes);
        let layout = detect_zip_layout(Path::new("fat.jar"), &mut cur).unwrap();
        assert_eq!(layout.prefix_len, prefix);
        assert_eq!(
            layout.view_shift, prefix,
            "unadjusted Spring default must not invert to view_shift=0"
        );
        cur.seek(SeekFrom::Start(0)).unwrap();
        let chosen = ZipArchive::new(ZipView::new(&mut cur, layout.view_shift)).unwrap();
        assert_eq!(chosen.len() as u64, outer);
    }

    /// Same STORE nested zip as `store_nested_fat(false)`, but the prefix starts
    /// with `PK\x03\x04` so `detect_zip_layout` returns `prefix_len = 0`.
    fn pk_start_unadjusted_store_nested_latch() -> Vec<u8> {
        let (bytes, prefix) = store_nested_fat(false);
        assert!(
            prefix > 4,
            "decoy prefix must be large enough for an inner EOCD to fall in the ZipArchive search window"
        );
        let mut pk = b"PK\x03\x04".to_vec();
        pk.resize(prefix as usize, 0xAA);
        let mut out = pk;
        out.extend_from_slice(&bytes[prefix as usize..]);
        out
    }

    #[test]
    fn pk_start_store_nested_scan_from_bytes_latch_is_err() {
        let bytes = pk_start_unadjusted_store_nested_latch();
        assert_eq!(&bytes[..4], &LOCAL_FILE_MAGIC);
        let file_len = bytes.len() as u64;
        let mut cur = Cursor::new(bytes.clone());
        let layout = detect_zip_layout(Path::new("latch.jar"), &mut cur).unwrap();
        assert_eq!(layout.prefix_len, 0, "PK-start must skip prefix detection");
        assert_eq!(layout.view_shift, 0);

        let mut cur = Cursor::new(bytes.clone());
        let (_, _, _, homemade) =
            find_cd_bounds(Path::new("latch.jar"), &mut cur, file_len).unwrap();
        assert_eq!(homemade, 3, "App + 2 STORE BOOT-INF/lib");

        let mut cur = Cursor::new(bytes.clone());
        let mut latched = ZipArchive::new(ZipView::new(&mut cur, 0)).expect("latch Ok");
        assert_ne!(
            latched.len() as u64,
            homemade,
            "ZipArchive on PK-start unadjusted nested must bind an inner EOCD"
        );
        let names: Vec<String> = (0..latched.len())
            .map(|i| latched.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(
            !names
                .iter()
                .any(|n| n == "App.class" || n.starts_with("BOOT-INF/lib/")),
            "latched names must omit the outer listing, got {names:?}"
        );

        match scan_from_bytes(&bytes, u64::MAX) {
            Err(AyzenpackError::FormatOwned(msg)) => {
                assert!(
                    msg.contains("homemade central directory count"),
                    "got {msg}"
                );
            }
            Err(other) => panic!("latch must be FormatOwned, got {other:?}"),
            Ok(_) => panic!("latch must be Err, not a latched listing"),
        }
    }

    #[test]
    fn pk_start_store_nested_outer_scan_refuses_latch() {
        let bytes = pk_start_unadjusted_store_nested_latch();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latch.jar");
        std::fs::write(&path, &bytes).unwrap();
        match scan_jar(&path, u64::MAX) {
            Err(AyzenpackError::FormatOwned(msg)) => {
                assert!(
                    msg.contains("homemade central directory count"),
                    "got {msg}"
                );
            }
            Err(other) => panic!("outer latch must be FormatOwned, got {other:?}"),
            Ok(scanned) => panic!(
                "outer scan must not pack a latched listing, got {:?}",
                scanned.entries.iter().map(|e| &e.name).collect::<Vec<_>>()
            ),
        }
    }
}
