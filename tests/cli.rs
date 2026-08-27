//! CLI contract for dehydrate/pack/list/verify (assert_cmd).

#[path = "fixtures.rs"]
mod fixtures;

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use assert_cmd::Command;
use ayzenpack::format::{read_ayz_file, write_ayz_file, Record};
use ayzenpack::{list, Manifest};
use fixtures::{
    write_jar, write_leading_pad_pk_decoy_truncated_cd_zip, write_range_overlap_local_zip,
    write_stored_zip, write_truncated_cd_overlap_unknown_deflate_sibling,
    write_unknown_deflate_zip,
};
use predicates::prelude::*;

fn ayzenpack() -> Command {
    Command::cargo_bin("ayzenpack").expect("binary must be named ayzenpack, not jded")
}

fn sample_jar(dir: &Path) -> std::path::PathBuf {
    let jar = dir.join("a.jar");
    write_jar(&jar, &[("x.txt", b"hello")]);
    jar
}

#[test]
fn help_lists_dehydrate_and_pack_alias() {
    // Guards binary renamed to jded, or pack alias omitted from --help.
    ayzenpack()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("ayzenpack"))
        .stdout(predicate::str::contains("dehydrate"))
        .stdout(predicate::str::contains("pack"))
        .stdout(predicate::str::contains("jded").not());
}

