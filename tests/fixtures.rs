//! Shared ZipWriter helpers for in-test JAR fixtures (no JDK).
#![allow(dead_code)]

use std::fs::File;
use std::io::Write;
use std::path::Path;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

pub fn write_jar(path: &Path, files: &[(&str, &[u8])]) {
    let mut z = ZipWriter::new(File::create(path).unwrap());
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in files {
        z.start_file(*name, opts).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap();
}

pub enum JarEntry<'a> {
    File {
        name: &'a str,
        data: &'a [u8],
        method: CompressionMethod,
    },
    Dir {
        name: &'a str,
    },
}

pub fn write_jar_entries(path: &Path, entries: &[JarEntry<'_>]) {
    write_jar_entries_with_mtime(path, entries, DateTime::default());
}

pub fn write_jar_entries_with_mtime(path: &Path, entries: &[JarEntry<'_>], mtime: DateTime) {
    let mut z = ZipWriter::new(File::create(path).unwrap());
    for entry in entries {
        match entry {
            JarEntry::File { name, data, method } => {
                let opts = SimpleFileOptions::default()
                    .compression_method(*method)
                    .last_modified_time(mtime);
                z.start_file(*name, opts).unwrap();
                z.write_all(data).unwrap();
            }
            JarEntry::Dir { name } => {
                let opts = SimpleFileOptions::default().last_modified_time(mtime);
                z.add_directory(*name, opts).unwrap();
            }
        }
    }
    z.finish().unwrap();
}

/// Stored ZIP whose local + central DOS timestamps are the invalid pair 0,0.
/// Scan records `dos_date=0, dos_time=0`; rehydrate must not panic.
pub fn write_stored_jar_dos_zero(path: &Path, files: &[(&str, &[u8])]) {
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
