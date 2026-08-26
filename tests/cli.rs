//! CLI contract for dehydrate/pack (assert_cmd).

#[path = "fixtures.rs"]
mod fixtures;

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use fixtures::write_jar;
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
