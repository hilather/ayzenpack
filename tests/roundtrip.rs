//! Dehydrate → rehydrate: new packs are bit-identical; old archives stay functional.

#[path = "fixtures.rs"]
mod fixtures;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use ayzenpack::error::AyzenpackError;
use ayzenpack::format::{read_ayz_file, write_ayz_file, Record, BUF_WRITER_CAP, TRAILER_LEN};
use ayzenpack::hashutil::blake3_bytes;
use ayzenpack::manifest::Manifest;
use ayzenpack::{dehydrate, rehydrate, verify, DehydrateOptions, RehydrateOptions};
use fixtures::{
    spring_boot_launch_script, write_data_descriptor_zip, write_jar, write_jar_entries,
    write_jar_with_comment, write_non_utf8_name_zip, write_padded_locals_zip,
    write_signed_looking_jar, write_stored_jar_dos_zero, write_stored_zip, write_wrapped_jar,
    write_wrapped_jar_adjusted, write_wrapped_zip64_jar, zip64_jar_bytes, JarEntry,
    SPRING_LAUNCHER,
};
use zip::{CompressionMethod, DateTime, ZipArchive};

const EMPTY_BLAKE3: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";

fn opts(output: &Path, inputs: Vec<std::path::PathBuf>) -> DehydrateOptions {
    DehydrateOptions {
        output: output.to_path_buf(),
        inputs,
        ..DehydrateOptions::default()
    }
}

fn fill_incompressible(len: usize) -> Vec<u8> {
    let mut data = vec![0u8; len];
    let mut x: u32 = 0x1234_5678;
    for b in &mut data {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *b = (x >> 16) as u8;
    }
    data
}

fn blob_records(records: &[Record]) -> Vec<&Record> {
    records
        .iter()
        .filter(|r| matches!(r, Record::Blob { .. }))
        .collect()
}

fn read_archive(path: &Path) -> (ayzenpack::FileHeader, ayzenpack::Trailer, Vec<Record>) {
    let mut f = File::open(path).unwrap();
    read_ayz_file(&mut f).unwrap()
}

fn manifest_from_records(records: &[Record]) -> Manifest {
    let json = records
        .iter()
        .find_map(|r| match r {
            Record::Manifest { json } => Some(json.as_slice()),
            _ => None,
        })
        .expect("MANIFEST record");
    serde_json::from_slice(json).unwrap()
}

#[test]
fn dehydrate_shared_hello_unique_blob_count_is_3() {
    // Guards hashing compressed ZIP bytes or exploding each JAR into its own blob set.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("A.jar");
    let b = dir.path().join("B.jar");
    write_jar(&a, &[("AAA.txt", b"AAA"), ("HELLO.txt", b"HELLO")]);
    write_jar(&b, &[("BBB.txt", b"BBB"), ("HELLO.txt", b"HELLO")]);
    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![a, b])).unwrap();
    assert_eq!(summary.jar_count, 2);
    assert_eq!(summary.file_entry_count, 4);

    let (_header, trailer, records) = read_archive(&out);
    assert_eq!(trailer.blob_count, summary.unique_blob_count);
    assert_eq!(
        blob_records(&records).len(),
        summary.unique_blob_count as usize
    );
    let m = manifest_from_records(&records);
    assert_eq!(content_blob_ids(&m).len(), 3);
    assert!(
        summary.unique_blob_count >= 3,
        "content blobs plus exact extras, got {}",
        summary.unique_blob_count
    );
    assert_eq!(m.stats.unique_blob_count, summary.unique_blob_count);
}

#[test]
fn dehydrate_does_not_write_duplicate_blob_records() {
    // Guards emitting a second BLOB record for the same BLAKE3.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("A.jar");
    let b = dir.path().join("B.jar");
    write_jar(&a, &[("AAA.txt", b"AAA"), ("HELLO.txt", b"HELLO")]);
    write_jar(&b, &[("BBB.txt", b"BBB"), ("HELLO.txt", b"HELLO")]);
    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![a, b])).unwrap();

    let (_header, trailer, records) = read_archive(&out);
    let blobs = blob_records(&records);
    assert_eq!(blobs.len(), summary.unique_blob_count as usize);
    assert_eq!(blobs.len() as u64, trailer.blob_count);

    let mut hashes = Vec::new();
    for rec in blobs {
        match rec {
            Record::Blob { hash, .. } => hashes.push(*hash),
            _ => unreachable!(),
        }
    }
    hashes.sort();
    let mut unique = hashes.clone();
    unique.dedup();
    assert_eq!(hashes, unique, "BLOB records must not repeat a hash");
}

#[test]
fn dehydrate_empty_file_writes_one_zero_blob() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("empty.jar");
    write_jar(&jar, &[("empty.dat", b"")]);
    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![jar])).unwrap();
    assert_eq!(summary.file_entry_count, 1);

    let (_header, trailer, records) = read_archive(&out);
    assert_eq!(trailer.blob_count, summary.unique_blob_count);
    match &records[0] {
        Record::Blob { hash, data } => {
            assert!(data.is_empty(), "empty entry must write a size-0 BLOB");
            assert_eq!(hash, &blake3_bytes(b""));
        }
        other => panic!("expected BLOB first, got {other:?}"),
    }
    let m = manifest_from_records(&records);
    assert_eq!(m.blobs[0].size, 0);
    assert_eq!(m.blobs[0].blake3, EMPTY_BLAKE3);
    assert_eq!(content_blob_ids(&m).len(), 1);
    assert!(
        m.blobs
            .iter()
            .any(|b| b.blake3 == EMPTY_BLAKE3 && b.size == 0),
        "empty uncompressed entry must remain in the catalog"
    );
}

#[test]
fn dehydrate_sort_inputs_is_byte_identical_twice() {
    // Guards created_unix wall-clock and unsorted input order leaking into the archive.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.jar");
    let b = dir.path().join("b.jar");
    write_jar(&a, &[("a.txt", b"aaaa")]);
    write_jar(&b, &[("b.txt", b"bbbb")]);

    let out1 = dir.path().join("one.ayz");
    let out2 = dir.path().join("two.ayz");
    let mut o1 = opts(&out1, vec![b.clone(), a.clone()]);
    o1.sort_inputs = true;
    let mut o2 = opts(&out2, vec![a, b]);
    o2.sort_inputs = true;

    dehydrate(&o1).unwrap();
    dehydrate(&o2).unwrap();
    assert_eq!(
        fs::read(&out1).unwrap(),
        fs::read(&out2).unwrap(),
        "sort_inputs must be byte-identical (created_unix=0)"
    );

    let (header, _trailer, _records) = read_archive(&out1);
    assert_eq!(header.created_unix, 0);
}

