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

/// Official launch.script + Zip64 outer + `zip -A` + several fat `BOOT-INF/lib`
/// members (**DEFLATED**, not STORE). DEFLATE hides rust zip's STORE-nested
/// EOCD latch on `ZipView(prefix)` — keep this fixture; do not flip it to STORE.
/// Listable on 0.2.1; homemade `find_cd_bounds` there missed the Zip64 locator
/// (classic EOCD fields are not sentinels) → `ZipExact::Raw`.
pub fn write_fat_spring_zip64_zipa_jar(path: &Path) {
    use std::io::Cursor;
    let launcher = spring_boot_launch_script();
    let nested: Vec<Vec<u8>> = (0..4)
        .map(|i| inner_incompressible_jar(i, 128 * 1024))
        .collect();
    let mut z = ZipWriter::new(Cursor::new(Vec::new()));
    z.set_zip64_comment(Some(""));
    // DEFLATE nested libs so zip -A + ZipView(prefix) cannot latch onto an
    // inner jar's CD (STORE nested PK\x01\x02 at a wrong seek). Payloads stay
    // large because the inner jars are incompressible.
    let opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(true);
    z.start_file("App.class", opts).unwrap();
    z.write_all(b"class-bytes-outer").unwrap();
    z.start_file("BOOT-INF/classes/application.properties", opts)
        .unwrap();
    z.write_all(b"x=1\n").unwrap();
    for (i, lib) in nested.iter().enumerate() {
        let name = format!("BOOT-INF/lib/lib{i}.jar");
        z.start_file(&name, opts).unwrap();
        z.write_all(lib).unwrap();
    }
    let zip = z.finish().unwrap().into_inner();
    assert!(
        zip.windows(4).any(|w| w == b"PK\x06\x06"),
        "outer must be Zip64"
    );
    std::fs::write(path, prepend_launcher(&zip, launcher, true)).unwrap();
}

/// Classic-u32 fat: official launch.script + `zip -A` + ≥2 STORE `BOOT-INF/lib`
/// members that are **complete inner zips** (own `PK\x05\x06`). This is the
/// 0.2.2 latch: `ZipArchive::new(ZipView(prefix))` binds an inner EOCD.
pub fn write_fat_spring_store_nested_zipa_jar(path: &Path) {
    write_fat_spring_store_nested(path, true);
}

/// Same STORE nested libs, unadjusted prefix (Spring default, no `zip -A`).
pub fn write_fat_spring_store_nested_jar(path: &Path) {
    write_fat_spring_store_nested(path, false);
}

/// zip-A STORE-nested fat with caller `App.class` and planted inner-jar bytes.
pub fn write_fat_spring_store_nested_zipa_with(
    path: &Path,
    app_class: &[u8],
    lib_payloads: &[Vec<u8>],
) {
    write_store_nested_fat(path, true, app_class, lib_payloads);
}

fn write_fat_spring_store_nested(path: &Path, zip_a: bool) {
    let nested: Vec<Vec<u8>> = (0..2)
        .map(|i| inner_incompressible_jar(i, 128 * 1024))
        .collect();
    write_store_nested_fat(path, zip_a, b"class-bytes-outer", &nested);
}

fn write_store_nested_fat(path: &Path, zip_a: bool, app_class: &[u8], lib_payloads: &[Vec<u8>]) {
    use std::io::Cursor;
    let launcher = spring_boot_launch_script();
    let mut z = ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    z.start_file("App.class", opts).unwrap();
    z.write_all(app_class).unwrap();
    z.start_file("BOOT-INF/classes/application.properties", opts)
        .unwrap();
    z.write_all(b"x=1\n").unwrap();
    for (i, lib) in lib_payloads.iter().enumerate() {
        let name = format!("BOOT-INF/lib/lib{i}.jar");
        z.start_file(name, opts).unwrap();
        z.write_all(lib).unwrap();
    }
    let zip = z.finish().unwrap().into_inner();
    assert!(
        zip.windows(4).any(|w| w == b"PK\x05\x06"),
        "outer must have a classic EOCD"
    );
    assert!(
        !zip.windows(4).any(|w| w == b"PK\x06\x06"),
        "classic u32 helper must not be Zip64"
    );
    for lib in lib_payloads {
        assert!(
            lib.windows(4).any(|w| w == b"PK\x05\x06"),
            "STORE member must be a complete inner zip"
        );
    }
    std::fs::write(path, prepend_launcher(&zip, launcher, zip_a)).unwrap();
}

