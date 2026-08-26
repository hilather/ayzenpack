use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};

use super::record::{blob_record_len, write_record};
use super::toc::{write_toc, Toc, TocEntry};
use super::{
    io_error, write_header, write_trailer, FileHeader, Record, Trailer, BUF_WRITER_CAP, TRAILER_LEN,
    TRAILER_MAGIC,
};
use crate::error::{AyzenpackError, Result};

/// Flush a blob frame when uncompressed BLOB record bytes would exceed this.
pub const BLOB_FRAME_FLUSH: u64 = 4 * 1024 * 1024;

enum AyzState<W: Write> {
    Idle(BufWriter<W>),
    Frame(zstd::stream::Encoder<'static, BufWriter<W>>),
}

struct PendingRow {
    blake3: [u8; 32],
    zstd_off: u64,
    rec_off: u64,
}

/// Streaming v2 writer: Idle or one open zstd frame.
pub struct AyzWriter<W: Write + Read + Seek> {
    state: Option<AyzState<W>>,
    zstd_level: i32,
    header_total: u64,
    header_len: u32,
    payload_bytes: u64,
    pending: Vec<PendingRow>,
    toc_entries: Vec<TocEntry>,
    frame_rec_bytes: u64,
    frame_rec_off: u64,
    manifest_zstd_off: u64,
    manifest_zstd_len: u64,
}

impl AyzWriter<File> {
    /// Open `output`, write the header, leave the writer Idle (no blob frame yet).
    pub fn start(output: &std::path::Path, header: &FileHeader) -> Result<Self> {
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
        let header_total = file.stream_position().map_err(io_error)?;
        Ok(Self::after_header(
            file,
            header_len,
            header_total,
            header.zstd_level,
        ))
    }
}

impl<W: Write + Read + Seek> AyzWriter<W> {
    pub fn after_header(inner: W, header_len: u32, header_total: u64, zstd_level: i32) -> Self {
        Self {
            state: Some(AyzState::Idle(BufWriter::with_capacity(
                BUF_WRITER_CAP,
                inner,
            ))),
            zstd_level,
            header_total,
            header_len,
            payload_bytes: 0,
            pending: Vec::new(),
            toc_entries: Vec::new(),
            frame_rec_bytes: 0,
            frame_rec_off: 0,
            manifest_zstd_off: 0,
            manifest_zstd_len: 0,
        }
    }

    pub fn header_total(&self) -> u64 {
        self.header_total
    }

    pub fn header_len(&self) -> u32 {
        self.header_len
    }

    pub fn payload_bytes(&self) -> u64 {
        self.payload_bytes
    }

    pub fn toc(&self) -> Toc {
        Toc {
            entries: self.toc_entries.clone(),
            manifest_zstd_off: self.manifest_zstd_off,
            manifest_zstd_len: self.manifest_zstd_len,
        }
    }

    fn in_frame(&self) -> bool {
        matches!(self.state, Some(AyzState::Frame(_)))
    }

    fn start_frame(&mut self) -> Result<()> {
        let idle = match self.state.take() {
            Some(AyzState::Idle(w)) => w,
            Some(other) => {
                self.state = Some(other);
                return Ok(());
            }
            None => return Err(AyzenpackError::Format("AyzWriter missing state")),
        };
        let mut enc = zstd::stream::Encoder::new(idle, self.zstd_level).map_err(io_error)?;
        enc.include_checksum(false).map_err(io_error)?;
        self.state = Some(AyzState::Frame(enc));
        self.frame_rec_bytes = 0;
        self.frame_rec_off = 0;
        Ok(())
    }

    fn end_frame(&mut self) -> Result<u64> {
        let enc = match self.state.take() {
            Some(AyzState::Frame(enc)) => enc,
            Some(idle) => {
                self.state = Some(idle);
                return Ok(0);
            }
            None => return Err(AyzenpackError::Format("AyzWriter missing state")),
        };
        let mut w = enc.finish().map_err(io_error)?;
        w.flush().map_err(io_error)?;
        let file_pos = w.get_mut().stream_position().map_err(io_error)?;
        let start = self
            .header_total
            .checked_add(self.payload_bytes)
            .ok_or(AyzenpackError::Format("payload_bytes overflow"))?;
        if file_pos < start {
            return Err(AyzenpackError::Format(
                "zstd payload shorter than file header",
            ));
        }
        let zstd_len = file_pos - start;
        self.payload_bytes = self
            .payload_bytes
            .checked_add(zstd_len)
            .ok_or(AyzenpackError::Format("payload_bytes overflow"))?;
        for row in self.pending.drain(..) {
            self.toc_entries.push(TocEntry {
                blake3: row.blake3,
                zstd_off: row.zstd_off,
                zstd_len,
                rec_off: row.rec_off,
            });
        }
        self.state = Some(AyzState::Idle(w));
        self.frame_rec_bytes = 0;
        self.frame_rec_off = 0;
        Ok(zstd_len)
    }

