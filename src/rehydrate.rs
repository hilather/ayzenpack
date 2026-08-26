//! Restore JARs from a `.ayz` archive (functional identity, not ZIP bit-identity).
//!
//! Decode the zstd record stream into a CAS directory, then rebuild each JAR with
//! `ZipWriter`. Directory entries use `add_directory`; DOS `0,0` falls back to
//! `DateTime::default()` (1980-01-01).

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

use crate::cas;
use crate::error::{AyzenpackError, Result};
use crate::format::{read_header, read_record, read_trailer, Record};
use crate::hashutil::{blake3_bytes, hex_lower, parse_blake3_hex};
use crate::manifest::{Manifest, MANIFEST_FORMAT};

const DEFAULT_DEFLATE_LEVEL: i32 = 6;
const ZIP64_BYTES_THR: u64 = 0xFFFF_FFFF;

pub struct RehydrateOptions {
    pub input: PathBuf,
    pub dir: PathBuf,
    pub cas_dir: Option<PathBuf>,
    pub keep_cas: bool,
    pub store_all: bool,
    pub deflate_level: i32,
    pub clean: bool,
    pub overwrite: bool,
    pub only: Vec<String>,
    pub quiet: bool,
    pub verbose: bool,
    pub json_logs: bool,
}

impl Default for RehydrateOptions {
    fn default() -> Self {
        Self {
            input: PathBuf::new(),
            dir: PathBuf::new(),
            cas_dir: None,
            keep_cas: false,
            store_all: false,
            deflate_level: DEFAULT_DEFLATE_LEVEL,
            clean: false,
            overwrite: false,
            only: Vec::new(),
            quiet: false,
            verbose: false,
            json_logs: false,
        }
    }
}

pub fn rehydrate(opts: &RehydrateOptions) -> Result<()> {
    if !(0..=9).contains(&opts.deflate_level) {
        return Err(AyzenpackError::Usage(format!(
            "deflate level must be 0..=9, got {}",
            opts.deflate_level
        )));
    }

    fs::create_dir_all(&opts.dir).map_err(|source| AyzenpackError::Io {
        source,
        path: Some(opts.dir.clone()),
    })?;

    let mut tmp_guard = None;
    let cas_path = match &opts.cas_dir {
        Some(p) => {
            fs::create_dir_all(p).map_err(|source| AyzenpackError::Io {
                source,
                path: Some(p.clone()),
            })?;
            p.clone()
        }
        None => {
            let tmp =
                tempfile::tempdir().map_err(|source| AyzenpackError::Io { source, path: None })?;
            let p = tmp.path().to_path_buf();
            tmp_guard = Some(tmp);
            p
        }
    };

    let manifest = spill_to_cas(&opts.input, &cas_path)?;
    restore_jars(opts, &manifest, &cas_path)?;

    // Default CAS is a tempfile deleted on success unless --keep-cas.
    if opts.keep_cas {
        if let Some(tmp) = tmp_guard.take() {
            let _ = tmp.keep();
        }
    }
    Ok(())
}

