//! `--restore-paths` dehydrate metadata and rehydrate dest/mode/owner.

#[path = "fixtures.rs"]
mod fixtures;

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use ayzenpack::Manifest;
use fixtures::write_jar;
use predicates::prelude::*;

fn ayzenpack() -> Command {
    Command::cargo_bin("ayzenpack").expect("binary must be named ayzenpack")
}

fn write_sample(path: &Path) {
    write_jar(path, &[("x.txt", b"restore-paths")]);
}

fn sidecar_manifest(pack: &Path) -> Manifest {
    let json = pack.with_extension("ayz.manifest.json");
    serde_json::from_slice(&fs::read(json).unwrap()).unwrap()
}

fn pack_restore(dir: &Path, jar: &Path, out_name: &str) -> PathBuf {
    let out = dir.join(out_name);
    let sidecar = dir.join(format!("{out_name}.manifest.json"));
    ayzenpack()
        .args(["dehydrate", "--restore-paths", "--write-sidecar-manifest"])
        .arg(&sidecar)
        .arg("-o")
        .arg(&out)
        .arg(jar)
        .assert()
        .success();
    out
}

#[test]
fn dehydrate_restore_paths_records_abs_path_and_mode() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("a.jar");
    write_sample(&jar);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let mut perms = fs::metadata(&jar).unwrap().permissions();
        perms.set_mode(0o640);
        fs::set_permissions(&jar, perms).unwrap();
        let meta = fs::metadata(&jar).unwrap();
        let want_mode = meta.mode();
        let want_uid = meta.uid();
        let want_gid = meta.gid();
        let _ = pack_restore(dir.path(), &jar, "all.ayz");
        let m = sidecar_manifest(&dir.path().join("all.ayz"));
        let rec = &m.jars[0];
        assert_eq!(rec.name, "a.jar");
        let got = rec.restore_path.as_deref().expect("restore_path");
        assert_eq!(got, jar.canonicalize().unwrap().to_str().unwrap());
        assert!(Path::new(got).is_absolute());
        assert_eq!(rec.restore_mode, Some(want_mode));
        assert_eq!(rec.restore_uid, Some(want_uid));
        assert_eq!(rec.restore_gid, Some(want_gid));
        assert!(!rec.source_path.is_empty());
    }
    #[cfg(not(unix))]
    {
        let _ = pack_restore(dir.path(), &jar, "all.ayz");
        let m = sidecar_manifest(&dir.path().join("all.ayz"));
        let rec = &m.jars[0];
        assert_eq!(rec.name, "a.jar");
        let got = rec.restore_path.as_deref().expect("restore_path");
        assert_eq!(got, jar.canonicalize().unwrap().to_str().unwrap());
        assert!(Path::new(got).is_absolute());
        assert!(rec.restore_mode.is_some());
        assert_eq!(rec.restore_uid, None);
        assert_eq!(rec.restore_gid, None);
    }
}

#[test]
fn rehydrate_restore_paths_writes_overwrites_and_restores_mode() {
    let dir = tempfile::tempdir().unwrap();
    let dest_dir = dir.path().join("orig");
    fs::create_dir_all(&dest_dir).unwrap();
    let jar = dest_dir.join("a.jar");
    write_sample(&jar);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let mut perms = fs::metadata(&jar).unwrap().permissions();
        perms.set_mode(0o640);
        fs::set_permissions(&jar, perms).unwrap();
        let want_mode = fs::metadata(&jar).unwrap().mode() & 0o7777;
        let want_uid = fs::metadata(&jar).unwrap().uid();
        let want_gid = fs::metadata(&jar).unwrap().gid();
        let orig = fs::read(&jar).unwrap();
        let pack = pack_restore(dir.path(), &jar, "all.ayz");

        File::create(&jar).unwrap().write_all(b"stale").unwrap();
        ayzenpack()
            .args(["rehydrate", "--restore-paths", "-i"])
            .arg(&pack)
            .assert()
            .success();
        assert_eq!(fs::read(&jar).unwrap(), orig);
        let got = fs::metadata(&jar).unwrap();
        assert_eq!(got.mode() & 0o7777, want_mode);
        if got.uid() == want_uid {
            assert_eq!(got.uid(), want_uid);
            assert_eq!(got.gid(), want_gid);
        }
    }
    #[cfg(not(unix))]
    {
        let orig = fs::read(&jar).unwrap();
        let pack = pack_restore(dir.path(), &jar, "all.ayz");
        File::create(&jar).unwrap().write_all(b"stale").unwrap();
        ayzenpack()
            .args(["rehydrate", "--restore-paths", "-i"])
            .arg(&pack)
            .assert()
            .success();
        assert_eq!(fs::read(&jar).unwrap(), orig);
    }
}

#[test]
fn rehydrate_restore_paths_without_metadata_errors() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("a.jar");
    write_sample(&jar);
    let pack = dir.path().join("plain.ayz");
    ayzenpack()
        .arg("dehydrate")
        .arg("-o")
        .arg(&pack)
        .arg(&jar)
        .assert()
        .success();
    ayzenpack()
        .args(["rehydrate", "--restore-paths", "-i"])
        .arg(&pack)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "pack was not created with --restore-paths",
        ));
}

