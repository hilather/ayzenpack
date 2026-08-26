//! Corruption and integrity: truncated trailer, flipped blobs, END digest.

#[path = "fixtures.rs"]
mod fixtures;

use std::fs::{self, File};
use std::path::Path;

use assert_cmd::Command;
use ayzenpack::error::AyzenpackError;
use ayzenpack::format::{read_ayz_file, write_ayz_file, Record};
use ayzenpack::{dehydrate, list, verify, DehydrateOptions};
use fixtures::{write_jar, write_stored_zip};
use predicates::prelude::*;

fn opts(output: &Path, inputs: Vec<std::path::PathBuf>) -> DehydrateOptions {
    DehydrateOptions {
        output: output.to_path_buf(),
        inputs,
        ..DehydrateOptions::default()
    }
}

fn pack_hello(dir: &Path) -> std::path::PathBuf {
    let jar = dir.join("a.jar");
    write_jar(&jar, &[("x.txt", b"hello")]);
    let out = dir.join("out.ayz");
    dehydrate(&opts(&out, vec![jar])).unwrap();
    out
}

fn rewrite_records(path: &Path, map: impl FnOnce(Vec<Record>) -> Vec<Record>) {
    let mut f = File::open(path).unwrap();
    let (header, trailer, records) = read_ayz_file(&mut f).unwrap();
    drop(f);
    let records = map(records);
    let mut w = File::create(path).unwrap();
    write_ayz_file(&mut w, &header, &records, trailer.jar_count).unwrap();
}

#[test]
fn verify_fresh_archive_ok() {
    let dir = tempfile::tempdir().unwrap();
    let out = pack_hello(dir.path());
    verify(&out).unwrap();
    let m = list(&out).unwrap();
    assert_eq!(m.jars[0].name, "a.jar");
    assert!(m.stats.unique_blob_count >= 1);
    assert!(m.jars[0].entries[0].blob.is_some());
}

#[test]
fn verify_wrong_end_digest_fails() {
    let dir = tempfile::tempdir().unwrap();
    let out = pack_hello(dir.path());
    rewrite_records(&out, |mut records| {
        for rec in &mut records {
            if let Record::End { digest } = rec {
                digest[0] ^= 0xff;
            }
        }
        records
    });
    let err = verify(&out).unwrap_err();
    assert!(
        matches!(err, AyzenpackError::HashMismatch(ref s) if s.contains("END")),
        "wrong END digest must be HashMismatch, got {err:?}"
    );
}

#[test]
fn verify_flipped_blob_bytes_fails() {
    // Flip payload bytes, keep the BLOB record hash field — blake3(payload) != id.
    let dir = tempfile::tempdir().unwrap();
    let out = pack_hello(dir.path());
    rewrite_records(&out, |mut records| {
        let mut flipped = false;
        for rec in &mut records {
            if let Record::Blob { data, .. } = rec {
                assert!(!data.is_empty(), "test payload must be non-empty");
                data[0] ^= 0xff;
                flipped = true;
                break;
            }
        }
        assert!(flipped, "archive must contain a BLOB record");
        records
    });
    let err = verify(&out).unwrap_err();
    assert!(
        matches!(err, AyzenpackError::HashMismatch(ref s) if s.contains("blob")),
        "flipped blob bytes must be HashMismatch, got {err:?}"
    );
}

