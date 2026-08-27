# Design

ayzenpack dehydrates a set of JAR/ZIP/WAR/EAR files into one `.ayz` archive and rehydrates them. **Storage efficiency is of the utmost importance.** Dedup is **BLAKE3 of uncompressed entry bytes** — one CAS blob per unique payload. The manifest is a ZIP-slot **index** (ratarmount-style pointers), not a second copy of file bytes. New packs are **format v2**: uncompressed header, record-aligned zstd BLOB groups (the only pack compression), a final zstd frame of MANIFEST+END, an uncompressed TOC, and a 64-byte trailer. v1 files (one zstd frame, no TOC) still read and rehydrate.

This is the v2 format and library contract. Standing agent rules: [`AGENTS.md`](https://github.com/hilather/ayzenpack/blob/main/AGENTS.md). CLI flag names live in `src/cli.rs` and the [README](https://github.com/hilather/ayzenpack/blob/main/README.md). Manifest JSON field names are unchanged from v1. Do not rename `blob`, `local_header_offset`, `cdata_blob`, ….

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

**Priorities (in order):**

1. **Lean pack.** Keep the `.ayz` as small as the contract allows (mix `output_len` gate, no dual encodings, no `cdata_blob`, no `raw_zip` of a listed jar).
2. **Complete rehydrate.** Dest JARs restore the outer listing and nested STORE payload bytes from index + blobs. `source_*` **must** match when every slot hits (`bit_identical_restore`).
3. **Class-level dedup.** Same `.class` (and any other uncompressed payload) across JARs — including depth-1 nested STORE libs — is **one** CAS blob keyed by BLAKE3 of uncompressed entry bytes. Restore reassembles inner ZIPs from the child stencil + those shared blobs (`reconstruct_child_zip`). Never CAS `blake3(inner zip)` when the slot is `zip_index`.

---

## Why this shape

A set of application classpaths repeats the same `.class` and resource bytes tens or hundreds of times. `tar`/`zip` of a `lib/` directory does not content-dedup across archives. Exploding JARs to a class forest is slow, huge, and drops ZIP metadata (order, DOS time, CRC).

Storing **uncompressed** entry bytes into **record-aligned zstd groups** usually beats concatenating already-deflated ZIP members: DEFLATE is not a concatenable input for a second compressor, and a second copy of those streams is why packs ballooned (200MB → ~3GB). Identical class files across JARs collapse to one BLOB. The manifest is an index of ZIP slots pointing at those blobs — one file in, N JARs out. v2 groups BLOBs into record-aligned frames (flush at 4 MiB uncompressed BLOB **record** bytes) so the zstd window stays warm and `list` can seek the last frame via the TOC.

Never store a second encoding of the same entry (`cdata_blob` next to the content blob, or `raw_zip` of a listed jar). Never switch to per-file/per-blob zstd frames.

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
  "tool_version": "0.2.1"
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

Canonical schema: [`schemas/manifest.v1.schema.json`](https://github.com/hilather/ayzenpack/blob/main/schemas/manifest.v1.schema.json). Example: [`examples/tiny.manifest.json`](https://github.com/hilather/ayzenpack/blob/main/examples/tiny.manifest.json).

Root: `format`, `version`, `hash_algo`, `mode`, `jars[]`, `blobs[]`, `stats`.

`blobs[]` is first-seen order (must match BLOB records and END). `jars[].entries[]` is central-directory order. Directory entries have `blob: null`. `jars[].name` is a single path segment; `..`, `/`, `\` are rejected.

v1 **content** rebuild (old archives) uses `name` (Unicode from `ZipFile::name()`), `is_dir`, `blob`, `crc32`, `dos_*` via `DateTime::try_from_msdos` with `DateTime::default()` fallback, and `unix_mode`. `utf8_flag` is recorded only. `name_raw_hex` is not used on write. Arm 3 ZipWriter (no `tail_blob`) STOREs directories, `--store-all`, `method_code == 0`, and `zip_index`; method-8 files DEFLATE at `deflate_level`. Payload is always uncompressed (`read_entry_content` / `reconstruct_child_zip`); never `resolve_cdata`.

New packs store optional ZIP-slot index fields (omitted when absent, same style as `prefix_blob`). Unknown keys stay ignored on read. These fields point at CAS blobs and record local-header / tail bytes. They are not a license to store a second copy of entry payloads.

`--restore-paths` dehydrate adds optional `jars[].restore_path` (canonical absolute path), `restore_mode`, and on Unix `restore_uid` / `restore_gid`. Omitted when the flag is off. Default rehydrate still writes `dir/name`. `--restore-paths` rehydrate writes `restore_path` (overwrites; dest symlink is replaced, not followed). Restore does **not** unlink dest first: every writer (`write_exact_jar` / `write_rebuilt_jar` / `write_skip_exact_seek` / `write_skip_exact_concat` / `write_jar`) emits a sibling `dest.file_name() + ".tmp"`, runs `set_len` / prefix `chmod` / exact `source_*` checks on that tmp, then `replace_file` onto dest. `apply_restore_attrs` runs on dest after replace. A failed restore Drop-unlinks tmp only; dest keeps its original bytes. Outer exact remains a file seek-walk on tmp (no outer `Vec`).

---

## Storage (locked)

North star: **one CAS blob + ZIP index + zstd blocks**. Not metadata-only exact. Not codec-hit as a requirement. Not Java-zlib bit-identical files.

The original JAR is gone after dehydrate. What remains:

| Kind | What it is |
|------|------------|
| Data | One BLOB per unique **uncompressed** entry. Dedup key = BLAKE3 of those bytes. |
| Index | Manifest ZIP slots: `name`, CD order, `method` / `method_code`, `crc32`, compressed/uncompressed sizes, GPBF (inside `local_header_*`, not a new JSON field), `local_header_offset`, local header / data-descriptor / pad, `tail_blob` (CD through EOF), `prefix_blob` if any, `blob` hash. |
| Pack compression | Format v2 record-aligned zstd **groups** of those uncompressed BLOB records (flush at 4 MiB record bytes), then MANIFEST+END, uncompressed TOC. |

**Forbidden on write:** a second encoding of the same entry. No default `cdata_blob` beside the content blob. No `raw_zip` of a listed jar (`entries[]` already populated). Count mismatch and homemade parse failure on a listed jar are skip-exact, not `raw_zip`. `raw_zip` only if listing never produced `entries[]` (`UnsupportedArchive` spanning / `NotZip`). Do not store pre-deflated ZIP cdata as the CAS payload. Do not switch to per-file zstd frames.

Crate **0.2.1** never writes leftover `cdata_blob`. Crate **0.2.2** never writes `raw_zip` of a listed jar. Crate 0.2.0 MixedExact / class-4 dual copies still read. [`PLAN.md`](https://github.com/hilather/ayzenpack/blob/main/PLAN.md) / [`AGENTS.md`](https://github.com/hilather/ayzenpack/blob/main/AGENTS.md). New work must not add more dual copies. Mix identity gates stay: `cdata_blob == 0` on every mix entry; mix `.ayz` `output_len <= 569539 * 115 / 100`. Do not loosen either. Corpus lucene/jackson `source_*` stays gated until every method-8 slot is a measured hit.

---

## Reconstruction

Rehydrate builds a **valid ZIP** from index + blobs. Outer exact (`write_exact_jar`) is a **file seek-walk** on a sibling tmp (no outer `Vec`). `Vec<u8>` is only for nested children (`reconstruct_child_zip`) and `verify`. Skip-exact has no tail and does not pre-size to `source_size`. Arm 1 homemade-`None` with captured local headers and every slot at recorded `compressed_size` is stencil seek + synthetic CD (`write_skip_exact_seek`; FileAbs iff `prefix_size > 0`; `leading_pad_blob` at `prefix_len`). Arm 2 (headers present, a slot would change csize) is CD-order concat + synthetic CD (`write_skip_exact_concat`; FileAbs `prefix_len + zip_rel` iff `prefix_size > 0`; drop leading_pad and pads). Arm 3 (no captured headers: overlap / ZipArchive count mismatch / slice `Err`) uses `write_jar` ZipWriter STORE over uncompressed payload (`read_entry_content` / `reconstruct_child_zip`); never `resolve_cdata`; FileAbs CD/EOCD offsets when `prefix_size > 0`. Equal-offset last-wins with matching homemade count exact-splices (pad of the unreferenced second physical local kept; unique content 1). Prefix+hole **(A)** `[non-PK prefix][hole][first CD local]` is already `prefix_blob` covering bash+hole (`find_cd_first_local`); not skip-exact. Prefix+hole **(B)** `prefix_len > 0 && min(zip_rel) != 0` after convert is a dead defensive `Err`; keep it; do not call (B) absorbed. `source_*` must match iff `bit_identical_restore()`. Remaining homemade-`None` never gets `tail_blob`.

Paths, in order:

- **STORE:** splice the content `blob` at the recorded local offset. No `cdata_blob`.
- **DEFLATE codec hit:** optional `cdata_codec` when a pack-time trial matched original cdata: `deflate-raw:zlib:{1,3,6,9}` (in-process zlib-rs, raw/nowrap), `deflate-raw:flate2:{1,3,6,9}` (existing miniz), or `deflate-raw:stored` (BTYPE 00). Rehydrate encodes and splices. A hit is luck, not a goal. A miss must not drop sibling codecs.
- **Child `zip_index`:** reconstruct the nested ZIP from `jars[].nestedindexes[i]` + CAS (`reconstruct_child_zip`), then splice or rebuild that outer slot. Depth 1. `blob` is null. Never a second whole-ZIP CAS.
- **Otherwise rebuild:** neither `cdata_blob` nor `cdata_codec`. Patch local header / data descriptor / CD / EOCD (and Zip64 extras that already exist). Same names, CD order, timestamps, extras, uncompressed bytes. New compressed sizes. **`source_*` may change. That is acceptable.**
- **Skip-exact arm 1** (no `tail_blob`, no `raw_zip`; remaining homemade-`None` with captured local headers; every slot keeps recorded `compressed_size`): stencil seek + synthetic CD. `write_prefix`; `leading_pad_blob` at `prefix_len` if Some; `write_slot` at `offsetheader` including pads; `resolve_cdata(..., false)` after classification; `cdata.len() == compressed_size` or error; `cd_start = max(slot_end)`; FileAbs `local_offset = offsetheader` iff `prefix_size > 0` else `local_header_offset`; `set_len(cd_start + cd.len())`. Synthetic CD file name is the listing (`name_raw_hex` / `entry.name`) when it differs from the aliased local header; GPBF stays from the captured local. Never `verify_source_identity`. Locals-region identity; original-file `source_*` must not be required (tail withheld). Seek backends write one physical local (idempotent `write_slot`).
- **Skip-exact arm 2** (headers present but a slot would change csize): `write_skip_exact_concat`. `resolve_cdata(..., true)`; `patch_local_rebuild_fields` + `patch_data_descriptor`; CD-order concat; drop `leading_pad_blob` and per-slot pads; `zip_rel` = running concat offset; FileAbs `local_offset = prefix_len + zip_rel` iff `prefix_size > 0` else `zip_rel`; `cd_start = prefix_len + concat.len()`. Synthetic CD file name is the listing (`name_raw_hex` / `entry.name`) when it differs from the aliased local header; GPBF stays from the captured local. Concat **may emit two sequential dest locals** for equal-offset source slots (do not make concat idempotent). Never put recorded `offsetheader` / `local_header_offset` in the synthetic CD. Never `verify_source_identity`. **`source_*` may change.**
- **Skip-exact arm 3** (no captured headers: overlap / ZipArchive count mismatch / slice `Err`): `write_jar` ZipWriter that STOREs `method_code == 0` / `zip_index` over uncompressed payload (`read_entry_content` / `reconstruct_child_zip`); never `resolve_cdata`. Method-8 misses DEFLATE at `deflate_level` (zip crate, not `deflate_raw` 6). Never seek `offsetheader`. Never `verify_source_identity`. After `finish`, **always** when `prefix_len > 0` (even with no STORE nested): offset-only `fileabs_shift_tail` adds `prefix_len` to CD local offsets (classic u32 and Zip64 extra 0x0001 if present) and to classic EOCD CD start and Zip64 EOCD/locator CD start. Do not touch method/crc/csize. Do not reuse `patch_central_directory` with `jar.entries`. Nested STORE `zip_index` members stay STORE (reassembled from shared class blobs; never late-CAS `blake3(inner zip)`). Dest size stays in the same league (`restored * 2 >= source`). **`source_*` may change.**
- **Legacy `cdata_blob`:** read 0.1.6–0.1.8 dual-copy packs and 0.2.0 MixedExact leftovers. **Never write this shape again.** Maven/Java empty DEFLATE directories (`03 00`, usize 0) are codec/empty, not exotic.

**Leftover-junk CD:** N complete CD records + trailing junk with `N == ZipArchive::len()` is homemade_ok + `tail_blob` (phys CD→EOF, junk included). Those listed zips take the exact **file seek-walk** when every slot hits; `source_*` **must** match. Remaining homemade-`None` (true parse failure, truncated/malformed CD) **never** gets `tail_blob`. Never attach tail while homemade parse is `None`. Range overlap, ZipArchive count mismatch, and slice `Err` are other skip-exact reasons (also no `tail_blob`); they are not parse-`None`. Equal-offset last-wins with matching homemade count is exact splice (never `raw_zip`; pad of the unreferenced second physical local is kept; unique content 1). Prefix+hole **(A)** `[non-PK prefix][hole][first CD local]` is already `prefix_blob` covering bash+hole (`find_cd_first_local`); not skip-exact. Prefix+hole **(B)** `prefix_len > 0 && min(zip_rel) != 0` after convert is a dead defensive `Err` ("first local header is not at zip offset 0"); keep it; do not call (B) absorbed.

Per jar: `tail_blob` / `tail_size` is the CD-through-EOF index blob (structural, not a second copy of entry payloads).

`raw_zip_blob` is only for a zip that cannot be listed at all. It is not the codec-miss path, not the exotic-sibling path, and not a slice/count-mismatch fallback.

**Old archives** (no tail / `raw_zip`): keep the 0.1.x `ZipWriter` path, with STORE for `method_code == 0` and `zip_index`. `--verbatim` is not a CLI flag.

### Restore hash policy

`source_*` **must** match iff `Jar::bit_identical_restore()`. Hash matching is a restore-time walk of the stencil over shared CAS blobs. Do not buy a match by storing `cdata_blob`, `raw_zip` of a listed jar, or `blake3(inner zip)` next to exploded class blobs.

`source_*` (`source_size` / `source_blake3` / `source_sha256`) are hashes of the **original input file** (prefix + ZIP), recorded at dehydrate. `Jar::bit_identical_restore()` is true iff `raw_zip` **or** (`tail_blob` **and** every `slot_exact`). `verify_source_identity` compares dest bytes to those original hashes. Do **not** retarget pack-time `source_*` at the stencil-reconstructed dest (that would lie about the original file). Do **not** flip `bit_identical_restore()` true without `tail_blob` (mix `tests/corpus.rs` requires `hashes_eq` whenever `bit_identical_restore()`).

| Condition | Restore backend | Dest vs original file | `source_*` |
|---|---|---|---|
| Every slot hits + original `tail_blob` / `raw_zip` (`bit_identical_restore`), including leftover-junk listed zips that gained a tail | `write_exact_jar` | byte-identical (`set_len(source_size)`) | **must match** |
| Per-entry codec miss, tail present (`metadata_rebuild`) | `write_rebuilt_jar` | pads/hole dropped; CD patched | **may change** |
| Homemade-`None`, headers present, every slot keeps recorded csize (**arm 1**) | stencil seek + **synthetic CD** | locals region byte-identical to source; CD is specified (not original truncated tail); FileAbs iff `prefix_size > 0` | **must not** require original-file match (tail withheld). Locals-region identity + FileAbs listing are the bit-identical gates |
| Headers present, some slot would change csize (**arm 2**) | concat + synthetic CD (new zip_rel, patched locals) | rebuilt locals; FileAbs = `prefix_len + zip_rel` | **may change** |
| Equal-offset last-wins, homemade_ok, every slot hits | `write_exact_jar` | dest == source (pad of unreferenced second local kept; unique content 1) | **must match** |
| No captured headers: overlap / ZipArchive count mismatch / slice `Err` (**arm 3**) | ZipWriter STORE | valid ZIP, same size league | **may change** |
| 0.1.x archive, no tail | ZipWriter | functional identity | **may change** |

**Bit-identical means walking the stencil**, not a second encoding:

```
prefix_blob
leading_pad_blob at prefix_len          # if Some; arm 1 only
each slot at offsetheader:              # captured local header + cdata + descriptor + pad
    cdata = STORE blob
          | encode_codec(cdata_codec)
          | reconstruct_child_zip(zip_index)
then either:
    original tail_blob                  # leftover-junk / healthy parse
  | specified synthetic CD              # homemade-None arm 1 (stencil offsets) / arm 2 (concat zip_rel); never the withheld tail
```

A zopfli / unknown-deflate **codec miss** still rebuilds **that slot only**; sibling codecs kept; whole-file hashes **may** change. Do **not** propose Java-zlib bit-identical work or `cdata_blob` for misses.

**Why homemade-`None` cannot match original `source_*` (Status: wontfix).** Remaining homemade-`None` is N complete `PK\x01\x02` rows plus a 46-byte magic-but-short stub counted in EOCD `cd_size`. Policy: **never** store that phys CD→EOF as `tail_blob`. A specified synthetic CD is N complete records + (Zip64 EOCD+locator if needed) + classic EOCD with `jar.comment`. It is **not** the original truncated tail:

```
source: [prefix][locals][N CD records][46-byte truncated stub][EOCD]
dest:   [prefix][locals][N synthetic CD records][Zip64?][classic EOCD]
```

Locals-region identity (`dest[0 .. cd_start] == source[0 .. phys_cd]`) is required on arm 1. Whole-file `verify_source_identity` against original `source_*` would fail unless we attached the withheld tail — which is forbidden. Mix / corpus hash match stays **iff** `bit_identical_restore()`. Homemade-`None` arm 1 is stencil-faithful skip-exact, not a tail splice.

Always-on hash gates: baked zlib-rs bitstream; STORE zip-A fat after `zip_index`; leading-pad PK decoy **with tail**; leftover-junk listed zip when every slot hits. Corpus lucene/jackson `source_*` stays gated on 100% measured method-8 / zlib hits (`AYZENPACK_CORPUS_DIR`); not always-on CI until those counts are 100%. Mix `cdata_blob == 0` and `output_len <= 569539 * 115 / 100` stay.

```
∀ jar (bit_identical_restore: STORE splice / codec-hit / leftover-junk exact / legacy cdata_blob / raw_zip):
  restored_bytes == source_bytes
  blake3(restored) == jars[].source_blake3
  sha256(restored) == jars[].source_sha256
  len(restored) == jars[].source_size

∀ jar (rebuild):
  ZipArchive opens; names, CD order, uncompressed bytes match
  source_* are the original file and are not verified

∀ jar (skip-exact arm 1 homemade-None):
  dest[0 .. cd_start] == source[0 .. phys_cd]   # locals-region identity
  ZipArchive::new(File) lists outer names (FileAbs iff prefix_size > 0)
  prefixed source listing is scan_jar / ZipView, not ZipArchive::new(File)
  original-file source_* are not verified (tail withheld)

∀ jar (skip-exact arm 2 / arm 3):
  ZipArchive opens; names, uncompressed bytes, CRC match
  method_code 0 / zip_index members are STORE
  source_* are the original file and are not verified
```

Do not add a Java subprocess / vendor `Deflater` or `cdata_blob` for misses to move a rebuild jar into the first block. In-process zlib-rs raw-deflate hits are the 0.2.4 path.

Invalid DOS pairs including `0,0` still fall back to 1980-01-01 on the content path. Never `from_msdos_unchecked`.

### FileAbs listing oracle

Synthetic-CD dests are **not** the original file. For `prefix_size > 0` on arm 1, emit **FileAbs** CD local offsets (`offsetheader`) and FileAbs `cd_start` so `ZipArchive::new(File)` sees **outer** names (`BOOT-INF/lib/…`, `lib/inner.jar`), not an inner nested EOCD. Arm 2 FileAbs uses **new** concat offsets (`prefix_len + zip_rel`), not `offsetheader`. Arm 3 ZipWriter dests with `prefix_size > 0` run offset-only `fileabs_shift_tail` after `finish` (CD/EOCD offsets only; locals stay zip-rel) so dest `ZipArchive::new(File)` lists outer names even with no STORE nested. Exact zip-A fats already splice FileAbs tails; dest == source, so a latch on the dest is the same latch as the source. Rebuild backends still must not be fed `offsetheader` (PLAN: that makes zip-A look ZipRel and corrupts rebuild).

`tests/corpus.rs` `entry_map` / `cd_entries` / `assert_functional_identity` and `tests/roundtrip.rs` open `ZipArchive::new(File)` with **no** `ZipView`. That is the right **dest** oracle for FileAbs. It is the wrong **source** oracle for unadjusted prefix: source CD is zip-rel, so `ZipArchive::new(File)` latches a STORE nested EOCD. After FileAbs, prefixed arm 1 dests list **outer** names; a global `assert_functional_identity(&src, &dest)` would compare latched-inner vs outer and fail.

| Restore | `prefix_size` | Source listing | Dest listing |
|---|---|---|---|
| Exact splice (`bit_identical_restore`) | any | dest == source; `ZipArchive::new(File)` may latch both the same way | same bytes as source |
| Arm 1 / arm 2 synthetic CD | 0 | `ZipArchive::new(File)` | `ZipArchive::new(File)` (outer == source outer) |
| Arm 1 / arm 2 synthetic CD | > 0 | **`scan_jar` / `ZipView(prefix)`** (outer names) | **`ZipArchive::new(File)`** (FileAbs outer names) |
| Arm 3 ZipWriter | 0 | `ZipArchive::new(File)` | `ZipArchive::new(File)` |
| Arm 3 ZipWriter | > 0 | `scan_jar` | **`ZipArchive::new(File)`** (FileAbs outer names) |

**Do not** rewrite `assert_functional_identity` or `entry_map`. Those helpers are used for every mix jar. A global `if prefix_size > 0 { dest ZipArchive vs source scan_jar }` would compare a latched **exact** dest (zip-rel CD, dest==source) to source outer names and fail corpus CI on `spring-jackson-core.jar` / zip-A slf4j.

Gate only in the **mix loop**: when `jar.tail_blob.is_none() && jar.prefix_size.unwrap_or(0) > 0`, dest `ZipArchive::new(File)` vs source `scan_jar`. Else `assert_functional_identity`. Exact prefixed jars stay dest==source. Current mix has no prefixed homemade-`None`. Prefixed homemade-`None` tests must **not** call `assert_functional_identity` (source `ZipArchive::new(File)` latches).

### Remaining skip-exact matrix

| Class | Headers? | Tail? | Arm | Dest listing | Locals identity | `source_*` |
|---|---|---|---|---|---|---|
| Leftover-junk CD, every slot hits | yes | **yes** | exact splice | dest == source | yes | **must match** |
| Leftover-junk + method-8 miss | yes | yes | `write_rebuilt_jar` | patched CD | no (concat) | may change |
| Truncated/malformed CD, every slot hits | yes | **never** | **1** synthetic CD | `ZipArchive::new(File)` outer; FileAbs if prefix (source listing: `scan_jar` if prefixed) | **yes** (`dest[..cd_start] == source[..phys_cd]`) | original-file **no** |
| Truncated CD + method-8 miss | yes | never | **2** concat+synthetic | FileAbs if concat+prefix using **new** zip_rel, not `offsetheader` | no | may change |
| Truncated CD + STORE nested + prefix | yes | never | **1** | FileAbs; dest `ZipArchive::new(File)` vs source `scan_jar`; **not** `assert_functional_identity` | prefix+locals match | original-file **no** |
| Leading-pad PK decoy + truncated CD | yes + `leading_pad_blob` | never | **1** | pad at 0; CD after locals | includes decoy | original-file **no** |
| Equal-offset last-wins (matching homemade count), every slot hits | yes | **yes** | exact splice | dest == source | yes (pad of unreferenced second local kept; unique content 1) | **must match** |
| Equal-offset last-wins + homemade-`None` (truncated CD) | yes | **never** | **1** synthetic CD (listing names) | both names listed | **yes** including pad | original-file **no** |
| Range overlap (B offset inside A's cdata) | **no** (`Err`) | never | **3** ZipWriter | two listing names; FileAbs dest `ZipArchive::new(File)` vs source `scan_jar` if prefix | no | may change |
| ZipArchive count mismatch (`dup.txt`) | **no** (`Err`) | never | **3** ZipWriter | scanner-visible names; FileAbs dest `ZipArchive::new(File)` vs source `scan_jar` if prefix | no | may change |
| Prefix+hole **(A)** `[non-PK prefix][hole][first CD local]` | yes | yes when homemade_ok | **not skip-exact** (`prefix_blob` covers bash+hole via `find_cd_first_local`; do not split bash vs hole) | dest == source when slots hit | hole inside `prefix_blob`; `leading_pad` absent | **must match** when slots hit |
| Prefix+hole **(B)** `prefix_len > 0 && min(zip_rel) != 0` after convert | n/a | n/a | dead defensive `Err` at first-local-not-at-0 (keep; do not call (B) absorbed; unreachable for listable files given `zip_archive_opens`) | n/a | n/a | n/a |

Prefix+hole is two classes, not one skip-exact row. **(A)** is already absorbed into `prefix_blob`. **(B)** is a kept dead `Err` in `slice_from_reader`, not a dehydrate class. Do not extend `prefix_len` on PK-start files. Do not invent a bash-vs-hole splitter.

Leftover-junk is **not** the synthetic-CD path after 0.2.5. Do not restage leftover-junk tests as synthetic-CD gates.

### Corpus lucene/jackson `source_*`

Already implemented and **env-gated**: [`tests/corpus.rs`](https://github.com/hilather/ayzenpack/blob/main/tests/corpus.rs) `corpus_lucene_jackson_source_identity_only_when_every_slot_hits` skips unless `AYZENPACK_CORPUS_DIR` is set. It prints per-jar `method8` / `flate2` / `zlib` / `stored` / `miss` / `exact`, and asserts whole-file hashes **only** when `jar.bit_identical_restore()`. Mix `corpus_mix_regular_and_spring_whole_file_hashes` uses the same env gate.

**What CI requires:**

| Workflow | Command | `AYZENPACK_CORPUS_DIR` | `source_*` / whole-file hash |
|---|---|---|---|
| [`.github/workflows/ci.yml`](https://github.com/hilather/ayzenpack/blob/main/.github/workflows/ci.yml) `linux-test` / `windows-test` | `cargo test --locked` with only `CARGO_NET_OFFLINE` | **not set** (the workflow never assigns it; it does not `unset` it) | Mix and lucene/jackson tests **skip** when the env is absent (`corpus_dir()`). A runner that injected the var would run them. Always-on hash gates are synthetic fixtures in [`tests/roundtrip.rs`](https://github.com/hilather/ayzenpack/blob/main/tests/roundtrip.rs) (zlib-rs bitstream, STORE zip-A, leftover-junk, codec-hit zip-crate, Maven empty DEFLATE dir, etc.). |
| [`.github/workflows/ci.yml`](https://github.com/hilather/ayzenpack/blob/main/.github/workflows/ci.yml) `msrv` | `cargo +1.80 check` | not set | No restore hashes. |
| [`.github/workflows/corpus.yml`](https://github.com/hilather/ayzenpack/blob/main/.github/workflows/corpus.yml) | `cargo test --locked --test corpus -- --nocapture` | **set** to `.corpus` after `ci/download-corpus.sh` | Mix: `hashes_eq` **required iff** `jar.bit_identical_restore()`; `hash_mismatch_proven_miss` **allowed**. Lucene/jackson: print per-jar `method8/flate2/zlib/stored/miss/exact`; assert hashes **only when** `bit_identical_restore()`. CLI overlap: `verify` + `rehydrate` only — **no** dest vs original file hash. |

Default `cargo test` (the job developers run and `ci.yml` linux/windows) **does not require original-file hashes on Maven JARs.** `.github/workflows/ci.yml` does **not set** `AYZENPACK_CORPUS_DIR`. Those tests skip when the env is absent. Do **not** claim CI requires original-file hashes on every jar. It requires them on `bit_identical_restore` jars and on the synthetic Zip64 mix fat. [`.github/workflows/corpus.yml`](https://github.com/hilather/ayzenpack/blob/main/.github/workflows/corpus.yml) already caches `.corpus` on `hashFiles('ci/corpus.lock.json')`.

**How to enable (operators / agents):**

```bash
# 1. Fetch pinned Maven JARs (not in git). See ci/download-corpus.sh.
CORPUS_DIR=/path/to/corpus ci/download-corpus.sh

# 2. Measure method-8 / zlib hits (not always-on CI).
AYZENPACK_CORPUS_DIR=/path/to/corpus cargo test --test corpus \
  corpus_lucene_jackson_source_identity_only_when_every_slot_hits -- --nocapture
```

[`ci/download-corpus.sh`](https://github.com/hilather/ayzenpack/blob/main/ci/download-corpus.sh) verifies SHA-256 from [`ci/corpus.lock.json`](https://github.com/hilather/ayzenpack/blob/main/ci/corpus.lock.json). Promote those jars into always-on CI `source_*` **only when every printed line has `miss=0` and `exact=true`** (100% measured method-8 hits, every slot STORE/codec/`zip_index`). Until then: keep the env gate; do not fail default `cargo test` on a remaining zlib-3 / zopfli miss. Mix `proven_miss` already allows sibling codecs.

**Last measured (GitHub Actions corpus run `33098832157`, crate 0.2.7 / `70bcd73`, 2026-08-27, after `deflate-raw:zlib:{1,3,6,9}`):** corpus.yml **did** run. A local machine with `AYZENPACK_CORPUS_DIR` unset is not the source of truth.

Mix (`corpus_mix_regular_and_spring_whole_file_hashes`):

| Jar | `identical` | `bit_identical` | `rebuild` | `raw_zip` |
|---|---|---|---|---|
| `plain-failureaccess.jar` (Maven `failureaccess-1.0.2`) | false | false | **true** | false |
| `plain-slf4j.jar` (Maven `slf4j-api-2.0.16`) | false | false | **true** | false |
| `plain-jackson-annotations.jar` (Maven `jackson-annotations-2.17.2`) | false | false | **true** | false |
| `spring-jackson-core.jar` (launcher + jackson-core) | false | false | **true** | false |
| `spring-zipa-slf4j.jar` (zip -A launcher + slf4j) | false | false | **true** | false |
| `spring-zip64-nested.jar` (zip-crate Zip64 fat) | **true** | **true** | false | false |

```
mix stats jars=6 bytes_in_jars=835025 unique_blob_count=378
bytes_unique_blobs=1497510 ayz=592946 ratio=0.7101
codec_hit=226 codec_miss=207 cdata_blob=0
hash_match=1 hash_mismatch_proven_miss=5
```

`codec_hit` / `codec_miss` count **method-8 file entries on the outer listing only** (`tests/corpus.rs` skips `e.is_dir || e.method_code != 8` before incrementing). Maven empty DEFLATE dirs (`03 00`) and nested `zip_index` child slots are not in `226` / `207`. Mix Zip64 nested is opaque DEFLATE, so this mix is fine.

Gate check: `592946 <= 569539 * 115/100` (`654969`). Mix `cdata_blob == 0`. The five Maven/Spring wraps are `rebuild=true` — **`tail_blob` is present**. Capture stored the stencil. Restore did not ZipWriter them. Hashes changed because at least one method-8 slot missed the closed codec set.

Lucene/jackson (`corpus_lucene_jackson_source_identity_only_when_every_slot_hits`), same run — **every line `exact=false`**, so **no** whole-file hash assert fired:

| Jar | method8 | flate2 | zlib | stored | miss | exact |
|---|---:|---:|---:|---:|---:|---|
| jackson-annotations-2.17.2.jar | 79 | 0 | 57 | 0 | 22 | false |
| jackson-core-2.17.2.jar | 227 | 0 | 105 | 0 | 122 | false |
| jackson-databind-2.17.2.jar | 791 | 1 | 260 | 0 | 530 | false |
| lucene-analysis-common-9.11.1.jar | 680 | 1 | 218 | 0 | 461 | false |
| lucene-backward-codecs-9.11.1.jar | 407 | 0 | 204 | 0 | 203 | false |
| lucene-codecs-9.11.1.jar | 217 | 0 | 69 | 0 | 148 | false |
| lucene-core-9.11.1.jar | 2513 | 1 | 1042 | 0 | 1470 | false |
| lucene-highlighter-9.11.1.jar | 172 | 0 | 68 | 0 | 104 | false |
| lucene-queryparser-9.11.1.jar | 256 | 0 | 93 | 0 | 163 | false |
| lucene-suggest-9.11.1.jar | 126 | 0 | 51 | 0 | 75 | false |

Hit rate is real (zlib-rs matches a large minority / majority depending on the jar) and **insufficient for whole-file identity**. One miss poisons `bit_identical_restore` for the whole jar. jackson-annotations is 57/79 zlib hits and still `exact=false` because of 22 misses.

Default `cargo test` still skips `corpus_lucene_jackson_source_identity_only_when_every_slot_hits` when `AYZENPACK_CORPUS_DIR` is unset. Do not drop the env gate. Do not promote always-on until an operator paste of that test is 100% `miss=0` / `exact=true`.

Do **not** buy lucene/jackson hashes with `cdata_blob` or Java `Deflater`. Mix gates stay: `cdata_blob == 0`; `output_len <= 569539 * 115 / 100`; unique content not doubled; no inner-zip CAS on `zip_index`.

---

## Executable / prefixed JARs

Spring Boot “fully executable” JARs (`spring-boot-maven-plugin` `executable: true` / `bootJar { launchScript() }`) prepend the official `launch.script` before a ZIP (often Zip64). The file starts with `#!/bin/bash`, the Spring Boot banner, `### BEGIN INIT INFO` / chkconfig, and ends with `exit 0` — not `PK\x03\x04`. Placeholders like `{{mode:auto}}` are already substituted in a real build.

Detection uses no CLI flag. If the file does not start with ZIP magic, the prefix ends at the central directory's first local header (CD min local offset, or that offset after `zip -A` made it file-absolute) — not the first `PK\x03\x04` in the stub. Prefix bytes are `[0, first_real_lh)` within 16 MiB. Then try, in order:

1. **Unadjusted** (Spring default): `ZipArchive` through `ZipView` shifted to the real first local header. ZIP offsets are relative to the ZIP start. This is what `file` sees after the script is deleted.
2. **Adjusted** (`zip -A`): if that open is rejected (see below), open the full file (no `ZipView` shift). CD and local-header offsets are already file-absolute.

`ZipArchive::new` success is **not** enough. rust zip may latch onto a STORE nested EOCD when the view's CD offset is wrong (`zip -A` file-absolute offsets vs a prefix-shifted view). Accept a view only when `archive.len()` equals the homemade outer CD count (`find_cd_bounds` entry count) **and** `header_start + view_shift == prefix_len` (first local at the prefix). STORE listable `BOOT-INF/lib/*.jar` become depth-1 `zip_index`; DEFLATE-wrapped nested libs stay opaque.

A file with no local headers stays `NotZip` except an empty prefixed archive (EOCD-only). Unadjusted empty archives use EOCD extra-data math. After `zip -A` on an empty archive, extra is 0 and the recorded CD offset is the prefix (file-absolute EOCD). 0.1.4 extra-data math alone is not sufficient for non-empty `zip -A` / Zip64: `extra == 0` (or inflated by the Zip64 footer) and `confirm_zip_at(0)` reads `#!` / ELF.

```
extra = (eocd_file_offset - cd_size) - recorded_cd_offset
```

The prefix is stored as a first-seen CAS BLOB (same `hash_both` path as entry payloads). Shared launchers across JARs dedup (`ref_count > 1`). Manifest `jars[]` may include optional `prefix_blob` (hex BLAKE3) and `prefix_size`. Omitted on normal ZIPs so old archives still list/rehydrate.

Rehydrate writes the prefix bytes first. Splice packs (STORE / codec-hit / legacy `cdata_blob` / `raw_zip`) keep `[official launch.script][zip]` at original offsets. Rebuild packs rewrite the zip portion after the prefix and patch CD offsets (zip-relative or file-absolute after `zip -A`). On Unix the restored file is `chmod 0755` so it stays executable.

`source_blake3` / `source_sha256` / `source_size` remain hashes/size of the **whole** input (prefix + ZIP) and are checked after splice restore only. Rebuild may change them.

A STORE listable `BOOT-INF/lib/*.jar` is a depth-1 child `zip_index` on new packs (reconstruct the inner ZIP from `nestedindexes` + CAS). DEFLATE-wrapped nested libs stay opaque. Encrypted inner STORE payloads stay opaque; only an encrypted outer listing fails dehydrate. Child ZipArchive latch / homemade CD count mismatch is also opaque: `scan_from_bytes` returns `Err` and the probe `commit_blob`s the combined inner (never `zip_index`, never explode of a latched inner-inner, never explode-plus-inner dual copy). Do not fail the outer.

The original JAR is still gone after dehydrate. The index is a ratarmount-style **stencil** (write recipe): `offsetheader` / `data_start` / `local_header_offset` / name / sizes / method, plus `nestedindexes` at **depth 1**. Reconstruct walks prefix → `leading_pad_blob` → local header → payload → data descriptor → pad → tail. Do not copy SQLite into the `.ayz`. Do not rename `blob`, `local_header_offset`, `cdata_blob`.

Closed codec set (record the id that hit original cdata; restore re-encodes): STORE; `deflate-raw:zlib:{1,3,6,9}` (in-process zlib-rs, not a Java `Deflater` process); `deflate-raw:flate2:{1,3,6,9}`; `deflate-raw:stored`. Trial order: zlib GPBF hint ∩ `{1,3,6,9}`, remaining array order, flate2 `{1,3,6,9}`, stored. First-match-wins if two levels emit identical bytes. Other methods rebuild **that entry**. No new `cdata_blob`. A miss must not drop sibling codecs.

A healthy zip -A fat whose first local is the prefix already slices on 0.2.3. Leftover junk after N complete CD records with `N == ZipArchive::len()` is homemade_ok + `tail_blob` (exact file seek-walk when every slot hits). Remaining homemade-`None` (true parse failure, truncated/malformed CD) **never** gets `tail_blob`; never attach tail while parse is `None`. Range overlap, ZipArchive count mismatch, and slice `Err` are other skip-exact reasons (also no `tail_blob`). Equal-offset last-wins with matching homemade count is exact splice (never `raw_zip`; pad of the unreferenced second physical local is kept; unique content 1). Prefix+hole **(A)** `[non-PK prefix][hole][first CD local]` is already `prefix_blob` covering bash+hole (`find_cd_first_local`); not skip-exact; do not split bash vs hole. Prefix+hole **(B)** `prefix_len > 0 && min(zip_rel) != 0` after convert is a dead defensive `Err` at first-local-not-at-0; keep it; do not call (B) absorbed. Arm 1 homemade-`None` with captured headers is stencil seek + synthetic CD. Arm 2 csize-changing skip-exact concatenates patched locals and synthesizes a CD (new zip_rel, not recorded `offsetheader`). Arm 3 (no captured headers: overlap / ZipArchive count mismatch / slice `Err`) uses `write_jar` ZipWriter that STOREs `method_code == 0` / `zip_index` over uncompressed payload (`read_entry_content` / `reconstruct_child_zip`); never `resolve_cdata`. Leading pad is `leading_pad_blob` on a PK-start hole (do not extend `prefix_len` on PK-start files). Decide `zip_index` vs opaque before `jobs==1` `commit_blob` and before `--jobs` `spawn_file` via `scan_from_bytes` + `slice_from_bytes` on the STORE payload. After the child `NestedIndex` is built, `probe_explode` reconstructs against the in-hand pending blobs and requires those bytes equal the STORE payload. Mismatch (including child codec length) is opaque `commit_blob` of the combined ZIP **instead of** explode — never both. Do not use `Jar::bit_identical_restore()` as the probe predicate (child tail is still pending). Child `Encrypted` / empty listing / range overlap / reconstruct mismatch / homemade count mismatch / ZipArchive latch / listing `uncompressed_size` > `--max-entry-bytes` / file-entry count > 65535 → opaque (do not fail the outer). Opaque is **instead of** explode: one combined-inner blob, not a latched inner-inner `zip_index` and not explode-plus-inner dual copy. Prefixed children already ran `zip_archive_opens` in `detect_zip_layout`. If `prefix_len == 0`, `scan_from_bytes` still requires homemade CD count (`find_cd_bounds`) == `ZipArchive::len()`; mismatch is `Err`. Do not require first `header_start == 0` (that is leading_pad). Encrypted outer still fails. `--jobs` applies inner `remember_blob` only from `Sequenced::Exploded` (one seq; first-seen stays jobs-invariant). Child stencil is tail-bearing (child `leading_pad_blob` if the inner ZIP has a hole); cap child file entries at 65535. Never CAS the whole inner ZIP if the slot is a `zip_index`. `verify` / `write_jar` / exact / rebuild all use `reconstruct_child_zip(index, get_blob)`.

Old packs still read (opaque nested blob, flate2-only `cdata_codec`, zip-rel `local_header_offset`). New packs may add `offsetheader` / `data_start` / `zip_index` / `nestedindexes`. 0.2.3 cannot restore a pack that replaced a nested blob with `zip_index`.

---

## Signed JARs

`META-INF/*.SF` / `MANIFEST.MF` digest uncompressed entry bytes, not the deflate stream. `*.RSA` / `*.DSA` / `*.EC` sign that `.SF`. Rebuild keeps names, CD order, and those uncompressed bytes, so jarsigner should still verify. Whole-file `source_*` may change. That is expected. ayzenpack does not re-sign. Do not store `cdata_blob` or `raw_zip` of a healthy jar just to keep a file hash.

Dehydrate still warns `signed JAR <name>` for exact and rebuild, and still packs. `--fail-on-signed` aborts. `--strict` does not promote the signed notice.

---

## Library

```rust
pub fn dehydrate(opts: &DehydrateOptions) -> Result<DehydrateSummary>;
pub fn rehydrate(opts: &RehydrateOptions) -> Result<()>;
pub fn list(input: &Path) -> Result<Manifest>;
pub fn verify(input: &Path) -> Result<()>;
```

Options structs are the lib contract; they do not read process-global state. Field tables and a YAML job-file loader: [docs/library.md](https://github.com/hilather/ayzenpack/blob/main/docs/library.md).

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
| Signed JAR silently broken | Detect; warn `signed JAR <name>`; jarsigner should still verify after rebuild; `--fail-on-signed` |
| Traversal via `--cas-dir` / output | `--clean` only deletes names we write. CAS paths are hex |
| `unsafe` | `forbid(unsafe_code)` |
| SSRF | No network in the crate |

Treat a `.ayz` as sensitive as the input JARs: it contains file contents and original `source_path` strings.

---

## Non-goals (v2 / 0.2.4)

- Recursively exploding nested JARs (depth **> 1** / unlimited). Depth-1 STORE `zip_index` is crate 0.2.4.
- A `--verbatim` / `--exact-cdata` CLI flag
- Java/zlib bit-identical whole-file hashes (Matt locked this out)
- Dual `cdata_blob` + content; `raw_zip` of listed jars
- CAS of `blake3(inner zip)` when the slot is (or should be) `zip_index`; per-JAR copies of the same uncompressed class bytes
- Requiring corpus lucene/jackson `source_*` until every method-8 slot is a measured hit
- Per-blob zstd frames as the default
- HTTP CAS, S3, split archives, GUI, Maven/Gradle plugins
- Renaming v1 manifest fields (`blob`, `uncompressed_size`, `local_header_offset`, …)
- zstd-framed / edition-2024 dependencies
- Tokio, reqwest, openssl

Future: `--explode-nested`, a class-file zstd dictionary. Crate 0.2.1 already never emits `cdata_blob`.
