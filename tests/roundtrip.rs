//! Dehydrate → rehydrate functional identity (uncompressed bytes, names, CRC).

#[path = "fixtures.rs"]
mod fixtures;

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use ayzenpack::error::AyzenpackError;
use ayzenpack::format::{read_ayz_file, write_ayz_file, Record, BUF_WRITER_CAP, TRAILER_LEN};
use ayzenpack::hashutil::blake3_bytes;
use ayzenpack::manifest::Manifest;
use ayzenpack::{dehydrate, rehydrate, DehydrateOptions, RehydrateOptions};
use fixtures::{
    write_jar, write_jar_entries, write_stored_jar_dos_zero, write_stored_zip, JarEntry,
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
    assert_eq!(summary.unique_blob_count, 3);
    assert_eq!(summary.jar_count, 2);
    assert_eq!(summary.file_entry_count, 4);

    let (_header, trailer, records) = read_archive(&out);
    assert_eq!(trailer.blob_count, 3);
    assert_eq!(blob_records(&records).len(), 3);
    let m = manifest_from_records(&records);
    assert_eq!(m.blobs.len(), 3);
    assert_eq!(m.stats.unique_blob_count, 3);
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
    assert_eq!(summary.unique_blob_count, 1);
    assert_eq!(summary.bytes_unique_blobs, 0);
    assert_eq!(summary.file_entry_count, 1);

    let (_header, trailer, records) = read_archive(&out);
    assert_eq!(trailer.blob_count, 1);
    assert_eq!(trailer.blob_bytes, 0);
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
    assert_eq!(m.blobs[0].ref_count, 1);
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
    assert_eq!(summary.unique_blob_count, 1);
    assert_eq!(summary.jar_count, 1);

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
    assert_eq!(summary.unique_blob_count, 1);
    assert!(
        summary.bytes_unique_blobs < summary.bytes_uncompressed_entries,
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
    assert_eq!(trailer.blob_count, 1);
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
    assert_eq!(
        summary.unique_blob_count, one_copy_files,
        "unique blobs must equal one copy's file-entry count, not the sum"
    );
    let (_header, trailer, records) = read_archive(&out);
    assert_eq!(trailer.blob_count, one_copy_files);
    assert_eq!(blob_records(&records).len() as u64, one_copy_files);
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
    assert_eq!(summary.unique_blob_count, 3);

    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
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
    assert_eq!(summary.unique_blob_count, 1);
    assert_eq!(summary.bytes_unique_blobs, 0);

    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("empty.jar");
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

    let mut z = ZipArchive::new(File::open(&restored).unwrap()).unwrap();
    for i in 0..z.len() {
        let e = z.by_index(i).unwrap();
        let dt = e.last_modified().expect("rebuilt entries have a DOS time");
        assert_eq!(dt, DateTime::default());
        assert_eq!(dt.year(), 1980);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 1);
    }
}

#[test]
fn roundtrip_store_source_may_deflate_rebuilt() {
    // Content mode discards STORE; rebuilt files may deflate. Maps still equal.
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
    assert_eq!(entry_map(&jar), entry_map(&restored));
    assert_eq!(entry_crcs(&jar), entry_crcs(&restored));
    assert_ne!(
        fs::read(&jar).unwrap(),
        fs::read(&restored).unwrap(),
        "must not require ZIP bit-identity of a stored source"
    );
    let mut out_z = ZipArchive::new(File::open(&restored).unwrap()).unwrap();
    assert_eq!(
        out_z.by_index(0).unwrap().compression(),
        CompressionMethod::Deflated
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
    let jar_sum = fs::metadata(&a).unwrap().len() + fs::metadata(&b).unwrap().len();

    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![a.clone(), b.clone()])).unwrap();
    assert_eq!(summary.jar_count, 2);
    assert_eq!(summary.file_entry_count, 400);
    assert_eq!(
        summary.unique_blob_count, 200,
        "two identical JARs must dedup to 200 blobs, not 400"
    );
    assert_eq!(
        summary.bytes_unique_blobs,
        summary.bytes_uncompressed_entries / 2
    );
    assert!(
        summary.output_len < jar_sum,
        "archive {} must be smaller than sum of JARs {}",
        summary.output_len,
        jar_sum
    );

    let (_header, trailer, records) = read_archive(&out);
    assert_eq!(trailer.blob_count, 200);
    assert_eq!(blob_records(&records).len(), 200);

    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
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
    assert_eq!(
        summary.unique_blob_count, 2,
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
    assert_functional_identity(&outer, &restored);
    let map = entry_map(&restored);
    assert_eq!(
        map.get("lib/inner.jar").map(Vec::as_slice),
        Some(inner_bytes.as_slice())
    );
    assert!(!map.contains_key("com/Inner.class"));
    assert_eq!(map.len(), 2);
}
