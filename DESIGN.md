# Design

ayzenpack dehydrates a set of JAR/ZIP/WAR/EAR files into one `.ayz` archive and rehydrates them. Dedup is **BLAKE3 of uncompressed entry bytes**. The container is a single file: uncompressed header, one zstd frame of records, uncompressed 64-byte trailer.

This is the v1 format and library contract. CLI flag names live in `src/cli.rs` and the README.

| | |
|---|---|
| Binary / crate | `ayzenpack` |
| Extension | `.ayz` (recommended; magic identifies the file) |
| File magic | `AYZP\x01\x00\x00\x00` |
| Trailer magic | `AYZPTLR1` |
| Manifest `format` | `ayzenpack-manifest` |
| MSRV | Rust 1.80, edition 2021, stable only |
| License | MIT OR Apache-2.0 |
| `unsafe` | forbidden |

---

## Why this shape

A set of application classpaths repeats the same `.class` and resource bytes tens or hundreds of times. `tar`/`zip` of a `lib/` directory does not content-dedup across archives. Exploding JARs to a class forest is slow, huge, and drops ZIP metadata (order, DOS time, CRC).

Storing **uncompressed** entry bytes into **one zstd frame** usually beats concatenating already-deflated ZIP members: DEFLATE is not a concatenable input for a second compressor. Identical class files across JARs collapse to one BLOB. The manifest is inside the archive so rehydrate is one file in, N JARs out.

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
      →  zstd frame + trailer
```

Rehydrate seeks the trailer, decodes the zstd frame into a content-addressed directory (`xx/yy/hex`), then rebuilds each JAR with `ZipWriter`.

Scan is sequential (central-directory order). `--jobs` hashes on a pool; a single writer emits BLOBs in first-seen order so END digest and `blobs[]` stay deterministic. `--sort-inputs` archives are byte-identical at any `--jobs`.

---

## Container

Little-endian. No protobuf. Manual length-prefixed records.

```
┌─────────────────────────────────────────────┐
│  FileHeader (uncompressed)                  │
│    magic[8]  header_len:u32  header_json    │
├─────────────────────────────────────────────┤
│  zstd frame (one standard frame)            │
│    Record*   type:u8 + payload              │
├─────────────────────────────────────────────┤
│  Trailer (uncompressed, last 64 bytes)      │
└─────────────────────────────────────────────┘
```

### File header

| Offset | Size | Field |
|--------|------|--------|
| 0 | 8 | `AYZP\x01\x00\x00\x00` |
| 8 | 4 | `header_len` u32le |
| 12 | `header_len` | UTF-8 JSON, no NUL pad |

Unknown header keys are ignored. Version byte (offset 4) `> 1` is `unsupported version`. Version `0` is invalid.

```json
{
  "format": "ayzenpack",
  "version": 1,
  "hash": "blake3",
  "sha256": true,
  "mode": "content",
  "zstd_level": 3,
  "created_unix": 1710000000,
  "tool": "ayzenpack",
  "tool_version": "0.1.8"
}
```

`--sort-inputs` forces `created_unix` to `0`. `tool_version` is `CARGO_PKG_VERSION`.

### Records (inside zstd)

| Byte | Name | Payload |
|-----:|------|---------|
| `0x01` | BLOB | `blake3[32] + size:u64le + bytes[size]` |
| `0x02` | MANIFEST | `size:u64le + json[size]` |
| `0x03` | END | `blake3[32]` of concat(first-seen blob hashes) |

BLOBs before MANIFEST. Exactly one MANIFEST. Exactly one END, last. Empty blobs are valid. Duplicate BLAKE3 BLOBs are not written. Unknown type bytes error.

### Trailer (64 bytes at EOF)

| Off | Size | Field |
|-----|------|--------|
| 0 | 8 | `AYZPTLR1` |
| 8 | 8 | `payload_bytes` — zstd frame size |
| 16 | 8 | `manifest_len` |
| 24 | 8 | `blob_count` |
| 32 | 8 | `blob_bytes` — sum of uncompressed blob sizes |
| 40 | 8 | `jar_count` |
| 48 | 4 | `header_len` (repeat) |
| 52 | 4 | `version` = 1 |
| 56 | 8 | reserved, zero |

`payload_bytes` is measured **after** `Encoder::finish()` + `BufWriter::flush()`, **before** the trailer is written: `file_len - header_total`. Never write the trailer to a raw `File` while the `BufWriter` still holds zstd bytes.

Reader: `seek(End(-64))`, parse trailer, `seek(Start(0))`, parse header, decode the middle as one zstd frame of length `payload_bytes`.

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

New packs default to **bit-identical** restore. After dehydrate + rehydrate, `source_blake3` / `source_sha256` / `source_size` match and `cmp` is equal.

Uncompressed entry `blob`s are still stored for listing, CRC checks, and cross-jar content dedup (`BOOT-INF/lib`). Nested JARs stay opaque entry blobs.

Per file entry, optional `cdata_blob` is the BLAKE3 of the original compressed payload (read by seeking to the local header from the central directory — not the zip crate's inflating reader). For `method=stored`, `cdata_blob` and `blob` are the same CAS object (one put, two refs).

Each entry also records enough to emit the original local record:

- raw local header (`30 + name + extra`) as `local_header_hex` when small, else `local_header_blob`
- optional `data_descriptor_hex` when GPBF bit 3 is set
- pad after the record to the next local (`pad_zeros` if all zeros, else `pad_blob`)
- zip-relative `local_header_offset`

Per jar: `tail_blob` / `tail_size` is bytes from the start of the central directory through zip EOF (CD + Zip64 EOCD + locator + EOCD + archive comment). Copying the tail is simpler than reconstructing CD extras, which often differ from local extras.

Exact rehydrate: write prefix (existing path), write each local at its offset, write `cdata`, write descriptor, pad, write tail, `chmod 0755` if a prefix is present, then verify whole-file hashes. Fail on mismatch.

If a zip cannot be sliced cleanly (spanning, parse failure, ZipArchive entry count ≠ CD count): store `raw_zip_blob` of the entire zip portion after the prefix and copy it. Prefix stays a separate CAS blob. Whole-file hashes are still verified.

**Old archives** (no `cdata` / `tail` / `raw_zip`): keep the 0.1.x `ZipWriter` path. Functional identity only — deflate bitstream, extras, descriptors, and alignment are not preserved. `--verbatim` is not a CLI flag; new packs do not need one.

```
∀ jar (exact pack):
  restored_bytes == source_bytes
  blake3(restored) == jars[].source_blake3
  sha256(restored) == jars[].source_sha256
  len(restored) == jars[].source_size
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

