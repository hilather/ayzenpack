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
    matt_dehydrate, matt_rehydrate, pk_start_unadjusted_store_nested_latch_bytes,
    spring_boot_launch_script, write_codec_hit_plus_unknown_deflate, write_data_descriptor_zip,
    write_deflate_miss_plus_dir_cdata, write_deflate_miss_plus_empty_deflate_dir,
    write_encrypted_store_zip, write_fat_spring_store_nested_jar,
    write_fat_spring_store_nested_zipa_jar, write_fat_spring_zip64_zipa_jar, write_jar,
    write_jar_entries, write_jar_with_comment, write_leading_pad_pk_decoy_zip,
    write_leftover_junk_listed_zip, write_leftover_junk_plus_store_nested, write_non_utf8_name_zip,
    write_overlapping_local_plus_store_nested, write_overlapping_local_zip,
    write_padded_locals_zip, write_signed_looking_jar, write_store_file_plus_dir_cdata,
    write_store_file_plus_empty_deflate_dir, write_store_file_plus_leftover_csize_dir,
    write_stored_block_deflate_zip, write_stored_jar_dos_zero, write_stored_zip,
    write_truncated_cd_listed_zip, write_unknown_deflate_wrapped, write_unknown_deflate_zip,
    write_wrapped_jar, write_wrapped_jar_adjusted, write_wrapped_zip64_jar, write_zlib_deflate_zip,
    zip64_jar_bytes, JarEntry, SPRING_LAUNCHER,
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
fn unique_overlap_content_blobs_not_dual_copy() {
    // HELLO + A + B = 3 distinct content blobs. Index tails/headers are allowed.
    // Dual cdata_blob encodings are not.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("A.jar");
    let b = dir.path().join("B.jar");
    write_jar(&a, &[("A.txt", b"AAA"), ("HELLO.txt", b"HELLO")]);
    write_jar(&b, &[("B.txt", b"BBB"), ("HELLO.txt", b"HELLO")]);
    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![a, b])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    let content: BTreeSet<_> = content_blob_ids(&m);
    assert_eq!(content.len(), 3, "HELLO + A + B");
    for jar in &m.jars {
        for e in &jar.entries {
            assert!(
                e.cdata_blob.is_none(),
                "{}!{} must not write a second encoding",
                jar.name,
                e.name
            );
        }
    }
    assert!(
        summary.unique_blob_count < (content.len() as u64) * 2,
        "unique_blob_count {} must not be ~2× content count {}",
        summary.unique_blob_count,
        content.len()
    );
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
    assert_eq!(
        file_len,
        header_total + trailer.payload_bytes + trailer.toc_len + TRAILER_LEN
    );
    assert_eq!(summary.output_len, file_len);
    assert_eq!(trailer.version, 2);
    assert!(trailer.toc_len >= 28);
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

fn blob_payloads(records: &[Record]) -> std::collections::HashMap<[u8; 32], Vec<u8>> {
    records
        .iter()
        .filter_map(|r| match r {
            Record::Blob { hash, data } => Some((*hash, data.clone())),
            _ => None,
        })
        .collect()
}

fn content_blob_ids(m: &Manifest) -> BTreeSet<String> {
    m.jars
        .iter()
        .flat_map(|j| {
            j.entries.iter().filter_map(|e| e.blob.clone()).chain(
                j.nestedindexes
                    .iter()
                    .flat_map(|n| n.entries.iter().filter_map(|e| e.blob.clone())),
            )
        })
        .collect()
}

fn assert_bit_identical(src: &Path, dest: &Path) {
    let a = fs::read(src).unwrap();
    let b = fs::read(dest).unwrap();
    assert_eq!(a.len(), b.len(), "size {} vs {}", a.len(), b.len());
    assert_eq!(a, b, "restored bytes must match source ({})", src.display());
}

/// Local + CD header fields for `name`. `assert_functional_identity` skips dirs.
type ZipSizes = (u16, u32, u32, u32);