#[test]
fn dehydrate_dry_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("a.jar");
    write_jar(&jar, &[("x.txt", b"payload")]);
    let out = dir.path().join("out.ayz");
    assert!(!out.exists());

    let mut o = opts(&out, vec![jar.clone()]);
    o.dry_run = true;
    let summary = dehydrate(&o).unwrap();
    assert!(!out.exists(), "dry-run must not create the output file");
    assert_eq!(summary.output_len, 0);
    assert_eq!(summary.jar_count, 1);
    assert!(summary.unique_blob_count >= 1);

    fs::write(&out, b"keep-me").unwrap();
    let summary2 = dehydrate(&o).unwrap();
    assert_eq!(fs::read(&out).unwrap(), b"keep-me");
    assert_eq!(summary2.output_len, 0);
}

#[test]
fn dehydrate_output_smaller_than_sum_when_duplicated() {
    // Unique uncompressed bytes must shrink; archive vs tiny ZIP can be noisy.
    let dir = tempfile::tempdir().unwrap();
    let payload = vec![0x5a; 8 * 1024];
    let mut inputs = Vec::new();
    for i in 0..5 {
        let p = dir.path().join(format!("c{i}.jar"));
        write_jar(&p, &[("dup.bin", payload.as_slice())]);
        inputs.push(p);
    }
    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, inputs)).unwrap();
    assert_eq!(
        content_blob_ids(&manifest_from_records(&read_archive(&out).2)).len(),
        1
    );
    assert!(
        summary.bytes_unique_blobs < summary.bytes_uncompressed_entries * 2,
        "unique {} uncompressed {}",
        summary.bytes_unique_blobs,
        summary.bytes_uncompressed_entries
    );
}

#[test]
fn dehydrate_payload_bytes_measured_before_trailer_write() {
    // Guards writing the trailer then deriving payload_bytes from file_len-64,
    // and writing the trailer to a raw File while BufWriter still holds zstd bytes.
    let dir = tempfile::tempdir().unwrap();
    let data = fill_incompressible(512 * 1024);
    let jar = dir.path().join("big.jar");
    write_jar(&jar, &[("blob.bin", data.as_slice())]);
    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![jar])).unwrap();

    let (header, trailer, records) = read_archive(&out);
    assert_eq!(header.zstd_level, 3);
    assert!(
        trailer.payload_bytes > BUF_WRITER_CAP as u64,
        "zstd payload must exceed BufWriter cap so a dirty buffer would matter, got {}",
        trailer.payload_bytes
    );
    let header_total = 12 + u64::from(trailer.header_len);
    let file_len = fs::metadata(&out).unwrap().len();
    assert_eq!(file_len, header_total + trailer.payload_bytes + TRAILER_LEN);
    assert_eq!(summary.output_len, file_len);
    assert!(matches!(&records[0], Record::Blob { data: d, .. } if d.len() == data.len()));
}

#[test]
fn dehydrate_overwrites_existing_output() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("a.jar");
    write_jar(&jar, &[("x.txt", b"hello")]);
    let out = dir.path().join("out.ayz");
    fs::write(&out, b"not-an-ayzenpack-file").unwrap();
    let summary = dehydrate(&opts(&out, vec![jar])).unwrap();
    assert!(summary.output_len > 0);
    let bytes = fs::read(&out).unwrap();
    assert_eq!(&bytes[..4], b"AYZP");
    assert_ne!(&bytes, b"not-an-ayzenpack-file");
    let (_header, trailer, _records) = read_archive(&out);
    assert_eq!(trailer.blob_count, summary.unique_blob_count);
}

#[test]
fn tiny_overlap_20x_10kib_unique_blobs_eq_one_copy() {
    // Always-on overlap smoke: 20 copies of one ~10 KiB JAR share one copy's blobs.
    let dir = tempfile::tempdir().unwrap();
    let chunk_a = vec![0x11; 6 * 1024];
    let chunk_b = vec![0x22; 4 * 1024];
    let proto = dir.path().join("proto.jar");
    write_jar(
        &proto,
        &[("a.bin", chunk_a.as_slice()), ("b.bin", chunk_b.as_slice())],
    );
    let one_copy_files = 2u64;
    let mut inputs = Vec::new();
    for i in 0..20 {
        let p = dir.path().join(format!("copy{i}.jar"));
        fs::copy(&proto, &p).unwrap();
        inputs.push(p);
    }
    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, inputs)).unwrap();
    assert_eq!(summary.jar_count, 20);
    assert_eq!(summary.file_entry_count, 20 * one_copy_files);
    let m = manifest_from_records(&read_archive(&out).2);
    assert_eq!(
        content_blob_ids(&m).len() as u64,
        one_copy_files,
        "content blobs must equal one copy's file-entry count, not the sum"
    );
    let (_header, trailer, records) = read_archive(&out);
    assert_eq!(trailer.blob_count, summary.unique_blob_count);
    assert_eq!(
        blob_records(&records).len() as u64,
        summary.unique_blob_count
    );
}

