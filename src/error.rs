#[derive(Debug, thiserror::Error)]
pub enum AyzenpackError {
    #[error("I/O error{path}: {source}", path = path.as_ref().map(|p| format!(" ({})", p.display())).unwrap_or_default())]
    Io {
        #[source]
        source: std::io::Error,
        path: Option<std::path::PathBuf>,
    },
    #[error("ZIP error ({path}): {source}")]
    Zip {
        source: zip::result::ZipError,
        path: std::path::PathBuf,
    },
    #[error("{0}")]
    Format(&'static str),
    #[error("{0}")]
    FormatOwned(String),
    #[error("hash mismatch: {0}")]
    HashMismatch(String),
    #[error("{0}")]
    Usage(String),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not a ZIP: {path}")]
    NotZip { path: std::path::PathBuf },
    #[error("encrypted ZIP not supported: {path}")]
    Encrypted { path: std::path::PathBuf },
    #[error("entry exceeds --max-entry-bytes ({max}): {path}!{name} ({size} bytes)")]
    EntryTooLarge {
        path: std::path::PathBuf,
        name: String,
        size: u64,
        max: u64,
    },
    #[error("unsupported ayzenpack version {0}")]
    UnsupportedVersion(u8),
    #[error("not an ayzenpack file")]
    NotAyzenpack,
    #[error("path rejected: {0}")]
    UnsafePath(String),
}

pub type Result<T> = std::result::Result<T, AyzenpackError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_display_smoke() {
        // Guards silent message-format drift used by later CLI exit mapping.
        let err = AyzenpackError::Format("truncated trailer");
        assert_eq!(err.to_string(), "truncated trailer");
    }

    #[test]
    fn format_owned_display_smoke() {
        let err = AyzenpackError::FormatOwned("bad record".into());
        assert_eq!(err.to_string(), "bad record");
    }

    #[test]
    fn hash_mismatch_display_smoke() {
        let err = AyzenpackError::HashMismatch("blob abc".into());
        assert_eq!(err.to_string(), "hash mismatch: blob abc");
    }

    #[test]
    fn not_ayzenpack_display_smoke() {
        let err = AyzenpackError::NotAyzenpack;
        assert_eq!(err.to_string(), "not an ayzenpack file");
        assert!(!err.to_string().contains("jded"));
    }
}
