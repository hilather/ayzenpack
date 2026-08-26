//! Dehydrate JARs into one `.ayz` (dedup BLOBs + embedded manifest).
//!
//! Payloads are hashed, written if first-seen, then dropped. Rehydrate is a later PR.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{AyzenpackError, Result};
use crate::format::{
    write_header, write_record, write_trailer, FileHeader, Record, Trailer, BUF_WRITER_CAP,
    REC_BLOB, TRAILER_LEN,
};
use crate::hashutil::{hash_both, hex_lower};
use crate::manifest::{Blob, Entry, Jar, Manifest, Stats, MANIFEST_FORMAT};
use crate::scan::for_each_jar_entry;
use crate::stats::dedup_ratio;

const DEFAULT_LEVEL: i32 = 3;
const DEFAULT_MAX_ENTRY: u64 = 2_147_483_647;

pub struct DehydrateOptions {
    pub output: PathBuf,
    pub inputs: Vec<PathBuf>,
    pub recursive: bool,
    pub sort_inputs: bool,
    pub level: i32,
    pub max_entry_bytes: u64,
    pub strict: bool,
    pub fail_on_signed: bool,
    pub dry_run: bool,
    pub write_sidecar_manifest: Option<PathBuf>,
    pub pretty_manifest: bool,
    pub follow_symlinks: bool,
    pub exclude: Vec<String>,
    pub quiet: bool,
    pub verbose: bool,
    pub json_logs: bool,
}

impl Default for DehydrateOptions {
    fn default() -> Self {
        Self {
            output: PathBuf::new(),
            inputs: Vec::new(),
            recursive: false,
            sort_inputs: false,
            level: DEFAULT_LEVEL,
            max_entry_bytes: DEFAULT_MAX_ENTRY,
            strict: false,
            fail_on_signed: false,
            dry_run: false,
            write_sidecar_manifest: None,
            pretty_manifest: false,
            follow_symlinks: false,
            exclude: Vec::new(),
            quiet: false,
            verbose: false,
            json_logs: false,
        }
    }
}

/// Returned by [`dehydrate`]. Field names match manifest `stats` plus `output_len`.
#[derive(Debug, Clone, PartialEq)]
pub struct DehydrateSummary {
    pub output_len: u64,
    pub jar_count: u64,
    pub entry_count: u64,
    pub file_entry_count: u64,
    pub unique_blob_count: u64,
    pub bytes_in_jars: u64,
    pub bytes_uncompressed_entries: u64,
    pub bytes_unique_blobs: u64,
    pub dedup_ratio: f64,
    pub signed_jars: Vec<String>,
}

struct AyzWriter {
    enc: zstd::stream::Encoder<'static, BufWriter<File>>,
    header_total: u64,
    header_len: u32,
}

