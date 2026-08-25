//! Shared ZipWriter helpers for in-test JAR fixtures (no JDK).
#![allow(dead_code)]

use std::fs::File;
use std::io::Write;
use std::path::Path;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

pub fn write_jar(path: &Path, files: &[(&str, &[u8])]) {
    let mut z = ZipWriter::new(File::create(path).unwrap());
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in files {
        z.start_file(*name, opts).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap();
}

pub enum JarEntry<'a> {
    File {
        name: &'a str,
        data: &'a [u8],
        method: CompressionMethod,
    },
    Dir {
        name: &'a str,
    },
}

pub fn write_jar_entries(path: &Path, entries: &[JarEntry<'_>]) {
    write_jar_entries_with_mtime(path, entries, DateTime::default());
}

pub fn write_jar_entries_with_mtime(path: &Path, entries: &[JarEntry<'_>], mtime: DateTime) {
    let mut z = ZipWriter::new(File::create(path).unwrap());
    for entry in entries {
        match entry {
            JarEntry::File { name, data, method } => {
                let opts = SimpleFileOptions::default()
                    .compression_method(*method)
                    .last_modified_time(mtime);
                z.start_file(*name, opts).unwrap();
                z.write_all(data).unwrap();
            }
            JarEntry::Dir { name } => {
                let opts = SimpleFileOptions::default().last_modified_time(mtime);
                z.add_directory(*name, opts).unwrap();
            }
        }
    }
    z.finish().unwrap();
}
