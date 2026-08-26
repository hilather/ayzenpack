//! Corruption and integrity: truncated trailer, flipped blobs, END digest.

#[path = "fixtures.rs"]
mod fixtures;

use std::fs::{self, File};
use std::path::Path;

use assert_cmd::Command;
use ayzenpack::error::AyzenpackError;
use ayzenpack::format::{
    open_ayz_layout, read_ayz_file, read_toc_at, write_ayz_file, write_ayz_file_v1, Record,
};
use ayzenpack::{dehydrate, list, rehydrate, verify, DehydrateOptions, RehydrateOptions};
use fixtures::{write_jar, write_stored_zip, write_wrapped_jar, SPRING_LAUNCHER};
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

fn rehydrate_opts(input: &Path, dir: &Path) -> RehydrateOptions {
    RehydrateOptions {
        input: input.to_path_buf(),
        dir: dir.to_path_buf(),
        ..RehydrateOptions::default()
    }
}

#[test]
fn jar_input_names_jar_zip_not_generic_magic() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("a.jar");
    write_jar(&jar, &[("x.txt", b"hello")]);
    let dest = dir.path().join("out");

    let r = rehydrate(&rehydrate_opts(&jar, &dest)).unwrap_err();
    assert!(
        r.to_string().contains("JAR/ZIP"),
        "rehydrate jar must name JAR/ZIP, got {r}"
    );
    let l = list(&jar).unwrap_err();
    assert!(
        l.to_string().contains("JAR/ZIP"),
        "list jar must name JAR/ZIP, got {l}"
    );
    let v = verify(&jar).unwrap_err();
    assert!(
        v.to_string().contains("JAR/ZIP"),
        "verify jar must name JAR/ZIP, got {v}"
    );
}

#[test]
fn shebang_jar_input_names_executable_script() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("app.jar");
    write_wrapped_jar(&jar, SPRING_LAUNCHER, &[("A.class", b"AAA")]);

    let err = verify(&jar).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("executable") || msg.contains("script"),
        "shebang+zip must mention executable/script, got {msg}"
    );
    assert!(
        !msg.contains("saw "),
        "shebang+zip must not be generic magic only, got {msg}"
    );
}

#[test]
fn ayzp_header_garbage_trailer_names_missing_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("trunc.ayz");
    let mut bytes = b"AYZP\x01\x00\x00\x00".to_vec();
    bytes.extend_from_slice(&[0u8; 64]);
    fs::write(&p, bytes).unwrap();

    let err = verify(&p).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("missing") || msg.contains("truncated"),
        "AYZP + garbage trailer must mention missing/truncated, got {msg}"
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

fn patch_trailer_toc_len(path: &Path, toc_len: u64) {
    let mut bytes = fs::read(path).unwrap();
    let n = bytes.len();
    assert!(n >= 64);
    bytes[n - 8..n].copy_from_slice(&toc_len.to_le_bytes());
    fs::write(path, bytes).unwrap();
}

fn patch_trailer_version(path: &Path, version: u32) {
    let mut bytes = fs::read(path).unwrap();
    let n = bytes.len();
    bytes[n - 12..n - 8].copy_from_slice(&version.to_le_bytes());
    fs::write(path, bytes).unwrap();
}

#[test]
fn toc_len_corrupt_truncated_too_big_v2_zero() {
    let dir = tempfile::tempdir().unwrap();
    let out = pack_hello(dir.path());
    let orig = fs::read(&out).unwrap();
    let (_, trailer, _) = {
        let mut f = File::open(&out).unwrap();
        read_ayz_file(&mut f).unwrap()
    };
    assert_eq!(trailer.version, 2);
    assert!(trailer.toc_len >= 28);

    let truncated = dir.path().join("trunc-toc.ayz");
    let keep = orig.len() - trailer.toc_len as usize + 10;
    fs::write(&truncated, &orig[..keep]).unwrap();
    let err = list(&truncated).unwrap_err();
    assert!(
        matches!(err, AyzenpackError::Format(_))
            || err.to_string().contains("toc")
            || err.to_string().contains("truncated"),
        "truncated TOC must fail, got {err:?}"
    );

    let too_big = dir.path().join("big-toc.ayz");
    fs::write(&too_big, &orig).unwrap();
    patch_trailer_toc_len(&too_big, trailer.toc_len + 4096);
    let err = verify(&too_big).unwrap_err();
    assert!(
        matches!(err, AyzenpackError::Format("toc_len mismatch"))
            || err.to_string().contains("toc"),
        "toc_len too big must fail, got {err:?}"
    );

    let zero = dir.path().join("zero-toc.ayz");
    // Drop TOC bytes so expected_toc is 0, then set trailer.toc_len=0 to hit the v2 arm.
    let header_total = 12 + trailer.header_len as usize;
    let payload = trailer.payload_bytes as usize;
    let mut stripped = orig[..header_total + payload].to_vec();
    stripped.extend_from_slice(&orig[orig.len() - 64..]);
    fs::write(&zero, &stripped).unwrap();
    patch_trailer_toc_len(&zero, 0);
    let err = list(&zero).unwrap_err();
    assert!(
        matches!(err, AyzenpackError::Format("v2 toc_len must not be 0")),
        "v2 toc_len=0 with matching geometry must fail that arm, got {err:?}"
    );
}