#[test]
fn help_lists_pretty_manifest() {
    // Guards clap sketch dropping --pretty-manifest.
    ayzenpack()
        .args(["dehydrate", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--pretty-manifest"));
}

#[test]
fn help_lists_restore_paths_on_dehydrate_and_rehydrate() {
    ayzenpack()
        .args(["dehydrate", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--restore-paths"));
    ayzenpack()
        .args(["rehydrate", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--restore-paths"))
        .stdout(predicate::str::contains("--dir"));
}

#[test]
fn rehydrate_requires_dir_without_restore_paths() {
    ayzenpack()
        .args(["rehydrate", "-i", "x.ayz"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn help_lists_jobs_and_max_inflight_bytes() {
    // Guards clap sketch dropping PR-18 pipeline flags.
    ayzenpack()
        .args(["dehydrate", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--jobs"))
        .stdout(predicate::str::contains("--max-inflight-bytes"));
}

#[test]
fn dehydrate_requires_output_exit_2() {
    // Guards treating missing -o as an operational error (exit 1) instead of clap usage.
    ayzenpack()
        .args(["dehydrate", "foo.jar"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn pack_alias_writes_magic_ayzp() {
    let dir = tempfile::tempdir().unwrap();
    let jar = sample_jar(dir.path());
    let out = dir.path().join("out.ayz");
    ayzenpack()
        .arg("pack")
        .arg("-o")
        .arg(&out)
        .arg(&jar)
        .assert()
        .success();
    let bytes = fs::read(&out).unwrap();
    assert!(bytes.len() >= 4, "archive too short");
    assert_eq!(&bytes[..4], b"AYZP");
}

#[test]
fn stdout_quiet_on_success() {
    // Guards stats on stdout breaking pipes.
    let dir = tempfile::tempdir().unwrap();
    let jar = sample_jar(dir.path());
    let out = dir.path().join("out.ayz");
    ayzenpack()
        .arg("dehydrate")
        .arg("-o")
        .arg(&out)
        .arg(&jar)
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("ayzenpack:"))
        .stderr(predicate::str::contains("jars"))
        .stderr(predicate::str::contains("unique blobs"));
}

#[test]
fn stats_line_on_stderr_not_stdout() {
    // Guards stats/progress on stdout breaking pipes.
    let dir = tempfile::tempdir().unwrap();
    let jar = sample_jar(dir.path());
    let out = dir.path().join("out.ayz");
    let output = ayzenpack()
        .arg("dehydrate")
        .arg("-o")
        .arg(&out)
        .arg(&jar)
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .get_output()
        .clone();
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("ayzenpack:")
            && err.contains("jars")
            && err.contains("entries")
            && err.contains("unique blobs")
            && err.contains("unique, zstd")
            && err.contains("of jar bytes"),
        "stats line missing from stderr: {err}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("entries") && !stdout.contains("["),
        "progress/stats leaked to stdout: {stdout:?}"
    );
}

#[test]
fn quiet_suppresses_progress() {
    // Guards progress on stdout, or still drawing under -q.
    let dir = tempfile::tempdir().unwrap();
    let jar = sample_jar(dir.path());

    let noisy_out = dir.path().join("noisy.ayz");
    let noisy = ayzenpack()
        .arg("dehydrate")
        .arg("-o")
        .arg(&noisy_out)
        .arg(&jar)
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .get_output()
        .clone();
    let noisy_err = String::from_utf8_lossy(&noisy.stderr);
    assert!(
        noisy_err.contains("a.jar:"),
        "expected per-JAR progress on stderr, got: {noisy_err}"
    );

    let quiet_out = dir.path().join("quiet.ayz");
    ayzenpack()
        .arg("-q")
        .arg("dehydrate")
        .arg("-o")
        .arg(&quiet_out)
        .arg(&jar)
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("a.jar:").not())
        .stderr(predicate::str::contains("[").not());
}

#[test]
fn json_logs_one_object_per_event() {
    // Guards mixing human lines with --json-logs, or events on stdout.
    let dir = tempfile::tempdir().unwrap();
    let jar = sample_jar(dir.path());
    let out = dir.path().join("out.ayz");
    let output = ayzenpack()
        .arg("--json-logs")
        .arg("dehydrate")
        .arg("-o")
        .arg(&out)
        .arg(&jar)
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .get_output()
        .clone();
    let err = String::from_utf8(output.stderr.clone()).unwrap();
    let mut saw_jar_done = false;
    for line in err.lines().filter(|l| !l.is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("stderr line is not one JSON object ({e}): {line}"));
        assert!(
            v.get("event").and_then(|e| e.as_str()).is_some(),
            "JSON event missing event field: {line}"
        );
        if v["event"] == "jar_done" {
            saw_jar_done = true;
            assert_eq!(v["name"], "a.jar");
            assert!(v["entries"].as_u64().unwrap_or(0) >= 1);
        }
    }
    assert!(saw_jar_done, "missing jar_done JSON event in stderr: {err}");
}

#[test]
fn dehydrate_o_overwrites_existing_ayz() {
    let dir = tempfile::tempdir().unwrap();
    let jar = sample_jar(dir.path());
    let out = dir.path().join("out.ayz");
    fs::write(&out, b"not-an-ayzenpack-file").unwrap();
    ayzenpack()
        .arg("dehydrate")
        .arg("-o")
        .arg(&out)
        .arg(&jar)
        .assert()
        .success();
    let bytes = fs::read(&out).unwrap();
    assert!(bytes.len() >= 4, "archive too short");
    assert_eq!(&bytes[..4], b"AYZP");
    assert_ne!(&bytes, b"not-an-ayzenpack-file");
}

#[test]
fn unpack_alias_works() {
    let dir = tempfile::tempdir().unwrap();
    let jar = sample_jar(dir.path());
    let out = dir.path().join("out.ayz");
    ayzenpack()
        .arg("dehydrate")
        .arg("-o")
        .arg(&out)
        .arg(&jar)
        .assert()
        .success();
    let dest = dir.path().join("restored");
    ayzenpack()
        .arg("unpack")
        .arg("-i")
        .arg(&out)
        .arg("-d")
        .arg(&dest)
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    let restored = dest.join("a.jar");
    assert!(restored.is_file(), "unpack must restore a.jar");
    let mut z = zip::ZipArchive::new(fs::File::open(&restored).unwrap()).unwrap();
    let mut f = z.by_name("x.txt").unwrap();
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).unwrap();
    assert_eq!(buf, b"hello");
}

fn pack_sample(dir: &Path) -> std::path::PathBuf {
    let jar = sample_jar(dir);
    let out = dir.join("out.ayz");
    ayzenpack()
        .arg("dehydrate")
        .arg("-o")
        .arg(&out)
        .arg(&jar)
        .assert()
        .success();
    out
}

#[test]
fn list_prints_jar_names_and_blob_count() {
    let dir = tempfile::tempdir().unwrap();
    let out = pack_sample(dir.path());
    ayzenpack()
        .arg("list")
        .arg("-i")
        .arg(&out)
        .assert()
        .success()
        .stdout(predicate::str::contains("a.jar"))
        .stdout(predicate::str::contains("unique blobs"));
}

#[test]
fn list_json_stdout_deserializes_as_full_manifest() {
    // Guards --json being a summary object instead of the full pretty Manifest.
    let dir = tempfile::tempdir().unwrap();
    let out = pack_sample(dir.path());
    let stdout = ayzenpack()
        .arg("list")
        .arg("-i")
        .arg(&out)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(stdout).unwrap();
    assert!(
        s.contains('\n'),
        "list --json must be pretty-printed, got {s}"
    );
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert!(v.get("jars").and_then(|j| j.as_array()).is_some());
    assert!(v.get("blobs").and_then(|b| b.as_array()).is_some());
    assert!(v.get("stats").and_then(|st| st.as_object()).is_some());
    assert_eq!(v["format"], "ayzenpack-manifest");
    let m: Manifest = serde_json::from_str(&s).unwrap();
    assert_eq!(m.format, "ayzenpack-manifest");
    assert_eq!(m.jars[0].name, "a.jar");
    assert!(!m.blobs.is_empty());
    assert_eq!(m.stats.unique_blob_count, m.blobs.len() as u64);
    assert!(
        !s.contains("restore_backend"),
        "list --json must stay the Manifest (no restore_backend key): {s}"
    );
}

fn flip_first_blob_payload(path: &Path) {
    let mut f = File::open(path).unwrap();
    let (header, trailer, mut records) = read_ayz_file(&mut f).unwrap();
    drop(f);
    let mut flipped = false;
    for rec in &mut records {
        if let Record::Blob { data, .. } = rec {
            assert!(!data.is_empty());
            data[0] ^= 0xff;
            flipped = true;
            break;
        }
    }
    assert!(flipped);
    let mut w = File::create(path).unwrap();
    write_ayz_file(&mut w, &header, &records, trailer.jar_count).unwrap();
}

#[test]
fn cli_verify_corrupt_exits_3() {
    let dir = tempfile::tempdir().unwrap();
    let out = pack_sample(dir.path());
    flip_first_blob_payload(&out);
    ayzenpack()
        .arg("verify")
        .arg("-i")
        .arg(&out)
        .assert()
        .failure()
        .code(3);
}

#[test]
fn cli_verify_missing_file_exits_1() {
    ayzenpack()
        .args(["verify", "-i", "/no/such/ayzenpack-archive.ayz"])
        .assert()
        .failure()
        .code(1);
}

fn pack_one(dir: &Path, jar: &Path) -> std::path::PathBuf {
    let out = dir.join("out.ayz");
    ayzenpack()
        .arg("dehydrate")
        .arg("-o")
        .arg(&out)
        .arg(jar)
        .assert()
        .success();
    out
}

fn assert_list_restore(out: &Path, want: &str) {
    let m = list(out).unwrap();
    assert_eq!(m.jars[0].restore_backend(), want, "{}", m.jars[0].name);
    let stdout = String::from_utf8(
        ayzenpack()
            .arg("list")
            .arg("-i")
            .arg(out)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(
        stdout.contains("RESTORE") && stdout.contains("M8MISS"),
        "human list must have RESTORE and M8MISS columns: {stdout}"
    );
    assert!(
        stdout.contains(want),
        "human list must print {want}: {stdout}"
    );
}

fn rehydrate_stderr(out: &Path, dest: &Path, extra: &[&str]) -> String {
    let mut cmd = ayzenpack();
    for a in extra {
        cmd.arg(a);
    }
    let output = cmd
        .arg("rehydrate")
        .arg("-i")
        .arg(out)
        .arg("-d")
        .arg(dest)
        .assert()
        .success()
        .stdout(predicate::str::is_empty())
        .get_output()
        .clone();
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn list_restore_store_is_exact() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("store.jar");
    write_stored_zip(
        &jar,
        &[("x.txt", b"hello".as_slice(), crc32fast::hash(b"hello"))],
    );
    let out = pack_one(dir.path(), &jar);
    assert_list_restore(&out, "exact");
}

#[test]
fn list_restore_unknown_deflate_with_tail_is_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("miss.jar");
    write_unknown_deflate_zip(&jar, "a.txt", &vec![b'a'; 256]);
    let out = pack_one(dir.path(), &jar);
    assert_list_restore(&out, "rebuild");
}

#[test]
fn list_restore_truncated_cd_leading_pad_is_skip_exact_seek() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("lead-trunc.jar");
    write_leading_pad_pk_decoy_truncated_cd_zip(&jar, "a.txt", b"leading-pad-plain");
    let out = pack_one(dir.path(), &jar);
    assert_list_restore(&out, "skip_exact_seek_synthetic_cd");
}

#[test]
fn list_restore_method8_miss_without_tail_is_skip_exact_concat() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("trunc-overlap-miss.jar");
    write_truncated_cd_overlap_unknown_deflate_sibling(&jar);
    let out = pack_one(dir.path(), &jar);
    assert_list_restore(&out, "skip_exact_concat_synthetic_cd");
}

#[test]
fn list_restore_range_overlap_is_skip_exact_zipwriter() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("range-overlap.jar");
    write_range_overlap_local_zip(&jar);
    let out = pack_one(dir.path(), &jar);
    assert_list_restore(&out, "skip_exact_zipwriter");
}

#[test]
fn list_restore_dup_txt_is_skip_exact_zipwriter() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("dup.jar");
    let first = b"first-payload".as_slice();
    let second = b"second-payload".as_slice();
    write_stored_zip(
        &jar,
        &[
            ("dup.txt", first, crc32fast::hash(first)),
            ("dup.txt", second, crc32fast::hash(second)),
        ],
    );
    let out = pack_one(dir.path(), &jar);
    assert_list_restore(&out, "skip_exact_zipwriter");
}

#[test]
fn rehydrate_default_prints_rebuild_notice() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("miss.jar");
    write_unknown_deflate_zip(&jar, "a.txt", &vec![b'a'; 256]);
    let out = pack_one(dir.path(), &jar);
    let dest = dir.path().join("restored");
    let err = rehydrate_stderr(&out, &dest, &[]);
    assert!(
        err.contains("ayzenpack: restoring miss.jar (rebuild)"),
        "default rehydrate must notice rebuild: {err}"
    );
}

#[test]
fn rehydrate_default_is_silent_for_exact_store() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("store.jar");
    write_stored_zip(
        &jar,
        &[("x.txt", b"hello".as_slice(), crc32fast::hash(b"hello"))],
    );
    let out = pack_one(dir.path(), &jar);
    let dest = dir.path().join("restored");
    let err = rehydrate_stderr(&out, &dest, &[]);
    assert!(
        !err.contains("restoring"),
        "exact STORE must stay silent by default: {err}"
    );
}

#[test]
fn rehydrate_quiet_suppresses_rebuild_notice() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("miss.jar");
    write_unknown_deflate_zip(&jar, "a.txt", &vec![b'a'; 256]);
    let out = pack_one(dir.path(), &jar);
    let dest = dir.path().join("restored");
    let err = rehydrate_stderr(&out, &dest, &["-q"]);
    assert!(
        !err.contains("restoring"),
        "--quiet must suppress the rebuild notice: {err}"
    );
}

#[test]
fn rehydrate_verbose_still_prints_exact() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("store.jar");
    write_stored_zip(
        &jar,
        &[("x.txt", b"hello".as_slice(), crc32fast::hash(b"hello"))],
    );
    let out = pack_one(dir.path(), &jar);
    let dest = dir.path().join("restored");
    let err = rehydrate_stderr(&out, &dest, &["-v"]);
    assert!(
        err.contains("ayzenpack: restoring store.jar (exact)"),
        "--verbose must still print exact: {err}"
    );
}