pub fn dehydrate(opts: &DehydrateOptions) -> Result<DehydrateSummary> {
    if !(1..=19).contains(&opts.level) {
        return Err(AyzenpackError::Usage(format!(
            "zstd level must be 1..=19, got {}",
            opts.level
        )));
    }

    let mut inputs = opts.inputs.clone();
    if opts.sort_inputs {
        inputs.sort();
    }
    inputs = dedupe_inputs(inputs, opts);

    let created_unix = if opts.sort_inputs { 0 } else { unix_now() };
    let header = FileHeader::new(opts.level, created_unix);

    let mut writer = if opts.dry_run {
        None
    } else {
        Some(start_ayz_file(&opts.output, &header)?)
    };

    let mut seen: HashMap<[u8; 32], usize> = HashMap::new();
    let mut blobs: Vec<Blob> = Vec::new();
    let mut jars: Vec<Jar> = Vec::new();
    let mut signed_jars: Vec<String> = Vec::new();
    let mut used_names: HashMap<String, u32> = HashMap::new();
    let mut first_seen = blake3::Hasher::new();
    let mut entry_count = 0u64;
    let mut file_entry_count = 0u64;
    let mut bytes_in_jars = 0u64;
    let mut bytes_uncompressed_entries = 0u64;

    for path in &inputs {
        match fs::metadata(path) {
            Err(source) => {
                let err = AyzenpackError::Io {
                    source,
                    path: Some(path.clone()),
                };
                if opts.strict {
                    return Err(err);
                }
                warn(opts, &err.to_string());
                continue;
            }
            Ok(meta) if meta.is_dir() => {
                let msg = format!(
                    "skipping directory {} (recursive walk is not enabled)",
                    path.display()
                );
                if opts.strict {
                    return Err(AyzenpackError::Usage(msg));
                }
                warn(opts, &msg);
                continue;
            }
            Ok(_) => {}
        }

        let mut jar_entries = Vec::new();
        let scanned = for_each_jar_entry(path, opts.max_entry_bytes, |meta, payload| {
            entry_count += 1;
            if meta.is_dir {
                jar_entries.push(entry_from_scan(meta, None, None));
                return Ok(());
            }
            let buf = payload.ok_or_else(|| {
                AyzenpackError::FormatOwned(format!(
                    "missing payload for file entry {}!{}",
                    path.display(),
                    meta.name
                ))
            })?;
            file_entry_count += 1;
            bytes_uncompressed_entries += buf.len() as u64;

            if opts.strict && meta.name_raw_hex.is_some() {
                return Err(AyzenpackError::FormatOwned(format!(
                    "non-UTF-8 entry name in {}!{}",
                    path.display(),
                    meta.name
                )));
            }

            let recomputed = crc32fast::hash(buf);
            if recomputed != meta.crc32 {
                let msg = format!(
                    "CRC mismatch for {}!{}: header {:#x} computed {:#x}",
                    path.display(),
                    meta.name,
                    meta.crc32,
                    recomputed
                );
                if opts.strict {
                    return Err(AyzenpackError::FormatOwned(msg));
                }
                warn(opts, &msg);
            }

            let (b3, s256) = hash_both(buf);
            if let Some(&i) = seen.get(&b3) {
                blobs[i].ref_count += 1;
            } else {
                if let Some(ref mut w) = writer {
                    // Write from the scan buffer; do not clone a second payload Vec.
                    write_blob_record(&mut w.enc, &b3, buf)?;
                }
                first_seen.update(&b3);
                seen.insert(b3, blobs.len());
                blobs.push(Blob {
                    blake3: hex_lower(&b3),
                    sha256: hex_lower(&s256),
                    size: buf.len() as u64,
                    ref_count: 1,
                });
            }
            jar_entries.push(entry_from_scan(
                meta,
                Some(hex_lower(&b3)),
                Some(hex_lower(&s256)),
            ));
            Ok(())
        })?;

        let jar_name = unique_basename(path, &mut used_names)?;
        if scanned.signed {
            signed_jars.push(jar_name.clone());
            let msg = format!("signed JAR {jar_name} (rebuild will break the signature)");
            if opts.fail_on_signed {
                return Err(AyzenpackError::Usage(msg));
            }
            warn(opts, &msg);
        }
        bytes_in_jars += scanned.source_size;
        jars.push(Jar {
            name: jar_name,
            source_path: path.to_string_lossy().into_owned(),
            source_size: scanned.source_size,
            source_blake3: hex_lower(&scanned.source_blake3),
            source_sha256: hex_lower(&scanned.source_sha256),
            comment: scanned.comment,
            signed: scanned.signed,
            entries: jar_entries,
        });
    }

    let bytes_unique_blobs: u64 = blobs.iter().map(|b| b.size).sum();
    let unique_blob_count = blobs.len() as u64;
    let jar_count = jars.len() as u64;
    let ratio = dedup_ratio(bytes_unique_blobs, bytes_uncompressed_entries);
    let stats = Stats {
        jar_count,
        entry_count,
        file_entry_count,
        unique_blob_count,
        bytes_in_jars,
        bytes_uncompressed_entries,
        bytes_unique_blobs,
        dedup_ratio: ratio,
    };
    let manifest = Manifest {
        format: MANIFEST_FORMAT.to_string(),
        version: 1,
        hash_algo: "blake3".into(),
        mode: "content".into(),
        jars,
        blobs,
        stats,
    };
    let json = serde_json::to_vec(&manifest)?;
    let digest = *first_seen.finalize().as_bytes();
    let manifest_len = json.len() as u64;

    let output_len = if let Some(mut w) = writer {
        write_record(&mut w.enc, &Record::Manifest { json })?;
        write_record(&mut w.enc, &Record::End { digest })?;
        finish_ayz_file(
            w,
            manifest_len,
            unique_blob_count,
            bytes_unique_blobs,
            jar_count,
        )?
    } else {
        0
    };

    if !opts.dry_run {
        if let Some(side) = &opts.write_sidecar_manifest {
            write_sidecar(side, &manifest, opts.pretty_manifest)?;
        }
    }

    Ok(DehydrateSummary {
        output_len,
        jar_count,
        entry_count,
        file_entry_count,
        unique_blob_count,
        bytes_in_jars,
        bytes_uncompressed_entries,
        bytes_unique_blobs,
        dedup_ratio: ratio,
        signed_jars,
    })
}

