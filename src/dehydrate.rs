//! Dehydrate JARs into one `.ayz` (dedup BLOBs + embedded manifest).
//!
//! Scan is sequential. `--jobs` hashes on a pool; a single writer emits BLOBs in
//! first-seen (scan) order so END digest and `blobs[]` stay deterministic.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{SystemTime, UNIX_EPOCH};

use walkdir::WalkDir;

use crate::error::{AyzenpackError, Result};
use crate::exact::{capture_zip_exact, ExactLocal, ZipExact};
use crate::format::{
    write_header, write_record, write_trailer, FileHeader, Record, Trailer, BUF_WRITER_CAP,
    REC_BLOB, TRAILER_LEN, TRAILER_MAGIC,
};
use crate::hashutil::{hash_both, hex_lower};
use crate::manifest::{Blob, Entry, Jar, Manifest, Stats, MANIFEST_FORMAT};
use crate::scan::{for_each_jar_entry_with_len, ScannedEntry};
use crate::stats::{dedup_ratio, json_event, PackProgress};

/// Inline hex for local headers / descriptors under this size; larger values become CAS blobs.
const HEX_INLINE_MAX: usize = 512;

const DEFAULT_LEVEL: i32 = 3;
const DEFAULT_MAX_ENTRY: u64 = 2_147_483_647;
const DEFAULT_JOBS: usize = 1;
/// Default `--max-inflight-bytes`: 64 MiB of uncompressed entry buffers.
const DEFAULT_MAX_INFLIGHT_BYTES: u64 = 64 * 1024 * 1024;

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
    /// Hash worker threads. `1` = sequential (default). `0` = available parallelism.
    pub jobs: usize,
    /// Cap on uncompressed entry buffers in the hash pipeline (default 64 MiB).
    pub max_inflight_bytes: u64,
    /// Record absolute restore path + mode (+ uid/gid on Unix) on each jar.
    pub restore_paths: bool,
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
            jobs: DEFAULT_JOBS,
            max_inflight_bytes: DEFAULT_MAX_INFLIGHT_BYTES,
            restore_paths: false,
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

/// First-seen writes plus catalog updates. Lives on the sequencer/writer thread.
struct BlobSink<'a> {
    writer: &'a mut Option<AyzWriter>,
    seen: &'a mut HashMap<[u8; 32], usize>,
    blobs: &'a mut Vec<Blob>,
    first_seen: &'a mut blake3::Hasher,
}

/// In-order item from the sequencer (scan order, not hash-completion order).
enum Sequenced {
    Dir(ScannedEntry),
    File(HashedFile),
}

struct HashedFile {
    seq: u64,
    meta: ScannedEntry,
    payload: Vec<u8>,
    blake3: [u8; 32],
    sha256: [u8; 32],
    crc: u32,
}

/// Hash pool + first-seen sequencer. The zstd encoder is never sent here.
struct HashPipeline {
    pool: rayon::ThreadPool,
    tx: Sender<std::result::Result<HashedFile, ()>>,
    rx: Receiver<std::result::Result<HashedFile, ()>>,
    pending: BTreeMap<u64, Sequenced>,
    next_seq: u64,
    next_emit: u64,
    spawned: u64,
    received: u64,
    inflight_bytes: u64,
    max_inflight_bytes: u64,
    peak_inflight_bytes: u64,
}

impl HashPipeline {
    fn new(jobs: usize, max_inflight_bytes: u64) -> Result<Self> {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(jobs)
            .thread_name(|i| format!("ayzenpack-hash-{i}"))
            .build()
            .map_err(|err| AyzenpackError::Usage(format!("failed to build hash pool: {err}")))?;
        let (tx, rx) = mpsc::channel();
        Ok(Self {
            pool,
            tx,
            rx,
            pending: BTreeMap::new(),
            next_seq: 0,
            next_emit: 0,
            spawned: 0,
            received: 0,
            inflight_bytes: 0,
            max_inflight_bytes,
            peak_inflight_bytes: 0,
        })
    }

    fn push_dir(&mut self, meta: ScannedEntry) -> Result<Vec<Sequenced>> {
        self.try_recv_all()?;
        let seq = self.next_seq;
        self.next_seq += 1;
        self.pending.insert(seq, Sequenced::Dir(meta));
        Ok(self.take_in_order())
    }

