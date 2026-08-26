//! Shared ZipWriter helpers for in-test JAR fixtures (no JDK).
#![allow(dead_code)]

use std::fs::File;
use std::io::Write;
use std::path::Path;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

#[path = "spring_launch.rs"]
mod spring_launch;
#[allow(unused_imports)]
pub use spring_launch::spring_boot_launch_script;

pub fn write_jar(path: &Path, files: &[(&str, &[u8])]) {
    let mut z = ZipWriter::new(File::create(path).unwrap());
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in files {
        z.start_file(*name, opts).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap();
}

/// Realistic Spring Boot `executable: true` launcher (shebang + short comment).
pub const SPRING_LAUNCHER: &[u8] = b"#!/bin/bash\n\
#    .   ____          _            __ _ _\n\
#   :: Spring Boot Startup Script ::\n\
";

/// Write `launcher` then a tiny JAR built with [`write_jar`].
pub fn write_wrapped_jar(path: &Path, launcher: &[u8], files: &[(&str, &[u8])]) {
    std::fs::write(path, wrapped_jar_bytes(launcher, files, false)).unwrap();
}

/// Same as [`write_wrapped_jar`], then add `launcher.len()` to the EOCD CD
/// offset and each central-directory local-header offset (`zip -A`).
pub fn write_wrapped_jar_adjusted(path: &Path, launcher: &[u8], files: &[(&str, &[u8])]) {
    std::fs::write(path, wrapped_jar_bytes(launcher, files, true)).unwrap();
}

fn wrapped_jar_bytes(launcher: &[u8], files: &[(&str, &[u8])], adjust: bool) -> Vec<u8> {
    use std::io::Cursor;
    let mut z = ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in files {
        z.start_file(*name, opts).unwrap();
        z.write_all(data).unwrap();
    }
    let zip = z.finish().unwrap().into_inner();
    let mut out = Vec::with_capacity(launcher.len() + zip.len());
    out.extend_from_slice(launcher);
    out.extend_from_slice(&zip);
    if adjust {
        adjust_self_extracting_offsets(&mut out, u32::try_from(launcher.len()).unwrap());
    }
    out
}

/// Prepend `launcher` to a Zip64 JAR (`large_file` so EOCD uses Zip64 sentinels).
pub fn write_wrapped_zip64_jar(path: &Path, launcher: &[u8], files: &[(&str, &[u8])]) {
    std::fs::write(path, wrapped_zip64_bytes(launcher, files)).unwrap();
}

pub fn zip64_jar_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Cursor;
    let mut z = ZipWriter::new(Cursor::new(Vec::new()));
    // Force a Zip64 EOCD/locator (same footer Spring fat JARs write when they flip Zip64).
    z.set_zip64_comment(Some(""));
    let opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(true);
    for (name, data) in files {
        z.start_file(*name, opts).unwrap();
        z.write_all(data).unwrap();
    }
    let zip = z.finish().unwrap().into_inner();
    assert!(
        zip.windows(4).any(|w| w == b"PK\x06\x06"),
        "Zip64 EOCD (PK\\x06\\x06) must be present"
    );
    assert!(
        zip.windows(4).any(|w| w == b"PK\x06\x07"),
        "Zip64 locator (PK\\x06\\x07) must be present"
    );
    zip
}

fn wrapped_zip64_bytes(launcher: &[u8], files: &[(&str, &[u8])]) -> Vec<u8> {
    let zip = zip64_jar_bytes(files);
    let mut out = Vec::with_capacity(launcher.len() + zip.len());
    out.extend_from_slice(launcher);
    out.extend_from_slice(&zip);
    out
}

/// Info-ZIP `zip -A`: CD/local offsets become file-absolute (include the stub).
fn adjust_self_extracting_offsets(buf: &mut [u8], delta: u32) {
    const EOCD_MIN: usize = 22;
    let eocd = {
        assert!(buf.len() >= EOCD_MIN);
        let mut i = buf.len() - EOCD_MIN;
        loop {
            if buf[i..i + 4] == *b"PK\x05\x06" {
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
        assert_eq!(&buf[i..i + 4], b"PK\x01\x02", "central directory signature");
        let name_len = u16::from_le_bytes([buf[i + 28], buf[i + 29]]) as usize;
        let extra_len = u16::from_le_bytes([buf[i + 30], buf[i + 31]]) as usize;
        let comment_len = u16::from_le_bytes([buf[i + 32], buf[i + 33]]) as usize;
        let local_off = u32::from_le_bytes(buf[i + 42..i + 46].try_into().unwrap());
        buf[i + 42..i + 46].copy_from_slice(&(local_off + delta).to_le_bytes());
        i += 46 + name_len + extra_len + comment_len;
    }
    assert_eq!(i, cd_end, "central directory walk must consume cd_size");
    buf[eocd + 16..eocd + 20]
        .copy_from_slice(&(u32::try_from(cd_off).unwrap() + delta).to_le_bytes());
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
    write_stored_zip(
        path,
        &files
            .iter()
            .map(|(name, data)| (*name, *data, crc32fast::hash(data)))
            .collect::<Vec<_>>(),
    );
}

/// Stored ZIP (method 0, DOS 0,0). Duplicate names become separate CD entries.
/// `crc` may disagree with the payload (lying CRC fixture).
pub fn write_stored_zip(path: &Path, files: &[(&str, &[u8], u32)]) {
    let mut local = Vec::new();
    let mut central = Vec::new();
    for (name, data, crc) in files {
        let name_b = name.as_bytes();
        let crc = *crc;
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
