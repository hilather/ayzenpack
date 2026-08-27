use serde::{Deserialize, Serialize};

/// Manifest JSON `format` discriminator. Not `jded-manifest`.
pub const MANIFEST_FORMAT: &str = "ayzenpack-manifest";

/// MANIFEST JSON (v1). Unknown keys are ignored (no `deny_unknown_fields`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub hash_algo: String,
    pub mode: String,
    pub jars: Vec<Jar>,
    pub blobs: Vec<Blob>,
    pub stats: Stats,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Jar {
    pub name: String,
    pub source_path: String,
    pub source_size: u64,
    pub source_blake3: String,
    pub source_sha256: String,
    pub comment: String,
    pub signed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_mode: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_gid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_size: Option<u64>,
    /// BLAKE3 of bytes from the central directory through zip EOF.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_size: Option<u64>,
    /// Whole zip portion after `prefix` when locals cannot be sliced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_zip_blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_zip_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leading_pad_blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leading_pad_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nestedindexes: Vec<NestedIndex>,
    pub entries: Vec<Entry>,
}

impl Jar {
    /// Bit-identical splice: `raw_zip`, or `tail` plus every entry can resolve cdata
    /// without rebuilding (`cdata_blob`, `cdata_codec`, STORE, or empty dir).
    pub fn exact_restore(&self) -> bool {
        self.bit_identical_restore()
    }

    pub fn bit_identical_restore(&self) -> bool {
        if self.raw_zip_blob.is_some() {
            return true;
        }
        if self.tail_blob.is_none() {
            return false;
        }
        self.entries.iter().all(|e| self.slot_exact(e))
    }

    pub fn slot_exact(&self, e: &Entry) -> bool {
        if e.blob.is_some() && e.zip_index.is_some() {
            return false;
        }
        if let Some(i) = e.zip_index {
            return self
                .nestedindexes
                .get(i)
                .is_some_and(NestedIndex::bit_identical_restore);
        }
        e.can_exact_cdata()
    }

    pub fn nested_index(&self, i: usize) -> Result<&NestedIndex, crate::error::AyzenpackError> {
        self.nestedindexes.get(i).ok_or_else(|| {
            crate::error::AyzenpackError::FormatOwned(format!(
                "zip_index {i} out of range for {}",
                self.name
            ))
        })
    }

    /// Sliced metadata is present but at least one DEFLATE entry has no cdata copy/codec.
    pub fn metadata_rebuild(&self) -> bool {
        self.raw_zip_blob.is_none() && self.tail_blob.is_some() && !self.bit_identical_restore()
    }
}

impl Entry {
    pub(crate) fn can_exact_cdata(&self) -> bool {
        if self.blob.is_some() && self.zip_index.is_some() {
            return false;
        }
        if self.zip_index.is_some() {
            // Jar-level: look up nestedindexes[i].
            return false;
        }
        if self.cdata_blob.is_some() || self.cdata_codec.is_some() {
            return true;
        }
        if self.is_dir {
            // Exact-splice a directory only when both sizes are 0 (empty STORE).
            // A method-0 dir with leftover local cdata (csize != 0) is not a splice.
            // A payload dir (uncompressed_size != 0, fixture DIRC) is not a splice.
            // A method-8 empty DEFLATE dir (`03 00`) needs `cdata_codec` or rebuild.
            return self.uncompressed_size == 0 && self.compressed_size == 0;
        }
        // STORE splice only when local cdata length is the payload and we have
        // the uncompressed bytes. zip_index slots are handled on the Jar.
        self.method_code == 0
            && self.blob.is_some()
            && self.compressed_size == self.uncompressed_size
    }
}

/// Depth-1 child ZIP stencil (Jar fields minus `nestedindexes`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct NestedIndex {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leading_pad_blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leading_pad_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_size: Option<u64>,
    #[serde(default)]
    pub entries: Vec<Entry>,
}