fn rehydrate_opts(input: &Path, dir: &Path) -> RehydrateOptions {
    RehydrateOptions {
        input: input.to_path_buf(),
        dir: dir.to_path_buf(),
        ..RehydrateOptions::default()
    }
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

fn content_blob_ids(m: &Manifest) -> BTreeSet<String> {
    m.jars
        .iter()
        .flat_map(|j| j.entries.iter().filter_map(|e| e.blob.clone()))
        .collect()
}

fn assert_bit_identical(src: &Path, dest: &Path) {
    let a = fs::read(src).unwrap();
    let b = fs::read(dest).unwrap();
    assert_eq!(a.len(), b.len(), "size {} vs {}", a.len(), b.len());
    assert_eq!(a, b, "restored bytes must match source ({})", src.display());
}

fn assert_functional_identity(src: &Path, dest: &Path) {
    // Functional identity: uncompressed bytes, Unicode names, CRC, CD order.
    // Do not compare full JAR bytes (deflate bitstream is not preserved).
    let src_map = entry_map(src);
    let dest_map = entry_map(dest);
    assert_eq!(src_map, dest_map);
    assert_eq!(entry_crcs(src), entry_crcs(dest));
    assert_eq!(cd_entries(src), cd_entries(dest));
    let dest_crcs = entry_crcs(dest);
    for (name, bytes) in &src_map {
        assert_eq!(crc32fast::hash(bytes), *dest_crcs.get(name).unwrap());
    }
}

#[test]
fn roundtrip_shared_class_entry_maps_and_crc_equal() {
    // Guards hashing compressed ZIP bytes, exploding per-JAR blob sets, or bit-identity compares.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("A.jar");
    let b = dir.path().join("B.jar");
    write_jar(&a, &[("AAA.txt", b"AAA"), ("HELLO.txt", b"HELLO")]);
    write_jar(&b, &[("BBB.txt", b"BBB"), ("HELLO.txt", b"HELLO")]);
    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![a.clone(), b.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    assert_eq!(content_blob_ids(&m).len(), 3);
    assert!(summary.unique_blob_count >= 3);

    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    assert_bit_identical(&a, &dest.join("A.jar"));
    assert_bit_identical(&b, &dest.join("B.jar"));
    assert_functional_identity(&a, &dest.join("A.jar"));
    assert_functional_identity(&b, &dest.join("B.jar"));
}

#[test]
fn roundtrip_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("empty.jar");
    write_jar(&jar, &[("empty.dat", b"")]);
    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    assert!(summary.unique_blob_count >= 1);

    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("empty.jar");
    assert_bit_identical(&jar, &restored);
    assert_functional_identity(&jar, &restored);
    let map = entry_map(&restored);
    assert_eq!(map.get("empty.dat").map(|v| v.len()), Some(0));
}

#[test]
fn roundtrip_directories_explicit_only() {
    // add_directory restores is_dir(); do not invent dir entries the source omitted.
    let dir = tempfile::tempdir().unwrap();
    let with_dirs = dir.path().join("with_dirs.jar");
    write_jar_entries(
        &with_dirs,
        &[
            JarEntry::Dir {
                name: "com/example/",
            },
            JarEntry::File {
                name: "com/example/A.class",
                data: b"class-bytes",
                method: CompressionMethod::Deflated,
            },
        ],
    );
    let no_dirs = dir.path().join("no_dirs.jar");
    write_jar(&no_dirs, &[("com/example/A.class", b"class-bytes")]);

    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![with_dirs.clone(), no_dirs.clone()])).unwrap();
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();

    let restored_with = dest.join("with_dirs.jar");
    assert_bit_identical(&with_dirs, &restored_with);
    assert_functional_identity(&with_dirs, &restored_with);
    let with_cd = cd_entries(&restored_with);
    assert!(
        with_cd
            .iter()
            .any(|(n, is_dir)| n == "com/example/" && *is_dir),
        "explicit dir must survive add_directory, got {with_cd:?}"
    );
    let mut z = ZipArchive::new(File::open(&restored_with).unwrap()).unwrap();
    let mut saw_dir = false;
    for i in 0..z.len() {
        let e = z.by_index(i).unwrap();
        if e.name() == "com/example/" {
            assert!(
                e.is_dir(),
                "ZipArchive::by_index[].is_dir() after add_directory"
            );
            saw_dir = true;
        }
    }
    assert!(saw_dir);

    let restored_no = dest.join("no_dirs.jar");
    assert_bit_identical(&no_dirs, &restored_no);
    assert_functional_identity(&no_dirs, &restored_no);
    let no_cd = cd_entries(&restored_no);
    assert_eq!(no_cd, vec![("com/example/A.class".into(), false)]);
    assert!(
        !no_cd.iter().any(|(_, is_dir)| *is_dir),
        "must not invent directory entries: {no_cd:?}"
    );
}

#[test]
fn roundtrip_utf8_names() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("utf8.jar");
    write_jar(&jar, &[("res/名前.txt", b"hello")]);
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("utf8.jar");
    assert_bit_identical(&jar, &restored);
    assert_functional_identity(&jar, &restored);
    assert!(entry_map(&restored).contains_key("res/名前.txt"));
}

#[test]
fn roundtrip_dos_time_zero_zero_does_not_panic() {
    // Guards from_msdos_unchecked and aborting on DOS 0,0 (common in JARs).
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("zero.jar");
    write_stored_jar_dos_zero(&jar, &[("a.txt", b"payload"), ("b.txt", b"more")]);
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("zero.jar");
    assert_bit_identical(&jar, &restored);
    assert_eq!(entry_map(&jar), entry_map(&restored));
    assert_eq!(
        cd_entries(&jar)
            .into_iter()
            .map(|(n, _)| n)
            .collect::<Vec<_>>(),
        cd_entries(&restored)
            .into_iter()
            .map(|(n, _)| n)
            .collect::<Vec<_>>()
    );

    // Exact restore keeps DOS 0,0. zip crate may surface that as None.
    let mut z = ZipArchive::new(File::open(&restored).unwrap()).unwrap();
    for i in 0..z.len() {
        let e = z.by_index(i).unwrap();
        match e.last_modified() {
            None => {}
            Some(dt) => {
                assert_eq!(dt, DateTime::default());
                assert_eq!(dt.year(), 1980);
                assert_eq!(dt.month(), 1);
                assert_eq!(dt.day(), 1);
            }
        }
    }
}

#[test]
fn roundtrip_store_source_is_bit_identical() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("stored.jar");
    write_jar_entries(
        &jar,
        &[JarEntry::File {
            name: "payload.bin",
            data: b"aaaaaaaaaaaaaaaa stored-source-bytes",
            method: CompressionMethod::Stored,
        }],
    );
    let mut src_z = ZipArchive::new(File::open(&jar).unwrap()).unwrap();
    assert_eq!(
        src_z.by_index(0).unwrap().compression(),
        CompressionMethod::Stored
    );

    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("stored.jar");
    assert_bit_identical(&jar, &restored);
    let mut out_z = ZipArchive::new(File::open(&restored).unwrap()).unwrap();
    assert_eq!(
        out_z.by_index(0).unwrap().compression(),
        CompressionMethod::Stored
    );
}