    /// Write a BLOB record, starting/flushing frames at 4 MiB uncompressed record bytes.
    pub fn write_blob(&mut self, hash: &[u8; 32], data: &[u8]) -> Result<()> {
        let rec_len = blob_record_len(data.len() as u64);
        if self.in_frame()
            && self.frame_rec_bytes > 0
            && self.frame_rec_bytes.saturating_add(rec_len) > BLOB_FRAME_FLUSH
        {
            self.end_frame()?;
        }
        if !self.in_frame() {
            self.start_frame()?;
        }
        self.pending.push(PendingRow {
            blake3: *hash,
            zstd_off: self.payload_bytes,
            rec_off: self.frame_rec_off,
        });
        match self.state.as_mut() {
            Some(AyzState::Frame(enc)) => {
                write_blob_bytes(enc, hash, data)?;
            }
            _ => return Err(AyzenpackError::Format("AyzWriter not in a frame")),
        }
        self.frame_rec_off = self.frame_rec_off.saturating_add(rec_len);
        self.frame_rec_bytes = self.frame_rec_bytes.saturating_add(rec_len);
        Ok(())
    }

    /// Close any blob frame, write MANIFEST+END in their own frame, then TOC + trailer.
    pub fn finish(
        mut self,
        manifest_json: &[u8],
        digest: [u8; 32],
        blob_count: u64,
        blob_bytes: u64,
        jar_count: u64,
        version: u32,
    ) -> Result<(Trailer, u64)> {
        if self.in_frame() {
            self.end_frame()?;
        }
        self.manifest_zstd_off = self.payload_bytes;
        self.start_frame()?;
        match self.state.as_mut() {
            Some(AyzState::Frame(enc)) => {
                write_record(
                    enc,
                    &Record::Manifest {
                        json: manifest_json.to_vec(),
                    },
                )?;
                write_record(enc, &Record::End { digest })?;
            }
            _ => return Err(AyzenpackError::Format("AyzWriter not in a frame")),
        }
        self.manifest_zstd_len = self.end_frame()?;

        let toc = Toc {
            entries: std::mem::take(&mut self.toc_entries),
            manifest_zstd_off: self.manifest_zstd_off,
            manifest_zstd_len: self.manifest_zstd_len,
        };
        let toc_len = toc.encoded_len();

        let mut w = match self.state.take() {
            Some(AyzState::Idle(w)) => w,
            _ => {
                return Err(AyzenpackError::Format(
                    "AyzWriter not idle after manifest frame",
                ))
            }
        };
        write_toc(&mut w, &toc)?;
        let trailer = Trailer {
            payload_bytes: self.payload_bytes,
            manifest_len: manifest_json.len() as u64,
            blob_count,
            blob_bytes,
            jar_count,
            header_len: self.header_len,
            version,
            toc_len,
        };
        write_trailer(&mut w, &trailer)?;
        w.flush().map_err(io_error)?;
        let expected_len = self
            .header_total
            .checked_add(self.payload_bytes)
            .and_then(|x| x.checked_add(toc_len))
            .and_then(|x| x.checked_add(TRAILER_LEN))
            .ok_or(AyzenpackError::Format("file length overflow"))?;
        verify_finished_ayz(w.get_mut(), expected_len)?;
        Ok((trailer, expected_len))
    }
}

fn write_blob_bytes<W: Write>(w: &mut W, hash: &[u8; 32], data: &[u8]) -> Result<()> {
    w.write_all(&[crate::format::REC_BLOB]).map_err(io_error)?;
    w.write_all(hash).map_err(io_error)?;
    w.write_all(&(data.len() as u64).to_le_bytes())
        .map_err(io_error)?;
    w.write_all(data).map_err(io_error)?;
    Ok(())
}

/// After flush: `stream_position` must match file length, and the last 64 bytes
/// must be a real `AYZPTLR1` trailer. Do not trust length alone.
pub fn verify_finished_ayz<F: Read + Seek>(file: &mut F, expected_len: u64) -> Result<()> {
    let pos = file.stream_position().map_err(io_error)?;
    let file_len = file.seek(SeekFrom::End(0)).map_err(io_error)?;
    if pos != file_len {
        return Err(AyzenpackError::Format(
            "stream position != written file length",
        ));
    }
    if file_len != expected_len {
        return Err(AyzenpackError::Format(
            "file length != header_total + payload_bytes + toc_len + 64",
        ));
    }
    if file_len < TRAILER_LEN {
        return Err(AyzenpackError::Format("truncated trailer"));
    }
    file.seek(SeekFrom::Start(file_len - TRAILER_LEN))
        .map_err(io_error)?;
    let mut tail = [0u8; 64];
    file.read_exact(&mut tail).map_err(io_error)?;
    if tail[0..8] != TRAILER_MAGIC {
        return Err(AyzenpackError::Format("trailer magic missing after write"));
    }
    Ok(())
}
