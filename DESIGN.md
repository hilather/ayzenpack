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
  "tool_version": "0.1.4"
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

v1 rebuild uses `name` (Unicode from `ZipFile::name()`), `is_dir`, `blob`, `crc32`, `dos_*` via `DateTime::try_from_msdos` with `DateTime::default()` fallback, and `unix_mode`. `utf8_flag` is recorded only. `name_raw_hex` is not used on write. `method` is advisory: rebuilt files deflate, directories store, unless `--store-all`.

---

## Reconstruction

Functional identity of uncompressed entries, names, central-directory order, and CRC-32. Not bit-identity of the ZIP container.

```
∀ jar, ∀ entry:
  uncompressed_bytes(rebuilt) == uncompressed_bytes(source)
  entry.name sequence equal          // Unicode string
  entry.crc32 equal
  valid DOS date/time preserved
```

Invalid DOS pairs including `0,0` (common in JARs) fall back to 1980-01-01. Never `from_msdos_unchecked`.

Not guaranteed: deflate bitstream, extra fields (dropped; zipalign is not preserved), data descriptors, GPBF bit 11, raw name encoding. `--verbatim` is not in v1.

Nested `.jar` entries are opaque blobs. They are not exploded.

---

## Executable / prefixed JARs

Spring Boot “fully executable” JARs (`spring-boot-maven-plugin` `executable: true` and similar) prepend a bash launcher before a normal ZIP. The file starts with `#!/bin/bash` (or similar), not `PK\x03\x04`.

Detection uses EOCD-from-EOF extra-data math, not a scan for the first local-file magic:

```
prefix_len = (eocd_file_offset - cd_size) - recorded_cd_offset
```

Zip64 uses the locator / Zip64 EOCD when the 32-bit fields are sentinels. The byte at `prefix_len` must be `PK\x03\x04` (or empty-archive EOCD). Prefixes larger than 16 MiB are rejected (`NotZip`).

The prefix is stored as a first-seen CAS BLOB (same `hash_both` path as entry payloads). Shared launchers across JARs dedup (`ref_count > 1`). Manifest `jars[]` may include optional `prefix_blob` (hex BLAKE3) and `prefix_size`. Omitted on normal ZIPs so old archives still list/rehydrate.

Rehydrate writes the prefix bytes first, then `ZipWriter` on the same file (`[prefix][zip]`). ZIP offsets stay ZIP-relative. On Unix the restored file is `chmod 0755` so it stays executable.

`source_blake3` / `source_sha256` / `source_size` remain hashes/size of the **whole** input (prefix + ZIP).

v1 still does not promise ZIP bit-identity: prefix bytes are restored exactly; ZIP entries follow the existing functional-identity rules (names, order, CRC, uncompressed bytes). Nested `BOOT-INF/lib/*.jar` entries stay opaque blobs.

---

## Signed JARs

`META-INF/*.SF` plus `*.RSA` / `*.DSA` / `*.EC` digest compressed or stored bytes. Rewriting DEFLATE invalidates signatures. ayzenpack does not re-sign.

Dehydrate warns and still packs. `--fail-on-signed` aborts. `--strict` does not promote the signed notice.

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

Rehydrate spills blobs to a CAS directory. Peak is the largest entry being copied into a `ZipWriter` plus the zstd decoder buffer.

---

## Security

| Threat | Mitigation |
|--------|------------|
| Zip-slip (`../` in entry or `jar.name`) | Reject `jar.name` with `/`, `\`, `..`. Skip entry components `..`. |
| Zip bomb | `--max-entry-bytes` |
| Encrypted ZIP | Fail with path; do not decrypt |
| Signed JAR silently broken | Detect; warn; `--fail-on-signed` |
| Traversal via `--cas-dir` / output | `--clean` only deletes names we write. CAS paths are hex |
| `unsafe` | `forbid(unsafe_code)` |
| SSRF | No network in the crate |

Treat a `.ayz` as sensitive as the input JARs: it contains file contents and original `source_path` strings.

---

## Non-goals (v1)

- Recursively exploding nested JARs
- Bit-identical ZIP reconstruction (`--verbatim`)
- HTTP CAS, S3, split archives, GUI, Maven/Gradle plugins
- Renaming v1 manifest fields
- Tokio, reqwest, openssl

Future: `--verbatim` (CAS key = BLAKE3 of compressed payload ‖ method), `--explode-nested`, seekable zstd + TOC for `list` without full decode, a class-file zstd dictionary.