#[test]
fn rehydrate_without_overwrite_fails_if_exists() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("a.jar");
    write_jar(&jar, &[("x.txt", b"hello")]);
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar])).unwrap();

    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("a.jar");
    assert!(restored.is_file());
    fs::write(&restored, b"do-not-clobber").unwrap();

    let err = rehydrate(&rehydrate_opts(&out, &dest)).unwrap_err();
    assert!(
        matches!(err, AyzenpackError::Usage(_)),
        "existing dest without --overwrite must fail, got {err:?}"
    );
    assert_eq!(fs::read(&restored).unwrap(), b"do-not-clobber");

    let mut ok = rehydrate_opts(&out, &dest);
    ok.overwrite = true;
    rehydrate(&ok).unwrap();
    assert_eq!(entry_map(&restored).get("x.txt").unwrap(), b"hello");
}

#[test]
fn rehydrate_reject_dotdot_jar_name() {
    // Crafted manifest: zip-slip jars[].name must not write outside -d.
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("a.jar");
    write_jar(&jar, &[("x.txt", b"hello")]);
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar])).unwrap();

    let mut f = File::open(&out).unwrap();
    let (header, _trailer, records) = read_ayz_file(&mut f).unwrap();
    let mut new_records = Vec::new();
    let mut jar_count = 0u64;
    for rec in records {
        match rec {
            Record::Manifest { json } => {
                let mut m: Manifest = serde_json::from_slice(&json).unwrap();
                jar_count = m.jars.len() as u64;
                m.jars[0].name = "../x.jar".into();
                new_records.push(Record::Manifest {
                    json: serde_json::to_vec(&m).unwrap(),
                });
            }
            other => new_records.push(other),
        }
    }
    let crafted = dir.path().join("crafted.ayz");
    let mut w = File::create(&crafted).unwrap();
    write_ayz_file(&mut w, &header, &new_records, jar_count).unwrap();

    let dest = dir.path().join("restored");
    fs::create_dir_all(&dest).unwrap();
    let err = rehydrate(&rehydrate_opts(&crafted, &dest)).unwrap_err();
    assert!(
        matches!(err, AyzenpackError::UnsafePath(ref s) if s == "../x.jar"),
        "got {err:?}"
    );
    assert!(!dir.path().join("x.jar").exists());
    assert!(!dest.join("x.jar").exists());
}

/// CD-order file payloads. Unlike `entry_map`, this keeps duplicate names.
fn cd_payloads(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut z = ZipArchive::new(File::open(path).unwrap()).unwrap();
    let mut out = Vec::new();
    for i in 0..z.len() {
        let mut e = z.by_index(i).unwrap();
        if e.is_dir() {
            continue;
        }
        let name = e.name().to_string();
        let mut buf = Vec::new();
        e.read_to_end(&mut buf).unwrap();
        out.push((name, buf));
    }
    out
}

fn count_cd_names(bytes: &[u8], name: &str) -> usize {
    let name_b = name.as_bytes();
    let mut n = 0usize;
    let mut i = 0usize;
    while i + 46 + name_b.len() <= bytes.len() {
        if &bytes[i..i + 4] == b"PK\x01\x02" {
            let name_len = u16::from_le_bytes([bytes[i + 28], bytes[i + 29]]) as usize;
            if name_len == name_b.len() && &bytes[i + 46..i + 46 + name_len] == name_b {
                n += 1;
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    n
}

#[test]
fn many_small_200_files_two_jars_dedup_to_200_blobs() {
    // Two identical 200-entry JARs must share one copy's blobs, not 400.
    let dir = tempfile::tempdir().unwrap();
    let owned: Vec<(String, Vec<u8>)> = (0u16..200)
        .map(|i| (format!("f{i:03}.dat"), vec![i as u8; 32]))
        .collect();
    let files: Vec<(&str, &[u8])> = owned
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_slice()))
        .collect();
    let a = dir.path().join("a.jar");
    let b = dir.path().join("b.jar");
    write_jar(&a, &files);
    fs::copy(&a, &b).unwrap();
    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![a.clone(), b.clone()])).unwrap();
    assert_eq!(summary.jar_count, 2);
    assert_eq!(summary.file_entry_count, 400);
    let packed = manifest_from_records(&read_archive(&out).2);
    assert_eq!(
        content_blob_ids(&packed).len(),
        200,
        "two identical JARs must dedup to 200 content blobs, not 400"
    );
    assert_eq!(
        content_blob_ids(&packed)
            .iter()
            .map(|h| packed
                .blobs
                .iter()
                .find(|b| b.blake3 == *h)
                .map(|b| b.size)
                .unwrap())
            .sum::<u64>(),
        summary.bytes_uncompressed_entries / 2
    );
    assert!(
        summary.bytes_unique_blobs < summary.bytes_in_jars,
        "unique blob bytes {} must be smaller than two source JARs {}",
        summary.bytes_unique_blobs,
        summary.bytes_in_jars
    );

    let (_header, trailer, records) = read_archive(&out);
    assert_eq!(trailer.blob_count, summary.unique_blob_count);
    assert_eq!(
        blob_records(&records).len() as u64,
        summary.unique_blob_count
    );

    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    assert_bit_identical(&a, &dest.join("a.jar"));
    assert_bit_identical(&b, &dest.join("b.jar"));
    assert_functional_identity(&a, &dest.join("a.jar"));
    assert_functional_identity(&b, &dest.join("b.jar"));
}

#[test]
fn duplicate_entry_names_in_one_jar_all_restored() {
    // Two CD entries share a name. Restore every entry ZipArchive yields, in order.
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
    assert_eq!(
        count_cd_names(&fs::read(&jar).unwrap(), "dup.txt"),
        2,
        "fixture must contain two central-directory records named dup.txt"
    );

    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("dup.jar");

    let src = cd_payloads(&jar);
    let dest_entries = cd_payloads(&restored);
    assert!(!src.is_empty(), "scanner must yield at least one dup.txt");
    assert!(
        src.iter().all(|(n, _)| n == "dup.txt"),
        "unexpected names: {src:?}"
    );
    match src.len() {
        2 => {
            assert_eq!(src[0].1, first);
            assert_eq!(src[1].1, second);
        }
        1 => {
            // zip 2.x indexes CD by name (IndexMap last-wins).
            assert_eq!(src[0].1, second);
        }
        n => panic!("unexpected scanner-visible entry count {n}"),
    }
    assert_eq!(
        dest_entries, src,
        "all scanner-visible duplicate-name entries must be restored in CD order"
    );
}

