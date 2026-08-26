//! ZIP scan contract: CD order, metadata-only entries, no class forest.

#[path = "fixtures.rs"]
mod fixtures;

use std::fs;
use std::path::Path;

use ayzenpack::error::AyzenpackError;
use ayzenpack::hashutil::hash_both;
use ayzenpack::scan::{for_each_jar_entry, scan_jar, zip_prefix_len, ScannedEntry};
use fixtures::{
    write_jar, write_jar_entries_with_mtime, write_wrapped_jar, write_wrapped_jar_adjusted,
    JarEntry, SPRING_LAUNCHER, SYSTEMD_LAUNCHER,
};
use zip::CompressionMethod;
use zip::DateTime;

const MAX_ENTRY: u64 = 2_147_483_647;

fn temp_jar(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    (dir, path)
}

#[test]
fn scan_two_files_one_dir_preserves_order_and_crc() {
    // Guards name-sorted ZipArchive iteration instead of by_index CD order.
    let (_dir, path) = temp_jar("order.jar");
    let mtime = DateTime::from_date_and_time(2020, 5, 6, 7, 8, 10).unwrap();
    let a = b"aaa";
    let zed = b"zzz-comes-last-alphabetically-but-was-written-second";
    write_jar_entries_with_mtime(
        &path,
        &[
            JarEntry::Dir {
                name: "com/example/",
            },
            JarEntry::File {
                name: "z-last-alpha.txt",
                data: zed,
                method: CompressionMethod::Deflated,
            },
            JarEntry::File {
                name: "a-first-alpha.txt",
                data: a,
                method: CompressionMethod::Stored,
            },
        ],
        mtime,
    );

    let scanned = scan_jar(&path, MAX_ENTRY).unwrap();
    assert_eq!(scanned.entries.len(), 3);
    assert_eq!(scanned.entries[0].name, "com/example/");
    assert!(scanned.entries[0].is_dir);
    assert_eq!(scanned.entries[0].method, "stored");
    assert_eq!(scanned.entries[0].method_code, 0);

    assert_eq!(scanned.entries[1].name, "z-last-alpha.txt");
    assert!(!scanned.entries[1].is_dir);
    assert_eq!(scanned.entries[1].crc32, crc32fast::hash(zed));
    assert_eq!(scanned.entries[1].method, "deflated");
    assert_eq!(scanned.entries[1].method_code, 8);
    assert_eq!(scanned.entries[1].uncompressed_size, zed.len() as u64);
    assert_eq!(scanned.entries[1].dos_date, mtime.datepart());
    assert_eq!(scanned.entries[1].dos_time, mtime.timepart());

    assert_eq!(scanned.entries[2].name, "a-first-alpha.txt");
    assert!(!scanned.entries[2].is_dir);
    assert_eq!(scanned.entries[2].crc32, crc32fast::hash(a));
    assert_eq!(scanned.entries[2].method, "stored");
    assert_eq!(scanned.entries[2].method_code, 0);

    let bytes = fs::read(&path).unwrap();
    let (b3, sha) = hash_both(&bytes);
    assert_eq!(scanned.source_blake3, b3);
    assert_eq!(scanned.source_sha256, sha);
    assert_eq!(scanned.source_size, bytes.len() as u64);
    assert_eq!(scanned.source_path, path);
}

#[test]
fn scan_empty_file_entry() {
    let (_dir, path) = temp_jar("empty.jar");
    write_jar(&path, &[("empty.dat", b"")]);
    let scanned = scan_jar(&path, MAX_ENTRY).unwrap();
    assert_eq!(scanned.entries.len(), 1);
    let e = &scanned.entries[0];
    assert_eq!(e.name, "empty.dat");
    assert!(!e.is_dir);
    assert_eq!(e.uncompressed_size, 0);
    assert_eq!(e.crc32, crc32fast::hash(b""));
}

