//! Stream ZIP/JAR entries in central-directory order without a class forest.
//!
//! `ScannedEntry` is metadata only. Payloads are yielded one at a time through
//! `for_each_jar_entry` and dropped before the next entry is inflated.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
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
const EOCD_MAGIC: [u8; 4] = *b"PK\x05\x06";

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
    pub entries: Vec<ScannedEntry>,
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
    let mut file = File::open(path).map_err(|source| io_at(source, path))?;
    let source_size = file.metadata().map_err(|source| io_at(source, path))?.len();
    ensure_zip_magic(path, &mut file)?;
    let (source_blake3, source_sha256) =
        hash_source(&mut file).map_err(|source| io_at(source, path))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_at(source, path))?;

    let reader = BufReader::with_capacity(ZIP_BUF, file);
    let mut archive = ZipArchive::new(reader).map_err(|err| map_archive_open_err(err, path))?;
    let comment = String::from_utf8_lossy(archive.comment()).into_owned();

    let mut entries = Vec::with_capacity(archive.len());
    let mut has_sf = false;
    let mut has_block = false;

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
        match signature_kind(&name) {
            Some(SignatureKind::Sf) => has_sf = true,
            Some(SignatureKind::Block) => has_block = true,
            None => {}
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

        f(&meta, Some(buf.as_slice()))?;
        drop(buf);
        entries.push(meta);
    }

    Ok(ScannedJar {
        source_path: path.to_path_buf(),
        source_size,
        source_blake3,
        source_sha256,
        comment,
        signed: has_sf && has_block,
        entries,
    })
}

enum SignatureKind {
    Sf,
    Block,
}

fn signature_kind(name: &str) -> Option<SignatureKind> {
    let lower = name.replace('\\', "/").to_ascii_lowercase();
    let rest = lower.strip_prefix("meta-inf/")?;
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    if rest.ends_with(".sf") {
        Some(SignatureKind::Sf)
    } else if rest.ends_with(".rsa") || rest.ends_with(".dsa") || rest.ends_with(".ec") {
        Some(SignatureKind::Block)
    } else {
        None
    }
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

fn ensure_zip_magic(path: &Path, file: &mut File) -> Result<()> {
    let mut magic = [0u8; 4];
    let n = file
        .read(&mut magic)
        .map_err(|source| io_at(source, path))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|source| io_at(source, path))?;
    if n == 4 && (magic == LOCAL_FILE_MAGIC || magic == EOCD_MAGIC) {
        return Ok(());
    }
    Err(AyzenpackError::NotZip {
        path: path.to_path_buf(),
    })
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

fn io_at(source: io::Error, path: &Path) -> AyzenpackError {
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