fn member_local_and_cd(path: &Path, name: &str) -> (ZipSizes, ZipSizes) {
    let data = fs::read(path).unwrap();
    let eocd = {
        let mut i = data.len() - 22;
        loop {
            if data[i..i + 4] == *b"PK\x05\x06" {
                break i;
            }
            assert!(i > 0, "EOCD");
            i -= 1;
        }
    };
    let cd_size = u32::from_le_bytes(data[eocd + 12..eocd + 16].try_into().unwrap()) as usize;
    let cd_off = u32::from_le_bytes(data[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    let want = name.as_bytes();
    let mut i = 0usize;
    while i + 30 <= data.len() {
        if data[i..i + 4] != *b"PK\x03\x04" {
            break;
        }
        let method = u16::from_le_bytes([data[i + 8], data[i + 9]]);
        let crc = u32::from_le_bytes(data[i + 14..i + 18].try_into().unwrap());
        let csize = u32::from_le_bytes(data[i + 18..i + 22].try_into().unwrap());
        let uncomp = u32::from_le_bytes(data[i + 22..i + 26].try_into().unwrap());
        let name_len = u16::from_le_bytes([data[i + 26], data[i + 27]]) as usize;
        let extra_len = u16::from_le_bytes([data[i + 28], data[i + 29]]) as usize;
        let n = &data[i + 30..i + 30 + name_len];
        if n == want {
            let mut cdi = cd_off;
            let cd_end = cd_off + cd_size;
            while cdi + 46 <= cd_end {
                assert_eq!(&data[cdi..cdi + 4], b"PK\x01\x02");
                let cd_method = u16::from_le_bytes([data[cdi + 10], data[cdi + 11]]);
                let cd_crc = u32::from_le_bytes(data[cdi + 16..cdi + 20].try_into().unwrap());
                let cd_csize = u32::from_le_bytes(data[cdi + 20..cdi + 24].try_into().unwrap());
                let cd_uncomp = u32::from_le_bytes(data[cdi + 24..cdi + 28].try_into().unwrap());
                let cd_name_len = u16::from_le_bytes([data[cdi + 28], data[cdi + 29]]) as usize;
                let cd_extra_len = u16::from_le_bytes([data[cdi + 30], data[cdi + 31]]) as usize;
                let cd_comment_len = u16::from_le_bytes([data[cdi + 32], data[cdi + 33]]) as usize;
                let cd_n = &data[cdi + 46..cdi + 46 + cd_name_len];
                if cd_n == want {
                    return (
                        (method, crc, csize, uncomp),
                        (cd_method, cd_crc, cd_csize, cd_uncomp),
                    );
                }
                cdi += 46 + cd_name_len + cd_extra_len + cd_comment_len;
            }
            panic!("CD missing {name}");
        }
        i += 30 + name_len + extra_len + csize as usize;
    }
    panic!("local header missing {name}");
}

fn assert_empty_store_dir(path: &Path, name: &str) {
    let (local, cd) = member_local_and_cd(path, name);
    assert_eq!(local, (0, 0, 0, 0), "local {name} must be empty STORE");
    assert_eq!(cd, (0, 0, 0, 0), "CD {name} must be empty STORE");
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
    let zip_len = fs::metadata(&jar).unwrap().len();
    let summary = dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
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
    let m = manifest_from_records(&read_archive(&out).2);
    assert!(
        m.jars[0].raw_zip_blob.is_none(),
        "dup.txt last-wins must not store raw_zip"
    );
    assert!(
        m.jars[0].tail_blob.is_none(),
        "dup.txt homemade CD count != entries; do not store a disagreeing tail"
    );
    for e in &m.jars[0].entries {
        assert!(e.cdata_blob.is_none());
    }
    assert!(
        summary.bytes_unique_blobs < zip_len,
        "unique blobs {} must not include a second copy of the {} zip",
        summary.bytes_unique_blobs,
        zip_len
    );
}

/// 0.2.1 `find_cd_bounds` used classic `eocd - cd_size` unless EOCD fields were
/// sentinels. rust zip `large_file` writes a Zip64 footer with real 32-bit
/// counts, so that lands in the footer (not `PK\x01\x02`) → homemade slice Err
/// → `ZipExact::Raw` of the whole zip portion.
fn v021_classic_bounds_miss_cd(jar: &[u8]) -> bool {
    if jar.len() < 22 {
        return false;
    }
    let mut i = jar.len() - 22;
    let eocd = loop {
        if jar[i..i + 4] == *b"PK\x05\x06" {
            let comment_len = u16::from_le_bytes([jar[i + 20], jar[i + 21]]) as usize;
            if i + 22 + comment_len == jar.len() {
                break i;
            }
        }
        if i == 0 {
            return false;
        }
        i -= 1;
    };
    let cd_size32 = u32::from_le_bytes(jar[eocd + 12..eocd + 16].try_into().unwrap());
    let cd_off32 = u32::from_le_bytes(jar[eocd + 16..eocd + 20].try_into().unwrap());
    let entries16 = u16::from_le_bytes(jar[eocd + 10..eocd + 12].try_into().unwrap());
    let sentinels = cd_size32 == u32::MAX || cd_off32 == u32::MAX || entries16 == u16::MAX;
    if sentinels {
        return false;
    }
    let has_zip64_locator = eocd >= 20 && jar[eocd - 20..eocd - 16] == *b"PK\x06\x07";
    if !has_zip64_locator {
        return false;
    }
    let Some(phys) = eocd.checked_sub(cd_size32 as usize) else {
        return true;
    };
    phys + 4 > jar.len() || jar[phys..phys + 4] != *b"PK\x01\x02"
}

#[test]
fn fat_spring_zip64_zipa_is_listed_raw_on_v021_no_dual_copy_now() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("app.jar");
    write_fat_spring_zip64_zipa_jar(&jar);
    let src = fs::read(&jar).unwrap();
    let zip_len = src.len() as u64;
    let prefix = spring_boot_launch_script().len() as u64;
    assert!(src.starts_with(spring_boot_launch_script()));
    assert!(
        src.windows(4).any(|w| w == b"PK\x06\x06"),
        "fat fixture must be Zip64"
    );

    let listed = {
        let mut za = ZipArchive::new(File::open(&jar).unwrap()).expect("fixture must be listable");
        let n = za.len();
        let names: Vec<String> = (0..n)
            .map(|i| za.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(
            names.iter().any(|n| n == "App.class"),
            "outer listing must include App.class, got {names:?}"
        );
        assert_eq!(
            names
                .iter()
                .filter(|n| n.starts_with("BOOT-INF/lib/"))
                .count(),
            4,
            "outer listing must include 4 nested libs, got {names:?}"
        );
        n
    };
    assert!(
        listed >= 6,
        "App + properties + 4 BOOT-INF/lib jars, got {listed}"
    );
    assert!(
        v021_classic_bounds_miss_cd(&src),
        "0.2.1 find_cd_bounds must miss the CD on this listable Zip64+prefix+zip-A jar"
    );

    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    assert_eq!(m.jars[0].entries.len(), listed);
    assert!(
        m.jars[0].raw_zip_blob.is_none(),
        "listed fat Spring jar must not store raw_zip"
    );
    assert!(
        m.jars[0].tail_blob.is_some(),
        "Zip64+zip-A fat must slice (tail), not skip-exact"
    );
    for e in &m.jars[0].entries {
        assert!(
            e.cdata_blob.is_none(),
            "{} must not grow a cdata_blob dual copy",
            e.name
        );
    }
    let zip_portion = zip_len - prefix;
    let biggest = m.blobs.iter().map(|b| b.size).max().unwrap_or(0);
    assert!(
        biggest < zip_portion,
        "no blob ({biggest}) may be the whole zip portion ({zip_portion})"
    );
    // 0.2.1 Raw unique ≈ prefix + zip_portion + CAS (no fill_exact headers).
    let cas: u64 = m.jars[0].entries.iter().map(|e| e.uncompressed_size).sum();
    let v021_unique = zip_len + cas;
    assert!(
        summary.bytes_unique_blobs < v021_unique,
        "unique {} must be below 0.2.1 CAS+whole-zip ~{}",
        summary.bytes_unique_blobs,
        v021_unique
    );
    assert!(
        v021_unique - summary.bytes_unique_blobs >= zip_portion * 8 / 10,
        "must drop most of the {} raw_zip; unique {} vs 0.2.1 ~{}",
        zip_portion,
        summary.bytes_unique_blobs,
        v021_unique
    );
    assert!(
        summary.bytes_unique_blobs < zip_len + cas / 2,
        "unique {} must stay below CAS+whole-zip (zip {} cas {})",
        summary.bytes_unique_blobs,
        zip_len,
        cas
    );

    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("app.jar");
    assert_functional_identity(&jar, &restored);
    if m.jars[0].bit_identical_restore() {
        assert_bit_identical(&jar, &restored);
    }
}

#[test]
fn store_nested_zipa_fat_is_outer_listing_tail_no_raw_zip() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("app.jar");
    write_fat_spring_store_nested_zipa_jar(&jar);
    let src_len = fs::metadata(&jar).unwrap().len();
    let src_n = ZipArchive::new(File::open(&jar).unwrap()).unwrap().len();
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    assert_eq!(m.jars[0].entries.len(), src_n);
    assert!(m.jars[0].raw_zip_blob.is_none());
    assert!(
        m.jars[0].tail_blob.is_some(),
        "STORE nested zip -A must slice (tail), not skip-exact"
    );
    assert_eq!(
        m.jars[0].prefix_size,
        Some(spring_boot_launch_script().len() as u64)
    );
    for e in &m.jars[0].entries {
        assert!(
            e.cdata_blob.is_none(),
            "{} must not grow cdata_blob",
            e.name
        );
    }
    assert!(
        m.jars[0].entries.iter().any(|e| e.name == "App.class"),
        "outer listing must include App.class"
    );
    let libs: Vec<_> = m.jars[0]
        .entries
        .iter()
        .filter(|e| e.name.starts_with("BOOT-INF/lib/"))
        .collect();
    assert_eq!(libs.len(), 2, "outer listing must keep both nested libs");
    for e in &libs {
        assert!(
            e.blob.is_none(),
            "{} must be zip_index not opaque CAS",
            e.name
        );
        assert!(e.zip_index.is_some());
        assert!(e.cdata_blob.is_none());
    }
    assert_eq!(m.jars[0].nestedindexes.len(), 2);
    for nested in &m.jars[0].nestedindexes {
        assert!(
            nested.tail_blob.is_some(),
            "child stencil must have tail_blob"
        );
    }
    assert!(m.jars[0].bit_identical_restore());
    let records = read_archive(&out).2;
    let payloads = blob_payloads(&records);
    for e in &libs {
        let inner = {
            let mut z = ZipArchive::new(File::open(&jar).unwrap()).unwrap();
            let mut buf = Vec::new();
            z.by_name(&e.name).unwrap().read_to_end(&mut buf).unwrap();
            buf
        };
        let inner_hex = ayzenpack::hashutil::hex_lower(&blake3_bytes(&inner));
        assert!(
            !m.blobs.iter().any(|b| b.blake3 == inner_hex),
            "blake3(inner zip) must not be in blobs[]"
        );
        let idx = e.zip_index.expect("zip_index");
        let got = ayzenpack::reconstruct_child_zip(
            &m.jars[0].nestedindexes[idx],
            e.uncompressed_size,
            |hex| {
                let h = ayzenpack::hashutil::parse_blake3_hex(hex).unwrap();
                Ok(payloads.get(&h).cloned().expect(hex))
            },
        )
        .unwrap();
        assert_eq!(
            got, inner,
            "reconstruct_child_zip must equal original inner ZIP"
        );
    }
    verify(&out).unwrap();
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("app.jar");
    assert_bit_identical(&jar, &restored);
    let got = fs::metadata(&restored).unwrap().len();
    assert!(
        got * 2 >= src_len,
        "restored {got} must stay in the same league as source {src_len}"
    );
}

#[test]
fn store_nested_unadjusted_fat_uses_prefix_shift() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("app.jar");
    write_fat_spring_store_nested_jar(&jar);
    // ZipArchive::new(File) is view_shift=0 and latches on unadjusted prefix.
    // Scan (correct view) must list the outer zip.
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    let names: Vec<&str> = m.jars[0].entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"App.class"), "outer listing, got {names:?}");
    assert_eq!(
        names
            .iter()
            .filter(|n| n.starts_with("BOOT-INF/lib/"))
            .count(),
        2
    );
    for e in m.jars[0]
        .entries
        .iter()
        .filter(|e| e.name.starts_with("BOOT-INF/lib/"))
    {
        assert!(e.blob.is_none());
        assert!(e.zip_index.is_some());
    }
    assert!(m.jars[0].raw_zip_blob.is_none());
    assert!(m.jars[0].tail_blob.is_some());
    for e in &m.jars[0].entries {
        assert!(
            e.cdata_blob.is_none(),
            "{} must not grow cdata_blob",
            e.name
        );
    }
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    // rust ZipArchive::new(File) latches on unadjusted prefix; use scan (correct view).
    let src_scan = ayzenpack::scan::scan_jar(&jar, u64::MAX).unwrap();
    let dest_scan = ayzenpack::scan::scan_jar(&dest.join("app.jar"), u64::MAX).unwrap();
    assert_eq!(
        src_scan
            .entries
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        dest_scan
            .entries
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(src_scan.entries.len(), dest_scan.entries.len());
}