fn start_ayz_file(output: &Path, header: &FileHeader) -> Result<AyzWriter> {
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|source| AyzenpackError::Io {
                source,
                path: Some(output.to_path_buf()),
            })?;
        }
    }
    let mut file = File::create(output).map_err(|source| AyzenpackError::Io {
        source,
        path: Some(output.to_path_buf()),
    })?;
    let header_len = write_header(&mut file, header)?;
    let header_total = file.stream_position().map_err(crate::format::io_error)?;

    let mut enc = zstd::stream::Encoder::new(
        BufWriter::with_capacity(BUF_WRITER_CAP, file),
        header.zstd_level,
    )
    .map_err(crate::format::io_error)?;
    enc.include_checksum(false)
        .map_err(crate::format::io_error)?;
    Ok(AyzWriter {
        enc,
        header_total,
        header_len,
    })
}

/// Finish the zstd frame, measure `payload_bytes`, then write the trailer on the same BufWriter.
/// Measuring after trailer would bake the 64-byte trailer into `payload_bytes`.
fn finish_ayz_file(
    writer: AyzWriter,
    manifest_len: u64,
    blob_count: u64,
    blob_bytes: u64,
    jar_count: u64,
) -> Result<u64> {
    let AyzWriter {
        enc,
        header_total,
        header_len,
    } = writer;
    let mut w = enc.finish().map_err(crate::format::io_error)?;
    w.flush().map_err(crate::format::io_error)?;
    let mid_len = w
        .get_ref()
        .metadata()
        .map_err(crate::format::io_error)?
        .len();
    if mid_len < header_total {
        return Err(AyzenpackError::Format(
            "zstd payload shorter than file header",
        ));
    }
    let payload_bytes = mid_len - header_total;
    let trailer = Trailer {
        payload_bytes,
        manifest_len,
        blob_count,
        blob_bytes,
        jar_count,
        header_len,
        version: 1,
    };
    write_trailer(&mut w, &trailer)?;
    w.flush().map_err(crate::format::io_error)?;
    let file_len = w
        .get_ref()
        .metadata()
        .map_err(crate::format::io_error)?
        .len();
    if file_len != header_total + payload_bytes + TRAILER_LEN {
        return Err(AyzenpackError::Format(
            "file length != header_total + payload_bytes + 64",
        ));
    }
    Ok(file_len)
}

