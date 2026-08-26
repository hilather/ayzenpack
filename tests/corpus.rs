//! Pinned Maven corpus lockfile, download-script guards, and overlap round-trip.
//!
//! Always-on `cargo test` stays offline. Overlap/CRC/unique-blob tests run when
//! `AYZENPACK_CORPUS_DIR` is set (corpus.yml). `#[ignore]`-style: skip unless env.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use ayzenpack::hashutil::{hash_reader, hex_lower};
use ayzenpack::{dehydrate, list, rehydrate, verify, DehydrateOptions, RehydrateOptions};
use zip::ZipArchive;

#[path = "fixtures.rs"]
mod fixtures;

const LOCK_JSON: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/ci/corpus.lock.json"));
const DOWNLOAD_SH: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/ci/download-corpus.sh"
));
const CORPUS_YML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/.github/workflows/corpus.yml"
));

const GUAVA_DEST: &str = "guava-33.2.1-jre.jar";

fn lock_root() -> serde_json::Value {
    serde_json::from_str(LOCK_JSON).expect("ci/corpus.lock.json must be valid JSON")
}

fn artifacts() -> Vec<serde_json::Value> {
    lock_root()
        .get("artifacts")
        .and_then(|v| v.as_array())
        .cloned()
        .expect("lockfile artifacts array")
}