#[test]
fn overlapping_locals_listed_jar_has_no_raw_zip() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("overlap.jar");
    write_overlapping_local_zip(&jar);
    let zip_len = fs::metadata(&jar).unwrap().len();
    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    assert!(
        m.jars[0].raw_zip_blob.is_none(),
        "listed overlap jar must not store raw_zip"
    );
    assert!(
        m.jars[0].tail_blob.is_none(),
        "overlap must skip exact (no broken tail)"
    );
    assert!(
        summary.bytes_unique_blobs < zip_len,
        "unique blobs {} must not include the {} zip portion",
        summary.bytes_unique_blobs,
        zip_len
    );
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    assert!(dest.join("overlap.jar").is_file());
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
    let inner_ent = m.jars[0]
        .entries
        .iter()
        .find(|e| e.name == "lib/inner.jar")
        .expect("inner");
    assert!(
        inner_ent.zip_index.is_none(),
        "DEFLATE nested must stay opaque"
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
    assert!(m.jars[0].tail_blob.is_some());
    assert!(m.jars[0].raw_zip_blob.is_none());
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
    let m = manifest_from_records(&read_archive(&out).2);
    assert!(m.jars[0].tail_blob.is_some());
    assert!(m.jars[0].raw_zip_blob.is_none());

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
    let m = manifest_from_records(&read_archive(&out).2);
    assert!(m.jars[0].tail_blob.is_some());
    assert!(m.jars[0].raw_zip_blob.is_none());
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
fn roundtrip_decoy_pk_in_launcher_stub() {
    // Issue #24: decoy PK\x03\x04 at offset 20 must not truncate a 37-byte prefix.
    let prefix = b"#!/bin/bash\n# decoy PK\x03\x04 here\nexit 0\n";
    assert_eq!(prefix.len(), 37);
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("falsepk.jar");
    write_wrapped_jar(&jar, prefix, &[("App.class", b"hello-app")]);
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    assert_eq!(m.jars[0].prefix_size, Some(prefix.len() as u64));
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("falsepk.jar");
    assert_eq!(&fs::read(&restored).unwrap()[..prefix.len()], prefix);
    assert_bit_identical(&jar, &restored);
}

#[test]
fn roundtrip_empty_prefixed_zip_a() {
    // Issue #25: empty ZIP after zip -A must pack and restore the prefix.
    let prefix = b"#!/bin/bash\nexit 0\n";
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("empty_zipA.jar");
    write_wrapped_jar_adjusted(&jar, prefix, &[]);
    let out = dir.path().join("emptyA.ayz");
    dehydrate(&opts(&out, vec![jar.clone()]))
        .expect("empty prefixed ZIP after zip -A must not be NotZip");
    let m = manifest_from_records(&read_archive(&out).2);
    assert_eq!(m.jars[0].prefix_size, Some(prefix.len() as u64));
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("empty_zipA.jar");
    assert_eq!(&fs::read(&restored).unwrap()[..prefix.len()], prefix);
    assert_bit_identical(&jar, &restored);
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
    assert!(m.jars[0].tail_blob.is_some());
    assert!(m.jars[0].raw_zip_blob.is_none());
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
            e.cdata_codec = None;
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
        for nested in &jar.nestedindexes {
            if let Some(p) = &nested.prefix_blob {
                keep.insert(p.clone());
            }
            if let Some(p) = &nested.leading_pad_blob {
                keep.insert(p.clone());
            }
            if let Some(t) = &nested.tail_blob {
                keep.insert(t.clone());
            }
            for e in &nested.entries {
                if let Some(b) = &e.blob {
                    keep.insert(b.clone());
                }
                if let Some(b) = &e.local_header_blob {
                    keep.insert(b.clone());
                }
                if let Some(b) = &e.pad_blob {
                    keep.insert(b.clone());
                }
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
        m.jars[0].entries[0].data_descriptor_hex.is_some(),
        "GPBF bit 3 must be captured"
    );
    assert!(m.jars[0].tail_blob.is_some());
    assert!(
        m.jars[0].raw_zip_blob.is_none(),
        "descriptor jar must not fall back to raw_zip"
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
    assert!(pad == Some(5), "expected pad_zeros=5, got {pad:?}");
    assert!(m.jars[0].tail_blob.is_some());
    assert!(
        m.jars[0].raw_zip_blob.is_none(),
        "zipalign jar must not fall back to raw_zip"
    );
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    assert_bit_identical(&jar, &dest.join("aligned.jar"));
}

#[test]
fn two_jars_share_nested_lib_content_blob() {
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
    let ea = m.jars[0]
        .entries
        .iter()
        .find(|e| e.name == "BOOT-INF/lib/dep.jar")
        .expect("a dep");
    let eb = m.jars[1]
        .entries
        .iter()
        .find(|e| e.name == "BOOT-INF/lib/dep.jar")
        .expect("b dep");
    assert_eq!(ea.blob, eb.blob);
    assert!(ea.blob.is_some(), "write_jar DEFLATE nested stays opaque");
    assert!(ea.zip_index.is_none());
    assert!(eb.zip_index.is_none());
    assert_eq!(ea.cdata_codec, eb.cdata_codec);
    assert!(ea.cdata_blob.is_none());
    assert!(eb.cdata_blob.is_none());
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
            data: b"content-mode-should-store",
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
    assert_eq!(
        entry_compression(&restored, "payload.bin"),
        CompressionMethod::Stored,
        "content-mode ZipWriter STOREs method_code 0"
    );
}

#[test]
fn skip_exact_zipwriter_deflates_method8_and_stores_method0() {
    // STORE-everything on skip-exact would keep method-8 Stored; this must fail that.
    let dir = tempfile::tempdir().unwrap();
    let inner_path = dir.path().join("inner.jar");
    write_jar_entries(
        &inner_path,
        &[JarEntry::File {
            name: "n.txt",
            data: b"nested-plain",
            method: CompressionMethod::Stored,
        }],
    );
    let inner = fs::read(&inner_path).unwrap();
    let jar = dir.path().join("mixed-skip.jar");
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
            JarEntry::File {
                name: "lib/inner.jar",
                data: &inner,
                method: CompressionMethod::Stored,
            },
        ],
    );
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();

    let mut f = File::open(&out).unwrap();
    let (header, _trailer, records) = read_ayz_file(&mut f).unwrap();
    let stripped = strip_exact_fields(records);
    let crafted = dir.path().join("skip.ayz");
    let mut w = File::create(&crafted).unwrap();
    write_ayz_file(&mut w, &header, &stripped, 1).unwrap();

    let m = manifest_from_records(&stripped);
    assert!(m.jars[0].tail_blob.is_none());
    assert!(m.jars[0].raw_zip_blob.is_none());
    assert!(!m.jars[0].bit_identical_restore());
    assert!(!m.jars[0].metadata_rebuild());
    let inner_e = m.jars[0]
        .entries
        .iter()
        .find(|e| e.name == "lib/inner.jar")
        .expect("inner");
    assert!(inner_e.blob.is_none());
    assert!(inner_e.zip_index.is_some());
    let deflated_e = m.jars[0]
        .entries
        .iter()
        .find(|e| e.name == "deflated.bin")
        .expect("deflated");
    assert_eq!(deflated_e.method_code, 8);
    let stored_e = m.jars[0]
        .entries
        .iter()
        .find(|e| e.name == "stored.bin")
        .expect("stored");
    assert_eq!(stored_e.method_code, 0);

    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&crafted, &dest)).unwrap();
    let restored = dest.join("mixed-skip.jar");
    assert_functional_identity(&jar, &restored);
    assert_eq!(
        entry_compression(&restored, "stored.bin"),
        CompressionMethod::Stored
    );
    assert_eq!(
        entry_compression(&restored, "lib/inner.jar"),
        CompressionMethod::Stored
    );
    assert_eq!(
        entry_compression(&restored, "deflated.bin"),
        CompressionMethod::Deflated,
        "method-8 skip-exact files must DEFLATE at deflate_level"
    );
}