fn write_blob_record<W: Write>(w: &mut W, hash: &[u8; 32], data: &[u8]) -> Result<()> {
    w.write_all(&[REC_BLOB]).map_err(crate::format::io_error)?;
    w.write_all(hash).map_err(crate::format::io_error)?;
    w.write_all(&(data.len() as u64).to_le_bytes())
        .map_err(crate::format::io_error)?;
    w.write_all(data).map_err(crate::format::io_error)?;
    Ok(())
}

fn write_sidecar(path: &Path, manifest: &Manifest, pretty: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|source| AyzenpackError::Io {
                source,
                path: Some(path.to_path_buf()),
            })?;
        }
    }
    let bytes = if pretty {
        serde_json::to_vec_pretty(manifest)?
    } else {
        serde_json::to_vec(manifest)?
    };
    fs::write(path, bytes).map_err(|source| AyzenpackError::Io {
        source,
        path: Some(path.to_path_buf()),
    })
}

fn entry_from_scan(
    meta: &crate::scan::ScannedEntry,
    blob: Option<String>,
    sha256: Option<String>,
) -> Entry {
    Entry {
        name: meta.name.clone(),
        is_dir: meta.is_dir,
        blob,
        sha256,
        crc32: meta.crc32,
        method: meta.method.clone(),
        method_code: meta.method_code,
        uncompressed_size: meta.uncompressed_size,
        compressed_size: meta.compressed_size,
        dos_date: meta.dos_date,
        dos_time: meta.dos_time,
        unix_mode: meta.unix_mode,
        utf8_flag: meta.utf8_flag,
        name_raw_hex: meta.name_raw_hex.clone(),
    }
}

fn unique_basename(path: &Path, used: &mut HashMap<String, u32>) -> Result<String> {
    let base = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| AyzenpackError::UnsafePath(path.display().to_string()))?;
    if base.contains('/') || base.contains('\\') || base == ".." || base == "." {
        return Err(AyzenpackError::UnsafePath(base.to_string()));
    }
    let n = {
        let slot = used.entry(base.to_string()).or_insert(0);
        *slot += 1;
        *slot
    };
    if n == 1 {
        return Ok(base.to_string());
    }
    let p = Path::new(base);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(base);
    match p.extension().and_then(|s| s.to_str()) {
        Some(ext) => Ok(format!("{stem}__{n}.{ext}")),
        None => Ok(format!("{stem}__{n}")),
    }
}

fn dedupe_inputs(inputs: Vec<PathBuf>, opts: &DehydrateOptions) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(inputs.len());
    for p in inputs {
        if !seen.insert(p.clone()) {
            warn(opts, &format!("duplicate input {}, skipping", p.display()));
            continue;
        }
        out.push(p);
    }
    out
}

fn warn(opts: &DehydrateOptions, msg: &str) {
    if opts.quiet {
        return;
    }
    eprintln!("ayzenpack: warning: {msg}");
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_basename_collides_to_underscore_n() {
        let mut used = HashMap::new();
        assert_eq!(
            unique_basename(Path::new("lib/a.jar"), &mut used).unwrap(),
            "a.jar"
        );
        assert_eq!(
            unique_basename(Path::new("other/a.jar"), &mut used).unwrap(),
            "a__2.jar"
        );
        assert_eq!(
            unique_basename(Path::new("a.jar"), &mut used).unwrap(),
            "a__3.jar"
        );
    }

    #[test]
    fn unique_basename_preserves_tar_jar_suffix() {
        let mut used = HashMap::new();
        assert_eq!(
            unique_basename(Path::new("lib.tar.jar"), &mut used).unwrap(),
            "lib.tar.jar"
        );
        assert_eq!(
            unique_basename(Path::new("copy/lib.tar.jar"), &mut used).unwrap(),
            "lib.tar__2.jar"
        );
    }

    #[test]
    fn unique_basename_rejects_dot_dot() {
        let mut used = HashMap::new();
        let err = unique_basename(Path::new(".."), &mut used).unwrap_err();
        assert!(matches!(err, AyzenpackError::UnsafePath(_)));
    }
}
