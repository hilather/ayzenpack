//! `--restore-paths` dehydrate metadata and rehydrate dest/mode/owner.

#[path = "fixtures.rs"]
mod fixtures;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use ayzenpack::hashutil::{blake3_bytes, hex_lower};
use ayzenpack::Manifest;
use fixtures::{
    inner_incompressible_jar, write_classic_nested_store_jar,
    write_fat_spring_store_nested_zipa_jar, write_fat_spring_store_nested_zipa_with,
    write_fat_spring_zip64_zipa_jar, write_jar, write_overlapping_local_plus_store_nested,
};
use predicates::prelude::*;
use zip::{CompressionMethod, ZipArchive};

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

fn file_entry_map(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut z = ZipArchive::new(File::open(path).unwrap()).unwrap();
    let mut map = BTreeMap::new();
    for i in 0..z.len() {
        let mut e = z.by_index(i).unwrap();
        if e.is_dir() {
            continue;
        }
        let mut buf = Vec::new();
        e.read_to_end(&mut buf).unwrap();
        map.insert(e.name().to_string(), buf);
    }
    map
}

fn file_entry_crcs(path: &Path) -> BTreeMap<String, u32> {
    let mut z = ZipArchive::new(File::open(path).unwrap()).unwrap();
    let mut map = BTreeMap::new();
    for i in 0..z.len() {
        let e = z.by_index(i).unwrap();
        if e.is_dir() {
            continue;
        }
        map.insert(e.name().to_string(), e.crc32());
    }
    map
}

fn file_content_blobs(m: &Manifest) -> BTreeSet<String> {
    m.jars
        .iter()
        .flat_map(|j| {
            j.entries
                .iter()
                .filter(|e| !e.is_dir)
                .filter_map(|e| e.blob.clone())
                .chain(
                    j.nestedindexes
                        .iter()
                        .flat_map(|n| n.entries.iter().filter_map(|e| e.blob.clone())),
                )
        })
        .collect()
}

fn assert_not_10x_smaller(restored: u64, source: u64) {
    assert!(
        restored * 10 >= source,
        "restored {restored} is ≥10× smaller than source {source}"
    );
}

fn assert_listed_no_dual(jar: &ayzenpack::manifest::Jar) {
    assert!(
        jar.raw_zip_blob.is_none(),
        "{} must not store raw_zip",
        jar.name
    );
    assert_eq!(jar.raw_zip_size.unwrap_or(0), 0);
    for e in &jar.entries {
        assert!(e.cdata_blob.is_none(), "{}!{} cdata_blob", jar.name, e.name);
        assert!(
            !(e.blob.is_some() && e.zip_index.is_some()),
            "{}!{} both blob and zip_index",
            jar.name,
            e.name
        );
        if !e.is_dir && e.zip_index.is_none() {
            assert!(
                e.blob.is_some(),
                "{}!{} file entry must have a content blob or zip_index",
                jar.name,
                e.name
            );
        }
        if e.zip_index.is_some() {
            assert!(e.blob.is_none());
        }
    }
    for nested in &jar.nestedindexes {
        for e in &nested.entries {
            assert!(e.cdata_blob.is_none(), "nested {} cdata_blob", e.name);
        }
    }
}

fn dehydrate_matt_dir(jars: &Path, pack: &Path, sidecar: &Path) {
    ayzenpack()
        .args([
            "dehydrate",
            "--recursive",
            "--sort-inputs",
            "--restore-paths",
            "--write-sidecar-manifest",
        ])
        .arg(sidecar)
        .arg("-o")
        .arg(pack)
        .arg(jars)
        .assert()
        .success();
}

