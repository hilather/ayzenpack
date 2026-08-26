# Design

ayzenpack dehydrates a set of JAR/ZIP/WAR/EAR files into one `.ayz` archive and rehydrates them. Dedup is **BLAKE3 of uncompressed entry bytes**. New packs are **format v2**: uncompressed header, record-aligned zstd BLOB groups, a final zstd frame of MANIFEST+END, an uncompressed TOC, and a 64-byte trailer. v1 files (one zstd frame, no TOC) still read and rehydrate.

This is the v2 format and library contract. CLI flag names live in `src/cli.rs` and the README. Manifest JSON field names are unchanged from v1.

| | |
|---|---|
| Binary / crate | `ayzenpack` |
| Extension | `.ayz` (recommended; magic identifies the file) |
| File magic | `AYZP\x02\x00\x00\x00` (v1: `AYZP\x01\x00\x00\x00`) |
| Trailer magic | `AYZPTLR1` |
| Manifest `format` | `ayzenpack-manifest` |
| MSRV | Rust 1.80, edition 2021, stable only |
| License | MIT OR Apache-2.0 |
| `unsafe` | forbidden |

---

## Why this shape

A set of application classpaths repeats the same `.class` and resource bytes tens or hundreds of times. `tar`/`zip` of a `lib/` directory does not content-dedup across archives. Exploding JARs to a class forest is slow, huge, and drops ZIP metadata (order, DOS time, CRC).

Storing **uncompressed** entry bytes into **zstd frames** usually beats concatenating already-deflated ZIP members: DEFLATE is not a concatenable input for a second compressor. Identical class files across JARs collapse to one BLOB. The manifest is inside the archive so rehydrate is one file in, N JARs out. v2 groups BLOBs into record-aligned frames (flush at 4 MiB uncompressed BLOB record bytes) so `list` can seek the last frame via the TOC instead of decoding every blob.

The tool never unpacks JARs to a forest of `.class` files, never talks to the network, and contains no `unsafe`.

---

## Pipeline

```
JARs  →  zip::ZipArchive by_index
      →  uncompressed entry bytes
      →  BLAKE3 + SHA-256 (one pass)
      →  first-seen? write BLOB : bump ref_count
      →  append ZipEntry metadata to Manifest
      →  compact MANIFEST + END
      →  zstd BLOB frames + MANIFEST/END frame + TOC + trailer
```

Rehydrate seeks the trailer, decodes every zstd frame in `payload_bytes` into a content-addressed directory (`xx/yy/hex`), then rebuilds each JAR. `list` on v2 seeks `header_total + manifest_zstd_off` and decodes only that last frame (`REC_MANIFEST` + JSON + `REC_END`).

Scan is sequential (central-directory order). `--jobs` hashes on a pool; a single writer emits BLOBs in first-seen order so END digest and `blobs[]` stay deterministic. `--sort-inputs` archives are byte-identical at any `--jobs`.

---

## Container

Little-endian. No protobuf. Manual length-prefixed records.

```
┌─────────────────────────────────────────────┐
│  FileHeader (uncompressed)                  │
│    magic[8]  header_len:u32  header_json    │
├─────────────────────────────────────────────┤
│  zstd frames (v2): record-aligned BLOB      │
│    groups, flush at 4 MiB uncompressed      │
│    BLOB record bytes                        │
├─────────────────────────────────────────────┤
│  final zstd frame: MANIFEST + END only      │
├─────────────────────────────────────────────┤
│  TOC (uncompressed) AYZPTOC2                │
├─────────────────────────────────────────────┤
│  Trailer (uncompressed, last 64 bytes)      │
└─────────────────────────────────────────────┘
```

v1 is one zstd frame of BLOB* + MANIFEST + END and no TOC (`toc_len = 0`). An empty v2 pack (no BLOBs) is **one** zstd frame of MANIFEST+END; do not append MANIFEST onto a partial blob frame.

### File header

| Offset | Size | Field |
|--------|------|--------|
| 0 | 8 | `AYZP{ver}\0\0\0` derived from `header.version` |
| 8 | 4 | `header_len` u32le |
| 12 | `header_len` | UTF-8 JSON, no NUL pad |

Unknown header keys are ignored. Write versions `{1,2}`. Read: `magic[0..4]==AYZP`, `magic[4]∈{1,2}`, `magic[5..8]==0`, and `header.version == u32::from(magic[4]) == trailer.version`. Version `0` is invalid. Version byte `> 2` is `unsupported version`. Magic / JSON / trailer disagreement is `VersionSkew` (the file is still an ayzenpack file).

