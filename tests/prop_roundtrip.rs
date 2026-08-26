//! Bounded property test: random ZIP trees dehydrate → rehydrate with equal maps.
//!
//! `proptest` is the ecosystem standard for property tests (highly used).
//! Guards against ZIP metadata combinations the hand-written fixtures missed.
//! Compares uncompressed bytes, names, CD order, and full-file bytes (exact packs).

#[path = "fixtures.rs"]
mod fixtures;

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use ayzenpack::{dehydrate, rehydrate, DehydrateOptions, RehydrateOptions};
use fixtures::{write_jar_entries, JarEntry};
use proptest::prelude::*;
use zip::{CompressionMethod, ZipArchive};

/// Cases stay small so this test remains in default `cargo test` / CI.
const CASES: u32 = 32;
const MAX_JARS: usize = 3;
const MAX_ENTRIES: usize = 4;
const MAX_BYTES: usize = 64;

#[derive(Clone, Debug)]
struct FileSpec {
    name: String,
    data: Vec<u8>,
    method: CompressionMethod,
}

fn ident() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9]{0,5}"
}

fn unicode_bit() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just(""), Just("名"), Just("é")]
}

/// Relative UTF-8 names: no `..`, NUL, or empty segments.
fn entry_name() -> impl Strategy<Value = String> {
    prop_oneof![
        (ident(), unicode_bit()).prop_map(|(id, u)| format!("{id}{u}.txt")),
        (ident(), ident()).prop_map(|(dir, file)| format!("{dir}/{file}.bin")),
        (ident(), ident(), ident()).prop_map(|(a, b, file)| format!("{a}/{b}/{file}")),
    ]
}

fn file_spec() -> impl Strategy<Value = FileSpec> {
    (
        entry_name(),
        prop::collection::vec(any::<u8>(), 0..=MAX_BYTES),
        any::<bool>(),
    )
        .prop_map(|(name, data, stored)| FileSpec {
            name,
            data,
            method: if stored {
                CompressionMethod::Stored
            } else {
                CompressionMethod::Deflated
            },
        })
}

fn jar_entries() -> impl Strategy<Value = Vec<FileSpec>> {
    prop::collection::vec(file_spec(), 1..=MAX_ENTRIES).prop_map(|entries| {
        let mut seen = HashSet::new();
        entries
            .into_iter()
            .filter(|e| seen.insert(e.name.clone()))
            .collect()
    })
}

fn write_specs(path: &Path, specs: &[FileSpec]) {
    let entries: Vec<JarEntry<'_>> = specs
        .iter()
        .map(|s| JarEntry::File {
            name: &s.name,
            data: &s.data,
            method: s.method,
        })
        .collect();
    write_jar_entries(path, &entries);
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

/// Central-directory order of Unicode names (files only; this generator writes files).
fn entry_order(path: &Path) -> Vec<String> {
    let mut z = ZipArchive::new(File::open(path).unwrap()).unwrap();
    (0..z.len())
        .filter_map(|i| {
            let e = z.by_index(i).unwrap();
            if e.is_dir() {
                None
            } else {
                Some(e.name().to_string())
            }
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: CASES,
        fork: false,
        ..ProptestConfig::default()
    })]
    #[test]
    fn prop_entry_maps_equal_after_roundtrip(
        jars in prop::collection::vec(jar_entries(), 1..=MAX_JARS),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut inputs = Vec::new();
        for (i, specs) in jars.iter().enumerate() {
            let p = dir.path().join(format!("j{i}.jar"));
            write_specs(&p, specs);
            inputs.push(p);
        }

        let out = dir.path().join("out.ayz");
        dehydrate(&DehydrateOptions {
            output: out.clone(),
            inputs: inputs.clone(),
            ..DehydrateOptions::default()
        })
        .unwrap();

        let dest = dir.path().join("restored");
        rehydrate(&RehydrateOptions {
            input: out,
            dir: dest.clone(),
            ..RehydrateOptions::default()
        })
        .unwrap();

        for (i, _) in jars.iter().enumerate() {
            let src = &inputs[i];
            let restored = dest.join(format!("j{i}.jar"));
            prop_assert_eq!(entry_map(src), entry_map(&restored));
            prop_assert_eq!(entry_order(src), entry_order(&restored));
            prop_assert_eq!(
                std::fs::read(src).unwrap(),
                std::fs::read(&restored).unwrap(),
                "exact pack must restore bit-identical JAR bytes"
            );
        }
    }
}