#[test]
fn nested_jar_not_exploded() {
    // Nested JARs are opaque blobs; inner classes must not become their own entries.
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

    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![outer.clone()])).unwrap();
    assert_eq!(summary.jar_count, 1);
    assert_eq!(summary.file_entry_count, 2);
    let m = manifest_from_records(&read_archive(&out).2);
    assert_eq!(
        content_blob_ids(&m).len(),
        2,
        "inner.jar bytes are one blob; do not explode Inner.class"
    );

    let m = manifest_from_records(&read_archive(&out).2);
    let names: Vec<&str> = m.jars[0].entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["lib/inner.jar", "App.class"]);
    assert!(
        !m.jars[0].entries.iter().any(|e| e.name.contains("Inner")),
        "nested jar must stay opaque, got {names:?}"
    );

    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("outer.jar");
    assert_bit_identical(&outer, &restored);
    assert_functional_identity(&outer, &restored);
    let map = entry_map(&restored);
    assert_eq!(
        map.get("lib/inner.jar").map(Vec::as_slice),
        Some(inner_bytes.as_slice())
    );
    assert!(!map.contains_key("com/Inner.class"));
    assert_eq!(map.len(), 2);
}

#[test]
fn sort_inputs_jobs_1_eq_jobs_n_byte_identical() {
    // Guards hash-completion order leaking into BLOB records or created_unix.
    let dir = tempfile::tempdir().unwrap();
    let mut inputs = Vec::new();
    let shared = b"SHARED-PAYLOAD";
    let other = b"OTHER-PAYLOAD";
    for i in 0..8 {
        let p = dir.path().join(format!("j{i}.jar"));
        let unique = format!("unique-{i}-payload");
        write_jar(
            &p,
            &[
                ("shared.txt", shared.as_slice()),
                ("unique.txt", unique.as_bytes()),
                ("other.txt", other.as_slice()),
            ],
        );
        inputs.push(p);
    }

    let out1 = dir.path().join("j1.ayz");
    let outn = dir.path().join("jn.ayz");
    let mut o1 = opts(&out1, inputs.clone());
    o1.sort_inputs = true;
    o1.jobs = 1;
    o1.quiet = true;
    let mut on = opts(&outn, inputs);
    on.sort_inputs = true;
    on.jobs = 4;
    on.quiet = true;
    dehydrate(&o1).unwrap();
    dehydrate(&on).unwrap();
    assert_eq!(
        fs::read(&out1).unwrap(),
        fs::read(&outn).unwrap(),
        "sort_inputs archives must be byte-identical at jobs=1 and jobs=N"
    );
}

#[test]
fn first_seen_blob_order_matches_scan_order_with_jobs() {
    // Guards writing BLOBs in hash-completion order instead of first-seen scan order.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.jar");
    let b = dir.path().join("b.jar");
    write_jar_entries(
        &a,
        &[
            JarEntry::File {
                name: "1.txt",
                data: b"alpha-payload",
                method: CompressionMethod::Deflated,
            },
            JarEntry::Dir { name: "d/" },
            JarEntry::File {
                name: "2.txt",
                data: b"bravo-payload",
                method: CompressionMethod::Deflated,
            },
            JarEntry::File {
                name: "3.txt",
                data: b"charlie-payload",
                method: CompressionMethod::Deflated,
            },
        ],
    );
    write_jar(
        &b,
        &[
            ("2.txt", b"bravo-payload"),
            ("4.txt", b"delta-payload"),
            ("1.txt", b"alpha-payload"),
        ],
    );

    let out = dir.path().join("out.ayz");
    let mut o = opts(&out, vec![a, b]);
    o.jobs = 4;
    o.sort_inputs = true;
    o.quiet = true;
    dehydrate(&o).unwrap();

    let expected = [
        blake3_bytes(b"alpha-payload"),
        blake3_bytes(b"bravo-payload"),
        blake3_bytes(b"charlie-payload"),
        blake3_bytes(b"delta-payload"),
    ];
    let (_h, _t, records) = read_archive(&out);
    let got: Vec<[u8; 32]> = records
        .iter()
        .filter_map(|r| match r {
            Record::Blob { hash, .. } => Some(*hash),
            _ => None,
        })
        .collect();
    let got_content: Vec<[u8; 32]> = got
        .iter()
        .copied()
        .filter(|h| expected.contains(h))
        .collect();
    assert_eq!(
        got_content, expected,
        "content BLOB order must match first-seen scan (exact extras may interleave)"
    );

    let m = manifest_from_records(&records);
    let hexes: Vec<String> = m.blobs.iter().map(|b| b.blake3.clone()).collect();
    let expected_hex: Vec<String> = expected
        .iter()
        .map(|h| ayzenpack::hashutil::hex_lower(h))
        .collect();
    let hexes_content: Vec<String> = hexes
        .into_iter()
        .filter(|h| expected_hex.contains(h))
        .collect();
    assert_eq!(
        hexes_content, expected_hex,
        "manifest blobs[] content hashes must match first-seen"
    );
}

#[test]
fn roundtrip_bash_prefixed_executable_jar() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("app.jar");
    write_wrapped_jar(
        &jar,
        SPRING_LAUNCHER,
        &[
            ("App.class", b"class-bytes"),
            ("application.properties", b"x=1"),
        ],
    );
    let src_bytes = fs::read(&jar).unwrap();
    assert!(src_bytes.starts_with(SPRING_LAUNCHER));

    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    assert!(summary.unique_blob_count >= 3, "prefix + two file entries");

    let m = manifest_from_records(&read_archive(&out).2);
    assert!(m.jars[0].prefix_blob.is_some());
    assert_eq!(m.jars[0].prefix_size, Some(SPRING_LAUNCHER.len() as u64));
    let prefix_hex = m.jars[0].prefix_blob.as_deref().unwrap();
    let prefix_blob = m
        .blobs
        .iter()
        .find(|b| b.blake3 == prefix_hex)
        .expect("prefix blob in catalog");
    assert_eq!(prefix_blob.size, SPRING_LAUNCHER.len() as u64);
    assert_eq!(prefix_blob.ref_count, 1);

    verify(&out).unwrap();
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("app.jar");
    let got = fs::read(&restored).unwrap();
    assert_eq!(
        &got[..SPRING_LAUNCHER.len()],
        SPRING_LAUNCHER,
        "restored file must start with the exact prefix"
    );
    assert_bit_identical(&jar, &restored);
    assert_functional_identity(&jar, &restored);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&restored).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "wrapped jar must be restored executable");
    }
}