fn rehydrate_restore_paths_only(pack: &Path) {
    ayzenpack()
        .args(["rehydrate", "--restore-paths", "-i"])
        .arg(pack)
        .assert()
        .success();
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
fn in_place_restore_paths_zip64_fat_and_classic() {
    // Matt CLI identity on the 0.2.2 Zip64 fixture (DEFLATE nested — does not
    // latch) plus a few-MiB classic. Same rule as tiny a.jar: --restore-paths
    // does not need --overwrite. On this tree every in-tree fat splices
    // (tail_blob); there is no skip-exact fat dest-dir rebuild to add.
    let dir = tempfile::tempdir().unwrap();
    let jars = dir.path().join("jars");
    fs::create_dir_all(&jars).unwrap();
    let fat = jars.join("app.jar");
    let classic = jars.join("plain.jar");
    write_fat_spring_zip64_zipa_jar(&fat);
    write_classic_nested_store_jar(&classic);
    let fat_src_len = fs::metadata(&fat).unwrap().len();
    let classic_src_len = fs::metadata(&classic).unwrap().len();
    let fat_src = file_entry_map(&fat);
    let classic_src = file_entry_map(&classic);
    let classic_crc = file_entry_crcs(&classic);
    assert!(fat_src.keys().any(|n| n.starts_with("BOOT-INF/lib/")));
    assert!(classic_src.contains_key("lib/payload.jar"));

    let pack = dir.path().join("pack.ayz");
    let sidecar = dir.path().join("pack.ayz.manifest.json");
    dehydrate_matt_dir(&jars, &pack, &sidecar);
    let m: Manifest = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
    assert_eq!(m.jars.len(), 2);
    let mut raw_zip_size_sum = 0u64;
    for jar in &m.jars {
        assert_listed_no_dual(jar);
        raw_zip_size_sum += jar.raw_zip_size.unwrap_or(0);
    }
    assert_eq!(raw_zip_size_sum, 0);

    rehydrate_restore_paths_only(&pack);

    let fat_got_len = fs::metadata(&fat).unwrap().len();
    let classic_got_len = fs::metadata(&classic).unwrap().len();
    assert_not_10x_smaller(fat_got_len, fat_src_len);
    assert_not_10x_smaller(classic_got_len, classic_src_len);
    assert_eq!(file_entry_map(&fat), fat_src);
    assert_eq!(file_entry_map(&classic), classic_src);
    assert_eq!(file_entry_crcs(&classic), classic_crc);
}

#[test]
fn in_place_restore_paths_store_nested_zipa_must_not_collapse() {
    // 134→5.5 class: STORE nested + zip-A + --restore-paths (no --overwrite).
    // ZipArchive::new(File) is the outer view on zip-A. Snapshot before restore.
    let dir = tempfile::tempdir().unwrap();
    let jars = dir.path().join("jars");
    fs::create_dir_all(&jars).unwrap();
    let fat = jars.join("app.jar");
    write_fat_spring_store_nested_zipa_jar(&fat);
    let src_len = fs::metadata(&fat).unwrap().len();
    let src = file_entry_map(&fat);
    assert!(
        src.keys().any(|n| n.starts_with("BOOT-INF/lib/")),
        "source must be the outer listing, got {:?}",
        src.keys()
    );
    assert!(src.contains_key("App.class"));

    let pack = dir.path().join("pack.ayz");
    let sidecar = dir.path().join("pack.ayz.manifest.json");
    dehydrate_matt_dir(&jars, &pack, &sidecar);
    let m: Manifest = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
    assert_listed_no_dual(&m.jars[0]);

    rehydrate_restore_paths_only(&pack);

    assert_not_10x_smaller(fs::metadata(&fat).unwrap().len(), src_len);
    assert_eq!(file_entry_map(&fat), src);
}

#[test]
fn in_place_restore_paths_two_fats_share_nested_lib_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let jars = dir.path().join("jars");
    fs::create_dir_all(&jars).unwrap();
    let shared = inner_incompressible_jar(3, 64 * 1024);
    let extra_a = inner_incompressible_jar(4, 32 * 1024);
    let extra_b = inner_incompressible_jar(5, 32 * 1024);
    let a = jars.join("a.jar");
    let b = jars.join("b.jar");
    write_fat_spring_store_nested_zipa_with(&a, b"app-A", &[shared.clone(), extra_a]);
    write_fat_spring_store_nested_zipa_with(&b, b"app-B", &[shared.clone(), extra_b]);
    let a_len = fs::metadata(&a).unwrap().len();
    let b_len = fs::metadata(&b).unwrap().len();
    let a_src = file_entry_map(&a);
    let b_src = file_entry_map(&b);
    assert!(a_src.contains_key("BOOT-INF/lib/lib0.jar"));
    assert!(b_src.contains_key("BOOT-INF/lib/lib0.jar"));
    assert_eq!(
        a_src.get("BOOT-INF/lib/lib0.jar"),
        Some(&shared),
        "source map must be the outer planted lib, not an inner listing"
    );
    assert_eq!(b_src.get("BOOT-INF/lib/lib0.jar"), Some(&shared));

    let pack = dir.path().join("pack.ayz");
    let sidecar = dir.path().join("pack.ayz.manifest.json");
    dehydrate_matt_dir(&jars, &pack, &sidecar);
    let m: Manifest = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
    let file_entries = m
        .jars
        .iter()
        .flat_map(|j| j.entries.iter())
        .filter(|e| !e.is_dir)
        .count();
    let blobs = file_content_blobs(&m);
    assert!(
        blobs.len() < file_entries,
        "unique file content blobs {} must be < file entries {}",
        blobs.len(),
        file_entries
    );
    let want = hex_lower(&blake3_bytes(&shared));
    assert!(
        !m.blobs.iter().any(|b| b.blake3 == want),
        "blake3(inner zip) must not be a CAS blob after explode"
    );
    for jar in &m.jars {
        assert_listed_no_dual(jar);
        let e = jar
            .entries
            .iter()
            .find(|e| e.name == "BOOT-INF/lib/lib0.jar")
            .expect("planted lib0");
        assert!(e.blob.is_none());
        assert!(e.zip_index.is_some());
    }

    rehydrate_restore_paths_only(&pack);
    assert_not_10x_smaller(fs::metadata(&a).unwrap().len(), a_len);
    assert_not_10x_smaller(fs::metadata(&b).unwrap().len(), b_len);
    assert_eq!(file_entry_map(&a), a_src);
    assert_eq!(file_entry_map(&b), b_src);
}

