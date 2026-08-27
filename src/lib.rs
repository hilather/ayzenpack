#![forbid(unsafe_code)]

pub mod cas;
mod deflate;
pub mod dehydrate;
pub mod error;
mod exact;
mod reconstruct;
pub mod format;
pub mod hashutil;
pub mod manifest;
pub mod rehydrate;
pub mod scan;
pub mod stats;

pub use dehydrate::{dehydrate, DehydrateOptions, DehydrateSummary};
pub use error::{AyzenpackError, Result};
pub use format::{FileHeader, Record, Trailer};
pub use manifest::Manifest;
pub use reconstruct::reconstruct_child_zip;
pub use rehydrate::{rehydrate, RehydrateOptions};

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use crate::format::{read_ayz_file, read_manifest_records};
use crate::hashutil::{blake3_bytes, hex_lower, parse_blake3_hex, sha256_bytes};
use crate::manifest::{Entry, NestedIndex, MANIFEST_FORMAT};

fn open_ayz(input: &Path) -> Result<(FileHeader, Trailer, Vec<Record>)> {
    let mut file = File::open(input).map_err(|source| AyzenpackError::Io {
        source,
        path: Some(input.to_path_buf()),
    })?;
    read_ayz_file(&mut file)
}

fn manifest_from_records(records: &[Record]) -> Result<Manifest> {
    let json = records
        .iter()
        .find_map(|r| match r {
            Record::Manifest { json } => Some(json.as_slice()),
            _ => None,
        })
        .ok_or(AyzenpackError::Format("missing MANIFEST record"))?;
    Ok(serde_json::from_slice(json)?)
}

/// First 12 hex chars of a BLAKE3 id; verify mismatch text includes this prefix.
fn hex_prefix(hash: &[u8; 32]) -> String {
    let hex = hex_lower(hash);
    hex.get(..12).unwrap_or(hex.as_str()).to_string()
}

/// Read the MANIFEST. v1 decompresses the full zstd payload; v2 seeks the last frame via the TOC.
pub fn list(input: &Path) -> Result<Manifest> {
    let mut file = File::open(input).map_err(|source| AyzenpackError::Io {
        source,
        path: Some(input.to_path_buf()),
    })?;
    let (_layout, records) = read_manifest_records(&mut file)?;
    manifest_from_records(&records)
}

/// Re-hash blobs; check END digest, catalog SHA-256/size, entry CRC, and blob presence.
///
/// Integrity failures are `HashMismatch` (CLI `verify` maps those to exit 3). Structural
/// problems (`NotAyzenpack`, truncated trailer, JSON) stay `Format`/`Io`/`Json`.
pub fn verify(input: &Path) -> Result<()> {
    let (_header, _trailer, records) = open_ayz(input)?;

    let mut payloads: HashMap<[u8; 32], Vec<u8>> = HashMap::new();
    let mut first_seen = blake3::Hasher::new();
    let mut manifest_json = None;
    let mut end_digest = None;

    for rec in records {
        match rec {
            Record::Blob { hash, data } => {
                let got = blake3_bytes(&data);
                if got != hash {
                    return Err(AyzenpackError::HashMismatch(format!(
                        "blob {}",
                        hex_prefix(&hash)
                    )));
                }
                first_seen.update(&hash);
                payloads.insert(hash, data);
            }
            Record::Manifest { json } => {
                manifest_json = Some(json);
            }
            Record::End { digest } => {
                end_digest = Some(digest);
            }
        }
    }

    let end_digest = end_digest.ok_or(AyzenpackError::Format("missing END record"))?;
    let stream_digest = *first_seen.finalize().as_bytes();
    if stream_digest != end_digest {
        return Err(AyzenpackError::HashMismatch("END digest".into()));
    }

    let json = manifest_json.ok_or(AyzenpackError::Format("missing MANIFEST record"))?;
    let manifest: Manifest = serde_json::from_slice(&json)?;
    if manifest.format != MANIFEST_FORMAT {
        return Err(AyzenpackError::Format(
            "manifest format must be ayzenpack-manifest",
        ));
    }

    let mut cat = blake3::Hasher::new();
    for blob in &manifest.blobs {
        let hash = parse_blake3_hex(&blob.blake3)?;
        cat.update(&hash);
        let data = payloads.get(&hash).ok_or_else(|| {
            AyzenpackError::HashMismatch(format!("missing blob {}", hex_prefix(&hash)))
        })?;
        if data.len() as u64 != blob.size {
            return Err(AyzenpackError::HashMismatch(format!(
                "blob {} size",
                hex_prefix(&hash)
            )));
        }
        let got_sha = hex_lower(&sha256_bytes(data));
        if !got_sha.eq_ignore_ascii_case(&blob.sha256) {
            return Err(AyzenpackError::HashMismatch(format!(
                "blob {} sha256",
                hex_prefix(&hash)
            )));
        }
    }
    if *cat.finalize().as_bytes() != end_digest {
        return Err(AyzenpackError::HashMismatch("END digest".into()));
    }

    for jar in &manifest.jars {
        if let Some(hex) = &jar.prefix_blob {
            let hash = parse_blake3_hex(hex)?;
            let data = payloads.get(&hash).ok_or_else(|| {
                AyzenpackError::HashMismatch(format!(
                    "missing prefix blob {} for {}",
                    hex_prefix(&hash),
                    jar.name
                ))
            })?;
            if let Some(sz) = jar.prefix_size {
                if data.len() as u64 != sz {
                    return Err(AyzenpackError::HashMismatch(format!(
                        "prefix blob {} {} size",
                        hex_prefix(&hash),
                        jar.name
                    )));
                }
            }
        }
        for (hex, sz, label) in [
            (jar.leading_pad_blob.as_deref(), jar.leading_pad_size, "leading_pad"),
            (jar.tail_blob.as_deref(), jar.tail_size, "tail"),
            (jar.raw_zip_blob.as_deref(), jar.raw_zip_size, "raw_zip"),
        ] {
            require_sized_blob(&payloads, hex, sz, label, &jar.name)?;
        }
        for e in &jar.entries {
            verify_entry(&payloads, &jar.name, e, Some(jar))?;
        }
        for (i, nested) in jar.nestedindexes.iter().enumerate() {
            verify_nested_index(&payloads, &jar.name, i, nested)?;
        }
    }

    Ok(())
}