#[test]
fn roundtrip_zip_a_adjusted_executable_jar() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("app.jar");
    write_wrapped_jar_adjusted(
        &jar,
        SPRING_LAUNCHER,
        &[
            ("App.class", b"class-bytes"),
            ("application.properties", b"x=1"),
        ],
    );
    let src_bytes = fs::read(&jar).unwrap();
    assert!(src_bytes.starts_with(SPRING_LAUNCHER));

    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![jar.clone()]))
        .expect("zip -A adjusted executable JAR must not be NotZip");
    assert!(summary.unique_blob_count >= 3, "prefix + two file entries");

    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("app.jar");
    let got = fs::read(&restored).unwrap();
    assert_eq!(
        &got[..SPRING_LAUNCHER.len()],
        SPRING_LAUNCHER,
        "restored file must start with the exact prefix"
    );
    assert_bit_identical(&jar, &restored);
    assert_functional_identity(&jar, &restored);
}

#[test]
fn roundtrip_official_launch_script_executable_jar() {
    let launcher = spring_boot_launch_script();
    assert!(launcher.len() > 200);
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("app.jar");
    write_wrapped_jar(
        &jar,
        launcher,
        &[
            ("App.class", b"class-bytes"),
            ("application.properties", b"x=1"),
        ],
    );

    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("app.jar");
    assert_eq!(&fs::read(&restored).unwrap()[..launcher.len()], launcher);
    assert_bit_identical(&jar, &restored);
    assert_functional_identity(&jar, &restored);
}

#[test]
fn roundtrip_official_script_plus_zip64_nested_lib() {
    let launcher = spring_boot_launch_script();
    let inner = zip64_jar_bytes(&[("com/Dep.class", b"dep-bytes")]);
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("app.jar");
    write_wrapped_zip64_jar(
        &jar,
        launcher,
        &[
            ("BOOT-INF/lib/dep.jar", inner.as_slice()),
            ("App.class", b"class-bytes"),
        ],
    );

    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()]))
        .expect("official launch.script + Zip64 fat JAR must not be NotZip");
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("app.jar");
    assert_eq!(&fs::read(&restored).unwrap()[..launcher.len()], launcher);
    assert_bit_identical(&jar, &restored);
    assert_functional_identity(&jar, &restored);
}

#[test]
fn mixed_regular_and_spring_pack_trailer_is_ayzptlr1_and_rehydrates() {
    // Field mix: regular JARs + official launch.script (stored/deflated, Zip64, zip -A).
    let launcher = spring_boot_launch_script();
    assert!(
        launcher.starts_with(b"#!/bin/bash"),
        "official launch.script must be a bash prefix"
    );
    let dir = tempfile::tempdir().unwrap();

    let regular_a = dir.path().join("lib-a.jar");
    let regular_b = dir.path().join("lib-b.jar");
    write_jar(
        &regular_a,
        &[("a.txt", b"regular-a"), ("shared.txt", b"SHARED")],
    );
    write_jar(
        &regular_b,
        &[("b.txt", b"regular-b"), ("shared.txt", b"SHARED")],
    );

    let mixed_zip = dir.path().join("mixed-inner.zip");
    write_jar_entries(
        &mixed_zip,
        &[
            JarEntry::File {
                name: "BOOT-INF/classes/stored.txt",
                data: b"store-me",
                method: CompressionMethod::Stored,
            },
            JarEntry::File {
                name: "App.class",
                data: b"class-bytes-deflate",
                method: CompressionMethod::Deflated,
            },
        ],
    );
    let official_mixed = dir.path().join("official-mixed.jar");
    let mut mixed_bytes = launcher.to_vec();
    mixed_bytes.extend(fs::read(&mixed_zip).unwrap());
    fs::write(&official_mixed, mixed_bytes).unwrap();
    assert!(fs::read(&official_mixed)
        .unwrap()
        .starts_with(b"#!/bin/bash"));

    let zip64 = dir.path().join("official-zip64.jar");
    write_wrapped_zip64_jar(
        &zip64,
        launcher,
        &[
            ("BOOT-INF/lib/dep.jar", b"dep-bytes"),
            ("App.class", b"zip64-app"),
        ],
    );

    let adjusted = dir.path().join("official-zip-a.jar");
    write_wrapped_jar_adjusted(
        &adjusted,
        launcher,
        &[
            ("App.class", b"zip-a-app"),
            ("application.properties", b"x=1"),
        ],
    );

    let out = dir.path().join("all.ayz");
    let inputs = vec![
        regular_a.clone(),
        regular_b.clone(),
        official_mixed.clone(),
        zip64.clone(),
        adjusted.clone(),
    ];
    dehydrate(&opts(&out, inputs)).expect("mixed regular+spring dehydrate");
    assert!(
        !dir.path().join("all.ayz.tmp").exists(),
        "successful dehydrate must not leave all.ayz.tmp"
    );

    let bytes = fs::read(&out).unwrap();
    assert!(
        bytes.len() >= 64,
        "all.ayz too short for a trailer: {}",
        bytes.len()
    );
    eprintln!(
        "all.ayz first 16: {:02x?} last 8: {:02x?} trailer magic: {:?}",
        &bytes[..16.min(bytes.len())],
        &bytes[bytes.len().saturating_sub(8)..],
        std::str::from_utf8(&bytes[bytes.len() - 64..bytes.len() - 56])
    );
    assert_eq!(&bytes[..4], b"AYZP", "first 16: {:02x?}", &bytes[..16]);
    assert_eq!(
        &bytes[bytes.len() - 64..bytes.len() - 56],
        b"AYZPTLR1",
        "last 64 must start with AYZPTLR1; last 8={:02x?} first 16={:02x?}",
        &bytes[bytes.len() - 8..],
        &bytes[..16]
    );

    let dest = dir.path().join("out");
    rehydrate(&rehydrate_opts(&out, &dest))
        .expect("mixed pack rehydrate must not fail trailer magic");
    verify(&out).unwrap();
    assert_bit_identical(&regular_a, &dest.join("lib-a.jar"));
    assert_bit_identical(&regular_b, &dest.join("lib-b.jar"));
    assert_bit_identical(&official_mixed, &dest.join("official-mixed.jar"));
    assert_bit_identical(&zip64, &dest.join("official-zip64.jar"));
    assert_bit_identical(&adjusted, &dest.join("official-zip-a.jar"));
}