    fn wait_admit(&mut self, next: u64) -> Result<Vec<Sequenced>> {
        let mut batch = Vec::new();
        loop {
            self.try_recv_all()?;
            batch.extend(self.take_in_order());
            if can_admit_inflight(self.inflight_bytes, next, self.max_inflight_bytes) {
                return Ok(batch);
            }
            if self.received == self.spawned {
                if batch.is_empty() {
                    return Err(AyzenpackError::Format("inflight accounting deadlock"));
                }
                return Ok(batch);
            }
            self.recv_one()?;
        }
    }

    fn spawn_file(&mut self, meta: ScannedEntry, payload: Vec<u8>) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.inflight_bytes += payload.len() as u64;
        self.peak_inflight_bytes = self.peak_inflight_bytes.max(self.inflight_bytes);
        self.spawned += 1;
        let tx = self.tx.clone();
        self.pool.spawn(move || {
            let sent = panic::catch_unwind(AssertUnwindSafe(|| {
                let crc = crc32fast::hash(&payload);
                let (blake3, sha256) = hash_both(&payload);
                HashedFile {
                    seq,
                    meta,
                    payload,
                    blake3,
                    sha256,
                    crc,
                }
            }));
            match sent {
                Ok(out) => {
                    let _ = tx.send(Ok(out));
                }
                Err(_) => {
                    let _ = tx.send(Err(()));
                }
            }
        });
    }

    fn drain_nonblocking(&mut self) -> Result<Vec<Sequenced>> {
        self.try_recv_all()?;
        Ok(self.take_in_order())
    }

    fn finish_jar(&mut self) -> Result<Vec<Sequenced>> {
        let mut batch = Vec::new();
        while self.received < self.spawned {
            self.recv_one()?;
            self.try_recv_all()?;
            batch.extend(self.take_in_order());
        }
        batch.extend(self.take_in_order());
        self.next_seq = 0;
        self.next_emit = 0;
        self.spawned = 0;
        self.received = 0;
        Ok(batch)
    }

    fn recv_one(&mut self) -> Result<()> {
        match self.rx.recv() {
            Ok(Ok(out)) => {
                self.received += 1;
                self.pending.insert(out.seq, Sequenced::File(out));
                Ok(())
            }
            Ok(Err(())) => Err(AyzenpackError::Format("hash worker panicked")),
            Err(_) => Err(AyzenpackError::Format("hash worker channel closed")),
        }
    }

    fn try_recv_all(&mut self) -> Result<()> {
        loop {
            match self.rx.try_recv() {
                Ok(Ok(out)) => {
                    self.received += 1;
                    self.pending.insert(out.seq, Sequenced::File(out));
                }
                Ok(Err(())) => return Err(AyzenpackError::Format("hash worker panicked")),
                Err(mpsc::TryRecvError::Empty) => return Ok(()),
                Err(mpsc::TryRecvError::Disconnected) => {
                    if self.received < self.spawned {
                        return Err(AyzenpackError::Format("hash worker channel closed"));
                    }
                    return Ok(());
                }
            }
        }
    }

    fn take_in_order(&mut self) -> Vec<Sequenced> {
        let mut out = Vec::new();
        while let Some(item) = self.pending.remove(&self.next_emit) {
            if let Sequenced::File(ref f) = item {
                self.inflight_bytes = self.inflight_bytes.saturating_sub(f.payload.len() as u64);
            }
            self.next_emit += 1;
            out.push(item);
        }
        out
    }
}

/// `jobs == 0` means available parallelism (at least 1).
fn resolve_jobs(jobs: usize) -> usize {
    if jobs == 0 {
        std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1)
    } else {
        jobs
    }
}