```json
{
  "format": "ayzenpack",
  "version": 2,
  "hash": "blake3",
  "sha256": true,
  "mode": "content",
  "zstd_level": 3,
  "created_unix": 1710000000,
  "tool": "ayzenpack",
  "tool_version": "0.2.0"
}
```

`--sort-inputs` forces `created_unix` to `0`. `tool_version` is `CARGO_PKG_VERSION`.

### Records (inside zstd)

| Byte | Name | Payload |
|-----:|------|---------|
| `0x01` | BLOB | `blake3[32] + size:u64le + bytes[size]` |
| `0x02` | MANIFEST | `size:u64le + json[size]` |
| `0x03` | END | `blake3[32]` of concat(first-seen blob hashes) |

BLOBs before MANIFEST. Exactly one MANIFEST. Exactly one END, last. Empty blobs are valid. Duplicate BLAKE3 BLOBs are not written. Unknown type bytes error. Default is **grouped** frames, not per-blob frames.

### TOC (v2, uncompressed)

```
"AYZPTOC2" n:u32le
n × { blake3[32], zstd_off:u64le, zstd_len:u64le, rec_off:u64le }
manifest_zstd_off:u64le  manifest_zstd_len:u64le
```

Length = `28 + n*56`. Offsets are **payload-relative** (0 = first zstd byte after the header). `manifest_zstd_len` is the compressed last-frame size — never a copy of `Trailer.manifest_len` (uncompressed JSON). Pending `{blake3,zstd_off,rec_off}` during a frame; back-fill `zstd_len` after `Encoder::finish` + `BufWriter::flush`.

### Trailer (64 bytes at EOF)

| Off | Size | Field |
|-----|------|--------|
| 0 | 8 | `AYZPTLR1` |
| 8 | 8 | `payload_bytes` — sum of zstd frames only |
| 16 | 8 | `manifest_len` (uncompressed JSON) |
| 24 | 8 | `blob_count` |
| 32 | 8 | `blob_bytes` — sum of uncompressed blob sizes |
| 40 | 8 | `jar_count` |
| 48 | 4 | `header_len` (repeat) |
| 52 | 4 | `version` = 1 or 2 |
| 56 | 8 | v1: reserved zero. v2: `toc_len` u64le |

`payload_bytes` is the sum of finished zstd frames, each measured **after** `Encoder::finish()` + `BufWriter::flush()`. `file_len = header_total + payload_bytes + toc_len + 64`. `header_total` is `12+header_len` / `stream_position` after `read_header`.

Read invariant (checked-sub): `expected_toc = file_len - 64 - header_total - payload_bytes` and `trailer.toc_len == expected_toc`. v1: `toc_len == 0`. v2: `toc_len == 28+n*56` and `>= 28`.

Reader: `seek(End(-64))`, parse trailer, `seek(Start(0))`, parse header, check versions + `toc_len`, `take(payload_bytes)`. v1: `.single_frame()`. v2: multi-frame (no `single_frame`).

---

## Hashing

| Algo | Role |
|------|------|
| BLAKE3 | CAS id, BLOB header, CAS filenames, END digest |
| SHA-256 | Manifest `sha256` fields, `verify` double-check |
| CRC-32 | copied from the source ZIP header; rebuild asserts `crc32fast(bytes) == recorded` |

Dehydrate hashes both BLAKE3 and SHA-256 in **one chunk loop** over the entry `Vec` (`hash_both`). Do not re-read the ZIP entry.

Empty blob:

- BLAKE3 `af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262`
- SHA-256 `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`

Hex is lowercase on write, mixed-case accepted on read.

---

## Manifest

Compact JSON inside the archive. Sidecar may be pretty (`--write-sidecar-manifest` / `--pretty-manifest`). Snake_case field names are the v1 contract — do not rename them. Serde does **not** `deny_unknown_fields`; extra keys in a future v1.1 archive must still `list` / `rehydrate`. Schema `additionalProperties: false` is a writer check.

Canonical schema: [`schemas/manifest.v1.schema.json`](schemas/manifest.v1.schema.json). Example: [`examples/tiny.manifest.json`](examples/tiny.manifest.json).

Root: `format`, `version`, `hash_algo`, `mode`, `jars[]`, `blobs[]`, `stats`.