fn require_sized_blob(
    payloads: &HashMap<[u8; 32], Vec<u8>>,
    hex: Option<&str>,
    sz: Option<u64>,
    label: &str,
    jar_name: &str,
) -> Result<()> {
    let Some(hex) = hex else {
        return Ok(());
    };
    let hash = parse_blake3_hex(hex)?;
    let data = payloads.get(&hash).ok_or_else(|| {
        AyzenpackError::HashMismatch(format!(
            "missing {label} blob {} for {}",
            hex_prefix(&hash),
            jar_name
        ))
    })?;
    if let Some(sz) = sz {
        if data.len() as u64 != sz {
            return Err(AyzenpackError::HashMismatch(format!(
                "{label} blob {} {} size",
                hex_prefix(&hash),
                jar_name
            )));
        }
    }
    Ok(())
}

fn require_present_blob(
    payloads: &HashMap<[u8; 32], Vec<u8>>,
    hex: &str,
    jar_name: &str,
    entry_name: &str,
) -> Result<[u8; 32]> {
    let hash = parse_blake3_hex(hex)?;
    if !payloads.contains_key(&hash) {
        return Err(AyzenpackError::HashMismatch(format!(
            "missing blob {} for {}!{}",
            hex_prefix(&hash),
            jar_name,
            entry_name
        )));
    }
    Ok(hash)
}

fn verify_entry(
    payloads: &HashMap<[u8; 32], Vec<u8>>,
    jar_name: &str,
    e: &Entry,
    jar: Option<&crate::manifest::Jar>,
) -> Result<()> {
    if e.blob.is_some() && e.zip_index.is_some() {
        return Err(AyzenpackError::FormatOwned(format!(
            "{}!{} has both blob and zip_index",
            jar_name, e.name
        )));
    }
    if let Some(i) = e.zip_index {
        let Some(jar) = jar else {
            return Err(AyzenpackError::FormatOwned(format!(
                "{}!{} nested zip_index past depth 1 is not supported",
                jar_name, e.name
            )));
        };
        let index = jar.nested_index(i)?;
        let bytes = reconstruct_child_zip(index, e.uncompressed_size, |hex| {
            let hash = parse_blake3_hex(hex)?;
            payloads.get(&hash).cloned().ok_or_else(|| {
                AyzenpackError::HashMismatch(format!(
                    "missing blob {} for {}!{}",
                    hex_prefix(&hash),
                    jar_name,
                    e.name
                ))
            })
        })?;
        if bytes.len() as u64 != e.uncompressed_size {
            return Err(AyzenpackError::HashMismatch(format!(
                "{}!{} zip_index size: recorded {} computed {}",
                jar_name,
                e.name,
                e.uncompressed_size,
                bytes.len()
            )));
        }
        let crc = crc32fast::hash(&bytes);
        if crc != e.crc32 {
            return Err(AyzenpackError::HashMismatch(format!(
                "{}!{} zip_index crc32",
                jar_name, e.name
            )));
        }
    } else if !e.is_dir {
        let hex = e.blob.as_deref().ok_or_else(|| {
            AyzenpackError::HashMismatch(format!("{}!{} missing blob id", jar_name, e.name))
        })?;
        let hash = parse_blake3_hex(hex)?;
        let data = payloads.get(&hash).ok_or_else(|| {
            AyzenpackError::HashMismatch(format!(
                "missing blob {} for {}!{}",
                hex_prefix(&hash),
                jar_name,
                e.name
            ))
        })?;
        let crc = crc32fast::hash(data);
        if crc != e.crc32 {
            return Err(AyzenpackError::HashMismatch(format!(
                "blob {} {}!{} crc32",
                hex_prefix(&hash),
                jar_name,
                e.name
            )));
        }
    }
    for hex in [
        e.cdata_blob.as_deref(),
        e.local_header_blob.as_deref(),
        e.pad_blob.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        require_present_blob(payloads, hex, jar_name, &e.name)?;
    }
    Ok(())
}

fn verify_nested_index(
    payloads: &HashMap<[u8; 32], Vec<u8>>,
    jar_name: &str,
    i: usize,
    nested: &NestedIndex,
) -> Result<()> {
    let label = format!("{jar_name} nestedindexes[{i}]");
    require_sized_blob(
        payloads,
        nested.prefix_blob.as_deref(),
        nested.prefix_size,
        "prefix",
        &label,
    )?;
    require_sized_blob(
        payloads,
        nested.leading_pad_blob.as_deref(),
        nested.leading_pad_size,
        "leading_pad",
        &label,
    )?;
    require_sized_blob(
        payloads,
        nested.tail_blob.as_deref(),
        nested.tail_size,
        "tail",
        &label,
    )?;
    for e in &nested.entries {
        verify_entry(payloads, &label, e, None)?;
    }
    Ok(())
}