#[cfg(test)]
thread_local! {
    static LAST_PEAK_INFLIGHT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn last_peak_inflight_bytes() -> u64 {
    LAST_PEAK_INFLIGHT.with(std::cell::Cell::get)
}

/// Admit at least one buffer so a single entry larger than the cap still processes.
pub(crate) fn can_admit_inflight(
    inflight_bytes: u64,
    next_bytes: u64,
    max_inflight_bytes: u64,
) -> bool {
    inflight_bytes == 0 || inflight_bytes.saturating_add(next_bytes) <= max_inflight_bytes
}

fn apply_sequenced(
    opts: &DehydrateOptions,
    path: &Path,
    sink: &mut BlobSink<'_>,
    jar_entries: &mut Vec<Entry>,
    items: Vec<Sequenced>,
) -> Result<()> {
    for item in items {
        match item {
            Sequenced::Dir(meta) => {
                jar_entries.push(entry_from_scan(&meta, None, None));
            }
            Sequenced::File(file) => {
                check_crc(opts, path, &file.meta, file.crc)?;
                commit_blob(
                    sink,
                    &file.meta,
                    &file.payload,
                    file.blake3,
                    file.sha256,
                    jar_entries,
                )?;
            }
        }
    }
    Ok(())
}

fn check_crc(opts: &DehydrateOptions, path: &Path, meta: &ScannedEntry, crc: u32) -> Result<()> {
    if crc == meta.crc32 {
        return Ok(());
    }
    let msg = format!(
        "CRC mismatch for {}!{}: header {:#x} computed {:#x}",
        path.display(),
        meta.name,
        meta.crc32,
        crc
    );
    if opts.strict {
        return Err(AyzenpackError::FormatOwned(msg));
    }
    warn(opts, &msg);
    Ok(())
}

fn commit_blob(
    sink: &mut BlobSink<'_>,
    meta: &ScannedEntry,
    buf: &[u8],
    b3: [u8; 32],
    s256: [u8; 32],
    jar_entries: &mut Vec<Entry>,
) -> Result<()> {
    remember_blob(sink, buf, b3, s256)?;
    jar_entries.push(entry_from_scan(
        meta,
        Some(hex_lower(&b3)),
        Some(hex_lower(&s256)),
    ));
    Ok(())
}

fn remember_blob(sink: &mut BlobSink<'_>, buf: &[u8], b3: [u8; 32], s256: [u8; 32]) -> Result<()> {
    if let Some(&i) = sink.seen.get(&b3) {
        sink.blobs[i].ref_count += 1;
    } else {
        if let Some(ref mut w) = sink.writer {
            write_blob_record(&mut w.enc, &b3, buf)?;
        }
        sink.first_seen.update(&b3);
        sink.seen.insert(b3, sink.blobs.len());
        sink.blobs.push(Blob {
            blake3: hex_lower(&b3),
            sha256: hex_lower(&s256),
            size: buf.len() as u64,
            ref_count: 1,
        });
    }
    Ok(())
}

pub fn dehydrate(opts: &DehydrateOptions) -> Result<DehydrateSummary> {
    if !(1..=19).contains(&opts.level) {
        return Err(AyzenpackError::Usage(format!(
            "zstd level must be 1..=19, got {}",
            opts.level
        )));
    }

    let inputs = expand_inputs(opts)?;

    let created_unix = if opts.sort_inputs { 0 } else { unix_now() };
    let header = FileHeader::new(opts.level, created_unix);

    let pending = if opts.dry_run {
        None
    } else {
        Some(PendingAyz::prepare(&opts.output)?)
    };
    let mut writer = match pending.as_ref() {
        Some(p) => Some(start_ayz_file(&p.tmp, &header)?),
        None => None,
    };

    let jobs = resolve_jobs(opts.jobs);
    let mut pipeline = if jobs > 1 {
        Some(HashPipeline::new(jobs, opts.max_inflight_bytes)?)
    } else {
        None
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
    let progress = PackProgress::new(opts.quiet, opts.json_logs);

    {
        let mut sink = BlobSink {
            writer: &mut writer,
            seen: &mut seen,
            blobs: &mut blobs,
            first_seen: &mut first_seen,
        };

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

            let jar_name = unique_basename(path, &mut used_names)?;
            verbose(opts, &format!("{}", path.display()));
            let mut jar_entries = Vec::new();
            let scanned = for_each_jar_entry_with_len(
                path,
                opts.max_entry_bytes,
                |n| progress.start_jar(&jar_name, n),
                |meta, payload| {
                    progress.inc_entry();
                    entry_count += 1;
                    if meta.is_dir {
                        if let Some(pipe) = pipeline.as_mut() {
                            apply_sequenced(
                                opts,
                                path,
                                &mut sink,
                                &mut jar_entries,
                                pipe.push_dir(meta.clone())?,
                            )?;
                        } else {
                            jar_entries.push(entry_from_scan(meta, None, None));
                        }
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

                    if let Some(pipe) = pipeline.as_mut() {
                        let n = buf.len() as u64;
                        loop {
                            let ready = pipe.wait_admit(n)?;
                            if ready.is_empty() {
                                break;
                            }
                            apply_sequenced(opts, path, &mut sink, &mut jar_entries, ready)?;
                        }
                        pipe.spawn_file(meta.clone(), buf);
                        apply_sequenced(
                            opts,
                            path,
                            &mut sink,
                            &mut jar_entries,
                            pipe.drain_nonblocking()?,
                        )?;
                    } else {
                        let recomputed = crc32fast::hash(&buf);
                        check_crc(opts, path, meta, recomputed)?;
                        let (b3, s256) = hash_both(&buf);
                        commit_blob(&mut sink, meta, &buf, b3, s256, &mut jar_entries)?;
                    }
                    Ok(())
                },
            )?;
            if let Some(pipe) = pipeline.as_mut() {
                apply_sequenced(opts, path, &mut sink, &mut jar_entries, pipe.finish_jar()?)?;
            }
            progress.finish_jar(&jar_name, scanned.entries.len() as u64);

            if scanned.signed {
                signed_jars.push(jar_name.clone());
                if opts.fail_on_signed {
                    return Err(AyzenpackError::Usage(format!("signed JAR {jar_name}")));
                }
            }
            bytes_in_jars += scanned.source_size;
            let (prefix_blob, prefix_size) = match &scanned.prefix {
                Some(prefix) if !prefix.is_empty() => {
                    let (b3, s256) = hash_both(prefix);
                    remember_blob(&mut sink, prefix, b3, s256)?;
                    (Some(hex_lower(&b3)), Some(prefix.len() as u64))
                }
                _ => (None, None),
            };
            let (restore_path, restore_mode, restore_uid, restore_gid) = if opts.restore_paths {
                collect_restore_meta(path)
            } else {
                (None, None, None, None)
            };
            let mut jar = Jar {
                name: jar_name,
                source_path: path.to_string_lossy().into_owned(),
                source_size: scanned.source_size,
                source_blake3: hex_lower(&scanned.source_blake3),
                source_sha256: hex_lower(&scanned.source_sha256),
                comment: scanned.comment,
                signed: scanned.signed,
                restore_path,
                restore_mode,
                restore_uid,
                restore_gid,
                prefix_blob,
                prefix_size,
                tail_blob: None,
                tail_size: None,
                raw_zip_blob: None,
                raw_zip_size: None,
                entries: jar_entries,
            };
            attach_exact(&mut sink, path, &mut jar)?;
            if jar.signed && !jar.exact_restore() {
                warn(
                    opts,
                    &format!("signed JAR {} (rebuild will break the signature)", jar.name),
                );
            } else if jar.signed {
                warn(opts, &format!("signed JAR {}", jar.name));
            }
            jars.push(jar);
        }
    }

    #[cfg(test)]
    LAST_PEAK_INFLIGHT.with(|c| {
        c.set(
            pipeline
                .as_ref()
                .map(|p| p.peak_inflight_bytes)
                .unwrap_or(0),
        );
    });

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

    if let Some(pending) = pending {
        maybe_inject_commit_failure()?;
        pending.commit()?;
    }

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

/// Sibling of `dest` (`all.ayz` → `all.ayz.tmp`) so `rename` stays on one filesystem.
fn sibling_tmp_path(dest: &Path) -> PathBuf {
    let mut name = dest
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| "ayzenpack.ayz".into());
    name.push(".tmp");
    dest.with_file_name(name)
}

/// Write the `.ayz` to a sibling temp and `rename` over `dest` only after the trailer is valid.
/// Drop deletes the temp so a failed or aborted dehydrate cannot replace `dest` with a
/// header-and-no-trailer file.
struct PendingAyz {
    tmp: PathBuf,
    dest: PathBuf,
    committed: bool,
}

impl PendingAyz {
    fn prepare(dest: &Path) -> Result<Self> {
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|source| AyzenpackError::Io {
                    source,
                    path: Some(dest.to_path_buf()),
                })?;
            }
        }
        Ok(Self {
            tmp: sibling_tmp_path(dest),
            dest: dest.to_path_buf(),
            committed: false,
        })
    }

    fn commit(mut self) -> Result<()> {
        replace_file(&self.tmp, &self.dest)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for PendingAyz {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.tmp);
        }
    }
}