Rehydrate writes the prefix bytes first. Exact packs then splice locals + tail (or `raw_zip`) so the file stays `[official launch.script][zip]` with original offsets. On Unix the restored file is `chmod 0755` so it stays executable.

`source_blake3` / `source_sha256` / `source_size` remain hashes/size of the **whole** input (prefix + ZIP) and are checked after exact restore.

Nested `BOOT-INF/lib/*.jar` entries stay opaque blobs.

---

## Signed JARs

`META-INF/*.SF` plus `*.RSA` / `*.DSA` / `*.EC` digest compressed or stored bytes. Exact restore keeps those bytes, so file-level signatures (JAR `.SF`, SHA256SUMS, cosign, vendor checksums) survive. ayzenpack does not re-sign.

Dehydrate still notes signed JARs and packs. `--fail-on-signed` aborts. `--strict` does not promote the signed notice. The “rebuild will break the signature” warning is only for the content/`ZipWriter` fallback.

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

Rehydrate spills blobs to a CAS directory. Peak is the largest blob being copied (entry, `cdata`, tail, or `raw_zip`) plus the zstd decoder buffer.

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

## Non-goals (v1)

- Recursively exploding nested JARs
- A `--verbatim` CLI flag (new packs are already bit-identical)
- HTTP CAS, S3, split archives, GUI, Maven/Gradle plugins
- Renaming v1 manifest fields
- Tokio, reqwest, openssl

Future: `--explode-nested`, seekable zstd + TOC for `list` without full decode, a class-file zstd dictionary.
