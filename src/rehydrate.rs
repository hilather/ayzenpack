//! Restore JARs from a `.ayz` archive.
//!
//! New packs keep ZIP metadata (local headers + CD tail) and either splice a
//! reproduced bitstream (`cdata_codec` / STORE / legacy `cdata_blob`) or rebuild
//! a valid ZIP with patched sizes. Archives without those fields keep the 0.1.x
//! `ZipWriter` path (functional identity). Prefix bytes are always bit-exact.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

use crate::cas;
use crate::dehydrate::{replace_file, sibling_tmp_path};
use crate::error::{AyzenpackError, Result};
use crate::exact::{
    detect_offset_mode, encode_offset, patch_central_directory, patch_data_descriptor,
    patch_eocd_cd_start, patch_local_rebuild_fields, RebuildPatch,
};
use crate::format::{decode_payload, open_ayz_layout, read_record, Record};
use crate::hashutil::{blake3_bytes, hash_reader, hex_lower, parse_blake3_hex, parse_hex};
use crate::manifest::{Entry, Jar, Manifest, MANIFEST_FORMAT};
use crate::reconstruct::{load_header, reconstruct_child_zip, resolve_slot_cdata, write_slot};
use crate::scan::ZipView;

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
    /// Write each jar to its recorded `restore_path` (mode/owner when present).
    pub restore_paths: bool,
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
            restore_paths: false,
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

    if !opts.restore_paths {
        fs::create_dir_all(&opts.dir).map_err(|source| AyzenpackError::Io {
            source,
            path: Some(opts.dir.clone()),
        })?;
    }

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
    let layout = open_ayz_layout(&mut file)?;
    let limited = Read::take(&mut file, layout.trailer.payload_bytes);
    let mut decoder = decode_payload(limited, layout.header.version)?;

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

    let selected: Vec<&Jar> = manifest
        .jars
        .iter()
        .filter(|jar| opts.only.is_empty() || opts.only.iter().any(|n| n == &jar.name))
        .collect();

    if opts.restore_paths {
        // Whole pack, not just --only: mixed/old archives must not restore a subset.
        for jar in &manifest.jars {
            if !matches!(jar.restore_path.as_deref(), Some(p) if !p.is_empty()) {
                return Err(AyzenpackError::Usage(format!(
                    "pack was not created with --restore-paths ({})",
                    jar.name
                )));
            }
        }
    }

    for jar in selected {
        let dest = if opts.restore_paths {
            restore_dest(jar)?
        } else {
            check_jar_name(&jar.name)?;
            opts.dir.join(&jar.name)
        };
        if opts.restore_paths {
            prepare_restore_dest(&dest)?;
        } else if dest.exists() {
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
        let apply_prefix_chmod = !(opts.restore_paths && jar.restore_mode.is_some());
        if jar.bit_identical_restore() {
            write_exact_jar(jar, cas_dir, &dest, apply_prefix_chmod)?;
        } else if jar.metadata_rebuild() {
            write_rebuilt_jar(jar, cas_dir, &dest, apply_prefix_chmod)?;
        } else {
            write_jar(opts, jar, cas_dir, &dest, apply_prefix_chmod)?;
        }
        if opts.restore_paths {
            apply_restore_attrs(opts, jar, &dest)?;
        }
    }
    Ok(())
}

fn restore_dest(jar: &Jar) -> Result<PathBuf> {
    let raw = jar.restore_path.as_deref().unwrap_or("");
    if raw.is_empty() || raw.contains('\0') {
        return Err(AyzenpackError::UnsafePath(raw.to_string()));
    }
    let dest = PathBuf::from(raw);
    if !dest.is_absolute() {
        return Err(AyzenpackError::Usage(format!(
            "restore_path for {} is not absolute (canonicalize failed at dehydrate): {raw}",
            jar.name
        )));
    }
    Ok(dest)
}

