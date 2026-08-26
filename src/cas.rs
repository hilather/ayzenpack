use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{AyzenpackError, Result};
use crate::hashutil::hex_lower;

fn blob_path(dir: &Path, hash: &[u8; 32]) -> PathBuf {
    let hex = hex_lower(hash);
    dir.join(&hex[0..2]).join(&hex[2..4]).join(&hex)
}

fn io_err(source: std::io::Error, path: PathBuf) -> AyzenpackError {
    AyzenpackError::Io {
        source,
        path: Some(path),
    }
}

pub fn put(dir: &Path, hash: &[u8; 32], bytes: &[u8]) -> Result<()> {
    let path = blob_path(dir, hash);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| io_err(source, parent.to_path_buf()))?;
    }
    fs::write(&path, bytes).map_err(|source| io_err(source, path))
}

pub fn get(dir: &Path, hash: &[u8; 32]) -> Result<Vec<u8>> {
    let path = blob_path(dir, hash);
    fs::read(&path).map_err(|source| io_err(source, path))
}

pub fn exists(dir: &Path, hash: &[u8; 32]) -> bool {
    blob_path(dir, hash).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_hash() -> [u8; 32] {
        let mut hash = [0u8; 32];
        hash[0] = 0xab;
        hash[1] = 0xcd;
        hash[2] = 0xef;
        hash
    }

    #[test]
    fn put_get_roundtrip() {
        // Guards a broken path join or a write that cannot be read back.
        let dir = tempdir().unwrap();
        let hash = sample_hash();
        let bytes = b"hello cas";
        put(dir.path(), &hash, bytes).unwrap();
        assert_eq!(get(dir.path(), &hash).unwrap(), bytes);
        assert!(exists(dir.path(), &hash));
        // Same-hash put is idempotent (overwrite OK).
        put(dir.path(), &hash, bytes).unwrap();
        assert_eq!(get(dir.path(), &hash).unwrap(), bytes);
    }

    #[test]
    fn get_missing_errors() {
        // Guards treating a missing blob as empty success.
        let dir = tempdir().unwrap();
        let hash = sample_hash();
        let err = get(dir.path(), &hash).unwrap_err();
        match err {
            AyzenpackError::Io { path, source } => {
                assert!(path.is_some(), "missing-blob get must include the CAS path");
                let path = path.unwrap();
                let hex = hex_lower(&hash);
                assert!(
                    path.ends_with(&hex),
                    "error path {path:?} should end with {hex}"
                );
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected Io with path, got {other:?}"),
        }
        assert!(!exists(dir.path(), &hash));
    }

    #[test]
    fn layout_uses_two_two_prefix_dirs() {
        // Guards dumping all blobs into one flat directory (filesystem limits).
        let dir = tempdir().unwrap();
        let hash = sample_hash();
        put(dir.path(), &hash, b"layout").unwrap();
        let hex = hex_lower(&hash);
        let expected = dir.path().join(&hex[0..2]).join(&hex[2..4]).join(&hex);
        assert!(expected.is_file(), "expected CAS path {expected:?}");
        assert_eq!(fs::read(&expected).unwrap(), b"layout");
        assert!(
            !dir.path().join(&hex).is_file(),
            "blob must not sit in a flat dir/{hex}"
        );
        assert!(
            !dir.path().join(&hex[0..2]).join(&hex).is_file(),
            "blob must not use a single prefix dir"
        );
        let rel = expected.strip_prefix(dir.path()).unwrap();
        let comps: Vec<_> = rel.iter().map(|c| c.to_str().unwrap()).collect();
        assert_eq!(comps, vec![&hex[0..2], &hex[2..4], hex.as_str()]);
        assert_eq!(comps[0].len(), 2);
        assert_eq!(comps[1].len(), 2);
        assert_eq!(comps[2].len(), 64);
    }

    #[test]
    fn hex_is_lowercase_filenames() {
        // Guards uppercase hex filenames (Linux CAS paths are case-sensitive).
        let dir = tempdir().unwrap();
        let hash = sample_hash();
        put(dir.path(), &hash, b"lower").unwrap();
        let hex = hex_lower(&hash);
        assert!(
            hex.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "hex_lower must emit lowercase: {hex}"
        );
        assert!(!hex.chars().any(|c| c.is_ascii_uppercase()));

        let mut names = Vec::new();
        let xx = fs::read_dir(dir.path()).unwrap().next().unwrap().unwrap();
        names.push(xx.file_name().into_string().unwrap());
        let yy = fs::read_dir(xx.path()).unwrap().next().unwrap().unwrap();
        names.push(yy.file_name().into_string().unwrap());
        let file = fs::read_dir(yy.path()).unwrap().next().unwrap().unwrap();
        names.push(file.file_name().into_string().unwrap());

        assert_eq!(names[0], hex[0..2]);
        assert_eq!(names[1], hex[2..4]);
        assert_eq!(names[2], hex);
        for name in &names {
            assert!(
                name.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
                "CAS path component must be lowercase hex: {name}"
            );
        }
        // NTFS is case-insensitive: Path != is case-sensitive but is_file()
        // follows the same bytes we wrote. Only assert a distinct uppercase
        // path on case-sensitive filesystems.
        #[cfg(unix)]
        {
            let upper = dir
                .path()
                .join(hex[0..2].to_ascii_uppercase())
                .join(hex[2..4].to_ascii_uppercase())
                .join(hex.to_ascii_uppercase());
            if upper != dir.path().join(&hex[0..2]).join(&hex[2..4]).join(&hex) {
                assert!(!upper.is_file(), "must not write uppercase hex filenames");
            }
        }
    }
}