#[test]
fn in_place_restore_paths_overlap_store_nested_stays_stored() {
    // Equal-offset outer exact-splices when inner zip_index hits. Dest
    // lib/inner.jar stays Stored. Matt CLI.
    let dir = tempfile::tempdir().unwrap();
    let jars = dir.path().join("jars");
    fs::create_dir_all(&jars).unwrap();
    let jar = jars.join("overlap-nested.jar");
    write_overlapping_local_plus_store_nested(&jar);
    let src_bytes = fs::read(&jar).unwrap();
    let src_len = src_bytes.len() as u64;
    let src = file_entry_map(&jar);
    assert!(
        src.contains_key("lib/inner.jar"),
        "source must be the outer listing, got {:?}",
        src.keys()
    );

    let pack = dir.path().join("pack.ayz");
    let sidecar = dir.path().join("pack.ayz.manifest.json");
    dehydrate_matt_dir(&jars, &pack, &sidecar);
    let m: Manifest = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
    assert_listed_no_dual(&m.jars[0]);
    assert!(
        m.jars[0].tail_blob.is_some(),
        "equal-offset outer homemade_ok must attach tail_blob"
    );
    assert!(m.jars[0].raw_zip_blob.is_none());
    assert!(m.jars[0].bit_identical_restore());
    let e = m.jars[0]
        .entries
        .iter()
        .find(|e| e.name == "lib/inner.jar")
        .expect("inner");
    assert!(e.blob.is_none());
    assert!(e.zip_index.is_some());
    let inner = src.get("lib/inner.jar").expect("inner bytes");
    let inner_hex = hex_lower(&blake3_bytes(inner));
    assert!(
        !m.blobs.iter().any(|b| b.blake3 == inner_hex),
        "blake3(inner zip) must not be in blobs[]"
    );
    assert_eq!(
        file_content_blobs(&m).len(),
        2,
        "unique content (SAME-payload + inner n.txt) must not be doubled"
    );

    rehydrate_restore_paths_only(&pack);
    assert_eq!(fs::read(&jar).unwrap(), src_bytes);
    let got_len = fs::metadata(&jar).unwrap().len();
    assert!(
        got_len * 2 >= src_len,
        "restored {got_len} must stay in the same league as source {src_len}"
    );
    assert_eq!(file_entry_map(&jar), src);
    let mut z = ZipArchive::new(File::open(&jar).unwrap()).unwrap();
    let inner_e = z.by_name("lib/inner.jar").unwrap();
    assert_eq!(inner_e.compression(), CompressionMethod::Stored);
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