#[test]
fn exact_rehydrate_fails_if_cdata_blob_swapped() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("two.jar");
    write_jar_entries(
        &jar,
        &[
            JarEntry::File {
                name: "a.txt",
                data: b"AAAA-payload",
                method: CompressionMethod::Stored,
            },
            JarEntry::File {
                name: "b.txt",
                data: b"BBBB-payload",
                method: CompressionMethod::Stored,
            },
        ],
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
                let a = m.jars[0].entries[0].blob.clone().expect("blob a");
                let b = m.jars[0].entries[1].blob.clone().expect("blob b");
                assert_ne!(a, b);
                m.jars[0].entries[0].cdata_blob = Some(b);
                m.jars[0].entries[1].cdata_blob = Some(a);
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
fn store_uses_content_blob_not_cdata_blob() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("stored.jar");
    write_jar_entries(
        &jar,
        &[JarEntry::File {
            name: "payload.bin",
            data: b"store-me-once-only",
            method: CompressionMethod::Stored,
        }],
    );
    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    let e = &m.jars[0].entries[0];
    assert!(e.cdata_blob.is_none(), "STORE must omit cdata_blob");
    assert!(e.cdata_codec.is_none());
    let content = e.blob.as_ref().expect("content blob");
    let matches = m.blobs.iter().filter(|b| b.blake3 == *content).count();
    assert_eq!(matches, 1);
    assert!(
        summary.bytes_unique_blobs < summary.bytes_in_jars,
        "unique blobs must not include a second payload copy"
    );
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    assert_bit_identical(&jar, &dest.join("stored.jar"));
}

#[test]
fn codec_hit_deflated_jar_is_bit_identical_without_cdata_blob() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("plain.jar");
    write_jar(&jar, &[("x.txt", b"hello-deflate-please")]);
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    let e = &m.jars[0].entries[0];
    assert!(e.cdata_blob.is_none());
    assert!(
        e.cdata_codec.as_deref().is_some_and(|c| {
            c.starts_with("deflate-raw:flate2:") || c.starts_with("deflate-raw:zlib:")
        }),
        "zip-crate deflate must hit cdata_codec, got {:?}",
        e.cdata_codec
    );
    assert!(m.jars[0].exact_restore());
    assert!(!m.jars[0].metadata_rebuild());
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    assert_bit_identical(&jar, &dest.join("plain.jar"));
}

#[test]
fn stored_block_deflate_is_stored_codec_hit() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("stored-hit.jar");
    let payload = vec![b'a'; 256];
    write_stored_block_deflate_zip(&jar, "a.txt", &payload);
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    let e = &m.jars[0].entries[0];
    assert!(e.cdata_blob.is_none());
    assert_eq!(e.cdata_codec.as_deref(), Some("deflate-raw:stored"));
    assert!(m.jars[0].exact_restore());
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    assert_bit_identical(&jar, &dest.join("stored-hit.jar"));
}

#[test]
fn codec_miss_rebuilds_valid_zip_keeping_extras() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("miss.jar");
    let payload = vec![b'a'; 256];
    write_unknown_deflate_zip(&jar, "a.txt", &payload);
    let src = fs::read(&jar).unwrap();
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    let e = &m.jars[0].entries[0];
    assert!(e.cdata_blob.is_none());
    assert!(e.cdata_codec.is_none());
    assert!(m.jars[0].raw_zip_blob.is_none());
    assert!(m.jars[0].tail_blob.is_some());
    assert!(m.jars[0].metadata_rebuild());
    assert!(!m.jars[0].exact_restore());
    assert_eq!(
        content_blob_ids(&m).len(),
        1,
        "miss pack must not store a second payload copy"
    );

    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("miss.jar");
    assert_functional_identity(&jar, &restored);
    let got = fs::read(&restored).unwrap();
    assert_ne!(src, got, "rebuild must change compressed sizes / file hash");
    // ZipWriter fallback would rewrite extras; we keep the source local header
    // (including whatever extra ZipWriter originally emitted) and only patch sizes.
    assert_eq!(&got[4..14], &src[4..14], "version/flags/method/time stay");
}

#[test]
fn codec_miss_with_prefix_rebuilds() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("prefixed-miss.jar");
    let payload = vec![b'a'; 256];
    write_unknown_deflate_wrapped(&jar, SPRING_LAUNCHER, "a.txt", &payload);
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    assert!(m.jars[0].prefix_blob.is_some());
    assert!(m.jars[0].metadata_rebuild());
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("prefixed-miss.jar");
    assert_functional_identity(&jar, &restored);
    let got = fs::read(&restored).unwrap();
    assert_eq!(&got[..SPRING_LAUNCHER.len()], SPRING_LAUNCHER);
    assert_ne!(fs::read(&jar).unwrap(), got);
}

#[test]
fn per_entry_miss_keeps_sibling_codec() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("mixed-codec.jar");
    let hit = vec![b'h'; 128];
    let miss = vec![b'm'; 256];
    write_codec_hit_plus_unknown_deflate(&jar, "hit.txt", &hit, "miss.txt", &miss);
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    let hit_e = m.jars[0]
        .entries
        .iter()
        .find(|e| e.name == "hit.txt")
        .expect("hit");
    let miss_e = m.jars[0]
        .entries
        .iter()
        .find(|e| e.name == "miss.txt")
        .expect("miss");
    assert_eq!(hit_e.cdata_codec.as_deref(), Some("deflate-raw:flate2:6"));
    assert!(hit_e.cdata_blob.is_none());
    assert!(miss_e.cdata_codec.is_none());
    assert!(miss_e.cdata_blob.is_none());
    assert!(m.jars[0].metadata_rebuild());
    assert!(!m.jars[0].exact_restore());
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    assert_functional_identity(&jar, &dest.join("mixed-codec.jar"));
    assert_ne!(
        fs::read(&jar).unwrap(),
        fs::read(dest.join("mixed-codec.jar")).unwrap()
    );
}

#[test]
fn old_style_cdata_blob_store_still_rehydrates() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("old.jar");
    write_jar_entries(
        &jar,
        &[JarEntry::File {
            name: "legacy.bin",
            data: b"old-cdata-blob-path",
            method: CompressionMethod::Stored,
        }],
    );
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();

    let mut f = File::open(&out).unwrap();
    let (header, _trailer, records) = read_ayz_file(&mut f).unwrap();
    let mut new_records = Vec::new();
    let mut jar_count = 0u64;
    for rec in records {
        match rec {
            Record::Manifest { json } => {
                let mut m: Manifest = serde_json::from_slice(&json).unwrap();
                jar_count = m.jars.len() as u64;
                let blob = m.jars[0].entries[0].blob.clone().expect("blob");
                m.jars[0].entries[0].cdata_blob = Some(blob);
                m.jars[0].entries[0].cdata_codec = None;
                new_records.push(Record::Manifest {
                    json: serde_json::to_vec(&m).unwrap(),
                });
            }
            other => new_records.push(other),
        }
    }
    let crafted = dir.path().join("old-style.ayz");
    let mut w = File::create(&crafted).unwrap();
    write_ayz_file(&mut w, &header, &new_records, jar_count).unwrap();
    verify(&crafted).unwrap();

    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&crafted, &dest)).unwrap();
    assert_bit_identical(&jar, &dest.join("old.jar"));
}