/// Create missing parents as 0755. Refuse a real directory dest.
/// Dest is not unlinked: writers commit via sibling tmp + `replace_file` so a
/// crash still leaves the original dest bytes.
fn prepare_restore_dest(dest: &Path) -> Result<()> {
    match fs::symlink_metadata(dest) {
        Ok(meta) => {
            if meta.file_type().is_dir() && !meta.file_type().is_symlink() {
                return Err(AyzenpackError::Usage(format!(
                    "refusing to overwrite directory {}",
                    dest.display()
                )));
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(AyzenpackError::Io {
                source,
                path: Some(dest.to_path_buf()),
            });
        }
    }
    create_parent_dirs_0755(dest)
}

fn create_parent_dirs_0755(dest: &Path) -> Result<()> {
    let Some(parent) = dest.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    let mut acc = PathBuf::new();
    for comp in parent.components() {
        acc.push(comp);
        // Windows canonicalize is `\\?\C:\...`. Stat of `\\?\C:` is ERROR_INVALID_FUNCTION.
        if matches!(comp, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        match fs::symlink_metadata(&acc) {
            Ok(_) => continue,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(AyzenpackError::Io {
                    source,
                    path: Some(acc),
                });
            }
        }
        mkdir_0755(&acc)?;
    }
    Ok(())
}

fn mkdir_0755(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(path)
            .map_err(|source| AyzenpackError::Io {
                source,
                path: Some(path.to_path_buf()),
            })?;
        let mut perms = fs::metadata(path)
            .map_err(|source| AyzenpackError::Io {
                source,
                path: Some(path.to_path_buf()),
            })?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).map_err(|source| AyzenpackError::Io {
            source,
            path: Some(path.to_path_buf()),
        })?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path).map_err(|source| AyzenpackError::Io {
            source,
            path: Some(path.to_path_buf()),
        })?;
    }
    Ok(())
}