#[test]
fn default_rehydrate_of_restore_paths_pack_uses_dir_basename() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("a.jar");
    write_sample(&jar);
    let pack = pack_restore(dir.path(), &jar, "all.ayz");
    let out = dir.path().join("restored");
    ayzenpack()
        .arg("rehydrate")
        .arg("-i")
        .arg(&pack)
        .arg("-d")
        .arg(&out)
        .assert()
        .success();
    assert!(out.join("a.jar").is_file());
    assert_eq!(
        fs::read(out.join("a.jar")).unwrap(),
        fs::read(&jar).unwrap()
    );
}

#[test]
fn restore_paths_replaces_symlink_without_touching_target() {
    let dir = tempfile::tempdir().unwrap();
    let real_dir = dir.path().join("real");
    fs::create_dir_all(&real_dir).unwrap();
    let jar = real_dir.join("a.jar");
    write_sample(&jar);
    let orig = fs::read(&jar).unwrap();
    let pack = pack_restore(dir.path(), &jar, "all.ayz");

    let target = dir.path().join("link-target.bin");
    fs::write(&target, b"do-not-touch").unwrap();
    fs::remove_file(&jar).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &jar).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&target, &jar).unwrap();
    assert!(fs::symlink_metadata(&jar).unwrap().file_type().is_symlink());

    ayzenpack()
        .args(["rehydrate", "--restore-paths", "-i"])
        .arg(&pack)
        .assert()
        .success();

    let meta = fs::symlink_metadata(&jar).unwrap();
    assert!(
        meta.file_type().is_file() && !meta.file_type().is_symlink(),
        "dest must be a regular file, not a symlink"
    );
    assert_eq!(fs::read(&jar).unwrap(), orig);
    assert_eq!(fs::read(&target).unwrap(), b"do-not-touch");
}

#[test]
fn restore_paths_creates_missing_parent_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("keep").join("nested").join("deep");
    fs::create_dir_all(&nested).unwrap();
    let jar = nested.join("a.jar");
    write_sample(&jar);
    let pack = pack_restore(dir.path(), &jar, "all.ayz");
    fs::remove_dir_all(dir.path().join("keep")).unwrap();
    assert!(!nested.exists());

    ayzenpack()
        .args(["rehydrate", "--restore-paths", "-i"])
        .arg(&pack)
        .assert()
        .success();
    assert!(jar.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let parent_mode = fs::metadata(jar.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(parent_mode & 0o777, 0o755);
    }
}

#[test]
fn restore_paths_does_not_require_dir() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("a.jar");
    write_sample(&jar);
    let pack = pack_restore(dir.path(), &jar, "all.ayz");
    fs::remove_file(&jar).unwrap();
    ayzenpack()
        .args(["rehydrate", "--restore-paths", "-i"])
        .arg(&pack)
        .assert()
        .success();
    assert!(jar.is_file());
}

#[test]
fn restore_paths_wins_over_dir() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("a.jar");
    write_sample(&jar);
    let orig = fs::read(&jar).unwrap();
    let pack = pack_restore(dir.path(), &jar, "all.ayz");
    fs::remove_file(&jar).unwrap();
    let unused = dir.path().join("unused-dir");
    ayzenpack()
        .args(["rehydrate", "--restore-paths", "-i"])
        .arg(&pack)
        .arg("-d")
        .arg(&unused)
        .assert()
        .success();
    assert!(jar.is_file());
    assert_eq!(fs::read(&jar).unwrap(), orig);
    assert!(
        !unused.exists()
            || unused
                .read_dir()
                .map(|mut d| d.next().is_none())
                .unwrap_or(true),
        "--dir must be unused when --restore-paths is set"
    );
}

#[cfg(unix)]
#[test]
fn restore_paths_recorded_mode_wins_over_prefix_0755() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("app.jar");
    fixtures::write_wrapped_jar(&jar, fixtures::SPRING_LAUNCHER, &[("BOOT-INF/x", b"y")]);
    let mut perms = fs::metadata(&jar).unwrap().permissions();
    perms.set_mode(0o644);
    fs::set_permissions(&jar, perms).unwrap();
    let pack = pack_restore(dir.path(), &jar, "spring.ayz");
    fs::remove_file(&jar).unwrap();
    ayzenpack()
        .args(["rehydrate", "--restore-paths", "-i"])
        .arg(&pack)
        .assert()
        .success();
    let mode = fs::metadata(&jar).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o644, "recorded mode must win over prefix 0755");
}

#[test]
fn dehydrate_without_flag_omits_restore_keys() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("a.jar");
    write_sample(&jar);
    let pack = dir.path().join("plain.ayz");
    let sidecar = dir.path().join("plain.ayz.manifest.json");
    ayzenpack()
        .args(["dehydrate", "--write-sidecar-manifest"])
        .arg(&sidecar)
        .arg("-o")
        .arg(&pack)
        .arg(&jar)
        .assert()
        .success();
    let text = fs::read_to_string(&sidecar).unwrap();
    assert!(!text.contains("restore_path"));
    assert!(!text.contains("restore_mode"));
}