#[test]
fn store_plus_maven_empty_deflate_dir_is_bit_identical() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("store-maven-dir.jar");
    write_store_file_plus_empty_deflate_dir(&jar, "a.txt", b"hello-store");
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    let dir_ent = m.jars[0]
        .entries
        .iter()
        .find(|e| e.is_dir || e.name.ends_with('/'))
        .expect("dir");
    assert!(dir_ent.blob.is_none());
    assert!(dir_ent.cdata_blob.is_none());
    assert_eq!(
        dir_ent.cdata_codec.as_deref(),
        Some("deflate-raw:zlib:6"),
        "empty DEFLATE dir must record codec, not a content blob"
    );
    assert!(m.jars[0].exact_restore());
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    assert_bit_identical(&jar, &dest.join("store-maven-dir.jar"));
}

#[test]
fn maven_empty_deflate_dir_does_not_force_cdata_blob() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("maven-dirs.jar");
    let payload = vec![b'a'; 256];
    write_deflate_miss_plus_empty_deflate_dir(&jar, "a.txt", &payload);
    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    let file = m.jars[0]
        .entries
        .iter()
        .find(|e| e.name == "a.txt")
        .expect("file");
    let dir_ent = m.jars[0]
        .entries
        .iter()
        .find(|e| e.is_dir || e.name.ends_with('/'))
        .expect("dir");
    for e in &m.jars[0].entries {
        assert!(
            e.cdata_blob.is_none(),
            "{} must not store cdata_blob just because dirs are empty DEFLATE",
            e.name
        );
    }
    assert_eq!(file.cdata_codec.as_deref(), Some("deflate-raw:stored"));
    assert_eq!(dir_ent.cdata_codec.as_deref(), Some("deflate-raw:zlib:6"));
    assert!(m.jars[0].bit_identical_restore());
    assert!(
        summary.bytes_unique_blobs < summary.bytes_in_jars + 4096,
        "empty-deflate dirs must not store a second copy of every file payload"
    );
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    assert_bit_identical(&jar, &dest.join("maven-dirs.jar"));
}

#[test]
fn class4_miss_plus_dir_cdata_rebuilds_empty_store_dir() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("class4.jar");
    let payload = vec![b'a'; 256];
    write_deflate_miss_plus_dir_cdata(&jar, "a.txt", &payload);
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()]))
        .expect("class-4 fixture must be dehydratable (dir-with-cdata + stored-block miss)");
    let m = manifest_from_records(&read_archive(&out).2);
    let file = m.jars[0]
        .entries
        .iter()
        .find(|e| e.name == "a.txt")
        .expect("file");
    let dir_ent = m.jars[0]
        .entries
        .iter()
        .find(|e| e.is_dir || e.name.ends_with('/'))
        .expect("dir");
    assert!(
        file.cdata_blob.is_none(),
        "class 4 file must not store cdata_blob"
    );
    assert!(
        dir_ent.cdata_blob.is_none(),
        "class 4 dir-with-cdata must not store cdata_blob"
    );
    assert_eq!(file.cdata_codec.as_deref(), Some("deflate-raw:stored"));
    assert!(dir_ent.cdata_codec.is_none());
    assert!(m.jars[0].raw_zip_blob.is_none());
    assert!(m.jars[0].metadata_rebuild());
    assert!(!m.jars[0].bit_identical_restore());
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("class4.jar");
    assert_functional_identity(&jar, &restored);
    assert_ne!(
        fs::read(&jar).unwrap(),
        fs::read(&restored).unwrap(),
        "class-4 rebuild must not be bit-identical"
    );
    assert_empty_store_dir(&restored, "marked/");
}

#[test]
fn exact_with_exotic_store_plus_dir_cdata_rebuilds() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("exact-exotic.jar");
    write_store_file_plus_dir_cdata(&jar, "a.txt", b"hello-store");
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    for e in &m.jars[0].entries {
        assert!(
            e.cdata_blob.is_none(),
            "{} must not write cdata_blob",
            e.name
        );
    }
    assert!(m.jars[0].raw_zip_blob.is_none());
    assert!(!m.jars[0].bit_identical_restore());
    assert!(m.jars[0].metadata_rebuild());
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("exact-exotic.jar");
    assert_functional_identity(&jar, &restored);
    assert_ne!(
        fs::read(&jar).unwrap(),
        fs::read(&restored).unwrap(),
        "ExactWithExotic rebuild must not be bit-identical"
    );
    assert_empty_store_dir(&restored, "marked/");
}

#[test]
fn leftover_csize_dir_rebuilds_empty_store() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("leftover-csize.jar");
    write_store_file_plus_leftover_csize_dir(&jar, "a.txt", b"hello-store");
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    for e in &m.jars[0].entries {
        assert!(
            e.cdata_blob.is_none(),
            "{} must not write cdata_blob",
            e.name
        );
    }
    assert!(m.jars[0].raw_zip_blob.is_none());
    assert!(m.jars[0].metadata_rebuild());
    assert!(!m.jars[0].bit_identical_restore());
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("leftover-csize.jar");
    assert_functional_identity(&jar, &restored);
    assert_empty_store_dir(&restored, "marked/");
}