`blobs[]` is first-seen order (must match BLOB records and END). `jars[].entries[]` is central-directory order. Directory entries have `blob: null`. `jars[].name` is a single path segment; `..`, `/`, `\` are rejected.

v1 **content** rebuild (old archives) uses `name` (Unicode from `ZipFile::name()`), `is_dir`, `blob`, `crc32`, `dos_*` via `DateTime::try_from_msdos` with `DateTime::default()` fallback, and `unix_mode`. `utf8_flag` is recorded only. `name_raw_hex` is not used on write. `method` is advisory: rebuilt files deflate, directories store, unless `--store-all`.

New packs also store optional exact-reconstruction fields (omitted when absent, same style as `prefix_blob`). Unknown keys stay ignored on read.

`--restore-paths` dehydrate adds optional `jars[].restore_path` (canonical absolute path), `restore_mode`, and on Unix `restore_uid` / `restore_gid`. Omitted when the flag is off. Default rehydrate still writes `dir/name`. `--restore-paths` rehydrate writes `restore_path` (overwrites; dest symlink is replaced, not followed).

---

## Reconstruction

New packs default to **metadata-only** exact restore. Uncompressed entry `blob`s are still stored for listing, CRC checks, and cross-jar content dedup. The original DEFLATE payload is **not** stored a second time.

Per file entry, dehydrate records local-header metadata (`local_header_hex` / `local_header_blob`, `data_descriptor_hex`, `pad_*`, `local_header_offset`) plus one of:

- **STORE:** no `cdata_blob`; rehydrate splices the content `blob`
- **DEFLATE codec hit:** optional `cdata_codec` (`deflate-raw:flate2:<level>`) when a pinned flate2 raw-deflate trial matches the original `cdata` byte-for-byte (GPBF bits 1–2 as a level hint, then 6, 9, 1). Rehydrate encodes and splices at the original offsets. `source_blake3` / `source_sha256` / `source_size` still match.
- **DEFLATE codec miss:** neither `cdata_blob` nor `cdata_codec`. Rehydrate rebuilds a valid ZIP: same names/order/timestamps/extras, new compressed sizes, patched local header / data descriptor / CD / EOCD (and Zip64 extras that already exist). **Do not** claim `source_*` match. A signed JAR on this path uses the existing “rebuild will break the signature” warning.
- **Legacy `cdata_blob`:** 0.1.6–0.1.8 packs, plus exotic / unreproducible methods (non-STORE/DEFLATE, or a directory with actual uncompressed payload). Maven/Java empty DEFLATE directories (`03 00`, usize 0) are a codec hit/miss, not exotic. Resolution order: `cdata_blob`, else `cdata_codec`, else STORE/content blob, else rebuild.

Per jar: `tail_blob` / `tail_size` is bytes from the start of the central directory through zip EOF.

If a zip cannot be sliced cleanly (spanning, parse failure, ZipArchive entry count ≠ CD count): store `raw_zip_blob` of the entire zip portion after the prefix and copy it. `raw_zip` is not the deflate-miss path.

**Old archives** (no tail / `raw_zip`): keep the 0.1.x `ZipWriter` path. `--verbatim` is not a CLI flag.

```
∀ jar (codec-hit / STORE / cdata_blob / raw_zip):
  restored_bytes == source_bytes
  blake3(restored) == jars[].source_blake3
  sha256(restored) == jars[].source_sha256
  len(restored) == jars[].source_size

∀ jar (metadata rebuild):
  ZipArchive opens; names, CD order, uncompressed bytes match
  source_* are the original file and are not verified
```

Invalid DOS pairs including `0,0` still fall back to 1980-01-01 on the content path. Never `from_msdos_unchecked`.

---

## Executable / prefixed JARs

Spring Boot “fully executable” JARs (`spring-boot-maven-plugin` `executable: true` / `bootJar { launchScript() }`) prepend the official `launch.script` before a ZIP (often Zip64). The file starts with `#!/bin/bash`, the Spring Boot banner, `### BEGIN INIT INFO` / chkconfig, and ends with `exit 0` — not `PK\x03\x04`. Placeholders like `{{mode:auto}}` are already substituted in a real build.

Detection uses no CLI flag. If the file does not start with ZIP magic, the prefix ends at the central directory's first local header (CD min local offset, or that offset after `zip -A` made it file-absolute) — not the first `PK\x03\x04` in the stub. Prefix bytes are `[0, first_real_lh)` within 16 MiB. Then try, in order:

1. **Unadjusted** (Spring default): `ZipArchive` through `ZipView` shifted to the real first local header. ZIP offsets are relative to the ZIP start. This is what `file` sees after the script is deleted.
2. **Adjusted** (`zip -A`): if that open fails, open the full file (no `ZipView` shift). CD and local-header offsets are already file-absolute.

A file with no local headers stays `NotZip` except an empty prefixed archive (EOCD-only). Unadjusted empty archives use EOCD extra-data math. After `zip -A` on an empty archive, extra is 0 and the recorded CD offset is the prefix (file-absolute EOCD). 0.1.4 extra-data math alone is not sufficient for non-empty `zip -A` / Zip64: `extra == 0` (or inflated by the Zip64 footer) and `confirm_zip_at(0)` reads `#!` / ELF.

