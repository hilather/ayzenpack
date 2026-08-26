//! Dehydrate-only round-trip fixtures (rehydrate lands in a later PR).

#[path = "fixtures.rs"]
mod fixtures;

use std::fs::{self, File};
use std::path::Path;

use ayzenpack::format::{read_ayz_file, Record, BUF_WRITER_CAP, TRAILER_LEN};
use ayzenpack::hashutil::blake3_bytes;
use ayzenpack::manifest::Manifest;
use ayzenpack::{dehydrate, DehydrateOptions};
use fixtures::write_jar;

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