#[test]
fn signed_rebuild_is_not_exact_restore() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("signed-miss.jar");
    let payload = vec![b'a'; 256];
    write_unknown_deflate_zip(&jar, "META-INF/FOO.SF", &payload);
    let out = dir.path().join("out.ayz");
    let summary = dehydrate(&opts(&out, vec![jar])).unwrap();
    assert_eq!(summary.signed_jars, vec!["signed-miss.jar".to_string()]);
    let m = manifest_from_records(&read_archive(&out).2);
    assert!(m.jars[0].signed);
    assert!(
        !m.jars[0].exact_restore(),
        "signed + rebuild must use the existing rebuild-breaks-signature warning path"
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

#[test]
fn zlib_rs_classic_matt_cli_source_identity() {
    let dir = tempfile::tempdir().unwrap();
    let jars = dir.path().join("jars");
    fs::create_dir_all(&jars).unwrap();
    let jar = jars.join("zlib.jar");
    let payload = b"zlib-rs classic fixture payload for 0.2.4".repeat(8);
    write_zlib_deflate_zip(&jar, "a.txt", &payload);
    let src = fs::read(&jar).unwrap();
    let pack = dir.path().join("out.ayz");
    matt_dehydrate(&pack, &jars);
    let m = ayzenpack::list(&pack).unwrap();
    let e = &m.jars[0].entries[0];
    assert!(e.cdata_blob.is_none());
    assert_eq!(e.cdata_codec.as_deref(), Some("deflate-raw:zlib:6"));
    assert!(m.jars[0].bit_identical_restore());
    verify(&pack).unwrap();
    matt_rehydrate(&pack);
    assert_eq!(fs::read(&jar).unwrap(), src);
    assert_eq!(m.jars[0].source_size, src.len() as u64);
}

#[test]
fn leading_pad_pk_decoy_matt_cli_source_identity() {
    let dir = tempfile::tempdir().unwrap();
    let jars = dir.path().join("jars");
    fs::create_dir_all(&jars).unwrap();
    let jar = jars.join("lead.jar");
    write_leading_pad_pk_decoy_zip(&jar, "a.txt", b"leading-pad-plain");
    let src = fs::read(&jar).unwrap();
    assert_eq!(&src[..4], b"PK\x03\x04");
    let pack = dir.path().join("out.ayz");
    matt_dehydrate(&pack, &jars);
    let m = ayzenpack::list(&pack).unwrap();
    assert_eq!(m.jars[0].prefix_size.unwrap_or(0), 0);
    assert!(m.jars[0].leading_pad_blob.is_some());
    assert!(m.jars[0].leading_pad_size.unwrap_or(0) > 0);
    assert!(m.jars[0].tail_blob.is_some());
    assert!(m.jars[0].raw_zip_blob.is_none());
    for e in &m.jars[0].entries {
        assert!(e.cdata_blob.is_none());
    }
    let first_oh = m.jars[0].entries[0].offsetheader.expect("offsetheader");
    assert_eq!(m.jars[0].leading_pad_size, Some(first_oh));
    assert!(m.jars[0].bit_identical_restore());
    verify(&pack).unwrap();
    matt_rehydrate(&pack);
    assert_eq!(fs::read(&jar).unwrap(), src);
}

#[test]
fn store_nested_zipa_matt_cli_zip_index() {
    let dir = tempfile::tempdir().unwrap();
    let jars = dir.path().join("jars");
    fs::create_dir_all(&jars).unwrap();
    let jar = jars.join("app.jar");
    write_fat_spring_store_nested_zipa_jar(&jar);
    let src = fs::read(&jar).unwrap();
    let pack = dir.path().join("out.ayz");
    matt_dehydrate(&pack, &jars);
    let m = ayzenpack::list(&pack).unwrap();
    for e in m.jars[0]
        .entries
        .iter()
        .filter(|e| e.name.starts_with("BOOT-INF/lib/"))
    {
        assert!(e.blob.is_none());
        assert!(e.zip_index.is_some());
        assert!(e.cdata_blob.is_none());
    }
    assert!(m.jars[0].bit_identical_restore());
    assert!(m.jars[0].raw_zip_blob.is_none());
    verify(&pack).unwrap();
    matt_rehydrate(&pack);
    assert_eq!(fs::read(&jar).unwrap(), src);
}

fn entry_compression(path: &Path, name: &str) -> CompressionMethod {
    let mut z = ZipArchive::new(File::open(path).unwrap()).unwrap();
    let file = z.by_name(name).unwrap();
    let method = file.compression();
    drop(file);
    method
}

#[test]
fn skip_exact_outer_explodes_store_inner() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("overlap-nested.jar");
    write_overlapping_local_plus_store_nested(&jar);
    let src_len = fs::metadata(&jar).unwrap().len();
    let src_map = entry_map(&jar);
    assert!(
        src_map.contains_key("lib/inner.jar"),
        "source must be the outer listing, got {:?}",
        src_map.keys()
    );
    let mut z = ZipArchive::new(File::open(&jar).unwrap()).unwrap();
    let mut inner = Vec::new();
    z.by_name("lib/inner.jar")
        .unwrap()
        .read_to_end(&mut inner)
        .unwrap();
    drop(z);
    let out = dir.path().join("out.ayz");
    matt_dehydrate(&out, &jar);
    let records = read_archive(&out).2;
    let m = manifest_from_records(&records);
    assert!(m.jars[0].tail_blob.is_none());
    assert!(m.jars[0].raw_zip_blob.is_none());
    assert!(!m.jars[0].bit_identical_restore());
    assert!(!m.jars[0].metadata_rebuild());
    for e in &m.jars[0].entries {
        assert!(e.cdata_blob.is_none(), "{} cdata_blob", e.name);
    }
    for nested in &m.jars[0].nestedindexes {
        for e in &nested.entries {
            assert!(e.cdata_blob.is_none(), "nested {} cdata_blob", e.name);
        }
    }
    let e = m.jars[0]
        .entries
        .iter()
        .find(|e| e.name == "lib/inner.jar")
        .expect("inner");
    assert!(e.blob.is_none());
    assert!(e.zip_index.is_some());
    let inner_hex = ayzenpack::hashutil::hex_lower(&blake3_bytes(&inner));
    assert!(
        !m.blobs.iter().any(|b| b.blake3 == inner_hex),
        "blake3(inner zip) must not be in blobs[]"
    );
    let payloads = blob_payloads(&records);
    let idx = e.zip_index.expect("zip_index");
    let got = ayzenpack::reconstruct_child_zip(
        &m.jars[0].nestedindexes[idx],
        e.uncompressed_size,
        |hex| {
            let h = ayzenpack::hashutil::parse_blake3_hex(hex).unwrap();
            Ok(payloads.get(&h).cloned().expect(hex))
        },
    )
    .unwrap();
    assert_eq!(got, inner);
    // SAME-payload (a.txt/b.txt) + inner n.txt; not a second encoding of the inner zip.
    assert_eq!(content_blob_ids(&m).len(), 2, "unique content not doubled");
    verify(&out).unwrap();
    matt_rehydrate(&out);
    let got_len = fs::metadata(&jar).unwrap().len();
    assert!(
        got_len * 2 >= src_len,
        "restored {got_len} must stay in the same league as source {src_len}"
    );
    assert_eq!(entry_map(&jar), src_map);
    assert_eq!(
        entry_compression(&jar, "lib/inner.jar"),
        CompressionMethod::Stored
    );
    assert_eq!(entry_compression(&jar, "a.txt"), CompressionMethod::Stored);
    let mut z = ZipArchive::new(File::open(&jar).unwrap()).unwrap();
    let mut got_inner = Vec::new();
    z.by_name("lib/inner.jar")
        .unwrap()
        .read_to_end(&mut got_inner)
        .unwrap();
    assert_eq!(got_inner, inner);
}

#[test]
fn sort_inputs_jobs_1_eq_jobs_n_store_nested_fat() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("app.jar");
    write_fat_spring_store_nested_zipa_jar(&jar);
    let out1 = dir.path().join("j1.ayz");
    let outn = dir.path().join("jn.ayz");
    let mut o1 = opts(&out1, vec![jar.clone()]);
    o1.sort_inputs = true;
    o1.jobs = 1;
    o1.quiet = true;
    let mut on = opts(&outn, vec![jar]);
    on.sort_inputs = true;
    on.jobs = 4;
    on.quiet = true;
    dehydrate(&o1).unwrap();
    dehydrate(&on).unwrap();
    assert_eq!(
        fs::read(&out1).unwrap(),
        fs::read(&outn).unwrap(),
        "STORE-nested fat packs must be byte-identical at jobs=1 and jobs=N"
    );
}

#[test]
fn listed_homemade_leftover_junk_cd_is_exact() {
    let dir = tempfile::tempdir().unwrap();
    let jars = dir.path().join("jars");
    fs::create_dir_all(&jars).unwrap();
    let jar = jars.join("leftover-junk.jar");
    write_leftover_junk_listed_zip(&jar);
    let listed = ZipArchive::new(File::open(&jar).unwrap()).unwrap().len();
    assert!(listed >= 1, "fixture must stay listable");
    let src = fs::read(&jar).unwrap();
    let pack = dir.path().join("out.ayz");
    matt_dehydrate(&pack, &jars);
    let m = ayzenpack::list(&pack).unwrap();
    assert!(
        m.jars[0].tail_blob.is_some(),
        "leftover-junk CD must get tail_blob (must not stay skip-exact)"
    );
    assert!(m.jars[0].raw_zip_blob.is_none());
    for e in &m.jars[0].entries {
        assert!(
            e.cdata_blob.is_none(),
            "{} must not grow cdata_blob",
            e.name
        );
    }
    assert!(
        m.jars[0].bit_identical_restore(),
        "STORE leftover-junk jar must exact-restore"
    );
    verify(&pack).unwrap();
    matt_rehydrate(&pack);
    assert_eq!(fs::read(&jar).unwrap(), src);
    assert_eq!(m.jars[0].source_size, src.len() as u64);
    assert_eq!(
        m.jars[0].source_blake3,
        ayzenpack::hashutil::hex_lower(&blake3_bytes(&src))
    );
    assert_eq!(
        m.jars[0].source_sha256,
        ayzenpack::hashutil::hex_lower(&ayzenpack::hashutil::sha256_bytes(&src))
    );
}

fn classic_eocd_cd_offset(buf: &[u8]) -> u64 {
    let mut i = buf.len() - 22;
    loop {
        if buf[i..i + 4] == *b"PK\x05\x06" {
            let comment_len = u16::from_le_bytes([buf[i + 20], buf[i + 21]]) as usize;
            if i + 22 + comment_len == buf.len() {
                return u32::from_le_bytes(buf[i + 16..i + 20].try_into().unwrap()) as u64;
            }
        }
        assert!(i > 0, "EOCD");
        i -= 1;
    }
}

