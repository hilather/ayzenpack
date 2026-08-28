//! Pinned Maven corpus lockfile, download-script guards, and overlap round-trip.
//!
//! Always-on `cargo test` stays offline. Overlap/CRC/unique-blob tests run when
//! `AYZENPACK_CORPUS_DIR` is set (corpus.yml). `#[ignore]`-style: skip unless env.

use std::collections::{BTreeMap, BTreeSet};
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
const DATAFLOW_4_DEST: &str = "spring-cloud-dataflow-server-2.11.4.jar";
const DATAFLOW_5_DEST: &str = "spring-cloud-dataflow-server-2.11.5.jar";

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

fn jar_uncompressed_file_bytes(path: &Path) -> u64 {
    let mut z = ZipArchive::new(File::open(path).unwrap()).unwrap();
    let mut bytes = 0u64;
    for i in 0..z.len() {
        let e = z.by_index(i).unwrap();
        if !e.is_dir() {
            bytes += e.size();
        }
    }
    bytes
}

/// Distinct `entries[].blob` ids and the sum of those content blob sizes.
/// Index tails / raw_zip / large local headers are not content.
fn unique_content_blobs(manifest: &ayzenpack::Manifest) -> (u64, u64) {
    let sizes: BTreeMap<&str, u64> = manifest
        .blobs
        .iter()
        .map(|b| (b.blake3.as_str(), b.size))
        .collect();
    let mut ids = BTreeSet::new();
    let mut bytes = 0u64;
    for jar in &manifest.jars {
        for e in &jar.entries {
            if let Some(id) = e.blob.as_deref() {
                if ids.insert(id) {
                    bytes += *sizes.get(id).expect("entry blob missing from blobs[]");
                }
            }
        }
    }
    (ids.len() as u64, bytes)
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
            if e.cdata_blob.is_some() {
                cdata_blob += 1;
            }
            if e.is_dir || e.method_code != 8 {
                continue;
            }
            if e.cdata_codec.is_some() {
                codec_hit += 1;
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
        assert!(
            jar.raw_zip_blob.is_none(),
            "{} must not store raw_zip on a listed mix member",
            jar.name
        );
        if jar.name == "spring-zip64-nested.jar" {
            assert!(
                jar.bit_identical_restore(),
                "zip-crate Zip64 fat must be bit-identical, not rebuild"
            );
            assert!(
                jar.raw_zip_blob.is_none(),
                "Zip64 fat bit-identical must be splice, not raw_zip"
            );
            assert!(hashes_eq, "Zip64 nested-lib fat whole-file hash must match");
            for e in &jar.entries {
                if e.name.starts_with("BOOT-INF/lib/") {
                    assert!(
                        e.zip_index.is_none(),
                        "DEFLATE-wrapped Zip64 nested must stay opaque"
                    );
                    assert!(e.blob.is_some());
                }
            }
        }
        for e in &jar.entries {
            if e.zip_index.is_some() {
                assert!(e.blob.is_none());
                let src_jar = PathBuf::from(&jar.source_path);
                let mut z = ZipArchive::new(File::open(&src_jar).unwrap()).unwrap();
                let mut inner = Vec::new();
                z.by_name(&e.name).unwrap().read_to_end(&mut inner).unwrap();
                let inner_hex = hex_lower(&ayzenpack::hashutil::blake3_bytes(&inner));
                assert!(
                    !manifest.blobs.iter().any(|b| b.blake3 == inner_hex),
                    "blake3(inner zip) must not be in blobs[] for STORE zip_index {}",
                    e.name
                );
            }
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
        // Hash match required iff bit_identical_restore; else valid ZIP +
        // functional identity. Skip-exact (!tail && !raw_zip) must not be
        // forced into metadata_rebuild().
        let skip_exact = jar.tail_blob.is_none() && jar.raw_zip_blob.is_none();
        if !skip_exact {
            assert!(
                jar.metadata_rebuild(),
                "{} must be metadata_rebuild (got neither exact, rebuild, nor skip-exact)",
                jar.name
            );
            let has_method8_file = jar.entries.iter().any(|e| !e.is_dir && e.method_code == 8);
            let has_miss = jar
                .entries
                .iter()
                .any(|e| !e.is_dir && e.method_code == 8 && e.cdata_codec.is_none());
            assert!(
                !has_method8_file || has_miss,
                "{} is not bit-identical but every method-8 file has a codec (no proven miss)",
                jar.name
            );
        }
        for e in &jar.entries {
            assert!(
                e.cdata_blob.is_none(),
                "{}!{} must not write cdata_blob on a rebuild/skip-exact jar",
                jar.name,
                e.name
            );
        }
        if jar.tail_blob.is_none() && jar.prefix_size.unwrap_or(0) > 0 {
            // Skip-exact prefixed dest is FileAbs (outer names via ZipArchive::new(File)).
            // Source CD is zip-rel, so ZipArchive::new(File) can latch; scan_jar is the oracle.
            let src_scan = ayzenpack::scan::scan_jar(&src, u64::MAX).unwrap();
            let mut z = ZipArchive::new(File::open(&dest).unwrap()).unwrap();
            assert_eq!(
                z.len(),
                src_scan.entries.len(),
                "{} dest ZipArchive len vs source scan_jar",
                jar.name
            );
            for (i, sc) in src_scan.entries.iter().enumerate() {
                let e = z.by_index(i).unwrap();
                assert_eq!(e.name(), sc.name, "{} dest vs scan_jar name[{i}]", jar.name);
                assert_eq!(
                    e.is_dir(),
                    sc.is_dir,
                    "{} dest vs scan_jar dir[{i}]",
                    jar.name
                );
                assert_eq!(
                    e.crc32(),
                    sc.crc32,
                    "{} dest vs scan_jar crc[{i}]",
                    jar.name
                );
            }
        } else {
            assert_functional_identity(&src, &dest);
        }
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
    assert_eq!(
        cdata_blob, 0,
        "mix must not write cdata_blob on any entry (file or dir, any method)"
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

#[test]
fn corpus_lucene_jackson_source_identity_only_when_every_slot_hits() {
    let Some(corpus) = corpus_dir() else {
        eprintln!(
            "skipping corpus_lucene_jackson_source_identity_only_when_every_slot_hits: AYZENPACK_CORPUS_DIR not set"
        );
        return;
    };
    let names: Vec<String> = artifacts()
        .into_iter()
        .filter_map(|a| a.get("dest")?.as_str().map(str::to_string))
        .filter(|n| n.starts_with("lucene-") || n.starts_with("jackson-"))
        .collect();
    assert!(!names.is_empty(), "lockfile must list lucene/jackson");
    let tmp = tempfile::tempdir().unwrap();
    let inputs: Vec<PathBuf> = names
        .iter()
        .map(|n| {
            let p = corpus.join(n);
            assert!(p.is_file(), "missing {}", p.display());
            p
        })
        .collect();
    let out = tmp.path().join("lj.ayz");
    let restored = tmp.path().join("restored");
    dehydrate(&dehydrate_opts(&out, inputs)).unwrap();
    verify(&out).unwrap();
    rehydrate(&rehydrate_opts(&out, &restored)).unwrap();
    let manifest = list(&out).unwrap();
    for jar in &manifest.jars {
        let mut method8 = 0u64;
        let mut flate2 = 0u64;
        let mut zlib = 0u64;
        let mut stored = 0u64;
        let mut miss = 0u64;
        for e in &jar.entries {
            if e.is_dir || e.method_code != 8 {
                continue;
            }
            method8 += 1;
            match e.cdata_codec.as_deref() {
                Some(c) if c.starts_with("deflate-raw:flate2:") => flate2 += 1,
                Some(c) if c.starts_with("deflate-raw:zlib:") => zlib += 1,
                Some("deflate-raw:stored") => stored += 1,
                Some(_) | None => miss += 1,
            }
        }
        println!(
            "lucene/jackson {} method8={} flate2={} zlib={} stored={} miss={} exact={}",
            jar.name,
            method8,
            flate2,
            zlib,
            stored,
            miss,
            jar.bit_identical_restore()
        );
        if !jar.bit_identical_restore() {
            continue;
        }
        let src = PathBuf::from(&jar.source_path);
        let dest = restored.join(&jar.name);
        let (src_b3, src_sha) = hash_reader(File::open(&src).unwrap()).unwrap();
        let (dest_b3, dest_sha) = hash_reader(File::open(&dest).unwrap()).unwrap();
        assert_eq!(hex_lower(&src_b3), hex_lower(&dest_b3), "{}", jar.name);
        assert_eq!(hex_lower(&src_sha), hex_lower(&dest_sha), "{}", jar.name);
    }
}

/// Two nearby Dataflow fat JARs. Listed + `raw_zip` is a bug, not a fallback.
#[test]
fn corpus_dataflow_pair_forbids_dual_copy() {
    let Some(corpus) = corpus_dir() else {
        eprintln!("skipping corpus_dataflow_pair_forbids_dual_copy: AYZENPACK_CORPUS_DIR not set");
        return;
    };
    let jar4 = corpus.join(DATAFLOW_4_DEST);
    let jar5 = corpus.join(DATAFLOW_5_DEST);
    assert!(
        jar4.is_file(),
        "missing pinned artifact {} (run ci/download-corpus.sh)",
        jar4.display()
    );
    assert!(
        jar5.is_file(),
        "missing pinned artifact {} (run ci/download-corpus.sh)",
        jar5.display()
    );

    let files_4 = jar_file_entry_count(&jar4);
    let files_5 = jar_file_entry_count(&jar5);
    let uncomp_4 = jar_uncompressed_file_bytes(&jar4);
    let uncomp_5 = jar_uncompressed_file_bytes(&jar5);
    assert!(files_4 > 0, "{DATAFLOW_4_DEST} listed no file entries");
    assert!(files_5 > 0, "{DATAFLOW_5_DEST} listed no file entries");

    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("dataflow-pair.ayz");
    let restored = tmp.path().join("restored");
    let mut opts = dehydrate_opts(&out, vec![jar4, jar5]);
    opts.recursive = false;
    let summary = dehydrate(&opts).unwrap();
    assert_eq!(summary.jar_count, 2, "pair must pack both fat JARs");
    verify(&out).unwrap();
    rehydrate(&rehydrate_opts(&out, &restored)).unwrap();

    let manifest = list(&out).unwrap();
    assert_eq!(manifest.jars.len(), 2);
    for jar in &manifest.jars {
        assert!(
            jar.raw_zip_blob.is_none(),
            "{} listed file entries so raw_zip_blob is a bug, not a fallback",
            jar.name
        );
        for e in &jar.entries {
            assert!(
                e.cdata_blob.is_none(),
                "{}!{} must not write cdata_blob",
                jar.name,
                e.name
            );
        }
        for (i, nested) in jar.nestedindexes.iter().enumerate() {
            for e in &nested.entries {
                assert!(
                    e.cdata_blob.is_none(),
                    "{} nestedindexes[{i}]!{} must not write cdata_blob",
                    jar.name,
                    e.name
                );
            }
        }
    }

    let (unique_content_count, unique_content_bytes) = unique_content_blobs(&manifest);
    let file_entry_sum = files_4 + files_5;
    let uncomp_sum = uncomp_4 + uncomp_5;
    println!(
        "dataflow pair files_4={} files_5={} file_entry_sum={} uncomp_4={} uncomp_5={} uncomp_sum={} unique_content_count={} unique_content_bytes={} unique_blob_count={} bytes_unique_blobs={} bytes_in_jars={} output_len={}",
        files_4,
        files_5,
        file_entry_sum,
        uncomp_4,
        uncomp_5,
        uncomp_sum,
        unique_content_count,
        unique_content_bytes,
        summary.unique_blob_count,
        summary.bytes_unique_blobs,
        summary.bytes_in_jars,
        summary.output_len
    );
    assert!(
        summary.bytes_unique_blobs < unique_content_bytes.saturating_mul(2),
        "unique blob bytes {} must stay in the same league as unique uncompressed content {} (one CAS; Raw dual-copy fails this)",
        summary.bytes_unique_blobs,
        unique_content_bytes
    );
    assert!(
        unique_content_count < file_entry_sum,
        "unique content blob count {} must be strictly less than file-entry sum {} (BOOT-INF/lib CAS overlap)",
        unique_content_count,
        file_entry_sum
    );
    assert!(
        summary.bytes_unique_blobs < uncomp_sum,
        "bytes_unique_blobs {} must be strictly less than uncompressed file-entry sum {} (BOOT-INF/lib CAS overlap)",
        summary.bytes_unique_blobs,
        uncomp_sum
    );
}