#[test]
fn two_wrapped_jars_share_one_prefix_blob() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.jar");
    let b = dir.path().join("b.jar");
    write_wrapped_jar(&a, SPRING_LAUNCHER, &[("A.class", b"AAA")]);
    write_wrapped_jar(&b, SPRING_LAUNCHER, &[("B.class", b"BBB")]);
    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![a.clone(), b.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    assert_eq!(
        content_blob_ids(&m).len() + 1,
        3,
        "shared launcher + two unique entries"
    );
    assert_eq!(summary.unique_blob_count, m.blobs.len() as u64);
    let pa = m.jars[0].prefix_blob.as_ref().expect("a.jar prefix");
    let pb = m.jars[1].prefix_blob.as_ref().expect("b.jar prefix");
    assert_eq!(pa, pb);
    assert_eq!(m.jars[0].prefix_size, Some(SPRING_LAUNCHER.len() as u64));
    assert_eq!(m.jars[1].prefix_size, Some(SPRING_LAUNCHER.len() as u64));
    let matches: Vec<_> = m.blobs.iter().filter(|b| b.blake3 == *pa).collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].ref_count, 2);

    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    assert_bit_identical(&a, &dest.join("a.jar"));
    assert_bit_identical(&b, &dest.join("b.jar"));
    assert_functional_identity(&a, &dest.join("a.jar"));
    assert_functional_identity(&b, &dest.join("b.jar"));
    assert!(fs::read(dest.join("a.jar"))
        .unwrap()
        .starts_with(SPRING_LAUNCHER));
    assert!(fs::read(dest.join("b.jar"))
        .unwrap()
        .starts_with(SPRING_LAUNCHER));
}

#[test]
fn normal_jar_has_no_prefix_fields_and_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("plain.jar");
    write_jar(&jar, &[("x.txt", b"hello")]);
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let (_h, _t, records) = read_archive(&out);
    let json = records
        .iter()
        .find_map(|r| match r {
            Record::Manifest { json } => Some(json.as_slice()),
            _ => None,
        })
        .unwrap();
    let s = std::str::from_utf8(json).unwrap();
    assert!(!s.contains("prefix_blob"), "{s}");
    assert!(!s.contains("prefix_size"), "{s}");
    let m = manifest_from_records(&records);
    assert!(m.jars[0].prefix_blob.is_none());
    assert!(m.jars[0].prefix_size.is_none());

    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    assert_bit_identical(&jar, &dest.join("plain.jar"));
    assert_functional_identity(&jar, &dest.join("plain.jar"));
    assert!(m.jars[0].tail_blob.is_some() || m.jars[0].raw_zip_blob.is_some());
}

fn strip_exact_fields(records: Vec<Record>) -> Vec<Record> {
    let mut blobs = Vec::new();
    let mut manifest_json = None;
    for rec in records {
        match rec {
            Record::Blob { hash, data } => blobs.push((hash, data)),
            Record::Manifest { json } => manifest_json = Some(json),
            Record::End { .. } => {}
        }
    }
    let mut m: Manifest = serde_json::from_slice(&manifest_json.expect("MANIFEST")).unwrap();
    let mut keep = BTreeSet::new();
    for jar in &mut m.jars {
        jar.tail_blob = None;
        jar.tail_size = None;
        jar.raw_zip_blob = None;
        jar.raw_zip_size = None;
        if let Some(p) = &jar.prefix_blob {
            keep.insert(p.clone());
        }
        for e in &mut jar.entries {
            e.cdata_blob = None;
            e.local_header_offset = None;
            e.local_header_hex = None;
            e.local_header_blob = None;
            e.data_descriptor_hex = None;
            e.pad_zeros = None;
            e.pad_blob = None;
            if let Some(b) = &e.blob {
                keep.insert(b.clone());
            }
        }
    }
    m.blobs.retain(|b| keep.contains(&b.blake3));
    m.stats.unique_blob_count = m.blobs.len() as u64;
    m.stats.bytes_unique_blobs = m.blobs.iter().map(|b| b.size).sum();

    let mut hasher = blake3::Hasher::new();
    let mut out = Vec::new();
    for (hash, data) in blobs {
        let hex = ayzenpack::hashutil::hex_lower(&hash);
        if !keep.contains(&hex) {
            continue;
        }
        hasher.update(&hash);
        out.push(Record::Blob { hash, data });
    }
    out.push(Record::Manifest {
        json: serde_json::to_vec(&m).unwrap(),
    });
    out.push(Record::End {
        digest: *hasher.finalize().as_bytes(),
    });
    out
}

#[test]
fn roundtrip_mixed_stored_and_deflated_is_bit_identical() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("mixed.jar");
    write_jar_entries(
        &jar,
        &[
            JarEntry::File {
                name: "stored.bin",
                data: b"store-me-please",
                method: CompressionMethod::Stored,
            },
            JarEntry::File {
                name: "deflated.bin",
                data: b"deflate-me-please-aaaaaaaa",
                method: CompressionMethod::Deflated,
            },
        ],
    );
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    assert_bit_identical(&jar, &dest.join("mixed.jar"));
}

#[test]
fn roundtrip_data_descriptor_is_bit_identical() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("desc.jar");
    write_data_descriptor_zip(&jar, "payload.bin", b"descriptor-payload");
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    assert!(
        m.jars[0].entries[0].data_descriptor_hex.is_some() || m.jars[0].raw_zip_blob.is_some(),
        "GPBF bit 3 must be captured"
    );
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    assert_bit_identical(&jar, &dest.join("desc.jar"));
}

#[test]
fn roundtrip_archive_comment_is_bit_identical() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("comment.jar");
    write_jar_with_comment(&jar, &[("a.txt", b"hello")], "archive comment here");
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    assert_bit_identical(&jar, &dest.join("comment.jar"));
}

#[test]
fn roundtrip_non_utf8_name_is_bit_identical() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("rawname.jar");
    write_non_utf8_name_zip(&jar, b"payload");
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    assert!(
        m.jars[0].entries[0].name_raw_hex.is_some(),
        "non-UTF-8 name must set name_raw_hex"
    );
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    assert_bit_identical(&jar, &dest.join("rawname.jar"));
}