fn splice_truncated_cd_stub(path: &Path) {
    let mut buf = fs::read(path).unwrap();
    let mut i = buf.len() - 22;
    loop {
        if buf[i..i + 4] == *b"PK\x05\x06" {
            let comment_len = u16::from_le_bytes([buf[i + 20], buf[i + 21]]) as usize;
            if i + 22 + comment_len == buf.len() {
                break;
            }
        }
        assert!(i > 0, "EOCD");
        i -= 1;
    }
    let eocd = i;
    let cd_size = u32::from_le_bytes(buf[eocd + 12..eocd + 16].try_into().unwrap());
    let mut stub = [0u8; 46];
    stub[..4].copy_from_slice(b"PK\x01\x02");
    stub[28..30].copy_from_slice(&100u16.to_le_bytes());
    buf.splice(eocd..eocd, stub.iter().copied());
    let new_eocd = eocd + stub.len();
    buf[new_eocd + 12..new_eocd + 16].copy_from_slice(&(cd_size + stub.len() as u32).to_le_bytes());
    fs::write(path, buf).unwrap();
}

#[test]
fn listed_true_homemade_none_has_no_tail_blob() {
    // Truncated/malformed CD (not leftover junk after N matching records).
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("truncated-cd.jar");
    write_truncated_cd_listed_zip(&jar);
    let listed = ZipArchive::new(File::open(&jar).unwrap()).unwrap().len();
    assert!(listed >= 1, "fixture must stay listable");
    let src = fs::read(&jar).unwrap();
    let src_len = src.len() as u64;
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    assert!(
        m.jars[0].tail_blob.is_none(),
        "remaining homemade-None must never get tail_blob"
    );
    assert!(m.jars[0].raw_zip_blob.is_none());
    assert!(!m.jars[0].bit_identical_restore());
    assert!(!m.jars[0].metadata_rebuild());
    for e in &m.jars[0].entries {
        assert!(e.cdata_blob.is_none(), "{} cdata_blob", e.name);
        assert!(
            e.local_header_hex.is_some() || e.local_header_blob.is_some(),
            "{} must capture a local header",
            e.name
        );
    }
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("truncated-cd.jar");
    let got = fs::read(&restored).unwrap();
    let got_len = got.len() as u64;
    assert!(
        got_len * 2 >= src_len,
        "restored {got_len} must stay in the same league as source {src_len}"
    );
    let mut z = ZipArchive::new(File::open(&restored).unwrap()).unwrap();
    assert!(
        z.by_name("a.txt").is_ok(),
        "dest ZipArchive::new(File) must list outer a.txt"
    );
    drop(z);
    let phys_cd = classic_eocd_cd_offset(&src);
    let cd_start = classic_eocd_cd_offset(&got);
    assert_eq!(
        &got[..cd_start as usize],
        &src[..phys_cd as usize],
        "arm 1 locals-region identity"
    );
    assert_functional_identity(&jar, &restored);
    assert_eq!(
        entry_compression(&restored, "a.txt"),
        CompressionMethod::Stored,
        "method-0 files must STORE on skip-exact arm 1"
    );
}

#[test]
fn truncated_cd_unknown_deflate_sibling_is_arm2_zipwriter() {
    // Arm 1 must not classify a miss as a hit (resolve_cdata false would miss cdata).
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("trunc-miss.jar");
    write_codec_hit_plus_unknown_deflate(
        &jar,
        "hit.bin",
        b"hit-payload-aaaa",
        "miss.bin",
        b"miss-payload-bbbb",
    );
    splice_truncated_cd_stub(&jar);
    let listed = ZipArchive::new(File::open(&jar).unwrap()).unwrap().len();
    assert_eq!(listed, 2, "fixture must stay listable");
    let src_len = fs::metadata(&jar).unwrap().len();
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let m = manifest_from_records(&read_archive(&out).2);
    assert!(m.jars[0].tail_blob.is_none());
    assert!(m.jars[0].raw_zip_blob.is_none());
    assert!(!m.jars[0].bit_identical_restore());
    assert!(!m.jars[0].metadata_rebuild());
    for e in &m.jars[0].entries {
        assert!(e.cdata_blob.is_none(), "{} cdata_blob", e.name);
        assert!(
            e.local_header_hex.is_some() || e.local_header_blob.is_some(),
            "{} must capture a local header",
            e.name
        );
    }
    let miss = m.jars[0]
        .entries
        .iter()
        .find(|e| e.name == "miss.bin")
        .expect("miss");
    assert_eq!(miss.method_code, 8);
    assert!(
        miss.cdata_codec.is_none(),
        "unknown-deflate must stay a miss"
    );
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("trunc-miss.jar");
    let got_len = fs::metadata(&restored).unwrap().len();
    assert!(
        got_len * 2 >= src_len,
        "restored {got_len} must stay in the same league as source {src_len}"
    );
    assert_functional_identity(&jar, &restored);
}

#[test]
fn leftover_junk_plus_store_nested_is_exact_zip_index() {
    let dir = tempfile::tempdir().unwrap();
    let jar = dir.path().join("cd-junk-nested.jar");
    write_leftover_junk_plus_store_nested(&jar);
    let mut z = ZipArchive::new(File::open(&jar).unwrap()).unwrap();
    let mut inner = Vec::new();
    z.by_name("lib/inner.jar")
        .unwrap()
        .read_to_end(&mut inner)
        .unwrap();
    drop(z);
    let src = fs::read(&jar).unwrap();
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let records = read_archive(&out).2;
    let m = manifest_from_records(&records);
    assert!(
        m.jars[0].tail_blob.is_some(),
        "CD-junk + STORE nested must be exact (tail_blob)"
    );
    assert!(m.jars[0].raw_zip_blob.is_none());
    for e in &m.jars[0].entries {
        assert!(
            e.cdata_blob.is_none(),
            "{} must not grow cdata_blob",
            e.name
        );
    }
    let e = m.jars[0]
        .entries
        .iter()
        .find(|e| e.name == "lib/inner.jar")
        .expect("inner");
    assert!(e.blob.is_none());
    assert!(e.zip_index.is_some());
    let inner_hex = ayzenpack::hashutil::hex_lower(&blake3_bytes(&inner));
    assert!(!m.blobs.iter().any(|b| b.blake3 == inner_hex));
    assert_eq!(content_blob_ids(&m).len(), 2, "a.txt + inner n.txt, not 2×");
    let payloads = blob_payloads(&records);
    let idx = e.zip_index.expect("zip_index");
    let got = ayzenpack::reconstruct_child_zip(
        &m.jars[0].nestedindexes[idx],
        e.uncompressed_size,
        |hex| {
            let h = ayzenpack::hashutil::parse_blake3_hex(hex).unwrap();
            Ok(payloads.get(&h).cloned().expect(hex))
        },
    )
    .unwrap();
    assert_eq!(got, inner);
    assert!(m.jars[0].bit_identical_restore());
    verify(&out).unwrap();
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let restored = dest.join("cd-junk-nested.jar");
    assert_bit_identical(&jar, &restored);
    let mut z = ZipArchive::new(File::open(&restored).unwrap()).unwrap();
    assert_eq!(
        z.by_name("lib/inner.jar").unwrap().compression(),
        CompressionMethod::Stored
    );
    assert_eq!(fs::read(&restored).unwrap(), src);
}