#[test]
fn v1_toc_len_nonzero_is_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let out = pack_hello(dir.path());
    let mut f = File::open(&out).unwrap();
    let (mut header, trailer, records) = read_ayz_file(&mut f).unwrap();
    drop(f);
    header.version = 1;
    let v1 = dir.path().join("v1.ayz");
    let mut w = File::create(&v1).unwrap();
    write_ayz_file_v1(&mut w, &header, &records, trailer.jar_count).unwrap();
    drop(w);
    patch_trailer_toc_len(&v1, 28);
    let err = list(&v1).unwrap_err();
    assert!(
        matches!(
            err,
            AyzenpackError::Format("toc_len mismatch")
                | AyzenpackError::Format("v1 toc_len must be 0")
        ) || err.to_string().contains("toc"),
        "v1 toc_len≠0 must fail, got {err:?}"
    );
}

#[test]
fn magic_json_trailer_version_skew() {
    let dir = tempfile::tempdir().unwrap();
    let out = pack_hello(dir.path());
    let dest = dir.path().join("skew.ayz");
    fs::copy(&out, &dest).unwrap();
    patch_trailer_version(&dest, 1);
    let err = verify(&dest).unwrap_err();
    assert!(
        matches!(err, AyzenpackError::VersionSkew { .. }),
        "header v2 / trailer v1 must be VersionSkew, got {err:?}"
    );
    assert!(!err.to_string().contains("not an ayzenpack"));
}

#[test]
fn list_v2_uses_last_frame_only() {
    let dir = tempfile::tempdir().unwrap();
    let out = pack_hello(dir.path());
    let mut f = File::open(&out).unwrap();
    let (header, trailer, _) = read_ayz_file(&mut f).unwrap();
    drop(f);
    let header_total = 12 + u64::from(trailer.header_len);
    assert_eq!(header.version, 2);
    let mut bytes = fs::read(&out).unwrap();
    let first_zstd = header_total as usize;
    assert!(first_zstd < bytes.len());
    bytes[first_zstd] ^= 0xff;
    fs::write(&out, &bytes).unwrap();
    let m = list(&out).unwrap();
    assert_eq!(m.jars[0].name, "a.jar");
    assert!(verify(&out).is_err(), "corrupt blob frame must fail verify");
}

#[test]
fn v1_pack_still_rehydrates() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("a.jar");
    write_jar(&jar, &[("x.txt", b"hello")]);
    let v2 = dir.path().join("v2.ayz");
    dehydrate(&opts(&v2, vec![jar.clone()])).unwrap();
    let mut f = File::open(&v2).unwrap();
    let (mut header, trailer, records) = read_ayz_file(&mut f).unwrap();
    drop(f);
    header.version = 1;
    let v1 = dir.path().join("v1.ayz");
    let mut w = File::create(&v1).unwrap();
    write_ayz_file_v1(&mut w, &header, &records, trailer.jar_count).unwrap();
    drop(w);
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&v1, &dest)).unwrap();
    assert_eq!(fs::read(dest.join("a.jar")).unwrap(), fs::read(&jar).unwrap());
    list(&v1).unwrap();
    verify(&v1).unwrap();
}

#[test]
fn trailer_toc_len_not_copied_from_manifest_len() {
    let dir = tempfile::tempdir().unwrap();
    let out = pack_hello(dir.path());
    let mut f = File::open(&out).unwrap();
    let layout = open_ayz_layout(&mut f).unwrap();
    let toc = read_toc_at(&mut f, &layout).unwrap();
    assert_ne!(toc.manifest_zstd_len, layout.trailer.manifest_len);
}