#[test]
fn scan_utf8_entry_name() {
    let (_dir, path) = temp_jar("utf8.jar");
    write_jar(&path, &[("res/\u{540d}\u{524d}.txt", b"hello")]);
    let scanned = scan_jar(&path, MAX_ENTRY).unwrap();
    assert_eq!(scanned.entries.len(), 1);
    assert_eq!(scanned.entries[0].name, "res/名前.txt");
    assert!(
        scanned.entries[0].name_raw_hex.is_none(),
        "valid UTF-8 names must not set name_raw_hex"
    );
}

#[test]
fn scan_nested_jar_is_one_entry() {
    // Nested JARs are opaque blobs; do not explode to a class forest.
    let dir = tempfile::tempdir().unwrap();
    let inner = dir.path().join("inner.jar");
    write_jar(&inner, &[("com/Inner.class", b"inner-class-bytes")]);
    let inner_bytes = fs::read(&inner).unwrap();

    let outer = dir.path().join("outer.jar");
    write_jar(
        &outer,
        &[
            ("lib/inner.jar", inner_bytes.as_slice()),
            ("App.class", b"app"),
        ],
    );

    let scanned = scan_jar(&outer, MAX_ENTRY).unwrap();
    let names: Vec<&str> = scanned.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["lib/inner.jar", "App.class"]);
    assert!(!scanned.entries.iter().any(|e| e.name.contains("Inner")));
    let nested = scanned
        .entries
        .iter()
        .find(|e| e.name == "lib/inner.jar")
        .unwrap();
    assert_eq!(nested.uncompressed_size, inner_bytes.len() as u64);
    assert!(!nested.is_dir);
}

#[test]
fn scan_max_entry_bytes_errors_with_path() {
    let (_dir, path) = temp_jar("too-big.jar");
    write_jar(&path, &[("payload.bin", b"0123456789")]);
    let err = scan_jar(&path, 5).unwrap_err();
    match err {
        AyzenpackError::EntryTooLarge {
            path: err_path,
            name,
            size,
            max,
        } => {
            assert_eq!(err_path, path);
            assert_eq!(name, "payload.bin");
            assert_eq!(size, 10);
            assert_eq!(max, 5);
        }
        other => panic!("expected EntryTooLarge, got {other:?}"),
    }
    let msg = scan_jar(&path, 5).unwrap_err().to_string();
    assert!(
        msg.contains(path.to_string_lossy().as_ref()),
        "error must include path: {msg}"
    );
    assert!(
        msg.contains("payload.bin"),
        "error must include name: {msg}"
    );
}

#[test]
fn scan_prefix_len_on_script_plus_zip() {
    let (_dir, path) = temp_jar("app.jar");
    write_wrapped_jar(&path, SPRING_LAUNCHER, &[("App.class", b"class-bytes")]);
    assert_eq!(zip_prefix_len(&path).unwrap(), SPRING_LAUNCHER.len() as u64);
    let scanned = scan_jar(&path, MAX_ENTRY).unwrap();
    assert_eq!(
        scanned.prefix.as_deref(),
        Some(SPRING_LAUNCHER),
        "prefix must be the exact launcher bytes"
    );
    assert_eq!(scanned.entries.len(), 1);
    assert_eq!(scanned.entries[0].name, "App.class");
    let bytes = fs::read(&path).unwrap();
    let (b3, sha) = hash_both(&bytes);
    assert_eq!(scanned.source_blake3, b3);
    assert_eq!(scanned.source_sha256, sha);
    assert_eq!(scanned.source_size, bytes.len() as u64);
}

#[test]
fn scan_adjusted_script_plus_zip() {
    let (_dir, path) = temp_jar("app-adjusted.jar");
    write_wrapped_jar_adjusted(&path, SPRING_LAUNCHER, &[("App.class", b"class-bytes")]);
    assert_eq!(zip_prefix_len(&path).unwrap(), SPRING_LAUNCHER.len() as u64);
    let scanned = scan_jar(&path, MAX_ENTRY).unwrap();
    assert_eq!(scanned.prefix.as_deref(), Some(SPRING_LAUNCHER));
    assert_eq!(scanned.entries.len(), 1);
    assert_eq!(scanned.entries[0].name, "App.class");
}