#[test]
fn prefixed_store_nested_records_child_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let inner_zip = dir.path().join("inner.zip");
    write_jar_entries(
        &inner_zip,
        &[JarEntry::File {
            name: "n.txt",
            data: b"pref",
            method: CompressionMethod::Stored,
        }],
    );
    let zip = fs::read(&inner_zip).unwrap();
    let inner = fixtures::prepend_launcher(&zip, SPRING_LAUNCHER, false);
    let jar = dir.path().join("outer.jar");
    write_jar_entries(
        &jar,
        &[
            JarEntry::File {
                name: "lib/inner.jar",
                data: &inner,
                method: CompressionMethod::Stored,
            },
            JarEntry::File {
                name: "App.class",
                data: b"app",
                method: CompressionMethod::Stored,
            },
        ],
    );
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let records = read_archive(&out).2;
    let m = manifest_from_records(&records);
    let e = m.jars[0]
        .entries
        .iter()
        .find(|e| e.name == "lib/inner.jar")
        .expect("inner");
    assert!(e.blob.is_none());
    assert!(e.zip_index.is_some());
    let idx = e.zip_index.unwrap();
    assert!(m.jars[0].nestedindexes[idx].prefix_blob.is_some());
    assert_eq!(
        m.jars[0].nestedindexes[idx].prefix_size,
        Some(SPRING_LAUNCHER.len() as u64)
    );
    let payloads = blob_payloads(&records);
    let got = ayzenpack::reconstruct_child_zip(
        &m.jars[0].nestedindexes[idx],
        e.uncompressed_size,
        |hex| {
            let h = ayzenpack::hashutil::parse_blake3_hex(hex).unwrap();
            Ok(payloads.get(&h).cloned().expect(hex))
        },
    )
    .unwrap();
    assert_eq!(got, inner);
    verify(&out).unwrap();
}

#[test]
fn child_ziparchive_latch_packs_opaque() {
    let dir = tempfile::tempdir().unwrap();
    let inner = pk_start_unadjusted_store_nested_latch_bytes();
    assert_eq!(&inner[..4], b"PK\x03\x04");
    let inner_hex = ayzenpack::hashutil::hex_lower(&blake3_bytes(&inner));
    let inner_class_hex = ayzenpack::hashutil::hex_lower(&blake3_bytes(&[1u8; 2048]));

    let jar = dir.path().join("outer.jar");
    write_jar_entries(
        &jar,
        &[
            JarEntry::File {
                name: "lib/latch.jar",
                data: &inner,
                method: CompressionMethod::Stored,
            },
            JarEntry::File {
                name: "App.class",
                data: b"outer-app",
                method: CompressionMethod::Stored,
            },
        ],
    );
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let records = read_archive(&out).2;
    let m = manifest_from_records(&records);
    assert!(m.jars[0].raw_zip_blob.is_none());
    let names: Vec<&str> = m.jars[0].entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["lib/latch.jar", "App.class"]);
    assert!(
        !names.iter().any(|n| n.contains("LatchInner")),
        "latched inner-inner must not become outer entries, got {names:?}"
    );
    let e = m.jars[0]
        .entries
        .iter()
        .find(|e| e.name == "lib/latch.jar")
        .expect("latch member");
    assert!(e.zip_index.is_none(), "latch child must stay opaque");
    assert_eq!(e.blob.as_deref(), Some(inner_hex.as_str()));
    assert!(e.cdata_blob.is_none());
    assert!(
        m.blobs.iter().any(|b| b.blake3 == inner_hex),
        "blake3(inner) must be the opaque combined blob"
    );
    assert!(
        m.jars[0].nestedindexes.is_empty(),
        "must not explode a latched inner-inner"
    );
    assert!(
        !content_blob_ids(&m).contains(&inner_class_hex),
        "must not dual-copy exploded inner-inner classes plus the combined zip"
    );
    for e in &m.jars[0].entries {
        assert!(e.cdata_blob.is_none());
    }
    verify(&out).unwrap();
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let mut z = ZipArchive::new(File::open(dest.join("outer.jar")).unwrap()).unwrap();
    let mut got = Vec::new();
    z.by_name("lib/latch.jar")
        .unwrap()
        .read_to_end(&mut got)
        .unwrap();
    assert_eq!(got, inner);
}

#[test]
fn store_nested_reconstruct_equality_omits_inner_zip_cas() {
    let dir = tempfile::tempdir().unwrap();
    let inner_path = dir.path().join("inner.jar");
    write_jar_entries(
        &inner_path,
        &[JarEntry::File {
            name: "a.class",
            data: b"class-bytes",
            method: CompressionMethod::Stored,
        }],
    );
    let inner = fs::read(&inner_path).unwrap();
    let jar = dir.path().join("outer.jar");
    write_jar_entries(
        &jar,
        &[
            JarEntry::File {
                name: "lib/inner.jar",
                data: &inner,
                method: CompressionMethod::Stored,
            },
            JarEntry::File {
                name: "App.class",
                data: b"app",
                method: CompressionMethod::Stored,
            },
        ],
    );
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let records = read_archive(&out).2;
    let m = manifest_from_records(&records);
    let e = m.jars[0]
        .entries
        .iter()
        .find(|e| e.name == "lib/inner.jar")
        .expect("inner");
    assert!(e.blob.is_none());
    assert!(e.zip_index.is_some());
    assert!(e.cdata_blob.is_none());
    let inner_hex = ayzenpack::hashutil::hex_lower(&blake3_bytes(&inner));
    assert!(
        !m.blobs.iter().any(|b| b.blake3 == inner_hex),
        "blake3(inner zip) must not be in blobs[] when reconstruct equality holds"
    );
    let class_hex = ayzenpack::hashutil::hex_lower(&blake3_bytes(b"class-bytes"));
    assert!(m.blobs.iter().any(|b| b.blake3 == class_hex));
    assert_eq!(
        content_blob_ids(&m).len(),
        2,
        "unique content is App.class + a.class, not doubled with inner zip"
    );
    for e in &m.jars[0].entries {
        assert!(e.cdata_blob.is_none());
    }
    assert!(m.jars[0].raw_zip_blob.is_none());
    verify(&out).unwrap();
}

#[test]
fn encrypted_child_store_stays_opaque() {
    let dir = tempfile::tempdir().unwrap();
    let inner_path = dir.path().join("enc.jar");
    write_encrypted_store_zip(&inner_path);
    let inner = fs::read(&inner_path).unwrap();
    {
        let mut z = ZipArchive::new(File::open(&inner_path).unwrap()).unwrap();
        match z.by_index(0) {
            Err(zip::result::ZipError::UnsupportedArchive(msg))
                if msg == zip::result::ZipError::PASSWORD_REQUIRED => {}
            Err(err) => panic!("fixture must be an encrypted listing, got {err:?}"),
            Ok(_) => panic!("fixture must be an encrypted listing"),
        };
    }
    let jar = dir.path().join("outer.jar");
    write_jar_entries(
        &jar,
        &[
            JarEntry::File {
                name: "lib/enc.jar",
                data: &inner,
                method: CompressionMethod::Stored,
            },
            JarEntry::File {
                name: "App.class",
                data: b"app",
                method: CompressionMethod::Stored,
            },
        ],
    );
    let out = dir.path().join("out.ayz");
    dehydrate(&opts(&out, vec![jar.clone()])).unwrap();
    let records = read_archive(&out).2;
    let m = manifest_from_records(&records);
    let e = m.jars[0]
        .entries
        .iter()
        .find(|e| e.name == "lib/enc.jar")
        .expect("enc");
    assert!(e.zip_index.is_none(), "encrypted child must stay opaque");
    let inner_hex = ayzenpack::hashutil::hex_lower(&blake3_bytes(&inner));
    assert_eq!(e.blob.as_deref(), Some(inner_hex.as_str()));
    assert!(m.jars[0].raw_zip_blob.is_none());
    verify(&out).unwrap();
    let dest = dir.path().join("restored");
    rehydrate(&rehydrate_opts(&out, &dest)).unwrap();
    let mut z = ZipArchive::new(File::open(dest.join("outer.jar")).unwrap()).unwrap();
    let mut got = Vec::new();
    z.by_name("lib/enc.jar")
        .unwrap()
        .read_to_end(&mut got)
        .unwrap();
    assert_eq!(got, inner);
}

#[test]
fn v023_tiny_pack_still_reads() {
    let pack = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/v0.2.3-tiny.ayz");
    assert!(
        pack.is_file(),
        "check in testdata/v0.2.3-tiny.ayz (0.2.3 pack, not an in-memory Manifest)"
    );
    verify(&pack).unwrap();
    let m = ayzenpack::list(&pack).unwrap();
    assert_eq!(m.format, "ayzenpack-manifest");
    assert!(!m.jars.is_empty());
    let dir = tempfile::tempdir().unwrap();
    rehydrate(&rehydrate_opts(&pack, dir.path())).unwrap();
    for jar in &m.jars {
        assert!(dir.path().join(&jar.name).is_file());
    }
}