fn corpus_dir() -> Option<PathBuf> {
    std::env::var_os("AYZENPACK_CORPUS_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn dehydrate_opts(output: &Path, inputs: Vec<PathBuf>) -> DehydrateOptions {
    DehydrateOptions {
        output: output.to_path_buf(),
        inputs,
        recursive: true,
        sort_inputs: true,
        quiet: true,
        ..DehydrateOptions::default()
    }
}

fn rehydrate_opts(input: &Path, dir: &Path) -> RehydrateOptions {
    RehydrateOptions {
        input: input.to_path_buf(),
        dir: dir.to_path_buf(),
        overwrite: true,
        quiet: true,
        ..RehydrateOptions::default()
    }
}

fn ensure_overlap_layout(corpus: &Path) {
    let copies = lock_root()
        .get("copies")
        .and_then(|v| v.as_array())
        .cloned()
        .expect("lockfile copies array");
    for copy in copies {
        let rel = copy["dir"].as_str().expect("copies.dir");
        let dest_dir = corpus.join(rel);
        fs::create_dir_all(&dest_dir).unwrap();
        for art in copy["artifacts"].as_array().expect("copies.artifacts") {
            let name = art.as_str().expect("artifact dest name");
            let src = corpus.join(name);
            assert!(
                src.is_file(),
                "missing pinned artifact {} (run ci/download-corpus.sh)",
                src.display()
            );
            let dst = dest_dir.join(name);
            if dst.exists() {
                continue;
            }
            fs::hard_link(&src, &dst)
                .or_else(|_| fs::copy(&src, &dst).map(|_| ()))
                .unwrap_or_else(|e| {
                    panic!("link/copy {} -> {}: {e}", src.display(), dst.display())
                });
        }
    }
}

fn jar_file_entry_count(path: &Path) -> u64 {
    let mut z = ZipArchive::new(File::open(path).unwrap()).unwrap();
    let n = z.len();
    let mut files = 0u64;
    for i in 0..n {
        if !z.by_index(i).unwrap().is_dir() {
            files += 1;
        }
    }
    files
}

/// Uncompressed file payloads keyed by Unicode ZIP name. Skips directories.
fn entry_map(path: &Path) -> BTreeMap<String, Vec<u8>> {
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

fn entry_crcs(path: &Path) -> BTreeMap<String, u32> {
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

fn cd_entries(path: &Path) -> Vec<(String, bool)> {
    let mut z = ZipArchive::new(File::open(path).unwrap()).unwrap();
    (0..z.len())
        .map(|i| {
            let e = z.by_index(i).unwrap();
            (e.name().to_string(), e.is_dir())
        })
        .collect()
}

fn assert_functional_identity(src: &Path, dest: &Path) {
    // Functional identity: uncompressed bytes, Unicode names, CRC, CD order.
    let src_map = entry_map(src);
    let dest_map = entry_map(dest);
    assert_eq!(
        src_map,
        dest_map,
        "entry map {} vs {}",
        src.display(),
        dest.display()
    );
    assert_eq!(
        entry_crcs(src),
        entry_crcs(dest),
        "CRC map {} vs {}",
        src.display(),
        dest.display()
    );
    assert_eq!(
        cd_entries(src),
        cd_entries(dest),
        "name order {} vs {}",
        src.display(),
        dest.display()
    );
    let dest_crcs = entry_crcs(dest);
    for (name, bytes) in &src_map {
        assert_eq!(crc32fast::hash(bytes), *dest_crcs.get(name).unwrap());
    }
}

#[test]
fn corpus_lock_has_no_latest_urls() {
    let lock = lock_root();
    let dump = lock.to_string();
    assert!(
        !dump.to_ascii_lowercase().contains("latest"),
        "corpus.lock.json must not contain 'latest'"
    );
    for art in artifacts() {
        let url = art["url"].as_str().unwrap_or("");
        assert!(
            !url.to_ascii_lowercase().contains("latest"),
            "artifact URL must not use latest: {url}"
        );
        assert!(
            url.starts_with("https://repo1.maven.org/maven2/"),
            "artifact URL must be pinned Maven Central: {url}"
        );
    }
}

#[test]
fn corpus_lock_every_entry_has_sha256() {
    let arts = artifacts();
    assert!(
        arts.len() >= 26,
        "lockfile must list all DESIGN.md artifacts, got {}",
        arts.len()
    );
    for art in &arts {
        let dest = art["dest"].as_str().unwrap_or("");
        let url = art["url"].as_str().unwrap_or("");
        let sha = art["sha256"].as_str().unwrap_or("");
        assert!(!dest.is_empty(), "empty dest");
        assert!(!url.is_empty(), "empty url for {dest}");
        assert!(
            !sha.is_empty(),
            "empty sha256 for {dest} is a merge blocker"
        );
        assert_eq!(
            sha.len(),
            64,
            "sha256 for {dest} must be 64 hex chars, got {}",
            sha.len()
        );
        assert!(
            sha.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "sha256 for {dest} must be lowercase hex, got {sha}"
        );
    }
}

#[test]
fn download_script_verifies_sha_and_documents_record() {
    assert!(
        DOWNLOAD_SH.contains("curl -fsSL --retry 3 --max-time 60"),
        "download-corpus.sh must curl --retry 3 --max-time 60"
    );
    assert!(
        DOWNLOAD_SH.contains("sha256sum"),
        "download-corpus.sh must verify SHA-256"
    );
    assert!(
        DOWNLOAD_SH.contains("--record"),
        "download-corpus.sh must document --record for pin bumps only"
    );
    assert!(
        DOWNLOAD_SH.contains("never in CI") || DOWNLOAD_SH.contains("Never in CI"),
        "--record must be documented as lockfile maintenance only, never in CI"
    );
}

#[test]
fn corpus_yml_linux_timeout_cache_dev_profile() {
    assert!(
        CORPUS_YML.contains("ubuntu-latest"),
        "corpus.yml is Linux only"
    );
    assert!(
        !CORPUS_YML.to_ascii_lowercase().contains("windows"),
        "corpus.yml must not run on Windows"
    );
    assert!(
        CORPUS_YML.contains("timeout-minutes: 25"),
        "corpus job timeout-minutes must be 25"
    );
    assert!(
        CORPUS_YML.contains("hashFiles('ci/corpus.lock.json')"),
        "corpus cache key must be lockfile hash"
    );
    assert!(
        CORPUS_YML.contains("cargo test --locked --test corpus"),
        "corpus.yml must cargo test --locked --test corpus (dev profile)"
    );
    assert!(
        !CORPUS_YML.contains("cargo test --release"),
        "corpus.yml must not cargo test --release under panic=abort"
    );
    assert!(
        CORPUS_YML.contains("cargo build --release"),
        "corpus.yml must cargo build --release for the CLI"
    );
    assert!(
        CORPUS_YML.contains("AYZENPACK_CORPUS_DIR"),
        "corpus.yml must set AYZENPACK_CORPUS_DIR"
    );
}

#[cfg(unix)]
#[test]
fn download_script_rejects_sha_mismatch() {
    // Fake file + file:// URL: no network. Wrong sha256 must fail closed.
    let dir = tempfile::tempdir().unwrap();
    let payload = dir.path().join("payload.jar");
    fs::write(&payload, b"not-the-bytes-the-lockfile-claims").unwrap();
    let url = format!("file://{}", payload.display());
    let lock_path = dir.path().join("corpus.lock.json");
    let lock = serde_json::json!({
        "artifacts": [{
            "dest": "fake.jar",
            "url": url,
            "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
        }],
        "copies": []
    });
    fs::write(&lock_path, serde_json::to_vec_pretty(&lock).unwrap()).unwrap();
    let corpus = dir.path().join("corpus");
    fs::create_dir_all(&corpus).unwrap();

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/download-corpus.sh");
    let out = std::process::Command::new("bash")
        .arg(&script)
        .env("LOCKFILE", &lock_path)
        .env("CORPUS_DIR", &corpus)
        .output()
        .expect("spawn download-corpus.sh");
    assert!(
        !out.status.success(),
        "download-corpus.sh must reject SHA-256 mismatch, stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
    .to_ascii_lowercase();
    assert!(
        combined.contains("mismatch")
            || combined.contains("sha256sum")
            || combined.contains("sha-256"),
        "mismatch failure should mention SHA-256, got: {combined}"
    );
}

#[test]
fn corpus_overlap_roundtrip() {
    let Some(corpus) = corpus_dir() else {
        eprintln!("skipping corpus_overlap_roundtrip: AYZENPACK_CORPUS_DIR not set");
        return;
    };
    assert!(
        corpus.is_dir(),
        "AYZENPACK_CORPUS_DIR={} is not a directory",
        corpus.display()
    );
    ensure_overlap_layout(&corpus);
    let apps = corpus.join("apps");
    assert!(apps.is_dir(), "overlap tree missing at {}", apps.display());

    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("overlap.ayz");
    let restored = tmp.path().join("restored");
    let summary = dehydrate(&dehydrate_opts(&out, vec![apps])).unwrap();
    assert!(
        summary.jar_count > 1,
        "overlap tree must pack more than one JAR"
    );
    verify(&out).unwrap();

    rehydrate(&rehydrate_opts(&out, &restored)).unwrap();
    let manifest = list(&out).unwrap();
    assert_eq!(manifest.jars.len() as u64, summary.jar_count);
    for jar in &manifest.jars {
        let src = PathBuf::from(&jar.source_path);
        let dest = restored.join(&jar.name);
        assert!(src.is_file(), "source {}", src.display());
        assert!(dest.is_file(), "restored {}", dest.display());
        assert_functional_identity(&src, &dest);
        // Nested JARs (kafka/lucene) stay opaque blobs; identity would fail if exploded.
    }
}

#[test]
fn corpus_guava_copies_unique_blobs_eq_one_jar_file_entries() {
    let Some(corpus) = corpus_dir() else {
        eprintln!(
            "skipping corpus_guava_copies_unique_blobs_eq_one_jar_file_entries: AYZENPACK_CORPUS_DIR not set"
        );
        return;
    };
    ensure_overlap_layout(&corpus);
    let web = corpus.join("apps/web/lib").join(GUAVA_DEST);
    let search = corpus.join("apps/search/lib").join(GUAVA_DEST);
    assert!(web.is_file(), "missing {}", web.display());
    assert!(search.is_file(), "missing {}", search.display());

    let one_jar_files = jar_file_entry_count(&web);
    assert!(one_jar_files > 0, "guava JAR has no file entries");

    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("guava.ayz");
    let mut opts = dehydrate_opts(&out, vec![web, search]);
    opts.recursive = false;
    let summary = dehydrate(&opts).unwrap();
    assert_eq!(summary.jar_count, 2);
    assert_eq!(summary.file_entry_count, one_jar_files * 2);
    let m = ayzenpack::list(&out).unwrap();
    let content: std::collections::BTreeSet<_> = m
        .jars
        .iter()
        .flat_map(|j| j.entries.iter().filter_map(|e| e.blob.clone()))
        .collect();
    assert_eq!(
        content.len() as u64,
        one_jar_files,
        "content blobs for duplicated guava copies must equal one JAR's file-entry count"
    );
    assert!(summary.unique_blob_count >= one_jar_files);
}

/// Regular Maven JARs + official launch.script wraps (unadjusted, zip -A, Zip64 nested-lib).
/// Whole-file blake3/sha256 is the bit-identical gate. Proven codec-miss rebuilds may
/// change the file hash but must still be a valid ZIP with matching entry bytes.
#[test]
fn corpus_mix_regular_and_spring_whole_file_hashes() {
    let Some(corpus) = corpus_dir() else {
        eprintln!(
            "skipping corpus_mix_regular_and_spring_whole_file_hashes: AYZENPACK_CORPUS_DIR not set"
        );
        return;
    };
    assert!(
        corpus.is_dir(),
        "AYZENPACK_CORPUS_DIR={} is not a directory",
        corpus.display()
    );

    let launcher = fixtures::spring_boot_launch_script();
    assert!(
        launcher.starts_with(b"#!/bin/bash"),
        "official launch.script must be a bash prefix"
    );

    let tmp = tempfile::tempdir().unwrap();
    let mix = tmp.path().join("mix");
    fs::create_dir_all(&mix).unwrap();

    let regular = [
        ("failureaccess-1.0.2.jar", "plain-failureaccess.jar"),
        ("slf4j-api-2.0.16.jar", "plain-slf4j.jar"),
        (
            "jackson-annotations-2.17.2.jar",
            "plain-jackson-annotations.jar",
        ),
    ];
    for (src_name, dest_name) in regular {
        let src = corpus.join(src_name);
        assert!(src.is_file(), "missing pinned artifact {}", src.display());
        fs::copy(&src, mix.join(dest_name)).unwrap();
    }

    let jackson_core = fs::read(corpus.join("jackson-core-2.17.2.jar")).unwrap();
    fs::write(
        mix.join("spring-jackson-core.jar"),
        fixtures::prepend_launcher(&jackson_core, launcher, false),
    )
    .unwrap();

    let slf4j = fs::read(corpus.join("slf4j-api-2.0.16.jar")).unwrap();
    fs::write(
        mix.join("spring-zipa-slf4j.jar"),
        fixtures::prepend_launcher(&slf4j, launcher, true),
    )
    .unwrap();

    let failureaccess = fs::read(corpus.join("failureaccess-1.0.2.jar")).unwrap();
    fixtures::write_wrapped_zip64_jar(
        &mix.join("spring-zip64-nested.jar"),
        launcher,
        &[
            ("BOOT-INF/lib/failureaccess.jar", failureaccess.as_slice()),
            ("App.class", b"zip64-app"),
        ],
    );

    let out = tmp.path().join("mix.ayz");
    let restored = tmp.path().join("restored");
    let summary = dehydrate(&dehydrate_opts(&out, vec![mix.clone()])).unwrap();
    assert_eq!(summary.jar_count, 6, "mix must pack all six members");
    verify(&out).unwrap();
    rehydrate(&rehydrate_opts(&out, &restored)).unwrap();
    let manifest = list(&out).unwrap();
    assert_eq!(manifest.jars.len(), 6);
    let names: BTreeMap<_, _> = manifest
        .jars
        .iter()
        .map(|j| (j.name.as_str(), ()))
        .collect();
    for expected in [
        "plain-failureaccess.jar",
        "plain-slf4j.jar",
        "plain-jackson-annotations.jar",
        "spring-jackson-core.jar",
        "spring-zipa-slf4j.jar",
        "spring-zip64-nested.jar",
    ] {
        assert!(
            names.contains_key(expected),
            "mix missing {expected}; got {:?}",
            manifest.jars.iter().map(|j| &j.name).collect::<Vec<_>>()
        );
    }

    let mut codec_hit = 0u64;
    let mut codec_miss = 0u64;
    let mut cdata_blob = 0u64;
    let mut hash_match = 0u64;
    let mut hash_mismatch_proven_miss = 0u64;
    for jar in &manifest.jars {
        for e in &jar.entries {
            if e.is_dir || e.method_code != 8 {
                continue;
            }
            if e.cdata_codec.is_some() {
                codec_hit += 1;
            } else if e.cdata_blob.is_some() {
                cdata_blob += 1;
            } else {
                codec_miss += 1;
            }
        }
        let src = PathBuf::from(&jar.source_path);
        let dest = restored.join(&jar.name);
        assert!(src.is_file(), "source {}", src.display());
        assert!(dest.is_file(), "restored {}", dest.display());
        let (src_b3, src_sha) = hash_reader(File::open(&src).unwrap()).unwrap();
        let (dest_b3, dest_sha) = hash_reader(File::open(&dest).unwrap()).unwrap();
        println!(
            "hash {} identical={} bit_identical={} rebuild={} raw_zip={} blake3 {} -> {} sha256 {} -> {}",
            jar.name,
            src_b3 == dest_b3 && src_sha == dest_sha,
            jar.bit_identical_restore(),
            jar.metadata_rebuild(),
            jar.raw_zip_blob.is_some(),
            hex_lower(&src_b3),
            hex_lower(&dest_b3),
            hex_lower(&src_sha),
            hex_lower(&dest_sha)
        );
        let hashes_eq = src_b3 == dest_b3 && src_sha == dest_sha;
        if jar.name == "spring-zip64-nested.jar" {
            assert!(
                jar.bit_identical_restore(),
                "zip-crate Zip64 fat must be bit-identical, not rebuild"
            );
            assert!(hashes_eq, "Zip64 nested-lib fat whole-file hash must match");
        }
        if jar.bit_identical_restore() {
            assert!(
                hashes_eq,
                "{} is bit_identical_restore but blake3/sha256 changed",
                jar.name
            );
            hash_match += 1;
            continue;
        }
        assert!(
            jar.metadata_rebuild(),
            "{} hash mismatch must be metadata_rebuild (got neither exact nor rebuild)",
            jar.name
        );
        let proven_miss = jar.entries.iter().all(|e| {
            e.is_dir || e.method_code != 8 || (e.cdata_codec.is_none() && e.cdata_blob.is_none())
        });
        assert!(
            proven_miss,
            "{} is not bit-identical but a method-8 file still has cdata_codec/cdata_blob (codec not proven miss)",
            jar.name
        );
        assert_functional_identity(&src, &dest);
        if jar.prefix_size.unwrap_or(0) > 0 {
            let dest_bytes = fs::read(&dest).unwrap();
            assert!(
                dest_bytes.starts_with(launcher),
                "{} must keep official launch.script prefix",
                jar.name
            );
        }
        if hashes_eq {
            hash_match += 1;
        } else {
            hash_mismatch_proven_miss += 1;
        }
    }

    println!(
        "mix stats jars={} bytes_in_jars={} unique_blob_count={} bytes_unique_blobs={} ayz={} ratio={:.4} codec_hit={} codec_miss={} cdata_blob={} hash_match={} hash_mismatch_proven_miss={}",
        summary.jar_count,
        summary.bytes_in_jars,
        summary.unique_blob_count,
        summary.bytes_unique_blobs,
        summary.output_len,
        summary.output_len as f64 / summary.bytes_in_jars.max(1) as f64,
        codec_hit,
        codec_miss,
        cdata_blob,
        hash_match,
        hash_mismatch_proven_miss
    );
    assert!(
        hash_match >= 1,
        "mix must include at least the Zip64 fat as a hash match"
    );
    assert!(
        summary.output_len < summary.bytes_in_jars * 3,
        "mix .ayz must not balloon toward cdata-full"
    );
    const MIX_V019_BYTES: u64 = 569539;
    assert!(
        summary.output_len <= MIX_V019_BYTES * 115 / 100,
        "mix output_len {} exceeds 569539 * 115/100 ({})",
        summary.output_len,
        MIX_V019_BYTES * 115 / 100
    );
}