fn replace_file(from: &Path, to: &Path) -> Result<()> {
    let map_err = |source| AyzenpackError::Io {
        source,
        path: Some(to.to_path_buf()),
    };
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Windows cannot replace an existing dest with rename.
            if to.exists() {
                fs::remove_file(to).map_err(map_err)?;
                fs::rename(from, to).map_err(map_err)
            } else {
                Err(map_err(e))
            }
        }
    }
}

#[cfg(test)]
thread_local! {
    static FAIL_BEFORE_AYZ_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn maybe_inject_commit_failure() -> Result<()> {
    #[cfg(test)]
    {
        if FAIL_BEFORE_AYZ_COMMIT.with(std::cell::Cell::get) {
            FAIL_BEFORE_AYZ_COMMIT.with(|c| c.set(false));
            return Err(AyzenpackError::Format("injected commit failure"));
        }
    }
    Ok(())
}

fn start_ayz_file(output: &Path, header: &FileHeader) -> Result<AyzWriter> {
    let mut file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(output)
        .map_err(|source| AyzenpackError::Io {
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

/// After flush: `stream_position` must match `metadata().len()`, and the last 64 bytes
/// must be a real `AYZPTLR1` trailer. Do not trust length alone.
fn verify_finished_ayz(file: &mut File, expected_len: u64) -> Result<()> {
    let file_len = file.metadata().map_err(crate::format::io_error)?.len();
    let pos = file.stream_position().map_err(crate::format::io_error)?;
    if pos != file_len {
        return Err(AyzenpackError::Format(
            "stream position != written file length",
        ));
    }
    if file_len != expected_len {
        return Err(AyzenpackError::Format(
            "file length != header_total + payload_bytes + 64",
        ));
    }
    if file_len < TRAILER_LEN {
        return Err(AyzenpackError::Format("truncated trailer"));
    }
    file.seek(SeekFrom::Start(file_len - TRAILER_LEN))
        .map_err(crate::format::io_error)?;
    let mut tail = [0u8; 64];
    file.read_exact(&mut tail)
        .map_err(crate::format::io_error)?;
    if tail[0..8] != TRAILER_MAGIC {
        return Err(AyzenpackError::Format("trailer magic missing after write"));
    }
    Ok(())
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
    let expected_len = header_total + payload_bytes + TRAILER_LEN;
    verify_finished_ayz(w.get_mut(), expected_len)?;
    Ok(expected_len)
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

fn entry_from_scan(meta: &ScannedEntry, blob: Option<String>, sha256: Option<String>) -> Entry {
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
        cdata_blob: None,
        local_header_offset: None,
        local_header_hex: None,
        local_header_blob: None,
        data_descriptor_hex: None,
        pad_zeros: None,
        pad_blob: None,
    }
}

fn attach_exact(sink: &mut BlobSink<'_>, path: &Path, jar: &mut Jar) -> Result<()> {
    match capture_zip_exact(path)? {
        ZipExact::Sliced(slice) if slice.locals.len() == jar.entries.len() => {
            for (entry, local) in jar.entries.iter_mut().zip(slice.locals.iter()) {
                fill_exact_entry(sink, entry, local)?;
            }
            let (b3, s256) = hash_both(&slice.tail);
            remember_blob(sink, &slice.tail, b3, s256)?;
            jar.tail_blob = Some(hex_lower(&b3));
            jar.tail_size = Some(slice.tail.len() as u64);
        }
        ZipExact::Raw(zip) => {
            let (b3, s256) = hash_both(&zip);
            remember_blob(sink, &zip, b3, s256)?;
            jar.raw_zip_blob = Some(hex_lower(&b3));
            jar.raw_zip_size = Some(zip.len() as u64);
        }
        ZipExact::Sliced(_) => {
            let zip = read_zip_after_prefix(path, jar.prefix_size.unwrap_or(0))?;
            let (b3, s256) = hash_both(&zip);
            remember_blob(sink, &zip, b3, s256)?;
            jar.raw_zip_blob = Some(hex_lower(&b3));
            jar.raw_zip_size = Some(zip.len() as u64);
        }
    }
    Ok(())
}

fn read_zip_after_prefix(path: &Path, prefix_len: u64) -> Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = File::open(path).map_err(|source| AyzenpackError::Io {
        source,
        path: Some(path.to_path_buf()),
    })?;
    let file_len = file
        .metadata()
        .map_err(|source| AyzenpackError::Io {
            source,
            path: Some(path.to_path_buf()),
        })?
        .len();
    if file_len < prefix_len {
        return Err(AyzenpackError::FormatOwned(format!(
            "prefix longer than file {}",
            path.display()
        )));
    }
    file.seek(SeekFrom::Start(prefix_len))
        .map_err(|source| AyzenpackError::Io {
            source,
            path: Some(path.to_path_buf()),
        })?;
    let mut zip = Vec::new();
    file.read_to_end(&mut zip)
        .map_err(|source| AyzenpackError::Io {
            source,
            path: Some(path.to_path_buf()),
        })?;
    Ok(zip)
}

fn fill_exact_entry(sink: &mut BlobSink<'_>, entry: &mut Entry, local: &ExactLocal) -> Result<()> {
    entry.local_header_offset = Some(local.zip_rel_offset);
    if local.header.len() <= HEX_INLINE_MAX {
        entry.local_header_hex = Some(hex_lower(&local.header));
    } else {
        let (b3, s256) = hash_both(&local.header);
        remember_blob(sink, &local.header, b3, s256)?;
        entry.local_header_blob = Some(hex_lower(&b3));
    }
    if !entry.is_dir || !local.cdata.is_empty() {
        let (b3, s256) = hash_both(&local.cdata);
        remember_blob(sink, &local.cdata, b3, s256)?;
        entry.cdata_blob = Some(hex_lower(&b3));
    }
    if let Some(desc) = &local.descriptor {
        entry.data_descriptor_hex = Some(hex_lower(desc));
    }
    if local.pad.iter().all(|&b| b == 0) && !local.pad.is_empty() {
        entry.pad_zeros = Some(local.pad.len() as u64);
    } else if !local.pad.is_empty() {
        let (b3, s256) = hash_both(&local.pad);
        remember_blob(sink, &local.pad, b3, s256)?;
        entry.pad_blob = Some(hex_lower(&b3));
    }
    Ok(())
}

/// Expand CLI inputs: optional recursive walk, `--exclude`, then sort + dedupe.
/// Exclude matches the path as given or the basename; glob `*` does not cross `/`.
fn expand_inputs(opts: &DehydrateOptions) -> Result<Vec<PathBuf>> {
    let patterns = compile_excludes(&opts.exclude)?;
    let mut out = Vec::new();
    for path in &opts.inputs {
        if path_excluded(path, &patterns) {
            continue;
        }
        if path.is_dir() && opts.recursive {
            expand_dir(path, opts, &patterns, &mut out)?;
        } else {
            out.push(path.clone());
        }
    }
    if opts.sort_inputs {
        out.sort();
    }
    Ok(dedupe_inputs(out, opts))
}

/// glob 0.3 defaults let `*` cross `/` and treat `**` as globstar.
/// v1: `*` is one path component; `**` is not recursive.
const EXCLUDE_MATCH: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

fn compile_excludes(globs: &[String]) -> Result<Vec<glob::Pattern>> {
    let mut out = Vec::with_capacity(globs.len());
    for g in globs {
        glob::Pattern::new(g)
            .map_err(|err| AyzenpackError::Usage(format!("invalid --exclude glob '{g}': {err}")))?;
        let flattened = g.replace("**", "*");
        out.push(glob::Pattern::new(&flattened).map_err(|err| {
            AyzenpackError::Usage(format!("invalid --exclude glob '{g}': {err}"))
        })?);
    }
    Ok(out)
}

fn path_excluded(path: &Path, patterns: &[glob::Pattern]) -> bool {
    let path_str = path.to_string_lossy();
    patterns.iter().any(|pat| {
        pat.matches_with(&path_str, EXCLUDE_MATCH)
            || path
                .file_name()
                .is_some_and(|n| pat.matches_with(&n.to_string_lossy(), EXCLUDE_MATCH))
    })
}

fn is_archive_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "jar" | "zip" | "war" | "ear"
            )
        })
}