/// No prefix. A few MiB of STORE nested payload (complete inner zip).
pub fn write_classic_nested_store_jar(path: &Path) {
    use std::io::Cursor;
    let lib = inner_incompressible_jar(7, 2 * 1024 * 1024);
    let mut z = ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    z.start_file("App.class", opts).unwrap();
    z.write_all(b"classic-app").unwrap();
    z.start_file("lib/payload.jar", opts).unwrap();
    z.write_all(&lib).unwrap();
    std::fs::write(path, z.finish().unwrap().into_inner()).unwrap();
}

/// Unadjusted STORE nested zip whose prefix starts with `PK\x03\x04`.
///
/// `detect_zip_layout` returns `prefix_len = 0`; rust zip latches onto an inner
/// nested EOCD. Homemade `find_cd_bounds` still sees the outer CD (App + 2 libs).
pub fn pk_start_unadjusted_store_nested_latch_bytes() -> Vec<u8> {
    use std::io::Cursor;
    let inner0 = latch_inner_zip(0);
    let inner1 = latch_inner_zip(1);
    let mut z = ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    z.start_file("App.class", opts).unwrap();
    z.write_all(b"class-bytes-outer").unwrap();
    z.start_file("BOOT-INF/lib/lib0.jar", opts).unwrap();
    z.write_all(&inner0).unwrap();
    z.start_file("BOOT-INF/lib/lib1.jar", opts).unwrap();
    z.write_all(&inner1).unwrap();
    let zip = z.finish().unwrap().into_inner();
    let mut prefix = b"PK\x03\x04".to_vec();
    prefix.resize(8192, 0xAA);
    prepend_launcher(&zip, &prefix, false)
}

fn latch_inner_zip(i: u8) -> Vec<u8> {
    use std::io::Cursor;
    let mut z = ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    z.start_file(format!("com/LatchInner{i}.class"), opts)
        .unwrap();
    z.write_all(&[i; 2048]).unwrap();
    let bytes = z.finish().unwrap().into_inner();
    assert!(
        bytes.windows(4).any(|w| w == b"PK\x05\x06"),
        "inner must be a complete zip"
    );
    bytes
}

pub fn inner_incompressible_jar(i: usize, nbytes: usize) -> Vec<u8> {
    use std::io::Cursor;
    let mut payload = vec![0u8; nbytes];
    let mut x: u32 = 0xA5A5_0000 ^ (i as u32).wrapping_mul(0x9E37);
    for b in &mut payload {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *b = (x >> 16) as u8;
    }
    let mut z = ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    z.start_file("META-INF/MANIFEST.MF", opts).unwrap();
    z.write_all(b"Manifest-Version: 1.0\n").unwrap();
    let class = format!("com/Lib{i}.class");
    z.start_file(&class, opts).unwrap();
    z.write_all(&payload).unwrap();
    z.finish().unwrap().into_inner()
}

/// Prepend `launcher` to an existing ZIP/JAR. `zip_a` applies Info-ZIP `zip -A`
/// (classic u32 CD/EOCD only — do not use on Zip64).
pub fn prepend_launcher(zip: &[u8], launcher: &[u8], zip_a: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(launcher.len() + zip.len());
    out.extend_from_slice(launcher);
    out.extend_from_slice(zip);
    if zip_a {
        adjust_self_extracting_offsets(&mut out, u32::try_from(launcher.len()).unwrap());
    }
    out
}