impl NestedIndex {
    pub fn bit_identical_restore(&self) -> bool {
        self.tail_blob.is_some()
            && self
                .entries
                .iter()
                .all(|e| e.zip_index.is_none() && e.can_exact_cdata())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Blob {
    pub blake3: String,
    pub sha256: String,
    pub size: u64,
    pub ref_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub jar_count: u64,
    pub entry_count: u64,
    pub file_entry_count: u64,
    pub unique_blob_count: u64,
    pub bytes_in_jars: u64,
    pub bytes_uncompressed_entries: u64,
    pub bytes_unique_blobs: u64,
    pub dedup_ratio: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub blob: Option<String>,
    pub sha256: Option<String>,
    pub crc32: u32,
    pub method: String,
    pub method_code: u16,
    pub uncompressed_size: u64,
    pub compressed_size: u64,
    pub dos_date: u16,
    pub dos_time: u16,
    pub unix_mode: Option<u32>,
    pub utf8_flag: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_raw_hex: Option<String>,
    /// BLAKE3 of the original compressed payload. Legacy 0.1.6–0.1.8 / exotic methods.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cdata_blob: Option<String>,
    /// Re-encode content `blob` with this raw-deflate codec (`deflate-raw:flate2:<level>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cdata_codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_header_offset: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_header_hex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_header_blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_descriptor_hex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad_zeros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad_blob: Option<String>,
    /// File-absolute local header offset (ratarmount `offsetheader`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offsetheader: Option<u64>,
    /// File-absolute payload start (ratarmount `data_start`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_start: Option<u64>,
    /// Index into `jars[].nestedindexes[]`. JSON `blob` is null when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip_index: Option<usize>,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            name: String::new(),
            is_dir: false,
            blob: None,
            sha256: None,
            crc32: 0,
            method: "stored".into(),
            method_code: 0,
            uncompressed_size: 0,
            compressed_size: 0,
            dos_date: 0,
            dos_time: 0,
            unix_mode: None,
            utf8_flag: true,
            name_raw_hex: None,
            cdata_blob: None,
            cdata_codec: None,
            local_header_offset: None,
            local_header_hex: None,
            local_header_blob: None,
            data_descriptor_hex: None,
            pad_zeros: None,
            pad_blob: None,
            offsetheader: None,
            data_start: None,
            zip_index: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TINY_JSON: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/tiny.manifest.json"
    ));
    const SCHEMA_JSON: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/schemas/manifest.v1.schema.json"
    ));

    const EMPTY_BLAKE3: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn sample_file_entry() -> Entry {
        Entry {
            name: "shared.txt".into(),
            is_dir: false,
            blob: Some(EMPTY_BLAKE3.into()),
            sha256: Some(EMPTY_SHA256.into()),
            crc32: 0,
            method: "deflated".into(),
            method_code: 8,
            uncompressed_size: 0,
            compressed_size: 2,
            dos_date: 0,
            dos_time: 0,
            unix_mode: None,
            utf8_flag: true,
            name_raw_hex: None,
            cdata_blob: None,
            cdata_codec: None,
            local_header_offset: None,
            local_header_hex: None,
            local_header_blob: None,
            data_descriptor_hex: None,
            pad_zeros: None,
            pad_blob: None,
            offsetheader: None,
            data_start: None,
            zip_index: None,
        }
    }

    fn sample_manifest() -> Manifest {
        Manifest {
            format: MANIFEST_FORMAT.into(),
            version: 1,
            hash_algo: "blake3".into(),
            mode: "content".into(),
            jars: vec![Jar {
                name: "a.jar".into(),
                source_path: "fixtures/a.jar".into(),
                source_size: 1200,
                source_blake3: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .into(),
                source_sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                    .into(),
                comment: String::new(),
                signed: false,
                restore_path: None,
                restore_mode: None,
                restore_uid: None,
                restore_gid: None,
                prefix_blob: None,
                prefix_size: None,
                tail_blob: None,
                tail_size: None,
                raw_zip_blob: None,
                raw_zip_size: None,
                leading_pad_blob: None,
                leading_pad_size: None,
                nestedindexes: Vec::new(),
                entries: vec![sample_file_entry()],
            }],
            blobs: vec![Blob {
                blake3: EMPTY_BLAKE3.into(),
                sha256: EMPTY_SHA256.into(),
                size: 0,
                ref_count: 1,
            }],
            stats: Stats {
                jar_count: 1,
                entry_count: 1,
                file_entry_count: 1,
                unique_blob_count: 1,
                bytes_in_jars: 1200,
                bytes_uncompressed_entries: 0,
                bytes_unique_blobs: 0,
                dedup_ratio: 0.0,
            },
        }
    }

    fn assert_compact(s: &str) {
        assert!(!s.contains('\n'), "JSON must be compact (no newline): {s}");
        assert!(
            !s.contains(": ") && !s.contains(", "),
            "JSON must be compact (no spaces after :/,): {s}"
        );
    }

    /// First occurrence of `"key":` after `from`; panics if missing. Guards BTreeMap reordering.
    fn find_key_after(json: &str, from: usize, key: &str) -> usize {
        let needle = format!("\"{key}\":");
        let rel = json[from..]
            .find(&needle)
            .unwrap_or_else(|| panic!("missing key {key} after byte {from} in {json}"));
        from + rel
    }

    fn assert_key_order(json: &str, keys: &[&str]) {
        let mut pos = 0usize;
        for key in keys {
            let at = find_key_after(json, pos, key);
            pos = at + key.len();
        }
    }

    #[test]
    fn tiny_example_deserializes() {
        // Guards jded-manifest discriminator and schema identity drift.
        let m: Manifest = serde_json::from_str(TINY_JSON).unwrap();
        assert_eq!(m, sample_manifest());
        assert_eq!(m.format, MANIFEST_FORMAT);
        assert_eq!(m.format, "ayzenpack-manifest");
        assert_ne!(m.format, "jded-manifest");
        assert!(!TINY_JSON.contains("jded"));
        assert!(!TINY_JSON.contains("jded-manifest"));
        assert!(SCHEMA_JSON.contains(
            "https://github.com/hilather/ayzenpack/raw/main/schemas/manifest.v1.schema.json"
        ));
        assert!(SCHEMA_JSON.contains("\"const\": \"ayzenpack-manifest\""));
        assert!(SCHEMA_JSON.contains("\"additionalProperties\": false"));
        assert!(SCHEMA_JSON.contains("name_raw_hex"));
        assert!(SCHEMA_JSON.contains("prefix_blob"));
        assert!(SCHEMA_JSON.contains("prefix_size"));
        assert!(SCHEMA_JSON.contains("restore_path"));
        assert!(SCHEMA_JSON.contains("restore_mode"));
        assert!(SCHEMA_JSON.contains("restore_uid"));
        assert!(SCHEMA_JSON.contains("restore_gid"));
        assert!(SCHEMA_JSON.contains("tail_blob"));
        assert!(SCHEMA_JSON.contains("raw_zip_blob"));
        assert!(SCHEMA_JSON.contains("cdata_blob"));
        assert!(SCHEMA_JSON.contains("cdata_codec"));
        assert!(SCHEMA_JSON.contains("deflate-raw:(flate2:[1369]|zlib:[169]|stored)"));
        assert!(SCHEMA_JSON.contains("local_header_hex"));
        assert!(SCHEMA_JSON.contains("local_header_offset"));
        assert!(SCHEMA_JSON.contains("offsetheader"));
        assert!(SCHEMA_JSON.contains("data_start"));
        assert!(SCHEMA_JSON.contains("zip_index"));
        assert!(SCHEMA_JSON.contains("leading_pad_blob"));
        assert!(SCHEMA_JSON.contains("nestedindexes"));
        assert!(SCHEMA_JSON.contains("\"nestedindex\""));
        assert!(
            !SCHEMA_JSON.contains("oneOf"),
            "schema must not use oneOf"
        );
        assert!(!SCHEMA_JSON.contains("jded"));
        assert_eq!(m.version, 1);
        assert_eq!(m.hash_algo, "blake3");
        assert_eq!(m.mode, "content");
        assert_eq!(m.jars[0].entries[0].blob.as_deref(), Some(EMPTY_BLAKE3));
        assert_eq!(m.blobs[0].sha256, EMPTY_SHA256);
    }

    #[test]
    fn compact_serialize_then_deserialize_eq() {
        let m = sample_manifest();
        let bytes = serde_json::to_vec(&m).unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        assert_compact(s);
        let m2: Manifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(m, m2);
        assert_eq!(m2.format, "ayzenpack-manifest");
        assert_ne!(m2.format, "jded-manifest");
    }

    #[test]
    fn field_order_stable_for_known_struct() {
        // Guards BTreeMap / alpha key order on compact serialize.
        let m = sample_manifest();
        let s = serde_json::to_string(&m).unwrap();
        assert_compact(&s);
        assert!(s.starts_with("{\"format\":\"ayzenpack-manifest\""));
        assert!(!s.contains("jded-manifest"));
        assert_key_order(
            &s,
            &[
                "format",
                "version",
                "hash_algo",
                "mode",
                "jars",
                "blobs",
                "stats",
            ],
        );
        assert_key_order(
            &serde_json::to_string(&m.jars[0]).unwrap(),
            &[
                "name",
                "source_path",
                "source_size",
                "source_blake3",
                "source_sha256",
                "comment",
                "signed",
                "entries",
            ],
        );
        let mut with_prefix = m.jars[0].clone();
        with_prefix.prefix_blob = Some(EMPTY_BLAKE3.into());
        with_prefix.prefix_size = Some(42);
        let mut with_restore = m.jars[0].clone();
        with_restore.restore_path = Some("/abs/a.jar".into());
        with_restore.restore_mode = Some(0o644);
        with_restore.restore_uid = Some(1000);
        with_restore.restore_gid = Some(1000);
        assert_key_order(
            &serde_json::to_string(&with_restore).unwrap(),
            &[
                "signed",
                "restore_path",
                "restore_mode",
                "restore_uid",
                "restore_gid",
                "entries",
            ],
        );
        assert_key_order(
            &serde_json::to_string(&with_prefix).unwrap(),
            &["signed", "prefix_blob", "prefix_size", "entries"],
        );
        let mut with_exact = m.jars[0].clone();
        with_exact.tail_blob = Some(EMPTY_BLAKE3.into());
        with_exact.tail_size = Some(80);
        with_exact.raw_zip_blob = Some(EMPTY_BLAKE3.into());
        with_exact.raw_zip_size = Some(100);
        assert_key_order(
            &serde_json::to_string(&with_exact).unwrap(),
            &[
                "signed",
                "tail_blob",
                "tail_size",
                "raw_zip_blob",
                "raw_zip_size",
                "entries",
            ],
        );
        assert_key_order(
            &serde_json::to_string(&m.blobs[0]).unwrap(),
            &["blake3", "sha256", "size", "ref_count"],
        );
        assert_key_order(
            &serde_json::to_string(&m.stats).unwrap(),
            &[
                "jar_count",
                "entry_count",
                "file_entry_count",
                "unique_blob_count",
                "bytes_in_jars",
                "bytes_uncompressed_entries",
                "bytes_unique_blobs",
                "dedup_ratio",
            ],
        );
        assert_key_order(
            &serde_json::to_string(&m.jars[0].entries[0]).unwrap(),
            &[
                "name",
                "is_dir",
                "blob",
                "sha256",
                "crc32",
                "method",
                "method_code",
                "uncompressed_size",
                "compressed_size",
                "dos_date",
                "dos_time",
                "unix_mode",
                "utf8_flag",
            ],
        );
        let mut exact_ent = m.jars[0].entries[0].clone();
        exact_ent.cdata_blob = Some(EMPTY_BLAKE3.into());
        exact_ent.cdata_codec = Some("deflate-raw:flate2:6".into());
        exact_ent.local_header_offset = Some(0);
        exact_ent.local_header_hex = Some("504b0304".into());
        exact_ent.data_descriptor_hex = Some("504b0708".into());
        exact_ent.pad_zeros = Some(3);
        assert_key_order(
            &serde_json::to_string(&exact_ent).unwrap(),
            &[
                "utf8_flag",
                "cdata_blob",
                "cdata_codec",
                "local_header_offset",
                "local_header_hex",
                "data_descriptor_hex",
                "pad_zeros",
            ],
        );
    }

    #[test]
    fn dir_entry_blob_null_roundtrip() {
        let e = Entry {
            name: "com/example/".into(),
            is_dir: true,
            blob: None,
            sha256: None,
            crc32: 0,
            method: "stored".into(),
            method_code: 0,
            uncompressed_size: 0,
            compressed_size: 0,
            dos_date: 0,
            dos_time: 0,
            unix_mode: None,
            utf8_flag: true,
            name_raw_hex: None,
            cdata_blob: None,
            cdata_codec: None,
            local_header_offset: None,
            local_header_hex: None,
            local_header_blob: None,
            data_descriptor_hex: None,
            pad_zeros: None,
            pad_blob: None,
            offsetheader: None,
            data_start: None,
            zip_index: None,
        };
        let s = serde_json::to_string(&e).unwrap();
        assert_compact(&s);
        assert!(s.contains("\"blob\":null"), "{s}");
        assert!(s.contains("\"sha256\":null"), "{s}");
        let e2: Entry = serde_json::from_str(&s).unwrap();
        assert_eq!(e, e2);
        assert!(e2.is_dir);
        assert_eq!(e2.blob, None);
        assert_eq!(e2.sha256, None);
    }

    #[test]
    fn unknown_manifest_key_is_ignored_on_read() {
        // Guards deny_unknown_fields: a v1.1 extra field must not break list/rehydrate.
        let m = sample_manifest();
        let mut v = serde_json::to_value(&m).unwrap();
        v.as_object_mut().unwrap().insert(
            "future_v1_1_field".into(),
            serde_json::json!({"nested": true}),
        );
        v["jars"][0]
            .as_object_mut()
            .unwrap()
            .insert("extra_jar_key".into(), serde_json::json!(1));
        v["jars"][0]["entries"][0]
            .as_object_mut()
            .unwrap()
            .insert("extra_entry_key".into(), serde_json::json!("ok"));
        v["blobs"][0]
            .as_object_mut()
            .unwrap()
            .insert("extra_blob_key".into(), serde_json::json!(false));
        v["stats"]
            .as_object_mut()
            .unwrap()
            .insert("extra_stats_key".into(), serde_json::json!(0));
        let got: Manifest = serde_json::from_value(v).unwrap();
        assert_eq!(got, m);
    }

    #[test]
    fn name_raw_hex_omitted_when_none() {
        let e = sample_file_entry();
        assert_eq!(e.name_raw_hex, None);
        let s = serde_json::to_string(&e).unwrap();
        assert_compact(&s);
        assert!(
            !s.contains("name_raw_hex"),
            "None name_raw_hex must be omitted: {s}"
        );
        let e2: Entry = serde_json::from_str(&s).unwrap();
        assert_eq!(e2.name_raw_hex, None);

        let mut with = e.clone();
        with.name_raw_hex = Some("cafebabe".into());
        let s2 = serde_json::to_string(&with).unwrap();
        assert!(s2.contains("\"name_raw_hex\":\"cafebabe\""), "{s2}");
        assert_key_order(&s2, &["utf8_flag", "name_raw_hex"]);
        let round: Entry = serde_json::from_str(&s2).unwrap();
        assert_eq!(round, with);
    }

    fn dir_entry(uncomp: u64, csize: u64, method: u16) -> Entry {
        let mut e = sample_file_entry();
        e.name = "marked/".into();
        e.is_dir = true;
        e.blob = None;
        e.sha256 = None;
        e.method = if method == 0 {
            "stored".into()
        } else {
            "deflated".into()
        };
        e.method_code = method;
        e.uncompressed_size = uncomp;
        e.compressed_size = csize;
        e
    }

    #[test]
    fn can_exact_cdata_dir_requires_both_sizes_zero() {
        let empty = dir_entry(0, 0, 0);
        assert!(empty.can_exact_cdata(), "empty STORE dir is exact-splice");

        let payload = dir_entry(4, 4, 0);
        assert!(
            !payload.can_exact_cdata(),
            "method-0 dir with payload must not splice []"
        );

        let leftover_csize = dir_entry(0, 4, 0);
        assert!(
            !leftover_csize.can_exact_cdata(),
            "method-0 dir with leftover local cdata is not exact"
        );

        let mut store_file = sample_file_entry();
        store_file.method = "stored".into();
        store_file.method_code = 0;
        store_file.uncompressed_size = 4;
        store_file.compressed_size = 4;
        assert!(store_file.can_exact_cdata());
        store_file.compressed_size = 8;
        assert!(
            !store_file.can_exact_cdata(),
            "STORE file with csize != uncomp is not exact"
        );

        let maven = dir_entry(0, 2, 8);
        assert!(
            !maven.can_exact_cdata(),
            "empty DEFLATE dir needs codec or rebuild"
        );

        let mut with_blob = payload.clone();
        with_blob.cdata_blob = Some(EMPTY_BLAKE3.into());
        assert!(with_blob.can_exact_cdata(), "legacy cdata_blob still wins");

        let mut with_codec = maven.clone();
        with_codec.cdata_codec = Some("deflate-raw:flate2:6".into());
        assert!(with_codec.can_exact_cdata(), "cdata_codec still wins");
    }

    #[test]
    fn prefix_fields_omitted_when_none() {
        let jar = sample_manifest().jars[0].clone();
        assert_eq!(jar.prefix_blob, None);
        assert_eq!(jar.prefix_size, None);
        let s = serde_json::to_string(&jar).unwrap();
        assert_compact(&s);
        assert!(
            !s.contains("prefix_blob"),
            "None prefix_blob must be omitted: {s}"
        );
        assert!(
            !s.contains("prefix_size"),
            "None prefix_size must be omitted: {s}"
        );
        let jar2: Jar = serde_json::from_str(&s).unwrap();
        assert_eq!(jar2.prefix_blob, None);
        assert_eq!(jar2.prefix_size, None);
    }

    #[test]
    fn restore_fields_omitted_when_none() {
        let jar = sample_manifest().jars[0].clone();
        assert_eq!(jar.restore_path, None);
        assert_eq!(jar.restore_mode, None);
        assert_eq!(jar.restore_uid, None);
        assert_eq!(jar.restore_gid, None);
        let s = serde_json::to_string(&jar).unwrap();
        assert_compact(&s);
        for key in ["restore_path", "restore_mode", "restore_uid", "restore_gid"] {
            assert!(!s.contains(key), "None {key} must be omitted: {s}");
        }
        let jar2: Jar = serde_json::from_str(&s).unwrap();
        assert_eq!(jar2.restore_path, None);
        assert_eq!(jar2.restore_mode, None);
        assert_eq!(jar2.restore_uid, None);
        assert_eq!(jar2.restore_gid, None);
    }

    #[test]
    fn exact_fields_omitted_when_none() {
        let jar = sample_manifest().jars[0].clone();
        let s = serde_json::to_string(&jar).unwrap();
        assert_compact(&s);
        for key in [
            "tail_blob",
            "tail_size",
            "raw_zip_blob",
            "raw_zip_size",
            "cdata_blob",
            "cdata_codec",
            "local_header_offset",
            "local_header_hex",
            "local_header_blob",
            "data_descriptor_hex",
            "pad_zeros",
            "pad_blob",
        ] {
            assert!(!s.contains(key), "None {key} must be omitted: {s}");
        }
        let e = serde_json::to_string(&jar.entries[0]).unwrap();
        for key in [
            "cdata_blob",
            "cdata_codec",
            "local_header_offset",
            "local_header_hex",
            "local_header_blob",
            "data_descriptor_hex",
            "pad_zeros",
            "pad_blob",
        ] {
            assert!(!e.contains(key), "None {key} must be omitted: {e}");
        }
    }
}