fn expand_dir(
    dir: &Path,
    opts: &DehydrateOptions,
    patterns: &[glob::Pattern],
    out: &mut Vec<PathBuf>,
) -> Result<()> {
    // follow_links default off: symlink directories are listed, not entered.
    let walker = WalkDir::new(dir).follow_links(opts.follow_symlinks);
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                let e = map_walk_error(err);
                if opts.strict {
                    return Err(e);
                }
                warn(opts, &e.to_string());
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !is_archive_path(path) || path_excluded(path, patterns) {
            continue;
        }
        out.push(path.to_path_buf());
    }
    Ok(())
}

fn map_walk_error(err: walkdir::Error) -> AyzenpackError {
    let path = err.path().map(Path::to_path_buf);
    let source = match err.io_error() {
        Some(ioe) => io::Error::new(ioe.kind(), ioe.to_string()),
        None => io::Error::other(err.to_string()),
    };
    AyzenpackError::Io { source, path }
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
    if opts.json_logs {
        json_event(&serde_json::json!({"event": "warning", "message": msg}));
        return;
    }
    if opts.quiet {
        return;
    }
    eprintln!("ayzenpack: warning: {msg}");
}

fn verbose(opts: &DehydrateOptions, msg: &str) {
    if !opts.verbose {
        return;
    }
    if opts.json_logs {
        json_event(&serde_json::json!({"event": "verbose", "message": msg}));
        return;
    }
    if opts.quiet {
        return;
    }
    eprintln!("ayzenpack: {msg}");
}

