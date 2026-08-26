//! Recursive walk, basename collision, and `--exclude` (glob 0.3).

#[path = "fixtures.rs"]
mod fixtures;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use assert_cmd::Command;
use fixtures::write_jar;
use predicates::prelude::*;

fn ayzenpack() -> Command {
    Command::cargo_bin("ayzenpack").expect("binary must be named ayzenpack, not jded")
}

fn write_rel_jar(root: &Path, rel: &str) {
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    write_jar(&p, &[("x.txt", b"hello")]);
}

fn restored_names(root: &Path) -> BTreeSet<String> {
    ayzenpack()
        .current_dir(root)
        .args(["rehydrate", "-i", "out.ayz", "-d", "restored"])
        .assert()
        .success();
    fs::read_dir(root.join("restored"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

#[allow(non_snake_case)] // required test name: lib.jar / lib__2.jar
#[test]
fn two_dirs_same_basename_become_lib_jar_and_lib__2_jar() {
    // Guards overwriting lib.jar instead of suffixing lib__2.jar.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_rel_jar(root, "web/lib.jar");
    write_rel_jar(root, "search/lib.jar");
    ayzenpack()
        .current_dir(root)
        .args(["dehydrate", "-o", "out.ayz", "--recursive", "web", "search"])
        .assert()
        .success();
    let names = restored_names(root);
    assert!(names.contains("lib.jar"), "got {names:?}");
    assert!(names.contains("lib__2.jar"), "got {names:?}");
    assert_eq!(names.len(), 2, "got {names:?}");
}

#[test]
fn recursive_picks_jar_zip_war_ear_not_txt() {
    // Guards recursing without the flag, or treating .txt as an archive.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_rel_jar(root, "a.jar");
    write_rel_jar(root, "b.zip");
    write_rel_jar(root, "c.war");
    write_rel_jar(root, "d.ear");
    write_rel_jar(root, "nested/e.JAR");
    fs::write(root.join("skip.txt"), b"not an archive").unwrap();
    fs::create_dir_all(root.join("nested")).unwrap();
    fs::write(root.join("nested/readme.txt"), b"also not").unwrap();

    ayzenpack()
        .current_dir(root)
        .args(["dehydrate", "-o", "out.ayz", "--recursive", "."])
        .assert()
        .success();
    let names = restored_names(root);
    assert!(names.contains("a.jar"), "got {names:?}");
    assert!(names.contains("b.zip"), "got {names:?}");
    assert!(names.contains("c.war"), "got {names:?}");
    assert!(names.contains("d.ear"), "got {names:?}");
    assert!(names.contains("e.JAR"), "got {names:?}");
    assert!(!names.iter().any(|n| n.ends_with(".txt")), "got {names:?}");
    assert_eq!(names.len(), 5, "got {names:?}");
}

#[test]
fn exclude_star_sources_jar_matches_basename_in_subdir() {
    // *.sources.jar must match apps/web/lib/foo.sources.jar via basename.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_rel_jar(root, "apps/web/lib/foo.sources.jar");
    write_rel_jar(root, "apps/web/lib/foo.jar");
    ayzenpack()
        .current_dir(root)
        .args([
            "dehydrate",
            "-o",
            "out.ayz",
            "--recursive",
            "apps",
            "--exclude",
            "*.sources.jar",
        ])
        .assert()
        .success();
    let names = restored_names(root);
    assert_eq!(
        names,
        BTreeSet::from(["foo.jar".to_string()]),
        "got {names:?}"
    );
}

#[test]
fn exclude_one_star_per_component() {
    // */secret/* vs vendor/secret/x.jar (one * per path component).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_rel_jar(root, "vendor/secret/x.jar");
    write_rel_jar(root, "vendor/ok.jar");
    ayzenpack()
        .current_dir(root)
        .args([
            "dehydrate",
            "-o",
            "out.ayz",
            "--recursive",
            "vendor",
            "--exclude",
            "*/secret/*",
        ])
        .assert()
        .success();
    let names = restored_names(root);
    assert_eq!(
        names,
        BTreeSet::from(["ok.jar".to_string()]),
        "got {names:?}"
    );
}

#[test]
fn exclude_exact_cli_path() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_rel_jar(root, "apps/web/lib/foo.jar");
    write_rel_jar(root, "apps/web/lib/bar.jar");
    ayzenpack()
        .current_dir(root)
        .args([
            "dehydrate",
            "-o",
            "out.ayz",
            "--recursive",
            "apps",
            "--exclude",
            "apps/web/lib/foo.jar",
        ])
        .assert()
        .success();
    let names = restored_names(root);
    assert_eq!(
        names,
        BTreeSet::from(["bar.jar".to_string()]),
        "got {names:?}"
    );
}

#[test]
fn exclude_globstar_does_not_match_nested() {
    // vendor/** is not globstar; * does not cross /. vendor/a/b.jar stays in.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_rel_jar(root, "vendor/a/b.jar");
    ayzenpack()
        .current_dir(root)
        .args([
            "dehydrate",
            "-o",
            "out.ayz",
            "--recursive",
            "vendor",
            "--exclude",
            "vendor/**",
        ])
        .assert()
        .success();
    let names = restored_names(root);
    assert_eq!(
        names,
        BTreeSet::from(["b.jar".to_string()]),
        "got {names:?}"
    );
}

#[test]
fn duplicate_input_path_warned_and_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_rel_jar(root, "a.jar");
    ayzenpack()
        .current_dir(root)
        .args(["dehydrate", "-o", "out.ayz", "a.jar", "a.jar"])
        .assert()
        .success()
        .stderr(predicate::str::contains("duplicate"))
        .stderr(predicate::str::contains("skipping"));
    let names = restored_names(root);
    assert_eq!(names, BTreeSet::from(["a.jar".to_string()]));
}

#[cfg(unix)]
#[test]
fn follow_symlinks_off_does_not_enter_symlink_dir() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_rel_jar(root, "in/keep.jar");
    write_rel_jar(root, "outside/secret.jar");
    symlink(root.join("outside"), root.join("in/link")).unwrap();

    ayzenpack()
        .current_dir(root)
        .args(["dehydrate", "-o", "out.ayz", "--recursive", "in"])
        .assert()
        .success();
    let names = restored_names(root);
    assert_eq!(
        names,
        BTreeSet::from(["keep.jar".to_string()]),
        "got {names:?}"
    );
}