#[test]
fn roundtrip_signed_looking_entries_are_bit_identical() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("signed.jar");
    write_signed_looking_jar(&jar);
    let src_sf = entry_map(&jar).get("META-INF/FOO.SF").unwrap().clone();
    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    assert_eq!(summary.signed_jars, vec!["signed.jar".to_string()]);
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("signed.jar");
    assert_bit_identical(&jar, &restored);
    assert_eq!(
        entry_map(&restored).get("META-INF/FOO.SF").unwrap(),
        &src_sf
    );
}

#[test]
fn roundtrip_zipalign_padding_is_bit_identical() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("aligned.jar");
    write_padded_locals_zip(&jar, ("a.txt", b"aa"), ("b.txt", b"bbbb"), 5);
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    let pad = m.jars[0].entries[0].pad_zeros;
    assert!(
        pad == Some(5) || m.jars[0].raw_zip_blob.is_some(),
        "expected pad_zeros=5 or raw_zip fallback, got {pad:?}"
    );
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    assert_bit_identical(&jar, &dest.join("aligned.jar"));
}

#[test]
fn two_jars_share_nested_lib_cdata_blob() {
    let dir = tempfile::tempdir().unwrap();
    let inner = dir.path().join("dep.jar");
    write_jar(&inner, &[("com/Dep.class", b"dep-bytes")]);
    let inner_bytes = fs::read(&inner).unwrap();
    let a = dir.path().join("app-a.jar");
    let b = dir.path().join("app-b.jar");
    write_jar(
        &a,
        &[
            ("BOOT-INF/lib/dep.jar", inner_bytes.as_slice()),
            ("A.class", b"aaa"),
        ],
    );
    write_jar(
        &b,
        &[
            ("BOOT-INF/lib/dep.jar", inner_bytes.as_slice()),
            ("B.class", b"bbb"),
        ],
    );
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![a.clone(), b.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    let ca = m.jars[0]
        .entries
        .iter()
        .find(|e| e.name == "BOOT-INF/lib/dep.jar")
        .and_then(|e| e.cdata_blob.as_ref())
        .expect("a cdata");
    let cb = m.jars[1]
        .entries
        .iter()
        .find(|e| e.name == "BOOT-INF/lib/dep.jar")
        .and_then(|e| e.cdata_blob.as_ref())
        .expect("b cdata");
    assert_eq!(ca, cb);
    let blob = m
        .blobs
        .iter()
        .find(|b| b.blake3 == *ca)
        .expect("cdata catalog");
    assert!(
        blob.ref_count >= 2,
        "shared nested lib cdata ref_count, got {}",
        blob.ref_count
    );
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    assert_bit_identical(&a, &dest.join("app-a.jar"));
    assert_bit_identical(&b, &dest.join("app-b.jar"));
}

#[test]
fn content_mode_archive_still_rehydrates_via_zipwriter() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("stored.jar");
    write_jar_entries(
        &jar,
        &[JarEntry::File {
            name: "payload.bin",
            data: b"content-mode-should-deflate",
            method: CompressionMethod::Stored,
        }],
    );
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();

    let mut f = File::open(&out).unwrap();
    let (header, _trailer, records) = read_ayz_file(&mut f).unwrap();
    let stripped = strip_exact_fields(records);
    let crafted = dir.path().join("content.ayz");
    let mut w = File::create(&crafted).unwrap();
    write_ayz_file(&mut w, &header, &stripped, 1).unwrap();

    let m = manifest_from_records(&stripped);
    assert!(m.jars[0].tail_blob.is_none());
    assert!(m.jars[0].raw_zip_blob.is_none());
    assert!(m.jars[0].entries[0].cdata_blob.is_none());

    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&crafted, &dest)).unwrap();
    let restored = dest.join("stored.jar");
    assert_functional_identity(&jar, &restored);
    assert_ne!(
        fs::read(&jar).unwrap(),
        fs::read(&restored).unwrap(),
        "content-mode ZipWriter must not be bit-identical for a stored source"
    );
    let mut out_z = ZipArchive::new(File::open(&restored).unwrap()).unwrap();
    assert_eq!(
        out_z.by_index(0).unwrap().compression(),
        CompressionMethod::Deflated
    );
}

#[test]
fn exact_rehydrate_fails_if_cdata_blob_swapped() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("two.jar");
    write_jar(
        &jar,
        &[("a.txt", b"AAAA-payload"), ("b.txt", b"BBBB-payload")],
    );
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar])).unwrap();

    let mut f = File::open(&out).unwrap();
    let (header, _trailer, records) = read_ayz_file(&mut f).unwrap();
    let mut new_records = Vec::new();
    let mut jar_count = 0u64;
    for rec in records {
        match rec {
            Record::Manifest { json } => {
                let mut m: Manifest = serde_json::from_slice(&json).unwrap();
                jar_count = m.jars.len() as u64;
                let a = m.jars[0].entries[0].cdata_blob.clone().expect("cdata a");
                let b = m.jars[0].entries[1].cdata_blob.clone().expect("cdata b");
                assert_ne!(a, b);
                m.jars[0].entries[0].cdata_blob = Some(b);
                new_records.push(Record::Manifest {
                    json: serde_json::to_vec(&m).unwrap(),
                });
            }
            other => new_records.push(other),
        }
    }
    let crafted = dir.path().join("swapped.ayz");
    let mut w = File::create(&crafted).unwrap();
    write_ayz_file(&mut w, &header, &new_records, jar_count).unwrap();

    let dest = dir.path().join("restored");
    let err = rehydrate(&rehydrate_opts(&crafted, &dest)).unwrap_err();
    assert!(
        matches!(err, AyzenpackError::HashMismatch(_)),
        "swapped cdata must fail source hash check, got {err:?}"
    );
}

#[test]
fn shebang_without_zip_is_still_not_zip_on_dehydrate() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("script.sh");
    fs::write(&script, b"#!/bin/bash\necho no zip here\n").unwrap();
    let out = dir.path().join("out.ayz");
    let err = dehydrate(&opts(&out, vec![script])).unwrap_err();
    assert!(
        matches!(err, AyzenpackError::NotZip { .. }),
        "#! without a zip must be NotZip, got {err:?}"
    );
}