#[test]
fn truncated_trailer_errors() {
    // Guards verify/list succeeding (or panicking) on a file shorter than 64 bytes.
    let dir = tempfile::tempdir().unwrap();
    for n in [0usize, 1, 63] {
        let p = dir.path().join(format!("short{n}.ayz"));
        fs::write(&p, vec![0u8; n]).unwrap();
        let v = verify(&p).unwrap_err();
        assert!(
            matches!(v, AyzenpackError::Format("truncated trailer")),
            "verify len={n} must be truncated trailer, got {v:?}"
        );
        let l = list(&p).unwrap_err();
        assert!(
            matches!(l, AyzenpackError::Format("truncated trailer")),
            "list len={n} must be truncated trailer, got {l:?}"
        );
    }

    let out = pack_hello(dir.path());
    let mut bytes = fs::read(&out).unwrap();
    assert!(bytes.len() > 64);
    bytes.truncate(40);
    fs::write(&out, &bytes).unwrap();
    let err = verify(&out).unwrap_err();
    assert!(
        matches!(err, AyzenpackError::Format("truncated trailer")),
        "truncated finished archive must not verify, got {err:?}"
    );
}

fn ayzenpack() -> Command {
    Command::cargo_bin("ayzenpack").expect("binary must be named ayzenpack, not jded")
}

fn write_signed_jar(path: &Path) {
    write_jar(
        path,
        &[
            ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n"),
            ("META-INF/FOO.SF", b"Signature-Version: 1.0\n"),
            ("META-INF/FOO.RSA", b"pkcs7-placeholder"),
            ("com/App.class", b"class"),
        ],
    );
}

#[test]
fn fail_on_signed_exits_error() {
    // Signed notice is a warning unless --fail-on-signed (strict does not promote it).
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("signed.jar");
    write_signed_jar(&jar);

    let out_ok = dir.path().join("ok.ayz");
    let mut o = opts(&out_ok, vec![jar.clone()]);
    o.strict = true;
    let summary = dehydrate(&o).unwrap();
    assert_eq!(summary.signed_jars, vec!["signed.jar".to_string()]);
    assert!(
        out_ok.is_file(),
        "signed JAR must still pack without the flag"
    );

    let out_fail = dir.path().join("fail.ayz");
    let mut o = opts(&out_fail, vec![jar.clone()]);
    o.fail_on_signed = true;
    let err = dehydrate(&o).unwrap_err();
    assert!(
        matches!(err, AyzenpackError::Usage(ref s) if s.contains("signed")),
        "fail_on_signed must be Usage, got {err:?}"
    );

    ayzenpack()
        .arg("dehydrate")
        .arg("-o")
        .arg(dir.path().join("cli-ok.ayz"))
        .arg(&jar)
        .assert()
        .success()
        .stderr(predicate::str::contains("signed"));

    ayzenpack()
        .arg("dehydrate")
        .arg("--fail-on-signed")
        .arg("-o")
        .arg(dir.path().join("cli-fail.ayz"))
        .arg(&jar)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("signed"));
}

#[test]
fn crc_mismatch_warns_or_strict_errors() {
    // Lying CD CRC. zip 2.x Crc32Reader often rejects this before dehydrate's check.
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("lie.jar");
    let payload = b"payload-with-lying-crc";
    let real = crc32fast::hash(payload);
    write_stored_zip(&jar, &[("a.txt", payload, real.wrapping_add(1))]);

    let out = dir.path().join("out.ayz");
    let mut o = opts(&out, vec![jar.clone()]);
    match dehydrate(&o) {
        Ok(_) => {
            o.strict = true;
            let err = dehydrate(&o).unwrap_err();
            assert!(
                matches!(err, AyzenpackError::FormatOwned(ref s) if s.to_ascii_lowercase().contains("crc")),
                "--strict must error on CRC mismatch, got {err:?}"
            );
            ayzenpack()
                .arg("dehydrate")
                .arg("-o")
                .arg(dir.path().join("warn.ayz"))
                .arg(&jar)
                .assert()
                .success()
                .stderr(predicate::str::contains("CRC mismatch"));
        }
        Err(err) => {
            // Skip: zip crate validates CRC on inflate, so dehydrate never sees a lying header.
            assert!(
                matches!(err, AyzenpackError::Io { .. } | AyzenpackError::Zip { .. }),
                "lying CRC must not be silently accepted, got {err:?}"
            );
        }
    }
}