/// Info-ZIP `zip -A`: CD/local offsets become file-absolute (include the stub).
pub fn adjust_self_extracting_offsets(buf: &mut [u8], delta: u32) {
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

/// Two distinct-name CD records that share local offset 0.
/// ZipArchive still lists both; homemade `slice_zip` fails (overlap) → 0.2.1 `Raw`.
pub fn write_overlapping_local_zip(path: &Path) {
    // Same CRC so ZipArchive::by_index lists both; distinct names so last-wins
    // does not collapse. Homemade slice still fails (overlapping offsets).
    let payload = b"SAME-payload";
    write_stored_zip(
        path,
        &[
            ("a.txt", payload, crc32fast::hash(payload)),
            ("b.txt", payload, crc32fast::hash(payload)),
        ],
    );
    let mut buf = std::fs::read(path).unwrap();
    let eocd = {
        let mut i = buf.len() - 22;
        loop {
            if buf[i..i + 4] == *b"PK\x05\x06" {
                let comment_len = u16::from_le_bytes([buf[i + 20], buf[i + 21]]) as usize;
                if i + 22 + comment_len == buf.len() {
                    break i;
                }
            }
            assert!(i > 0);
            i -= 1;
        }
    };
    let cd_size = u32::from_le_bytes(buf[eocd + 12..eocd + 16].try_into().unwrap()) as usize;
    let cd_off = u32::from_le_bytes(buf[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    let mut i = cd_off;
    let cd_end = cd_off + cd_size;
    let mut seen = 0u32;
    while i + 46 <= cd_end {
        assert_eq!(&buf[i..i + 4], b"PK\x01\x02");
        let name_len = u16::from_le_bytes(buf[i + 28..i + 30].try_into().unwrap()) as usize;
        let extra_len = u16::from_le_bytes(buf[i + 30..i + 32].try_into().unwrap()) as usize;
        let comment_len = u16::from_le_bytes(buf[i + 32..i + 34].try_into().unwrap()) as usize;
        if seen == 1 {
            buf[i + 42..i + 46].copy_from_slice(&0u32.to_le_bytes());
        }
        seen += 1;
        i += 46 + name_len + extra_len + comment_len;
    }
    assert_eq!(seen, 2);
    std::fs::write(path, buf).unwrap();
}

/// Stored ZIP with GPBF bit 3 and a signed data descriptor after the payload.
pub fn write_data_descriptor_zip(path: &Path, name: &str, data: &[u8]) {
    let name_b = name.as_bytes();
    let crc = crc32fast::hash(data);
    let mut buf = Vec::new();
    buf.extend_from_slice(b"PK\x03\x04");
    buf.extend_from_slice(&20u16.to_le_bytes());
    buf.extend_from_slice(&8u16.to_le_bytes()); // GPBF bit 3
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(name_b);
    buf.extend_from_slice(data);
    buf.extend_from_slice(b"PK\x07\x08");
    buf.extend_from_slice(&crc.to_le_bytes());
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());

    let cd_off = buf.len() as u32;
    buf.extend_from_slice(b"PK\x01\x02");
    buf.extend_from_slice(&20u16.to_le_bytes());
    buf.extend_from_slice(&20u16.to_le_bytes());
    buf.extend_from_slice(&8u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&crc.to_le_bytes());
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(name_b);

    let cd_len = buf.len() as u32 - cd_off;
    buf.extend_from_slice(b"PK\x05\x06");
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&cd_len.to_le_bytes());
    buf.extend_from_slice(&cd_off.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    std::fs::write(path, buf).unwrap();
}

/// Two stored locals with zero padding between them (zipalign-style gap).
pub fn write_padded_locals_zip(
    path: &Path,
    first: (&str, &[u8]),
    second: (&str, &[u8]),
    pad: usize,
) {
    let files = vec![
        (first.0, first.1, crc32fast::hash(first.1)),
        (second.0, second.1, crc32fast::hash(second.1)),
    ];
    let tmp = path.with_extension("tmp.jar");
    write_stored_zip(&tmp, &files);
    let bytes = std::fs::read(&tmp).unwrap();
    std::fs::remove_file(&tmp).unwrap();

    // Insert `pad` zeros after the first local record (header + name + data).
    let name_b = first.0.as_bytes();
    let first_rec = 30 + name_b.len() + first.1.len();
    let mut out = Vec::with_capacity(bytes.len() + pad);
    out.extend_from_slice(&bytes[..first_rec]);
    out.extend(std::iter::repeat(0u8).take(pad));
    out.extend_from_slice(&bytes[first_rec..]);
    // Patch second CD local offset and EOCD CD offset (+pad).
    let eocd = {
        let mut i = out.len() - 22;
        loop {
            if out[i..i + 4] == *b"PK\x05\x06" {
                break i;
            }
            i -= 1;
        }
    };
    let cd_off = u32::from_le_bytes(out[eocd + 16..eocd + 20].try_into().unwrap()) as usize + pad;
    out[eocd + 16..eocd + 20].copy_from_slice(&(cd_off as u32).to_le_bytes());
    let mut i = cd_off;
    let cd_size = u32::from_le_bytes(out[eocd + 12..eocd + 16].try_into().unwrap()) as usize;
    let cd_end = i + cd_size;
    let mut n = 0;
    while i + 46 <= cd_end {
        let name_len = u16::from_le_bytes([out[i + 28], out[i + 29]]) as usize;
        let extra_len = u16::from_le_bytes([out[i + 30], out[i + 31]]) as usize;
        let comment_len = u16::from_le_bytes([out[i + 32], out[i + 33]]) as usize;
        if n == 1 {
            let off = u32::from_le_bytes(out[i + 42..i + 46].try_into().unwrap());
            out[i + 42..i + 46].copy_from_slice(&(off + pad as u32).to_le_bytes());
        }
        n += 1;
        i += 46 + name_len + extra_len + comment_len;
    }
    std::fs::write(path, out).unwrap();
}

pub fn write_jar_with_comment(path: &Path, files: &[(&str, &[u8])], comment: &str) {
    let mut z = ZipWriter::new(File::create(path).unwrap());
    z.set_comment(comment);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in files {
        z.start_file(*name, opts).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap();
}

/// Stored ZIP whose file name is not valid UTF-8 (`name_raw_hex` on scan).
pub fn write_non_utf8_name_zip(path: &Path, data: &[u8]) {
    let name_b: &[u8] = &[0xff, 0xfe, b'.', b't', b'x', b't'];
    let crc = crc32fast::hash(data);
    let mut local = Vec::new();
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

    local.extend_from_slice(b"PK\x01\x02");
    local.extend_from_slice(&20u16.to_le_bytes());
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
    local.extend_from_slice(&0u16.to_le_bytes());
    local.extend_from_slice(&0u16.to_le_bytes());
    local.extend_from_slice(&0u16.to_le_bytes());
    local.extend_from_slice(&0u32.to_le_bytes());
    local.extend_from_slice(&0u32.to_le_bytes());
    local.extend_from_slice(name_b);

    let cd_off = (30 + name_b.len() + data.len()) as u32;
    let cd_len = (46 + name_b.len()) as u32;
    local.extend_from_slice(b"PK\x05\x06");
    local.extend_from_slice(&0u16.to_le_bytes());
    local.extend_from_slice(&0u16.to_le_bytes());
    local.extend_from_slice(&1u16.to_le_bytes());
    local.extend_from_slice(&1u16.to_le_bytes());
    local.extend_from_slice(&cd_len.to_le_bytes());
    local.extend_from_slice(&cd_off.to_le_bytes());
    local.extend_from_slice(&0u16.to_le_bytes());
    std::fs::write(path, local).unwrap();
}

pub fn write_signed_looking_jar(path: &Path) {
    write_jar(
        path,
        &[
            ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n"),
            (
                "META-INF/FOO.SF",
                b"Signature-Version: 1.0\nSHA-256-Digest-Manifest: abc\n",
            ),
            ("META-INF/FOO.RSA", b"pkcs7-placeholder"),
            ("com/App.class", b"class-bytes"),
        ],
    );
}

/// Raw stored-block DEFLATE (RFC 1951). After 0.2.4 this is a `deflate-raw:stored` hit.
pub fn raw_stored_deflate(plain: &[u8]) -> Vec<u8> {
    let len = u16::try_from(plain.len()).expect("stored-block fixture payload fits u16");
    let mut out = vec![0x01];
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(!len).to_le_bytes());
    out.extend_from_slice(plain);
    out
}

struct BuiltLocal {
    name: Vec<u8>,
    method: u16,
    crc: u32,
    uncomp: u32,
    cdata: Vec<u8>,
    extra: Vec<u8>,
}

fn write_locals_and_cd(path: &Path, entries: &[BuiltLocal]) {
    let mut local = Vec::new();
    let mut central = Vec::new();
    for e in entries {
        let off = local.len() as u32;
        local.extend_from_slice(b"PK\x03\x04");
        local.extend_from_slice(&20u16.to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(&e.method.to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(&e.crc.to_le_bytes());
        local.extend_from_slice(&(e.cdata.len() as u32).to_le_bytes());
        local.extend_from_slice(&e.uncomp.to_le_bytes());
        local.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        local.extend_from_slice(&(e.extra.len() as u16).to_le_bytes());
        local.extend_from_slice(&e.name);
        local.extend_from_slice(&e.extra);
        local.extend_from_slice(&e.cdata);

        central.extend_from_slice(b"PK\x01\x02");
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&e.method.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&e.crc.to_le_bytes());
        central.extend_from_slice(&(e.cdata.len() as u32).to_le_bytes());
        central.extend_from_slice(&e.uncomp.to_le_bytes());
        central.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        central.extend_from_slice(&(e.extra.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&off.to_le_bytes());
        central.extend_from_slice(&e.name);
        central.extend_from_slice(&e.extra);
    }
    let cd_off = local.len() as u32;
    let cd_len = central.len() as u32;
    let n = entries.len() as u16;
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

/// zlib-rs raw/nowrap DEFLATE (`window_bits = -15`).
pub fn zlib_raw_deflate(data: &[u8], level: u32) -> Vec<u8> {
    use zlib_rs::{compress_bound, compress_slice, DeflateConfig, ReturnCode};
    let mut cfg = DeflateConfig::new(level as i32);
    cfg.window_bits = -15;
    let bound = compress_bound(data.len()).max(16);
    let mut buf = vec![0u8; bound];
    let (out, rc) = compress_slice(&mut buf, data, cfg);
    assert_eq!(rc, ReturnCode::Ok, "zlib-rs raw deflate level {level}");
    out.to_vec()
}

pub fn flate2_raw_deflate(data: &[u8], level: u32) -> Vec<u8> {
    use flate2::write::DeflateEncoder;
    use flate2::Compression;
    let mut enc = DeflateEncoder::new(Vec::new(), Compression::new(level));
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

fn replace_first_cdata(zip: &[u8], new_cdata: &[u8]) -> Vec<u8> {
    assert_eq!(&zip[..4], b"PK\x03\x04");
    let name_len = u16::from_le_bytes([zip[26], zip[27]]) as usize;
    let extra_len = u16::from_le_bytes([zip[28], zip[29]]) as usize;
    let old_csize = u32::from_le_bytes(zip[18..22].try_into().unwrap()) as usize;
    let header_end = 30 + name_len + extra_len;
    let old_end = header_end + old_csize;
    let delta = new_cdata.len() as i64 - old_csize as i64;

    let mut out = Vec::with_capacity((zip.len() as i64 + delta) as usize);
    out.extend_from_slice(&zip[..18]);
    out.extend_from_slice(&(new_cdata.len() as u32).to_le_bytes());
    out.extend_from_slice(&zip[22..header_end]);
    out.extend_from_slice(new_cdata);
    out.extend_from_slice(&zip[old_end..]);

    let eocd = {
        let mut i = out.len() - 22;
        loop {
            if out[i..i + 4] == *b"PK\x05\x06" {
                break i;
            }
            i -= 1;
        }
    };
    let old_cd = u32::from_le_bytes(out[eocd + 16..eocd + 20].try_into().unwrap());
    let new_cd = (old_cd as i64 + delta) as u32;
    out[eocd + 16..eocd + 20].copy_from_slice(&new_cd.to_le_bytes());
    let i = new_cd as usize;
    out[i + 20..i + 24].copy_from_slice(&(new_cdata.len() as u32).to_le_bytes());
    out
}

fn write_deflate_cdata_zip(path: &Path, name: &str, data: &[u8], cdata: &[u8]) {
    let tmp = path.with_extension("tpl.jar");
    {
        let mut z = ZipWriter::new(File::create(&tmp).unwrap());
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        z.start_file(name, opts).unwrap();
        z.write_all(data).unwrap();
        z.finish().unwrap();
    }
    let tpl = std::fs::read(&tmp).unwrap();
    std::fs::remove_file(&tmp).unwrap();
    std::fs::write(path, replace_first_cdata(&tpl, cdata)).unwrap();
}

/// DEFLATE member whose raw cdata is a zlib-rs bitstream (closed set {1,6,9}).
pub fn write_zlib_deflate_zip(path: &Path, name: &str, data: &[u8]) {
    let cdata = zlib_raw_deflate(data, 6);
    write_deflate_cdata_zip(path, name, data, &cdata);
}

/// Valid inflate that is not zlib/flate2/stored (empty stored prefix + zlib-6).
pub fn unknown_deflate(plain: &[u8]) -> Vec<u8> {
    let mut out = vec![0x00, 0x00, 0x00, 0xff, 0xff];
    out.extend_from_slice(&zlib_raw_deflate(plain, 6));
    out
}

/// zlib-rs level 3 is not a reliable miss (can collide with zlib-6).
pub fn write_unknown_deflate_zip(path: &Path, name: &str, data: &[u8]) {
    write_deflate_cdata_zip(path, name, data, &unknown_deflate(data));
}

pub fn write_unknown_deflate_wrapped(path: &Path, launcher: &[u8], name: &str, data: &[u8]) {
    let tmp = path.with_extension("inner.jar");
    write_unknown_deflate_zip(&tmp, name, data);
    let zip = std::fs::read(&tmp).unwrap();
    std::fs::remove_file(&tmp).unwrap();
    let mut out = launcher.to_vec();
    out.extend_from_slice(&zip);
    std::fs::write(path, out).unwrap();
}

/// Flate2-reproducible file plus a zlib-3 miss (Test 4).
pub fn write_codec_hit_plus_unknown_deflate(
    path: &Path,
    hit_name: &str,
    hit: &[u8],
    miss_name: &str,
    miss: &[u8],
) {
    write_locals_and_cd(
        path,
        &[
            BuiltLocal {
                name: hit_name.as_bytes().to_vec(),
                method: 8,
                crc: crc32fast::hash(hit),
                uncomp: hit.len() as u32,
                cdata: flate2_raw_deflate(hit, 6),
                extra: Vec::new(),
            },
            BuiltLocal {
                name: miss_name.as_bytes().to_vec(),
                method: 8,
                crc: crc32fast::hash(miss),
                uncomp: miss.len() as u32,
                cdata: unknown_deflate(miss),
                extra: Vec::new(),
            },
        ],
    );
}

/// File starts with `PK\x03\x04` garbage; first CD local is after that decoy.
pub fn write_leading_pad_pk_decoy_zip(path: &Path, name: &str, data: &[u8]) {
    let tmp = path.with_extension("inner.jar");
    write_stored_zip(&tmp, &[(name, data, crc32fast::hash(data))]);
    let zip = std::fs::read(&tmp).unwrap();
    std::fs::remove_file(&tmp).unwrap();
    let mut decoy = b"PK\x03\x04".to_vec();
    decoy.extend_from_slice(&[0xAA; 28]);
    let mut out = decoy.clone();
    out.extend_from_slice(&zip);
    adjust_self_extracting_offsets(&mut out, u32::try_from(decoy.len()).unwrap());
    std::fs::write(path, out).unwrap();
}

fn classic_eocd_off(buf: &[u8]) -> usize {
    let mut i = buf.len() - 22;
    loop {
        if buf[i..i + 4] == *b"PK\x05\x06" {
            let comment_len = u16::from_le_bytes([buf[i + 20], buf[i + 21]]) as usize;
            if i + 22 + comment_len == buf.len() {
                return i;
            }
        }
        assert!(i > 0);
        i -= 1;
    }
}

fn splice_trailing_cd_junk(buf: &mut Vec<u8>, junk: &[u8]) {
    let eocd = classic_eocd_off(buf);
    let cd_size = u32::from_le_bytes(buf[eocd + 12..eocd + 16].try_into().unwrap());
    buf.splice(eocd..eocd, junk.iter().copied());
    let new_eocd = eocd + junk.len();
    buf[new_eocd + 12..new_eocd + 16].copy_from_slice(&(cd_size + junk.len() as u32).to_le_bytes());
}

const CD_TRAILING_JUNK: [u8; 4] = [0xAB, 0xCD, 0xEF, 0x01];

/// 46-byte `PK\x01\x02` header whose name_len overruns any leftover after it.
fn magic_but_short_cd_header() -> [u8; 46] {
    let mut stub = [0u8; 46];
    stub[..4].copy_from_slice(b"PK\x01\x02");
    stub[28..30].copy_from_slice(&100u16.to_le_bytes());
    stub
}

/// Listed zip with junk after the last complete CD record, counted in EOCD
/// `cd_size`. Homemade parse must accept N rows + leftover (not skip-exact).
pub fn write_leftover_junk_listed_zip(path: &Path) {
    write_stored_zip(path, &[("a.txt", b"hello", crc32fast::hash(b"hello"))]);
    let mut buf = std::fs::read(path).unwrap();
    splice_trailing_cd_junk(&mut buf, &CD_TRAILING_JUNK);
    std::fs::write(path, buf).unwrap();
}

/// Listed zip whose homemade CD parse is still `None`: N complete `PK\x01\x02`
/// rows, then leftover that starts with CD magic but whose name_len overruns
/// `cd_size`. ZipArchive still lists N via entry count.
pub fn write_truncated_cd_listed_zip(path: &Path) {
    write_stored_zip(path, &[("a.txt", b"hello", crc32fast::hash(b"hello"))]);
    let mut buf = std::fs::read(path).unwrap();
    splice_trailing_cd_junk(&mut buf, &magic_but_short_cd_header());
    std::fs::write(path, buf).unwrap();
}

/// Leftover-junk CD plus a STORE listable nested zip (`lib/inner.jar`).
pub fn write_leftover_junk_plus_store_nested(path: &Path) {
    let inner_tmp = path.with_extension("inner.jar");
    let inner_plain = b"nested-plain";
    write_stored_zip(
        &inner_tmp,
        &[(
            "n.txt",
            inner_plain.as_slice(),
            crc32fast::hash(inner_plain),
        )],
    );
    let inner = std::fs::read(&inner_tmp).unwrap();
    std::fs::remove_file(&inner_tmp).unwrap();
    write_stored_zip(
        path,
        &[
            ("a.txt", b"hello".as_slice(), crc32fast::hash(b"hello")),
            ("lib/inner.jar", inner.as_slice(), crc32fast::hash(&inner)),
        ],
    );
    let mut buf = std::fs::read(path).unwrap();
    splice_trailing_cd_junk(&mut buf, &CD_TRAILING_JUNK);
    std::fs::write(path, buf).unwrap();
}

/// Listed zip whose homemade CD parse is `None` because a CD record is truncated
/// (`extra_len` extends past EOCD `cd_size`). Not leftover junk after N complete
/// records. ZipArchive still lists via EOCD entry count.
pub fn write_truncated_cd_listed_zip(path: &Path) {
    write_stored_zip(path, &[("a.txt", b"hello", crc32fast::hash(b"hello"))]);
    let mut buf = std::fs::read(path).unwrap();
    let eocd = {
        let mut i = buf.len() - 22;
        loop {
            if buf[i..i + 4] == *b"PK\x05\x06" {
                let comment_len = u16::from_le_bytes([buf[i + 20], buf[i + 21]]) as usize;
                if i + 22 + comment_len == buf.len() {
                    break i;
                }
            }
            assert!(i > 0);
            i -= 1;
        }
    };
    let cd_off = u32::from_le_bytes(buf[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    assert_eq!(&buf[cd_off..cd_off + 4], b"PK\x01\x02");
    let extra_len = u16::from_le_bytes(buf[cd_off + 30..cd_off + 32].try_into().unwrap());
    buf[cd_off + 30..cd_off + 32].copy_from_slice(&(extra_len + 8).to_le_bytes());
    std::fs::write(path, buf).unwrap();
}

/// STORE zip with GPBF bit 0 set so rust zip reports `encrypted()`.
pub fn write_encrypted_store_zip(path: &Path) {
    write_stored_zip(path, &[("secret.txt", b"hi", crc32fast::hash(b"hi"))]);
    let mut buf = std::fs::read(path).unwrap();
    buf[6] |= 1;
    let eocd = {
        let mut i = buf.len() - 22;
        loop {
            if buf[i..i + 4] == *b"PK\x05\x06" {
                let comment_len = u16::from_le_bytes([buf[i + 20], buf[i + 21]]) as usize;
                if i + 22 + comment_len == buf.len() {
                    break i;
                }
            }
            assert!(i > 0);
            i -= 1;
        }
    };
    let cd_off = u32::from_le_bytes(buf[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    buf[cd_off + 8] |= 1;
    std::fs::write(path, buf).unwrap();
}

/// Overlap (same local offset, two names) plus one STORE listable inner zip.
pub fn write_overlapping_local_plus_store_nested(path: &Path) {
    let inner_tmp = path.with_extension("inner.jar");
    let inner_plain = b"nested-plain";
    write_stored_zip(
        &inner_tmp,
        &[(
            "n.txt",
            inner_plain.as_slice(),
            crc32fast::hash(inner_plain),
        )],
    );
    let inner = std::fs::read(&inner_tmp).unwrap();
    std::fs::remove_file(&inner_tmp).unwrap();
    let same = b"SAME-payload";
    write_stored_zip(
        path,
        &[
            ("a.txt", same.as_slice(), crc32fast::hash(same)),
            ("b.txt", same.as_slice(), crc32fast::hash(same)),
            ("lib/inner.jar", inner.as_slice(), crc32fast::hash(&inner)),
        ],
    );
    let mut buf = std::fs::read(path).unwrap();
    let eocd = {
        let mut i = buf.len() - 22;
        loop {
            if buf[i..i + 4] == *b"PK\x05\x06" {
                let comment_len = u16::from_le_bytes([buf[i + 20], buf[i + 21]]) as usize;
                if i + 22 + comment_len == buf.len() {
                    break i;
                }
            }
            assert!(i > 0);
            i -= 1;
        }
    };
    let cd_size = u32::from_le_bytes(buf[eocd + 12..eocd + 16].try_into().unwrap()) as usize;
    let cd_off = u32::from_le_bytes(buf[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    let mut i = cd_off;
    let cd_end = cd_off + cd_size;
    let mut seen = 0u32;
    while i + 46 <= cd_end {
        assert_eq!(&buf[i..i + 4], b"PK\x01\x02");
        let name_len = u16::from_le_bytes(buf[i + 28..i + 30].try_into().unwrap()) as usize;
        let extra_len = u16::from_le_bytes(buf[i + 30..i + 32].try_into().unwrap()) as usize;
        let comment_len = u16::from_le_bytes(buf[i + 32..i + 34].try_into().unwrap()) as usize;
        if seen == 1 {
            buf[i + 42..i + 46].copy_from_slice(&0u32.to_le_bytes());
        }
        seen += 1;
        i += 46 + name_len + extra_len + comment_len;
    }
    std::fs::write(path, buf).unwrap();
}

pub fn ayzenpack_cmd() -> assert_cmd::Command {
    assert_cmd::Command::cargo_bin("ayzenpack").unwrap()
}

pub fn matt_dehydrate(output: &Path, input: &Path) {
    ayzenpack_cmd()
        .args([
            "dehydrate",
            "--recursive",
            "--sort-inputs",
            "--restore-paths",
            "-o",
        ])
        .arg(output)
        .arg(input)
        .assert()
        .success();
}

pub fn matt_rehydrate(input: &Path) {
    ayzenpack_cmd()
        .args(["rehydrate", "--restore-paths", "-i"])
        .arg(input)
        .assert()
        .success();
}

/// Compressible payload + stored-block DEFLATE. Built from a ZipWriter template
/// so zip 2.4 can find the EOCD; only the bitstream and sizes change.
pub fn write_stored_block_deflate_zip(path: &Path, name: &str, data: &[u8]) {
    let tmp = path.with_extension("tpl.jar");
    {
        let mut z = ZipWriter::new(File::create(&tmp).unwrap());
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        z.start_file(name, opts).unwrap();
        z.write_all(data).unwrap();
        z.finish().unwrap();
    }
    let tpl = std::fs::read(&tmp).unwrap();
    std::fs::remove_file(&tmp).unwrap();
    std::fs::write(path, replace_first_cdata(&tpl, &raw_stored_deflate(data))).unwrap();
}

pub fn write_stored_block_deflate_wrapped(path: &Path, launcher: &[u8], name: &str, data: &[u8]) {
    let tmp = path.with_extension("inner.jar");
    write_stored_block_deflate_zip(&tmp, name, data);
    let zip = std::fs::read(&tmp).unwrap();
    std::fs::remove_file(&tmp).unwrap();
    let mut out = launcher.to_vec();
    out.extend_from_slice(&zip);
    std::fs::write(path, out).unwrap();
}

/// STORE file plus a Maven-style empty DEFLATE directory (`03 00`).
pub fn write_store_file_plus_empty_deflate_dir(path: &Path, name: &str, data: &[u8]) {
    write_locals_and_cd(
        path,
        &[
            BuiltLocal {
                name: name.as_bytes().to_vec(),
                method: 0,
                crc: crc32fast::hash(data),
                uncomp: data.len() as u32,
                cdata: data.to_vec(),
                extra: Vec::new(),
            },
            BuiltLocal {
                name: b"META-INF/".to_vec(),
                method: 8,
                crc: 0,
                uncomp: 0,
                cdata: vec![0x03, 0x00],
                extra: Vec::new(),
            },
        ],
    );
}

/// Stored-block DEFLATE file plus a Maven-style empty DEFLATE directory (`03 00`).
pub fn write_deflate_miss_plus_empty_deflate_dir(path: &Path, name: &str, data: &[u8]) {
    let cdata = raw_stored_deflate(data);
    write_locals_and_cd(
        path,
        &[
            BuiltLocal {
                name: name.as_bytes().to_vec(),
                method: 8,
                crc: crc32fast::hash(data),
                uncomp: data.len() as u32,
                cdata,
                extra: Vec::new(),
            },
            BuiltLocal {
                name: b"META-INF/".to_vec(),
                method: 8,
                crc: 0,
                uncomp: 0,
                cdata: vec![0x03, 0x00],
                extra: Vec::new(),
            },
        ],
    );
}

/// STORE file plus a method-0 directory with leftover local cdata (uncomp 0, csize 4).
pub fn write_store_file_plus_leftover_csize_dir(path: &Path, name: &str, data: &[u8]) {
    write_locals_and_cd(
        path,
        &[
            BuiltLocal {
                name: name.as_bytes().to_vec(),
                method: 0,
                crc: crc32fast::hash(data),
                uncomp: data.len() as u32,
                cdata: data.to_vec(),
                extra: Vec::new(),
            },
            BuiltLocal {
                name: b"marked/".to_vec(),
                method: 0,
                crc: 0,
                uncomp: 0,
                cdata: b"DIRC".to_vec(),
                extra: Vec::new(),
            },
        ],
    );
}

/// STORE file plus a method-0 directory whose local record has non-empty cdata.
/// No DEFLATE miss — today's ExactWithExotic arm (codec-hit/STORE + class-4 dir).
pub fn write_store_file_plus_dir_cdata(path: &Path, name: &str, data: &[u8]) {
    write_locals_and_cd(
        path,
        &[
            BuiltLocal {
                name: name.as_bytes().to_vec(),
                method: 0,
                crc: crc32fast::hash(data),
                uncomp: data.len() as u32,
                cdata: data.to_vec(),
                extra: Vec::new(),
            },
            BuiltLocal {
                name: b"marked/".to_vec(),
                method: 0,
                crc: 0,
                uncomp: 4,
                cdata: b"DIRC".to_vec(),
                extra: Vec::new(),
            },
        ],
    );
}

/// Stored-block DEFLATE file plus a directory whose local record has non-empty cdata.
pub fn write_deflate_miss_plus_dir_cdata(path: &Path, name: &str, data: &[u8]) {
    let cdata = raw_stored_deflate(data);
    write_locals_and_cd(
        path,
        &[
            BuiltLocal {
                name: name.as_bytes().to_vec(),
                method: 8,
                crc: crc32fast::hash(data),
                uncomp: data.len() as u32,
                cdata,
                extra: Vec::new(),
            },
            BuiltLocal {
                name: b"marked/".to_vec(),
                method: 0,
                crc: 0,
                uncomp: 4,
                cdata: b"DIRC".to_vec(),
                extra: Vec::new(),
            },
        ],
    );
}