```
extra = (eocd_file_offset - cd_size) - recorded_cd_offset
```

The prefix is stored as a first-seen CAS BLOB (same `hash_both` path as entry payloads). Shared launchers across JARs dedup (`ref_count > 1`). Manifest `jars[]` may include optional `prefix_blob` (hex BLAKE3) and `prefix_size`. Omitted on normal ZIPs so old archives still list/rehydrate.

Rehydrate writes the prefix bytes first. Bit-identical packs then splice locals + tail (or `raw_zip`) so the file stays `[official launch.script][zip]` with original offsets. Metadata-rebuild packs rewrite the zip portion after the prefix and patch CD offsets (zip-relative or file-absolute after `zip -A`). On Unix the restored file is `chmod 0755` so it stays executable.

`source_blake3` / `source_sha256` / `source_size` remain hashes/size of the **whole** input (prefix + ZIP) and are checked after bit-identical restore only.

Nested `BOOT-INF/lib/*.jar` entries stay opaque blobs.

---

## Signed JARs

`META-INF/*.SF` plus `*.RSA` / `*.DSA` / `*.EC` digest compressed or stored bytes. Bit-identical restore keeps those bytes, so file-level signatures (JAR `.SF`, SHA256SUMS, cosign, vendor checksums) survive. Metadata rebuild changes compressed sizes, so those signatures will not verify. ayzenpack does not re-sign.

Dehydrate still notes signed JARs and packs. `--fail-on-signed` aborts. `--strict` does not promote the signed notice. The “rebuild will break the signature” warning is for metadata-rebuild jars and the content/`ZipWriter` fallback (not for codec-hit / STORE / `cdata_blob` / `raw_zip` restores).

---

## Library

```rust
pub fn dehydrate(opts: &DehydrateOptions) -> Result<DehydrateSummary>;
pub fn rehydrate(opts: &RehydrateOptions) -> Result<()>;
pub fn list(input: &Path) -> Result<Manifest>;
pub fn verify(input: &Path) -> Result<()>;
```

Options structs are the lib contract; they do not read process-global state. Field tables and a YAML job-file loader: [docs/library.md](docs/library.md).

CLI exit codes: clap usage → 2. `verify` maps `HashMismatch` and integrity `Format` to **3**; I/O and “not an archive” stay **1**. Every other subcommand maps those variants to **1**.

---

## Determinism

Same inputs + `--sort-inputs` + same zstd level + same tool version → byte-identical `.ayz` when:

- ZIP iteration is `by_index` (central-directory order), never name-sorted
- JSON is serde structs in schema field order
- `--sort-inputs` forces `created_unix` to `0`
- Hex is lowercase on write
- BLOB records are first-seen order regardless of `--jobs`

---

## Memory

Dehydrate: one entry `Vec` at a time (or a bounded inflight set when `--jobs > 1`). `ScannedEntry` is metadata only. Peak RSS ≈ largest entry + zstd window + 256 KiB `BufWriter` + seen-hash set + in-memory manifest structs.

`--max-entry-bytes` (default 2 GiB − 1) is the zip-bomb cap. `--max-inflight-bytes` (default 64 MiB) caps uncompressed buffers in the hash pipeline; a single entry larger than the cap is still admitted so the pipeline cannot stall.

Rehydrate spills blobs to a CAS directory. Peak is the largest blob being copied (entry, legacy `cdata`, tail, or `raw_zip`) plus the zstd decoder buffer.

---

## Security

| Threat | Mitigation |
|--------|------------|
| Zip-slip (`../` in entry or `jar.name`) | Reject `jar.name` with `/`, `\`, `..`. Skip entry components `..`. |
| Zip bomb | `--max-entry-bytes` |
| Encrypted ZIP | Fail with path; do not decrypt |
| Signed JAR silently broken | Detect; warn; exact restore keeps bytes; `--fail-on-signed` |
| Traversal via `--cas-dir` / output | `--clean` only deletes names we write. CAS paths are hex |
| `unsafe` | `forbid(unsafe_code)` |
| SSRF | No network in the crate |

Treat a `.ayz` as sensitive as the input JARs: it contains file contents and original `source_path` strings.

---

## Non-goals (v2)

- Recursively exploding nested JARs
- A `--verbatim` / `--exact-cdata` CLI flag (metadata-only is the default; bit-identical when the codec hits)
- HTTP CAS, S3, split archives, GUI, Maven/Gradle plugins
- Renaming v1 manifest fields (`blob`, `uncompressed_size`, `local_header_offset`, …)
- Per-blob zstd frames as the default
- zstd-framed / edition-2024 dependencies
- Tokio, reqwest, openssl

Future: `--explode-nested`, a class-file zstd dictionary.