#[test]
fn scan_systemd_launcher_unadjusted() {
    assert!(SYSTEMD_LAUNCHER.len() > 200);
    let (_dir, path) = temp_jar("app-systemd.jar");
    write_wrapped_jar(&path, SYSTEMD_LAUNCHER, &[("App.class", b"class-bytes")]);
    assert_eq!(
        zip_prefix_len(&path).unwrap(),
        SYSTEMD_LAUNCHER.len() as u64
    );
    let scanned = scan_jar(&path, MAX_ENTRY).unwrap();
    assert_eq!(scanned.prefix.as_deref(), Some(SYSTEMD_LAUNCHER));
    assert_eq!(scanned.entries[0].name, "App.class");
}

#[test]
fn scan_shebang_without_zip_is_not_zip() {
    let (_dir, path) = temp_jar("script.jar");
    fs::write(&path, b"#!/bin/bash\necho no zip here\n").unwrap();
    let err = scan_jar(&path, MAX_ENTRY).unwrap_err();
    match err {
        AyzenpackError::NotZip { path: err_path } => assert_eq!(err_path, path),
        other => panic!("expected NotZip, got {other:?}"),
    }
}

#[test]
fn scan_normal_jar_has_no_prefix() {
    let (_dir, path) = temp_jar("plain.jar");
    write_jar(&path, &[("a.txt", b"hello")]);
    assert_eq!(zip_prefix_len(&path).unwrap(), 0);
    let scanned = scan_jar(&path, MAX_ENTRY).unwrap();
    assert!(scanned.prefix.is_none());
}

#[test]
fn scan_wrapped_nested_boot_inf_lib_is_one_entry() {
    // Nested BOOT-INF/lib/*.jar stay opaque; only the file-level launcher is unwrapped.
    let dir = tempfile::tempdir().unwrap();
    let inner = dir.path().join("dep.jar");
    write_jar(&inner, &[("com/Dep.class", b"dep-bytes")]);
    let inner_bytes = fs::read(&inner).unwrap();
    let outer = dir.path().join("app.jar");
    write_wrapped_jar(
        &outer,
        SPRING_LAUNCHER,
        &[
            ("BOOT-INF/lib/dep.jar", inner_bytes.as_slice()),
            ("App.class", b"app"),
        ],
    );
    let scanned = scan_jar(&outer, MAX_ENTRY).unwrap();
    let names: Vec<&str> = scanned.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["BOOT-INF/lib/dep.jar", "App.class"]);
    assert!(!scanned.entries.iter().any(|e| e.name.contains("Dep.class")));
    assert_eq!(scanned.prefix.as_deref(), Some(SPRING_LAUNCHER));
}

#[test]
fn scan_non_zip_errors_with_path() {
    let (_dir, path) = temp_jar("not-a-zip.jar");
    fs::write(&path, b"this is not a zip archive").unwrap();
    let err = scan_jar(&path, MAX_ENTRY).unwrap_err();
    match err {
        AyzenpackError::NotZip { path: err_path } => assert_eq!(err_path, path),
        other => panic!("expected NotZip, got {other:?}"),
    }
    let msg = scan_jar(&path, MAX_ENTRY).unwrap_err().to_string();
    assert!(
        msg.contains(path.to_string_lossy().as_ref()),
        "NotZip must include path: {msg}"
    );
}