fn spill_to_cas(input: &Path, cas_dir: &Path) -> Result<Manifest> {
    let mut file = File::open(input).map_err(|source| AyzenpackError::Io {
        source,
        path: Some(input.to_path_buf()),
    })?;
    let trailer = read_trailer(&mut file)?;
    file.seek(SeekFrom::Start(0))
        .map_err(|source| AyzenpackError::Io {
            source,
            path: Some(input.to_path_buf()),
        })?;
    let _header = read_header(&mut file)?;
    let limited = Read::take(&mut file, trailer.payload_bytes);
    let mut decoder = zstd::stream::Decoder::new(limited)
        .map_err(crate::format::io_error)?
        .single_frame();

    let mut stream_hasher = blake3::Hasher::new();
    let mut seen_manifest = false;
    let mut manifest = None;

    let end_digest = loop {
        let rec = match read_record(&mut decoder) {
            Ok(rec) => rec,
            Err(AyzenpackError::Format("truncated record")) => {
                return Err(AyzenpackError::Format("missing END record"));
            }
            Err(e) => return Err(e),
        };
        match rec {
            Record::Blob { hash, data } => {
                if seen_manifest {
                    return Err(AyzenpackError::Format("BLOB after MANIFEST"));
                }
                let got = blake3_bytes(&data);
                if got != hash {
                    return Err(AyzenpackError::HashMismatch(format!(
                        "blob {}",
                        hex_lower(&hash)
                    )));
                }
                cas::put(cas_dir, &hash, &data)?;
                stream_hasher.update(&hash);
            }
            Record::Manifest { json } => {
                if seen_manifest {
                    return Err(AyzenpackError::Format("multiple MANIFEST records"));
                }
                seen_manifest = true;
                manifest = Some(serde_json::from_slice(&json)?);
            }
            Record::End { digest } => {
                if !seen_manifest {
                    return Err(AyzenpackError::Format("missing MANIFEST record"));
                }
                break digest;
            }
        }
    };

    let manifest: Manifest = manifest.ok_or(AyzenpackError::Format("missing MANIFEST record"))?;

    let stream_digest = *stream_hasher.finalize().as_bytes();
    if stream_digest != end_digest {
        return Err(AyzenpackError::HashMismatch("END digest".into()));
    }

    let mut cat = blake3::Hasher::new();
    for blob in &manifest.blobs {
        cat.update(&parse_blake3_hex(&blob.blake3)?);
    }
    if *cat.finalize().as_bytes() != end_digest {
        return Err(AyzenpackError::HashMismatch("END digest".into()));
    }

    if manifest.format != MANIFEST_FORMAT {
        return Err(AyzenpackError::Format(
            "manifest format must be ayzenpack-manifest",
        ));
    }

    Ok(manifest)
}

fn restore_jars(opts: &RehydrateOptions, manifest: &Manifest, cas_dir: &Path) -> Result<()> {
    if !opts.only.is_empty() {
        for name in &opts.only {
            if !manifest.jars.iter().any(|j| j.name == *name) {
                warn(opts, &format!("--only name not in archive: {name}"));
            }
        }
    }

    for jar in &manifest.jars {
        if !opts.only.is_empty() && !opts.only.iter().any(|n| n == &jar.name) {
            continue;
        }
        check_jar_name(&jar.name)?;
        let dest = opts.dir.join(&jar.name);
        if dest.exists() {
            if opts.clean {
                fs::remove_file(&dest).map_err(|source| AyzenpackError::Io {
                    source,
                    path: Some(dest.clone()),
                })?;
            } else if !opts.overwrite {
                return Err(AyzenpackError::Usage(format!(
                    "refusing to overwrite {} (pass --overwrite)",
                    dest.display()
                )));
            }
        }
        if opts.verbose {
            eprintln!("ayzenpack: restoring {}", jar.name);
        }
        write_jar(opts, jar, cas_dir, &dest)?;
    }
    Ok(())
}