fn apply_restore_attrs(opts: &RehydrateOptions, jar: &Jar, dest: &Path) -> Result<()> {
    if let Some(mode) = jar.restore_mode {
        set_restore_mode(dest, mode)?;
    }
    #[cfg(unix)]
    {
        if let (Some(uid), Some(gid)) = (jar.restore_uid, jar.restore_gid) {
            if let Err(source) = std::os::unix::fs::chown(dest, Some(uid), Some(gid)) {
                if chown_denied(&source) {
                    warn(
                        opts,
                        &format!(
                            "could not chown {} to {uid}:{gid}: {source}",
                            dest.display()
                        ),
                    );
                } else {
                    return Err(AyzenpackError::Io {
                        source,
                        path: Some(dest.to_path_buf()),
                    });
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = opts;
    }
    Ok(())
}

#[cfg(unix)]
fn chown_denied(err: &io::Error) -> bool {
    if err.kind() == io::ErrorKind::PermissionDenied {
        return true;
    }
    // EPERM (1) and EACCES (13) on Linux/macOS; rustc 1.80 may not map both to PermissionDenied.
    matches!(err.raw_os_error(), Some(1) | Some(13))
}

fn set_restore_mode(dest: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(dest)
            .map_err(|source| AyzenpackError::Io {
                source,
                path: Some(dest.to_path_buf()),
            })?
            .permissions();
        perms.set_mode(mode);
        fs::set_permissions(dest, perms).map_err(|source| AyzenpackError::Io {
            source,
            path: Some(dest.to_path_buf()),
        })?;
    }
    #[cfg(not(unix))]
    {
        let mut perms = fs::metadata(dest)
            .map_err(|source| AyzenpackError::Io {
                source,
                path: Some(dest.to_path_buf()),
            })?
            .permissions();
        perms.set_readonly(mode & 0o222 == 0);
        fs::set_permissions(dest, perms).map_err(|source| AyzenpackError::Io {
            source,
            path: Some(dest.to_path_buf()),
        })?;
    }
    Ok(())
}

/// Sibling tmp for restore. Drop unlinks tmp unless `commit` renamed it onto dest.
struct PendingRestore {
    tmp: PathBuf,
    dest: PathBuf,
    committed: bool,
}

impl PendingRestore {
    fn prepare(dest: &Path) -> Self {
        Self {
            tmp: sibling_tmp_path(dest),
            dest: dest.to_path_buf(),
            committed: false,
        }
    }

    fn commit(mut self) -> Result<()> {
        replace_file(&self.tmp, &self.dest)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for PendingRestore {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.tmp);
        }
    }
}

#[cfg(test)]
thread_local! {
    static FAIL_AFTER_RESTORE_TMP_OPEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn maybe_inject_restore_tmp_failure() -> Result<()> {
    #[cfg(test)]
    {
        if FAIL_AFTER_RESTORE_TMP_OPEN.with(std::cell::Cell::get) {
            FAIL_AFTER_RESTORE_TMP_OPEN.with(|c| c.set(false));
            return Err(AyzenpackError::Format("injected restore failure"));
        }
    }
    Ok(())
}

/// Open dest's sibling tmp. Pending is first in the return so callers drop the
/// File before Drop-unlink (Windows cannot remove an open file).
fn create_restore_tmp(dest: &Path) -> Result<(PendingRestore, File)> {
    let pending = PendingRestore::prepare(dest);
    let file = File::create(&pending.tmp).map_err(|source| AyzenpackError::Io {
        source,
        path: Some(pending.tmp.clone()),
    })?;
    maybe_inject_restore_tmp_failure()?;
    Ok((pending, file))
}

fn write_exact_jar(jar: &Jar, cas_dir: &Path, dest: &Path, apply_prefix_chmod: bool) -> Result<()> {
    let (pending, mut file) = create_restore_tmp(dest)?;
    let prefix_len = write_prefix(jar, cas_dir, dest, &mut file)?;
    if let Some(hex) = &jar.leading_pad_blob {
        let pad = read_named_blob(cas_dir, hex, &format!("{} leading_pad", jar.name))?;
        if let Some(sz) = jar.leading_pad_size {
            if pad.len() as u64 != sz {
                return Err(AyzenpackError::HashMismatch(format!(
                    "{} leading_pad size: recorded {sz} computed {}",
                    jar.name,
                    pad.len()
                )));
            }
        }
        file.write_all(&pad).map_err(|source| AyzenpackError::Io {
            source,
            path: Some(dest.to_path_buf()),
        })?;
    }

    if let (Some(hex), Some(sz)) = (&jar.raw_zip_blob, jar.raw_zip_size) {
        let bytes = read_named_blob(cas_dir, hex, &format!("{} raw_zip", jar.name))?;
        if bytes.len() as u64 != sz {
            return Err(AyzenpackError::HashMismatch(format!(
                "{} raw_zip size: recorded {sz} computed {}",
                jar.name,
                bytes.len()
            )));
        }
        file.write_all(&bytes)
            .map_err(|source| AyzenpackError::Io {
                source,
                path: Some(dest.to_path_buf()),
            })?;
        drop(file);
        finish_exact(&pending.tmp, jar, prefix_len, apply_prefix_chmod)?;
        return pending.commit();
    }

    let tail = match (&jar.tail_blob, jar.tail_size) {
        (Some(hex), Some(sz)) => {
            let bytes = read_named_blob(cas_dir, hex, &format!("{} tail", jar.name))?;
            if bytes.len() as u64 != sz {
                return Err(AyzenpackError::HashMismatch(format!(
                    "{} tail size: recorded {sz} computed {}",
                    jar.name,
                    bytes.len()
                )));
            }
            bytes
        }
        _ => {
            return Err(AyzenpackError::FormatOwned(format!(
                "exact jar {} missing tail_blob/tail_size or raw_zip_blob/raw_zip_size",
                jar.name
            )));
        }
    };

    if jar.source_size < prefix_len + tail.len() as u64 {
        return Err(AyzenpackError::HashMismatch(format!(
            "{} source_size {} is smaller than prefix+tail",
            jar.name, jar.source_size
        )));
    }

    for e in &jar.entries {
        write_exact_entry(&mut file, jar, e, cas_dir, dest, prefix_len)?;
    }

    let tail_pos = jar.source_size - tail.len() as u64;
    file.seek(SeekFrom::Start(tail_pos))
        .map_err(|source| AyzenpackError::Io {
            source,
            path: Some(dest.to_path_buf()),
        })?;
    file.write_all(&tail).map_err(|source| AyzenpackError::Io {
        source,
        path: Some(dest.to_path_buf()),
    })?;
    file.set_len(jar.source_size)
        .map_err(|source| AyzenpackError::Io {
            source,
            path: Some(dest.to_path_buf()),
        })?;
    drop(file);
    finish_exact(&pending.tmp, jar, prefix_len, apply_prefix_chmod)?;
    pending.commit()
}

fn write_exact_entry(
    file: &mut File,
    jar: &Jar,
    e: &Entry,
    cas_dir: &Path,
    dest: &Path,
    prefix_len: u64,
) -> Result<()> {
    let cdata = resolve_cdata(jar, e, cas_dir, false)?;
    write_slot(
        file,
        e,
        prefix_len,
        &mut |hex| read_named_blob(cas_dir, hex, &format!("{}!{}", jar.name, e.name)),
        &cdata,
    )
    .map_err(|err| match err {
        AyzenpackError::Io { source, path: None } => AyzenpackError::Io {
            source,
            path: Some(dest.to_path_buf()),
        },
        other => other,
    })
}

/// `allow_rebuild` is never true on the exact splice path (jar-level rebuild).
fn resolve_cdata(jar: &Jar, e: &Entry, cas_dir: &Path, allow_rebuild: bool) -> Result<Vec<u8>> {
    if e.blob.is_some() && e.zip_index.is_some() {
        return Err(AyzenpackError::FormatOwned(format!(
            "{}!{} has both blob and zip_index",
            jar.name, e.name
        )));
    }
    if e.zip_index.is_some() {
        return read_entry_content(jar, e, cas_dir);
    }
    resolve_slot_cdata(
        e,
        &mut |hex| read_named_blob(cas_dir, hex, &format!("{}!{} cdata", jar.name, e.name)),
        allow_rebuild,
    )
}

/// Header fields after rebuild. Class-4 / leftover-csize / exotic dirs become empty STORE.
/// Maven empty DEFLATE dirs (`method 8`, uncomp 0) stay DEFLATE. Exotic files become DEFLATE.
/// Files keep crc / uncompressed size.
fn rebuild_index_fields(e: &Entry) -> (u16, u32, u64) {
    if e.is_dir {
        let maven_empty_deflate = e.method_code == 8 && e.uncompressed_size == 0;
        if !maven_empty_deflate
            && (e.uncompressed_size != 0 || e.compressed_size != 0 || e.method_code != 0)
        {
            return (0, 0, 0);
        }
    }
    if !e.is_dir && e.method_code != 0 && e.method_code != 8 {
        return (8, e.crc32, e.uncompressed_size);
    }
    (e.method_code, e.crc32, e.uncompressed_size)
}

fn read_entry_content(jar: &Jar, e: &Entry, cas_dir: &Path) -> Result<Vec<u8>> {
    if e.blob.is_some() && e.zip_index.is_some() {
        return Err(AyzenpackError::FormatOwned(format!(
            "{}!{} has both blob and zip_index",
            jar.name, e.name
        )));
    }
    if let Some(i) = e.zip_index {
        let index = jar.nested_index(i)?;
        return reconstruct_child_zip(index, e.uncompressed_size, |hex| {
            read_named_blob(cas_dir, hex, &format!("{}!{} child", jar.name, e.name))
        });
    }
    let hex = e.blob.as_deref().ok_or_else(|| {
        AyzenpackError::FormatOwned(format!("missing blob for {}!{}", jar.name, e.name))
    })?;
    read_named_blob(cas_dir, hex, &format!("{}!{}", jar.name, e.name))
}

fn write_rebuilt_jar(
    jar: &Jar,
    cas_dir: &Path,
    dest: &Path,
    apply_prefix_chmod: bool,
) -> Result<()> {
    let (pending, mut file) = create_restore_tmp(dest)?;
    let prefix_len = write_prefix(jar, cas_dir, dest, &mut file)?;

    let tail = match (&jar.tail_blob, jar.tail_size) {
        (Some(hex), Some(sz)) => {
            let bytes = read_named_blob(cas_dir, hex, &format!("{} tail", jar.name))?;
            if bytes.len() as u64 != sz {
                return Err(AyzenpackError::HashMismatch(format!(
                    "{} tail size: recorded {sz} computed {}",
                    jar.name,
                    bytes.len()
                )));
            }
            bytes
        }
        _ => {
            return Err(AyzenpackError::FormatOwned(format!(
                "rebuild jar {} missing tail_blob/tail_size",
                jar.name
            )));
        }
    };

    if jar.entries.is_empty() {
        file.write_all(&tail).map_err(|source| AyzenpackError::Io {
            source,
            path: Some(dest.to_path_buf()),
        })?;
        drop(file);
        if prefix_len > 0 && apply_prefix_chmod {
            chmod_executable(&pending.tmp)?;
        }
        return pending.commit();
    }

    let first_lh = jar.entries[0].local_header_offset.ok_or_else(|| {
        AyzenpackError::FormatOwned(format!(
            "missing local_header_offset for {}!{}",
            jar.name, jar.entries[0].name
        ))
    })?;
    let mode = detect_offset_mode(&tail, first_lh, prefix_len, &jar.name)?;

    let mut locals = Vec::new();
    let mut updates = Vec::with_capacity(jar.entries.len());
    for e in &jar.entries {
        let mut header = load_local_header(jar, e, cas_dir)?;
        let cdata = resolve_cdata(jar, e, cas_dir, true)?;
        let (method, crc, uncomp) = rebuild_index_fields(e);
        patch_local_rebuild_fields(
            &mut header,
            method,
            crc,
            cdata.len() as u64,
            uncomp,
            &jar.name,
        )?;
        let desc = match &e.data_descriptor_hex {
            Some(h) => Some(patch_data_descriptor(
                &parse_hex(h)?,
                crc,
                cdata.len() as u64,
                uncomp,
                &jar.name,
            )?),
            None => None,
        };
        updates.push(RebuildPatch {
            zip_rel: locals.len() as u64,
            method,
            crc,
            compressed_size: cdata.len() as u64,
            uncompressed_size: uncomp,
        });
        locals.extend_from_slice(&header);
        locals.extend_from_slice(&cdata);
        if let Some(d) = &desc {
            locals.extend_from_slice(d);
        }
    }

    let mut new_tail = tail;
    let cd_size = patch_central_directory(&mut new_tail, &updates, mode, prefix_len, &jar.name)?;
    let new_cd_start = encode_offset(mode, locals.len() as u64, prefix_len);
    patch_eocd_cd_start(&mut new_tail, cd_size, new_cd_start, &jar.name)?;

    file.write_all(&locals)
        .map_err(|source| AyzenpackError::Io {
            source,
            path: Some(dest.to_path_buf()),
        })?;
    file.write_all(&new_tail)
        .map_err(|source| AyzenpackError::Io {
            source,
            path: Some(dest.to_path_buf()),
        })?;
    let total = prefix_len + locals.len() as u64 + new_tail.len() as u64;
    file.set_len(total).map_err(|source| AyzenpackError::Io {
        source,
        path: Some(dest.to_path_buf()),
    })?;
    drop(file);
    if prefix_len > 0 && apply_prefix_chmod {
        chmod_executable(&pending.tmp)?;
    }
    pending.commit()
}

fn load_local_header(jar: &Jar, e: &Entry, cas_dir: &Path) -> Result<Vec<u8>> {
    load_header(e, &mut |hex| {
        read_named_blob(
            cas_dir,
            hex,
            &format!("{}!{} local_header", jar.name, e.name),
        )
    })
}

fn finish_exact(dest: &Path, jar: &Jar, prefix_len: u64, apply_prefix_chmod: bool) -> Result<()> {
    if prefix_len > 0 && apply_prefix_chmod {
        chmod_executable(dest)?;
    }
    verify_source_identity(dest, jar)
}

fn verify_source_identity(dest: &Path, jar: &Jar) -> Result<()> {
    let got_len = fs::metadata(dest)
        .map_err(|source| AyzenpackError::Io {
            source,
            path: Some(dest.to_path_buf()),
        })?
        .len();
    if got_len != jar.source_size {
        return Err(AyzenpackError::HashMismatch(format!(
            "{} size: recorded {} computed {got_len}",
            jar.name, jar.source_size
        )));
    }
    let file = File::open(dest).map_err(|source| AyzenpackError::Io {
        source,
        path: Some(dest.to_path_buf()),
    })?;
    let (b3, sha) = hash_reader(file).map_err(|source| AyzenpackError::Io {
        source,
        path: Some(dest.to_path_buf()),
    })?;
    let want_b3 = parse_blake3_hex(&jar.source_blake3)?;
    if b3 != want_b3 {
        return Err(AyzenpackError::HashMismatch(format!(
            "{} source_blake3",
            jar.name
        )));
    }
    let want_sha = parse_blake3_hex(&jar.source_sha256)?;
    if sha != want_sha {
        return Err(AyzenpackError::HashMismatch(format!(
            "{} source_sha256",
            jar.name
        )));
    }
    Ok(())
}

fn read_named_blob(cas_dir: &Path, hex: &str, label: &str) -> Result<Vec<u8>> {
    let hash = parse_blake3_hex(hex)?;
    let bytes = read_cas_blob(cas_dir, &hash)?;
    if blake3_bytes(&bytes) != hash {
        return Err(AyzenpackError::HashMismatch(format!("{label} blake3")));
    }
    Ok(bytes)
}

fn write_jar(
    opts: &RehydrateOptions,
    jar: &crate::manifest::Jar,
    cas_dir: &Path,
    dest: &Path,
    apply_prefix_chmod: bool,
) -> Result<()> {
    let (pending, mut file) = create_restore_tmp(dest)?;
    let prefix_len = write_prefix(jar, cas_dir, dest, &mut file)?;
    let mut writer = ZipWriter::new(ZipView::new(file, prefix_len));
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

        if e.blob.is_some() && e.zip_index.is_some() {
            return Err(AyzenpackError::FormatOwned(format!(
                "{}!{} has both blob and zip_index",
                jar.name, e.name
            )));
        }
        let bytes = read_entry_content(jar, e, cas_dir)?;
        if let Some(hex) = e.blob.as_deref() {
            let hash = parse_blake3_hex(hex)?;
            if blake3_bytes(&bytes) != hash {
                return Err(AyzenpackError::HashMismatch(format!(
                    "{}!{} blake3",
                    jar.name, e.name
                )));
            }
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
    if prefix_len > 0 && apply_prefix_chmod {
        chmod_executable(&pending.tmp)?;
    }
    pending.commit()
}

fn write_prefix(
    jar: &crate::manifest::Jar,
    cas_dir: &Path,
    dest: &Path,
    file: &mut File,
) -> Result<u64> {
    match (&jar.prefix_blob, jar.prefix_size) {
        (None, None) => Ok(0),
        (Some(hex), Some(sz)) => {
            let hash = parse_blake3_hex(hex)?;
            let bytes = read_cas_blob(cas_dir, &hash)?;
            if blake3_bytes(&bytes) != hash {
                return Err(AyzenpackError::HashMismatch(format!(
                    "{} prefix blake3",
                    jar.name
                )));
            }
            if bytes.len() as u64 != sz {
                return Err(AyzenpackError::HashMismatch(format!(
                    "{} prefix size: recorded {sz} computed {}",
                    jar.name,
                    bytes.len()
                )));
            }
            file.write_all(&bytes)
                .map_err(|source| AyzenpackError::Io {
                    source,
                    path: Some(dest.to_path_buf()),
                })?;
            Ok(sz)
        }
        _ => Err(AyzenpackError::FormatOwned(format!(
            "prefix_blob and prefix_size must both be set on {}",
            jar.name
        ))),
    }
}

fn chmod_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)
            .map_err(|source| AyzenpackError::Io {
                source,
                path: Some(path.to_path_buf()),
            })?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).map_err(|source| AyzenpackError::Io {
            source,
            path: Some(path.to_path_buf()),
        })?;
    }
    let _ = path;
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

    fn jar_restore(path: &str) -> Jar {
        Jar {
            name: "a.jar".into(),
            source_path: "a.jar".into(),
            source_size: 0,
            source_blake3: "00".repeat(32),
            source_sha256: "00".repeat(32),
            comment: String::new(),
            signed: false,
            restore_path: Some(path.into()),
            restore_mode: None,
            restore_uid: None,
            restore_gid: None,
            prefix_blob: None,
            prefix_size: None,
            tail_blob: None,
            tail_size: None,
            raw_zip_blob: None,
            raw_zip_size: None,
            leading_pad_blob: None,
            leading_pad_size: None,
            nestedindexes: Vec::new(),
            entries: Vec::new(),
        }
    }

    #[test]
    fn restore_dest_rejects_relative_and_nul() {
        let err = restore_dest(&jar_restore("relative/a.jar")).unwrap_err();
        assert!(
            matches!(err, AyzenpackError::Usage(ref s) if s.contains("not absolute")),
            "{err}"
        );
        let err = restore_dest(&jar_restore("a.jar")).unwrap_err();
        assert!(matches!(err, AyzenpackError::Usage(_)), "{err}");
        let err = restore_dest(&jar_restore("\0/abs.jar")).unwrap_err();
        assert!(matches!(err, AyzenpackError::UnsafePath(_)), "{err}");
        #[cfg(unix)]
        restore_dest(&jar_restore("/abs/a.jar")).unwrap();
        #[cfg(windows)]
        restore_dest(&jar_restore(r"C:\abs\a.jar")).unwrap();
    }

    #[test]
    fn create_parent_dirs_skips_prefix_and_root() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("missing").join("nested").join("a.jar");
        create_parent_dirs_0755(&dest).unwrap();
        assert!(dest.parent().unwrap().is_dir());
        create_parent_dirs_0755(&dest).unwrap();
    }

    #[test]
    fn both_blob_and_zip_index_is_refused() {
        let jar = jar_restore("/abs/a.jar");
        let e = Entry {
            name: "lib/inner.jar".into(),
            blob: Some("aa".repeat(32)),
            zip_index: Some(0),
            ..Entry::default()
        };
        let err = read_entry_content(&jar, &e, Path::new("/tmp")).unwrap_err();
        assert!(
            matches!(err, AyzenpackError::FormatOwned(ref s) if s.contains("both blob and zip_index")),
            "{err}"
        );
    }

    #[test]
    fn prepare_restore_dest_does_not_unlink_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.jar");
        fs::write(&dest, b"keep-me").unwrap();
        prepare_restore_dest(&dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"keep-me");
    }

    #[test]
    fn prepare_restore_dest_refuses_directory_without_removing_it() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("as_dir");
        fs::create_dir(&dest).unwrap();
        let err = prepare_restore_dest(&dest).unwrap_err();
        assert!(
            matches!(err, AyzenpackError::Usage(ref s) if s.contains("directory")),
            "{err}"
        );
        assert!(dest.is_dir());
    }

    fn write_test_jar(path: &Path, files: &[(&str, &[u8])]) {
        let mut z = ZipWriter::new(File::create(path).unwrap());
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, data) in files {
            z.start_file(*name, opts).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
    }

    fn assert_injected_restore_keeps_dest(err: AyzenpackError, dest: &Path, orig: &[u8]) {
        assert!(
            matches!(err, AyzenpackError::Format("injected restore failure")),
            "inject hook must fire, got {err:?}"
        );
        assert_eq!(
            fs::read(dest).unwrap(),
            orig,
            "failed restore must not unlink or truncate dest"
        );
        assert!(
            !sibling_tmp_path(dest).exists(),
            "temp must be deleted after injected failure"
        );
    }

    #[test]
    fn writer_tmp_open_failure_leaves_dest_original_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.jar");
        let orig = b"source-bytes-must-remain";
        let cas = dir.path().join("cas");
        fs::create_dir(&cas).unwrap();
        let jar = jar_restore(dest.to_str().unwrap());
        let opts = RehydrateOptions {
            quiet: true,
            ..RehydrateOptions::default()
        };

        for backend in ["exact", "rebuild", "zipwriter"] {
            fs::write(&dest, orig).unwrap();
            FAIL_AFTER_RESTORE_TMP_OPEN.with(|c| c.set(true));
            let err = match backend {
                "exact" => write_exact_jar(&jar, &cas, &dest, false),
                "rebuild" => write_rebuilt_jar(&jar, &cas, &dest, false),
                "zipwriter" => write_jar(&opts, &jar, &cas, &dest, false),
                _ => unreachable!(),
            }
            .unwrap_err();
            assert_injected_restore_keeps_dest(err, &dest, orig);
        }
    }

    #[test]
    fn rehydrate_tmp_open_failure_leaves_dest_original_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let jar = dir.path().join("a.jar");
        write_test_jar(&jar, &[("x.txt", b"source-bytes")]);
        let orig = fs::read(&jar).unwrap();
        let pack = dir.path().join("all.ayz");
        crate::dehydrate::dehydrate(&crate::dehydrate::DehydrateOptions {
            output: pack.clone(),
            inputs: vec![jar.clone()],
            restore_paths: true,
            quiet: true,
            ..crate::dehydrate::DehydrateOptions::default()
        })
        .unwrap();
        assert_eq!(fs::read(&jar).unwrap(), orig);

        FAIL_AFTER_RESTORE_TMP_OPEN.with(|c| c.set(true));
        let err = rehydrate(&RehydrateOptions {
            input: pack,
            restore_paths: true,
            quiet: true,
            ..RehydrateOptions::default()
        })
        .unwrap_err();
        assert_injected_restore_keeps_dest(err, &jar, &orig);
    }

    #[test]
    fn writer_error_after_tmp_open_unlinks_tmp_keeps_dest() {
        // Missing tail / blob after the tmp File is bound: File must drop before
        // PendingRestore so Windows can unlink dest.jar.tmp.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.jar");
        let orig = b"source-bytes-must-remain";
        let cas = dir.path().join("cas");
        fs::create_dir(&cas).unwrap();
        let jar = jar_restore(dest.to_str().unwrap());
        let opts = RehydrateOptions {
            quiet: true,
            ..RehydrateOptions::default()
        };

        for backend in ["exact", "rebuild", "zipwriter"] {
            fs::write(&dest, orig).unwrap();
            let err = match backend {
                "exact" => write_exact_jar(&jar, &cas, &dest, false),
                "rebuild" => write_rebuilt_jar(&jar, &cas, &dest, false),
                "zipwriter" => {
                    let mut skip = jar.clone();
                    skip.entries.push(Entry {
                        name: "x.txt".into(),
                        blob: Some("aa".repeat(32)),
                        ..Entry::default()
                    });
                    write_jar(&opts, &skip, &cas, &dest, false)
                }
                _ => unreachable!(),
            }
            .unwrap_err();
            match backend {
                "exact" | "rebuild" => assert!(
                    matches!(err, AyzenpackError::FormatOwned(ref s) if s.contains("missing tail")),
                    "backend {backend}: {err:?}"
                ),
                "zipwriter" => assert!(
                    matches!(err, AyzenpackError::HashMismatch(ref s) if s.contains("missing blob")),
                    "backend {backend}: {err:?}"
                ),
                _ => unreachable!(),
            }
            assert_eq!(fs::read(&dest).unwrap(), orig, "backend {backend}");
            assert!(
                !sibling_tmp_path(&dest).exists(),
                "backend {backend} tmp leftover"
            );
        }
    }
}