#[test]
fn scan_detects_signed_sf_rsa() {
    let (_dir, signed_path) = temp_jar("signed.jar");
    write_jar(
        &signed_path,
        &[
            ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n"),
            ("META-INF/FOO.SF", b"Signature-Version: 1.0\n"),
            ("META-INF/FOO.RSA", b"pkcs7-placeholder"),
            ("com/App.class", b"class"),
        ],
    );
    let signed = scan_jar(&signed_path, MAX_ENTRY).unwrap();
    assert!(signed.signed, "META-INF/*.SF + *.RSA must set signed");

    let (_dir_sf, sf_only) = temp_jar("sf-only.jar");
    write_jar(
        &sf_only,
        &[
            ("META-INF/FOO.SF", b"Signature-Version: 1.0\n"),
            ("com/App.class", b"class"),
        ],
    );
    assert!(
        scan_jar(&sf_only, MAX_ENTRY).unwrap().signed,
        "SF-only JAR must set signed (OR, not AND)"
    );

    let (_dir_rsa, rsa_only) = temp_jar("rsa-only.jar");
    write_jar(
        &rsa_only,
        &[
            ("META-INF/FOO.RSA", b"pkcs7-placeholder"),
            ("com/App.class", b"class"),
        ],
    );
    assert!(
        scan_jar(&rsa_only, MAX_ENTRY).unwrap().signed,
        "RSA-only JAR must set signed (OR, not AND)"
    );

    let (_dir2, unsigned_path) = temp_jar("unsigned.jar");
    write_jar(&unsigned_path, &[("com/App.class", b"class")]);
    let unsigned = scan_jar(&unsigned_path, MAX_ENTRY).unwrap();
    assert!(!unsigned.signed);
}

#[test]
fn scanned_entry_has_no_payload_field() {
    // Exhaustive destructure: a data/payload field would fail to compile.
    let _assert = |e: ScannedEntry| {
        let ScannedEntry {
            name,
            is_dir,
            crc32,
            method,
            method_code,
            uncompressed_size,
            compressed_size,
            dos_date,
            dos_time,
            unix_mode,
            utf8_flag,
            name_raw_hex,
        } = e;
        let _ = (
            name,
            is_dir,
            crc32,
            method,
            method_code,
            uncompressed_size,
            compressed_size,
            dos_date,
            dos_time,
            unix_mode,
            utf8_flag,
            name_raw_hex,
        );
    };
    let _ = _assert;
}

#[test]
fn for_each_jar_entry_drops_payload_before_next() {
    // Callback sees at most one payload slice; the ingest Vec is dropped
    // before the next entry, so two payloads cannot be observed at once.
    let (_dir, path) = temp_jar("two.jar");
    write_jar(
        &path,
        &[
            ("one.bin", b"first-payload"),
            ("two.bin", b"second-payload-xxx"),
        ],
    );

    let mut prev_owned: Option<Vec<u8>> = None;
    let mut file_payloads = 0usize;
    let mut max_live = 0usize;
    let mut live = 0usize;

    let scanned = for_each_jar_entry(&path, MAX_ENTRY, |meta, payload| {
        match payload {
            None => {
                assert!(meta.is_dir, "only directories yield None payload");
            }
            Some(bytes) => {
                assert!(!meta.is_dir);
                live += 1;
                max_live = max_live.max(live);
                assert_eq!(live, 1, "callback must not observe two payloads at once");
                if let Some(ref prev) = prev_owned {
                    assert_ne!(
                        bytes,
                        prev.as_slice(),
                        "current slice must be a different entry than the previous owned copy"
                    );
                }
                prev_owned = Some(bytes.to_vec());
                file_payloads += 1;
                live -= 1;
            }
        }
        Ok(())
    })
    .unwrap();

    assert_eq!(file_payloads, 2);
    assert_eq!(max_live, 1);
    assert_eq!(scanned.entries.len(), 2);
}

#[test]
fn scan_does_not_unpack_class_forest() {
    let (_dir, path) = temp_jar("plain.jar");
    write_jar(&path, &[("com/example/A.class", b"bytes")]);
    let _ = scan_jar(&path, MAX_ENTRY).unwrap();
    let parent = path.parent().unwrap();
    assert_no_class_forest(parent);
}

fn assert_no_class_forest(dir: &Path) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        assert!(
            name.ends_with(".jar") || name.ends_with(".zip"),
            "scan must not unpack a class forest, found {name}"
        );
        assert!(entry.file_type().unwrap().is_file());
    }
}