fn write_jar(
    opts: &RehydrateOptions,
    jar: &crate::manifest::Jar,
    cas_dir: &Path,
    dest: &Path,
) -> Result<()> {
    let file = File::create(dest).map_err(|source| AyzenpackError::Io {
        source,
        path: Some(dest.to_path_buf()),
    })?;
    let mut writer = ZipWriter::new(file);
    if !jar.comment.is_empty() {
        writer.set_comment(jar.comment.clone());
    }

    for e in &jar.entries {
        if entry_has_dotdot(&e.name) {
            warn(
                opts,
                &format!("skipping entry with .. in {}!{}", jar.name, e.name),
            );
            continue;
        }

        // Invalid DOS pairs (including 0,0) fall back to 1980-01-01; never from_msdos_unchecked.
        let dt = DateTime::try_from_msdos(e.dos_date, e.dos_time)
            .unwrap_or_else(|_| DateTime::default());

        let stored = e.is_dir || opts.store_all;
        let method = if stored {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        let mut options = SimpleFileOptions::default()
            .compression_method(method)
            .last_modified_time(dt);
        if !stored {
            options = options.compression_level(Some(i64::from(opts.deflate_level)));
        }
        if let Some(mode) = e.unix_mode {
            options = options.unix_permissions(mode);
        }
        if e.uncompressed_size >= ZIP64_BYTES_THR {
            options = options.large_file(true);
        }

        if e.is_dir {
            writer
                .add_directory(&e.name, options)
                .map_err(|err| zip_err(err, dest))?;
            continue;
        }

        let hex = e.blob.as_deref().ok_or_else(|| {
            AyzenpackError::FormatOwned(format!("missing blob for {}!{}", jar.name, e.name))
        })?;
        let hash = parse_blake3_hex(hex)?;
        let bytes = read_cas_blob(cas_dir, &hash)?;
        if blake3_bytes(&bytes) != hash {
            return Err(AyzenpackError::HashMismatch(format!(
                "{}!{} blake3",
                jar.name, e.name
            )));
        }
        let recomputed = crc32fast::hash(&bytes);
        if recomputed != e.crc32 {
            return Err(AyzenpackError::HashMismatch(format!(
                "{}!{} crc32: recorded {:#x} computed {:#x}",
                jar.name, e.name, e.crc32, recomputed
            )));
        }

        writer
            .start_file(&e.name, options)
            .map_err(|err| zip_err(err, dest))?;
        writer
            .write_all(&bytes)
            .map_err(|source| AyzenpackError::Io {
                source,
                path: Some(dest.to_path_buf()),
            })?;
    }

    writer.finish().map_err(|err| zip_err(err, dest))?;
    Ok(())
}

fn read_cas_blob(cas_dir: &Path, hash: &[u8; 32]) -> Result<Vec<u8>> {
    match cas::get(cas_dir, hash) {
        Ok(bytes) => Ok(bytes),
        Err(AyzenpackError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => Err(
            AyzenpackError::HashMismatch(format!("missing blob {}", hex_lower(hash))),
        ),
        Err(e) => Err(e),
    }
}

/// `jar.name` must be a single path segment: reject `..`, `/`, `\`.
fn check_jar_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name == "."
        || name == ".."
    {
        return Err(AyzenpackError::UnsafePath(name.to_string()));
    }
    let mut comps = Path::new(name).components();
    match (comps.next(), comps.next()) {
        (Some(Component::Normal(seg)), None) if seg == name => Ok(()),
        _ => Err(AyzenpackError::UnsafePath(name.to_string())),
    }
}

fn entry_has_dotdot(name: &str) -> bool {
    name.split(['/', '\\']).any(|c| c == "..")
}

fn zip_err(err: zip::result::ZipError, path: &Path) -> AyzenpackError {
    match err {
        zip::result::ZipError::Io(source) => AyzenpackError::Io {
            source,
            path: Some(path.to_path_buf()),
        },
        source => AyzenpackError::Zip {
            source,
            path: path.to_path_buf(),
        },
    }
}

fn warn(opts: &RehydrateOptions, msg: &str) {
    if opts.quiet {
        return;
    }
    eprintln!("ayzenpack: warning: {msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_jar_name_rejects_dotdot_and_separators() {
        // Guards zip-slip via jars[].name (`../x.jar`, `a/b.jar`, `a\b.jar`).
        for name in [
            "../x.jar",
            "a/b.jar",
            "a\\b.jar",
            "..",
            ".",
            "",
            "foo/bar.jar",
        ] {
            let err = check_jar_name(name).unwrap_err();
            assert!(
                matches!(err, AyzenpackError::UnsafePath(ref s) if s == name),
                "expected UnsafePath({name:?}), got {err:?}"
            );
        }
        check_jar_name("a.jar").unwrap();
        check_jar_name("lib__2.jar").unwrap();
        check_jar_name("foo..bar.jar").unwrap();
    }

    #[test]
    fn dos_zero_zero_falls_back_to_default_without_panic() {
        // Guards from_msdos_unchecked / aborting on the common JAR pair 0,0.
        let dt = DateTime::try_from_msdos(0, 0).unwrap_or_else(|_| DateTime::default());
        assert_eq!(dt, DateTime::default());
        assert_eq!(dt.year(), 1980);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 1);
    }
}