/// Filesystem metadata for `--restore-paths`. Canonical path when possible.
fn collect_restore_meta(path: &Path) -> (Option<String>, Option<u32>, Option<u32>, Option<u32>) {
    let restore_path = Some(match path.canonicalize() {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    });
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return (restore_path, None, None, None),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (
            restore_path,
            Some(meta.mode()),
            Some(meta.uid()),
            Some(meta.gid()),
        )
    }
    #[cfg(not(unix))]
    {
        let mode = if meta.permissions().readonly() {
            0o444
        } else {
            0o644
        };
        (restore_path, Some(mode), None, None)
    }
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

    #[test]
    fn can_admit_inflight_allows_first_buffer_even_when_over_cap() {
        // Guards refusing a single entry larger than the cap (would hang the pipeline).
        assert!(can_admit_inflight(0, 100, 50));
        assert!(can_admit_inflight(0, 1, 0));
        assert!(can_admit_inflight(40, 10, 50));
        assert!(!can_admit_inflight(40, 11, 50));
        assert!(!can_admit_inflight(50, 1, 50));
        assert!(!can_admit_inflight(1, 1, 0));
    }

    fn write_test_jar(path: &Path, files: &[(&str, &[u8])]) {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        use zip::ZipWriter;
        let mut z = ZipWriter::new(File::create(path).unwrap());
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, data) in files {
            z.start_file(*name, opts).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn max_inflight_bytes_is_honored() {
        // Logical cap: peak inflight never exceeds the budget except a lone oversized buffer.
        let dir = tempfile::tempdir().unwrap();
        let jar = dir.path().join("many.jar");
        let payload = vec![0x5a; 1000];
        let files: Vec<(String, Vec<u8>)> = (0..12)
            .map(|i| (format!("e{i}.bin"), payload.clone()))
            .collect();
        let refs: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_slice()))
            .collect();
        write_test_jar(&jar, &refs);

        let out = dir.path().join("out.ayz");
        let cap = 2500;
        let opts = DehydrateOptions {
            output: out,
            inputs: vec![jar],
            jobs: 4,
            max_inflight_bytes: cap,
            quiet: true,
            ..DehydrateOptions::default()
        };
        dehydrate(&opts).unwrap();
        let peak = last_peak_inflight_bytes();
        assert!(peak > 0, "pipeline must track inflight bytes");
        assert!(
            peak <= cap,
            "peak inflight {peak} exceeded --max-inflight-bytes {cap}"
        );
    }

    #[test]
    fn exclude_glob_matches_cli_path_or_basename_not_globstar() {
        // Guards treating * as globstar or matching only the full path.
        let cases = [
            ("*.sources.jar", "apps/web/lib/foo.sources.jar", true),
            ("*/secret/*", "vendor/secret/x.jar", true),
            ("apps/web/lib/foo.jar", "apps/web/lib/foo.jar", true),
            ("*.sources.jar", "foo.sources.jar", true),
            ("vendor/**", "vendor/a/b.jar", false),
        ];
        for (glob_s, path, want) in cases {
            let pats = compile_excludes(&[glob_s.to_string()]).unwrap();
            assert_eq!(
                path_excluded(Path::new(path), &pats),
                want,
                "{glob_s} vs {path}"
            );
        }
    }

    fn last64_is_trailer_magic(path: &Path) -> bool {
        let bytes = fs::read(path).unwrap();
        bytes.len() >= 64 && bytes[bytes.len() - 64..bytes.len() - 56] == TRAILER_MAGIC
    }

    #[test]
    fn sibling_tmp_path_is_dest_plus_tmp() {
        assert_eq!(
            sibling_tmp_path(Path::new("all.ayz")),
            PathBuf::from("all.ayz.tmp")
        );
        assert_eq!(
            sibling_tmp_path(Path::new("/out/all.ayz")),
            PathBuf::from("/out/all.ayz.tmp")
        );
    }

    #[test]
    fn pending_ayz_drop_does_not_leave_dest_with_bad_trailer() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("all.ayz");
        let pending = PendingAyz::prepare(&dest).unwrap();
        let tmp = pending.tmp.clone();
        let mut incomplete = b"AYZP\x01\x00\x00\x00".to_vec();
        incomplete.extend_from_slice(&[0u8; 64]);
        fs::write(&tmp, &incomplete).unwrap();
        assert!(!last64_is_trailer_magic(&tmp));
        drop(pending);
        assert!(!dest.exists(), "aborted finish must not create dest");
        assert!(!tmp.exists(), "aborted finish must delete the temp file");
    }

    #[test]
    fn pending_ayz_drop_preserves_existing_dest() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("all.ayz");
        fs::write(&dest, b"old-dest-bytes").unwrap();
        let pending = PendingAyz::prepare(&dest).unwrap();
        let tmp = pending.tmp.clone();
        let mut incomplete = b"AYZP\x01\x00\x00\x00".to_vec();
        incomplete.extend_from_slice(&[0u8; 64]);
        fs::write(&tmp, &incomplete).unwrap();
        drop(pending);
        assert_eq!(fs::read(&dest).unwrap(), b"old-dest-bytes");
        assert!(!tmp.exists());
    }

    #[test]
    fn dehydrate_commit_failure_does_not_leave_bad_dest_trailer() {
        let dir = tempfile::tempdir().unwrap();
        let jar = dir.path().join("a.jar");
        write_test_jar(&jar, &[("x.txt", b"hello")]);
        let dest = dir.path().join("all.ayz");
        fs::write(&dest, b"pre-existing").unwrap();
        FAIL_BEFORE_AYZ_COMMIT.with(|c| c.set(true));
        let opts = DehydrateOptions {
            output: dest.clone(),
            inputs: vec![jar],
            quiet: true,
            ..DehydrateOptions::default()
        };
        let err = dehydrate(&opts).unwrap_err();
        assert!(
            matches!(err, AyzenpackError::Format("injected commit failure")),
            "inject hook must fire, got {err:?}"
        );
        assert_eq!(
            fs::read(&dest).unwrap(),
            b"pre-existing",
            "failed commit must not replace dest with a header-only pack"
        );
        assert!(
            !sibling_tmp_path(&dest).exists(),
            "temp must be deleted after commit failure"
        );
        if dest.metadata().map(|m| m.len()).unwrap_or(0) >= 64 {
            assert!(
                last64_is_trailer_magic(&dest),
                "dest last 64 must not fail trailer magic after aborted finish"
            );
        }
    }

    #[test]
    fn dehydrate_success_leaves_no_tmp_and_valid_trailer() {
        let dir = tempfile::tempdir().unwrap();
        let jar = dir.path().join("a.jar");
        write_test_jar(&jar, &[("x.txt", b"hello")]);
        let dest = dir.path().join("all.ayz");
        let opts = DehydrateOptions {
            output: dest.clone(),
            inputs: vec![jar],
            quiet: true,
            ..DehydrateOptions::default()
        };
        dehydrate(&opts).unwrap();
        assert!(dest.is_file());
        assert!(!sibling_tmp_path(&dest).exists());
        assert!(
            last64_is_trailer_magic(&dest),
            "successful dehydrate must write AYZPTLR1"
        );
    }

    #[test]
    fn verify_finished_ayz_requires_position_len_and_magic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.ayz");
        let mut body = vec![0u8; 8];
        body.extend_from_slice(&TRAILER_MAGIC);
        body.extend_from_slice(&[0u8; 56]);
        fs::write(&path, &body).unwrap();
        let mut f = File::options().read(true).write(true).open(&path).unwrap();
        f.seek(SeekFrom::End(0)).unwrap();
        verify_finished_ayz(&mut f, body.len() as u64).unwrap();

        f.seek(SeekFrom::Start(0)).unwrap();
        let err = verify_finished_ayz(&mut f, body.len() as u64).unwrap_err();
        assert!(
            matches!(
                err,
                AyzenpackError::Format("stream position != written file length")
            ),
            "pos != len must error, got {err:?}"
        );

        fs::write(&path, vec![0u8; body.len()]).unwrap();
        let mut f = File::options().read(true).write(true).open(&path).unwrap();
        f.seek(SeekFrom::End(0)).unwrap();
        let err = verify_finished_ayz(&mut f, body.len() as u64).unwrap_err();
        assert!(
            matches!(
                err,
                AyzenpackError::Format("trailer magic missing after write")
            ),
            "garbage tail must error, got {err:?}"
        );
    }
}
