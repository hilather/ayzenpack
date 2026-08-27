# PLAN: 0.2.4 stencil restore (ratarmount zip_index + codec recipes)

Base: `main` / tag `v0.2.3` at crate **0.2.3** / format **v2** (`21e73bb`). Branch `cursor/plan-stencil-restore-d2d9`. This PR is **plan-only**. Do not implement. Do not merge. Do not tag. Do not bump `Cargo.toml`. Do not change restore behavior in this PR.

Crate 0.2.3 no-nested-STORE-latch, crate 0.2.2 no-`raw_zip`-on-listed-jars, and crate 0.2.1 single-CAS / no-`cdata_blob` are **shipped and locked**. Do not reopen MixedExact, ExactWithExotic, `store_cdata`, grouping vs per-blob frames, format v3, or `raw_zip` of a listed jar. Do not merge #36 or #37 into this work.

Skeptic/plan skills live in this checkout (`.cursor/skills/skeptic-plan-review`, `knowledge/plan-skepticism`). Origin is not required. This file is the plan-function shape (repro first, file-level work, tests that must fail, non-goals). Never skip sweep 1. Fresh skeptic each sweep. Cap 3, then BLOCKED.

Constraints: MSRV 1.80, `forbid(unsafe_code)`, no edition-2024 deps, no zstd-framed crate. Manifest JSON field names stay (`blob`, `local_header_offset`, `cdata_blob`, …). Format stays **v2** (additive keys only; serde does **not** `deny_unknown_fields`). No new `cdata_blob` puts. No Java process / vendor `Deflater` subprocess. No `raw_zip` of a listed jar. No SQLite in the `.ayz`.

---

## Locked storage policy (unchanged)

Storage efficiency is of the utmost importance. Do **not** store a second encoding of the same entry to keep hashes.

1. One CAS blob per unique **uncompressed** entry (BLAKE3 of those bytes). Same class across JARs (including depth-1 nested libs) is one blob.
2. Manifest is a ZIP-slot **stencil** (ratarmount-style pointers): a write recipe used **after the source file is gone**. Offsets are coordinates in the reconstructed binary, not seeks into the original path.
3. **Never** store a second encoding of the same entry. No default `cdata_blob`. `ZipArchive` count ≠ homemade CD count is a **second bug**, not a `raw_zip` case. Crate 0.2.2 never writes `raw_zip` of a listed jar. Last-wins / overlap (`dup.txt`) stays skip-exact.
4. Zstd in 4 MiB record-aligned BLOB groups (already v2). Do not switch to per-blob frames. Do not store pre-deflated ZIP cdata as the CAS payload.
5. Restore walks the stencil: prefix, local header, payload, data descriptor, pad, tail (CD through EOF). `source_*` **must** match when every slot hits (STORE / recorded codec / exact child `zip_index`). `source_*` may change on a zopfli / unknown-deflate miss.
6. Read old packs (v1, leftover `cdata_blob`, leftover `raw_zip`, 0.2.3 flate2-only codecs) but **never write** dual-copy.

Do not copy ratarmount SQLite into the pack. Reuse **column names** from `hilather/ratarmount-rs` (`ratarmount-formats-zip` `ZipMemberTable`, `ratarmount-index` `DurableZipMember` / `nestedindexes` / `DurableFileRow`). Do not invent a parallel schema.

---

## Repro (required; measured on 21e73bb, this tree)

Matt's flags, not a generic `-d` restore:

```
ayzenpack dehydrate --recursive --sort-inputs --restore-paths -o pack.ayz <dir-of-jars>
ayzenpack rehydrate --restore-paths -i pack.ayz
```

`--overwrite` is **not** used and is not required: `--restore-paths` skips the overwrite guard, `prepare_restore_dest` unlinks, then `write_exact_jar` / `write_rebuilt_jar` / `write_jar` `File::create`.

0.2.3 **fixed** the ZipArchive latch (stub overwrite, e.g. 134 MB → 5.5 MB). Pack size is good. Rehydrate sizes are still wrong on Matt's retest (some improved).

Two remaining drift classes, both in tree on `21e73bb`:

### A. `write_jar` / rebuild deflate (classic Java JAR)

`src/deflate.rs` trials only `deflate-raw:flate2:{1,3,6,9}` (miniz_oxide). Java / Maven / `jar` / Info-ZIP emit zlib deflate. `match_deflate` misses. `jar_store_policy` then sets **CleanMiss** (`src/dehydrate.rs`): **one** miss drops **every** sibling `cdata_codec` on that jar.

Restore then:

- **tail present** → `Jar::metadata_rebuild` → `write_rebuilt_jar` (flate2 level 6 for **all** DEFLATE entries; new compressed sizes; `source_*` not checked).
- **no tail** → `write_jar` (`ZipWriter`). Different deflate. Size drifts.

`deflate.rs` `stored_block_is_a_miss` already proves BTYPE 00 is a miss. There is no `deflate-raw:zlib:*` and no `deflate-raw:stored`.

### B. Remaining skip-exact (not “first local == prefix”)

`zip_archive_opens` already requires `header_start + view_shift == prefix_len`. `cd_offset_to_zip_rel` then subtracts `prefix_len` when `view_shift == 0 && prefix_len > 0`. A healthy zip -A fat whose first local **is** the prefix becomes `zip_rel == 0` and **already slices** on 21e73bb.

In-tree (already `tail_blob`; **not** a 0.2.4 skip-exact gate; do not flip them to STORE):

| Fixture | `tail_blob` | Notes |
|---|---|---|
| `write_fat_spring_zip64_zipa_jar` | some | DEFLATE nested |
| `write_fat_spring_store_nested_zipa_jar` | some | `store_nested_zipa_fat_is_outer_listing_tail_no_raw_zip` |
| `write_fat_spring_store_nested_jar` | some | unadjusted prefix |

`min_off != 0` (`"first local header is not at zip offset 0"` / first local is not at ZIP offset 0) fires only when the first local is not at ZIP offset 0 after that convert — e.g. leading pad inside the zip portion (`offsetheader != prefix_len`). That is **not** the prefix / zip -A file-absolute layout. Do **not** encode `first offsetheader == prefix_len` as the 0.2.4 skip-exact fix; that is the in-tree splice case.

Matt's remaining fat skip-exact must be **measured** on 21e73bb (`slice_fail` string) before coding. Named remaining classes:

1. **Homemade `parse_central_directory` `None`** on a listed outer zip (`ZipArchive` has the outer listing, locals do not overlap). Today skip-exact. 0.2.4 may attach slot rows / codecs from `archive_local_records`. **Never set `tail_blob` on homemade `None`** (listing completeness is unproven; a short listing + phys CD→EOF tail + `write_exact_jar` is `HashMismatch` / corrupt, not skip-exact). Restore is `write_jar`. Not `raw_zip`. Teach `classify_local` that an exact child `zip_index` is a hit, not `Unreproducible` (`method==0 && blob==None` today). No always-on homemade-`None` fixture unless the implement PR writes a listed, non-latch recipe (do not use a Zip64 latch zip).
2. **Leading pad** so `min(zip_rel) != 0` after today’s convert. A bash/`launch.script` prefix is **absorbed** into `prefix_len` (`find_cd_first_local`) and becomes zip-rel 0 — that is **not** this fixture. Always-on Test 2: file **starts with ZIP magic** (`prefix_len == 0`) but the first CD local is not at 0 (decoy / hole not in the CD). Encode `[0, first offsetheader)` as `leading_pad_blob` / `leading_pad_size`. Do **not** extend `prefix_len`. Do not silently zero-fill. Locals must not overlap. A **prefix + hole** (`prefix_len > 0` and `min(zip_rel) != 0`) stays skip-exact in 0.2.4 (Test 2 is `prefix_len == 0` only).
3. **Overlap / last-wins / `dup.txt`:** stay skip-exact. Do not attach a broken tail.

Do **not** drop a restore-hash gate if measurement finds no real homemade-`None` fat. Always-on hash gate: existing `write_fat_spring_store_nested_zipa_jar` keeps `bit_identical_restore` + `source_*` after `zip_index`.

Nested `BOOT-INF/lib/*.jar` stay one opaque content blob on 0.2.3 (`nested_jar_not_exploded`). A STORE listable nested lib is a complete ZIP. 0.2.4 may replace that slot with a child `zip_index` only under §2.

---

## Goal (crate 0.2.4, format v2)

Stay on **format v2**. Target crate **0.2.4** (bump only in the implement PR). Additive keys only.

### 1. Stencil (ratarmount columns, not a new schema)

Treat each JAR/ZIP as a ratarmount-style stencil: a write recipe. Offsets are **not** seeks into the original path.

Reuse `hilather/ratarmount-rs` names:

| ratarmount-rs | Meaning | ayzenpack mapping |
|---|---|---|
| `files.offsetheader` / `ZipMemberTable.headers` | local header file offset | additive `offsetheader` (file-absolute). Keep writing existing `local_header_offset` as **zip-relative** so 0.2.3 packs still read. |
| `files.offset` / `ZipMemberTable.data_start` / `DurableZipMember.data_start` | payload start | additive `data_start` (file-absolute) |
| `files.size` | uncompressed | existing `uncompressed_size` |
| `files.type` / `ZipMemberTable.method` | compression method | existing `method_code` |
| `files.path` / `name` | member name | existing `name` |
| `files.mtime` / `mode` | time / unix mode | existing `dos_*` / `unix_mode` |
| `ZipMemberTable.compressed_size` | compressed size | existing `compressed_size` |
| `ZipMemberTable.encrypted` | encrypted | still fail dehydrate; do not decrypt |
| `ZipMemberTable.index` | CD index | existing `entries[]` order |
| `DurableZipMember` / `nestedindexes` | nested ZIP sidecar | additive child `zip_index` + `jars[].nestedindexes[]` (JSON, not SQLite) |

Each slot payload pointer is **ONE** of:

1. CAS `blob` (uncompressed BLAKE3) + optional `cdata_codec` recipe
2. child `zip_index` (`usize` index into `jars[].nestedindexes[]`)

Never both-`Some`. Do **not** add JSON Schema `oneOf` (schema already `required: ["blob"]` as hex|null; dirs are `"blob": null`; serde is two `Option`s). Keep `blob` required (hex|null). `zip_index` optional. Runtime refuse `blob.is_some() && zip_index.is_some()` on write and read. Explode path is JSON `blob: null` + `zip_index` set. Test: both-`Some` refused.

Reconstruct **pre-sizes** the file to `source_size` (outer) / child `uncompressed_size` and **seeks** each `offsetheader` (CD order ≠ offset order). Walk: `prefix_blob`, `leading_pad_blob` (if any), then each slot (`local_header_*` at `offsetheader`, payload, `data_descriptor_*`, pad), then `tail_blob` (CD through EOF). Do not concat slots in CD order. New packs set `offsetheader = prefix_size.unwrap_or(0) + zip_rel` and `data_start = offsetheader + header.len()` in `fill_exact_entry` (`src/dehydrate.rs` ~932 today stores only zip-rel `local_header_offset`; `jar.prefix_size` is already set before `attach_exact`).

Old packs without `offsetheader`: restore stays `prefix_len + local_header_offset` (`write_exact_entry` today). New packs write **both**. 0.2.4 exact splice seeks `offsetheader` (file-absolute) and must **not** add `prefix_len` a second time. `detect_offset_mode` / `write_rebuilt_jar` keep taking **zip-rel** `local_header_offset`. Feeding `offsetheader` there makes zip -A look ZipRel (`cd_off == prefix_len == first_lh`) and corrupts rebuild.

Do not rename `blob` / `local_header_offset` / `cdata_blob`. Do not put SQLite in the `.ayz`.

### 2. Nested ZIP = child zip_index (depth 1)

Today `commit_blob` / `remember_blob` runs **before** `attach_exact` (`src/dehydrate.rs`). The STORE nested jar is already in the zstd BLOB group. Nulling `entry.blob` later leaves `blake3(inner.zip)` in `blobs[]` next to exploded class blobs (the 200MB→3GB failure mode). `content_blob_ids` that only read `jars[].entries[].blob` can stay green while `blobs[]` still holds the whole inner ZIP.

A STORE (`method_code == 0`) member whose uncompressed bytes are a **listable** ZIP may become a child `zip_index` **only** if dehydrate **never CAS-es those combined bytes**. Do not explode by name (`BOOT-INF/lib`). DEFLATE-wrapped nested (method 8), including mix `spring-zip64-nested.jar` / `write_fat_spring_zip64_zipa_jar`, stay opaque.

`commit_blob` on this tree does `remember_blob` **and** `jar_entries.push`. Pushing inner files through `commit_blob` / `--jobs` `Sequenced` adds extra **outer** rows → `slice.locals.len() != jar.entries.len()` → `attach_exact` skip-exact (no tail) on every exploded fat.

Rules:

- Decide opaque vs `zip_index` **before** `remember_blob` **and** before `--jobs` `spawn_file` of the combined inner ZIP. Probe the in-memory STORE payload with a new `slice_from_bytes(&[u8])` (do not write a temp file; do not `slice_from_archive` on a path we never CAS). Classify child slots **without** calling today’s `classify_local` on scan rows that still have `blob == None` (that is `Unreproducible` for every STORE file, `src/dehydrate.rs` ~884–887 — would withhold `zip_index` on any nested ZIP that contains a STORE member, including Test 8 `write_stored_zip`). Hash in-hand plains into a scratch `Entry.blob` (or compare `local.cdata` to the scan payload) **without** `remember_blob` / first-seen. Decide emit from those in-hand classes **plus in-hand tail bytes** — do **not** call `Jar::bit_identical_restore()` on a half-built child (`tail_blob` is still `None` so the method is always false, `src/manifest.rs` ~63–65). No child `remember_blob` during the probe. No commit-then-rollback (`first_seen` / END digest cannot un-CAS).
- **Child probe must not fail the outer.** Today `archive_local_records` returns `AyzenpackError::Encrypted` (`src/exact.rs` ~178) and `attach_exact` rethrows that (`src/dehydrate.rs` ~849). `slice_from_bytes` / the child probe maps encrypted child, empty listing, overlap, homemade count mismatch, latch (`zip_archive_opens` on the **child** bytes — same outer-listing rule as 0.2.3), or slice `Err` to **opaque**. Do not propagate `Encrypted` from a child. CRC-check the in-hand STORE payload (`check_crc`) before withholding CAS: mismatch → opaque (still `commit_blob` the combined bytes) unless `--strict` fails like today. Not-a-ZIP / unlistable → opaque.
- **`jobs==1` is the default and Matt’s CLI** (`src/cli.rs` ~65; pipeline is `None` unless `jobs > 1`, `src/dehydrate.rs` ~419–423). The sequential path today always `commit_blob`s (`~531–535`). **One helper** (`explode_store_member` or equivalent) used by **both** paths, at the same first-seen point as that outer member’s seq:
  - Probe via `scan_from_bytes` + `slice_from_bytes` (genericize scan as well as `archive_local_records` / `read_local` — slice locals have no `name` / `dos_*` / plains; `classify_local` needs an `Entry`). Check listing `uncompressed_size` **before** any `inflate_raw` / `io::copy`. Cap decoder output at `min(listing, max-entry-bytes)`; overflow → opaque.
  - If explode: `remember_blob` inner **files and** child tail / headers / `leading_pad_blob` / `prefix_blob`; push `NestedIndex`; one outer `blob: None` + `zip_index`. Never `commit_blob` the combined ZIP. Collect `Vec<NestedIndex>` during the scan callback (no `Jar` yet, `src/dehydrate.rs` ~474–585) and move it onto `Jar` after the loop.
  - If opaque: today’s `commit_blob` / `spawn_file` of the combined bytes.
  - `jobs==1`: call the helper instead of `commit_blob` at `~535`.
  - `jobs>1`: `apply_sequenced` on `Sequenced::Exploded` calls the **same** helper (do not remember child tail later in `attach_exact` on one path and inside Exploded on the other — that desyncs first-seen / END digest).
  `attach_exact` slices the **outer** file only and cannot CAS child-stencil bytes.
- **`--jobs` first-seen (blocking if omitted).** `Sequenced` is only `Dir | File` today; `File` always `commit_blob` (`src/dehydrate.rs` ~110, ~325). Explode must **not** `remember_blob` on the main thread while earlier seqs are still in-flight (that reorders `first_seen` / END digest vs `jobs==1`). Add `Sequenced::Exploded`:
  1. `wait_admit(sum of inner file bytes)` (today `wait_admit` is the only inflight gate; `spawn_file` is the only increment, ~188).
  2. Consume **exactly one** `next_seq` for the outer member (like `push_dir` / today’s one `spawn_file`). Do not `spawn_file` the combined ZIP (that would CAS it). Do not `spawn_file` each inner file (that increments `next_seq` per inner → extra outer rows).
  3. Hash inner file bytes (sync on the main thread after admit, or a pool job that still emits **one** seq). Insert `Exploded` into `pending`. Increment `inflight_bytes` by that inner-file sum; `take_in_order` must subtract it (today only `Sequenced::File` subtracts).
  4. `apply_sequenced` on `Exploded`: `remember_blob` each inner file + child tail / headers / child `leading_pad_blob` / child `prefix_blob` if any; push `Entry` rows **only** onto `nestedindexes[i].entries`; push **one** outer `entry_from_scan` with `blob: None` + `zip_index` (never `commit_blob`).
  `finish_jar` still waits `received == spawned`; `Exploded` is pending-only like `Dir`, so do not bump `spawned` unless hashing is a tracked pool job. `--sort-inputs` packs stay jobs-invariant. After apply, drop the combined `Vec`.
- If explode: never hash-commit the combined bytes. `jars[].entries.len()` stays the outer `ZipArchive` listing.
- If opaque: `remember_blob` / `commit_blob` the combined bytes as today (then `blake3(inner) ∈ blobs[]`).
- Assert on explode: `blake3(inner zip) ∉ blobs[]`. Unique **content** count = outer non-zip `Some(blob)` + all `nestedindexes[].entries[].blob`.
- `Jar` gains additive `nestedindexes: Vec<NestedIndex>` (same optional-skip style as `prefix_blob`). Each item is a **tail-bearing** stencil with the same slot fields as a `Jar` **minus** `nestedindexes` (depth 1): `entries` + `tail_blob`/`tail_size` + local-header / descriptor / pad + child `prefix_blob` if the inner file itself is prefixed + child `leading_pad_blob` if the inner ZIP has a PK-start hole. `reconstruct_child_zip(index, get_blob: impl Fn(&str) -> Result<Vec<u8>>) -> Vec<u8>` walks prefix → leading_pad → slots → tail. Child payloads are CAS, so `&NestedIndex` alone is not enough. `verify` passes the in-memory `payloads` map; rehydrate passes `cas_dir`. Used by `write_exact_entry`, `write_rebuilt_jar`, `write_jar`, and `verify`. Entries-only (no tail) would `ZipWriter` the child and lose `source_*` on fats that already splice.
- Emit `zip_index` only when the in-hand child would exact-restore (every child slot STORE/codec/empty dir; child has tail bytes). Do not use `Jar::bit_identical_restore()` as the predicate until after explode `remember_blob`s the child tail. Otherwise keep the opaque blob. A later outer skip-exact does **not** put the combined bytes back (they were never CAS-ed). Restore of that outer is `write_jar` + `reconstruct_child_zip` (uncompressed outer member bytes still match; `source_*` may change).
- **Opaque fallback (today’s blob, no `zip_index`):** child listing empty; **encrypted child** (do **not** decrypt; do **not** fail the outer — today only the outer `zf.encrypted()` fails, `src/scan.rs` ~177; an encrypted inner STORE payload is opaque bytes); overlap / last-wins / homemade count mismatch; child would not be exact; any child entry’s **listing** `uncompressed_size` exceeds `--max-entry-bytes` (**check listing sizes before `inflate_raw`** — `classify_local` inflates today at `src/dehydrate.rs` ~897; a small STORE zip can hide a huge DEFLATE member); child file-entry count (`!is_dir`) **> 65535** (`NESTED_MAX_FILE_ENTRIES`). Overflow → opaque, `blake3(inner) ∈ blobs[]`. Always-on **unit** on the cap predicate (fake listing of 65536 names). Do not build a 65536-file zip in CI. Depth 1 does not stop a 100k-file STORE bomb.
- Depth **1** only. Do not recurse into a child’s nested ZIPs.
- `zip_index` is a `usize` index into `jars[].nestedindexes[]`. Outer slot JSON: `blob: null` + `zip_index` set.
- Encrypted **outer** listing: fail like today. Encrypted **child**: opaque (see above).
- `nested_jar_not_exploded` (`tests/roundtrip.rs`, `write_jar` **DEFLATE** nested) **stays opaque**. It flips only if rewritten onto a STORE listable child. Do not change that test to expect `zip_index`.
- `slice_from_bytes` requires implement PR to genericize `archive_local_records` / `read_local` to `Read + Seek` (they take `Path` / `&mut File` today, `src/exact.rs` ~169, ~528). The generic `Encrypted` path must be catchable: child probe returns opaque; outer `attach_exact` still fails the job. Probe uses `zip_archive_opens` on the child bytes (latch → opaque, not a wrong listing).

0.2.3 readers ignore unknown keys and will not restore a 0.2.4 pack that replaced a nested blob with `zip_index`. That is acceptable. 0.2.4 must keep reading 0.2.3 packs (opaque nested blob + flate2-only `cdata_codec`).

### 3. Codec recipes (no `cdata_blob`)

Closed set for JARs. Record **only** the codec id that hit the original cdata (already read during slice). Restore re-encodes. No `cdata_blob`.

| id | When |
|---|---|
| STORE (method 0, `csize == usize`, content `blob` matches local cdata) | already exists; no `cdata_codec` string |
| `deflate-raw:zlib:{1,6,9}` | Java / Maven / `jar` / Info-ZIP |
| `deflate-raw:flate2:{1,3,6,9}` | already exists |
| `deflate-raw:stored` | method 8, raw DEFLATE BTYPE 00 |

Anything else (bzip2, zopfli, encrypted, unknown deflate) stays **rebuild that entry**. No `cdata_blob`. No `raw_zip` of a listed jar.

**Pin zlib-rs in-process** for **raw** DEFLATE (`window_bits = -15` / nowrap), not a zlib container. Do **not** spawn Java. Do **not** enable flate2 `zlib` / `zlib-ng` (C). Do **not** enable flate2’s `zlib-rs` feature — that would retcon `deflate-raw:flate2:*` and break 0.2.3 pack reads. Keep today's flate2 `rust_backend` / miniz path for `deflate-raw:flate2:*`. Two backends, two codec prefixes.

zlib-rs 0.6.7 is edition 2021 / rust-version 1.75 at time of plan. Implement PR must pin a version that is **not** edition-2024 and that stays on 1.80 in CI (`cargo +1.80 test`). If the pin pulls edition-2024 deps, pin an older line. Do not relax MSRV.

Trial original cdata at pack time. Suggested order after STORE classification: zlib GPBF-hint ∩ `{1,6,9}`, then remaining zlib `{1,6,9}`, then existing flate2 `trial_levels`, then `deflate-raw:stored`. First bitstream match wins. zlib **3** is not in Matt’s closed set — GPBF hint 3 is an accepted miss unless flate2 hits. Cache by `(blob, compressed_size, flags, blake3(original cdata))` (or re-verify the trial against this entry’s cdata on cache hit) so two encodings of the same uncompressed bytes cannot alias.

`cdata_codec` schema pattern today is `^deflate-raw:flate2:[1369]$`. Implement PR widens it. Serde does not `deny_unknown_fields`; 0.2.3 `parse_codec` will reject a zlib string if it ever sees one — 0.2.4 packs are not a 0.2.3 restore target.

### 4. Per-entry codecs (drop jar-level CleanMiss)

Today `jar_store_policy`: any `DeflateMiss` / `Unreproducible` → `CleanMiss` → **no** sibling `cdata_codec`. A zopfli neighbor wipes Maven hits.

0.2.4: a miss must **not** drop sibling codecs. `fill_exact_entry` records a codec on that entry iff that entry hit. `LocalClass::DeflateHit(u32)` + `deflate::parse_codec` → `deflate_raw` cannot represent zlib vs stored (`src/dehydrate.rs` ~860, `src/rehydrate.rs` ~576). Change `match_deflate` / `LocalClass` / `parse_codec` / `resolve_cdata` together (enum: `zlib` / `flate2` / `stored`).

Whole-file exact splice (`verify_source_identity` / `source_*` checked) only when **every** slot can resolve cdata: STORE with a content `blob`; recorded codec; empty STORE dir; child `zip_index` whose nested stencil `bit_identical_restore`s.

`Entry::can_exact_cdata` / `Jar::bit_identical_restore` / `resolve_cdata` / `write_exact_entry` / `write_rebuilt_jar` / **`write_jar` / `verify` (`src/lib.rs`)** must change together (today a STORE slot with `csize == usize` is exact even when `blob` is null → `read_entry_content` → missing blob; `verify` does `e.blob` + CRC of that blob at `src/lib.rs` ~183 and mix already `verify(&out)`):

- `blob.is_some() && zip_index.is_some()` is an error (`blob: null` + `zip_index` is the explode path).
- `zip_index` is exact iff `Jar` looks up `nestedindexes[i]` and that stencil `bit_identical_restore`s (`Entry::can_exact_cdata` cannot see the side table). STORE files that are **not** exploded still require `blob.is_some()` (today `method_code == 0 && csize == usize` is exact even when `blob` is null, `src/manifest.rs` ~87). Payload is `reconstruct_child_zip` bytes. Never `read_entry_content` on the outer slot.
- **`verify`:** if `zip_index` is set, require `blob` is null; `reconstruct_child_zip(..., payloads)`; `crc32fast(bytes) == entry.crc32`; `bytes.len() == uncompressed_size`. Also walk `jars[].nestedindexes[]` the same way as a jar (child `tail_blob`, each child file `blob`, `local_header_blob`, `pad_blob`, child `prefix_blob` if any). Walk `leading_pad_blob` like `prefix_blob`. Do not require an outer file `blob` on a `zip_index` slot.
- **`write_jar`:** same `reconstruct_child_zip(..., cas_dir)` bytes as the file payload (ZipWriter then STORE/DEFLATE that buffer). Skip-exact outer after explode has **no** opaque inner CAS — this is the only restore path for that member. No `cdata_blob`, no `raw_zip` of the listed outer, no late `commit_blob` of `blake3(inner)`.
- Mixed outer **or** non-exact child → `metadata_rebuild` only when `tail_blob` is present. No `source_*` check. If `tail_blob` is absent, `write_jar` (including `zip_index` slots). `write_rebuilt_jar` emits child-stencil bytes for a `zip_index` slot (not a content blob).
- `write_rebuilt_jar` today ignores dir `cdata_codec` (`allow_rebuild && is_dir`). After zlib empty-dir hits, a mixed rebuild can rewrite `03 00` as flate2:6. **Accept that** on a miss jar (hash may change). On an exact jar, dirs use the recorded codec and must not take that ignore path.
- `write_exact_jar`: after `prefix_blob`, write `leading_pad_blob` at file offset `prefix_len` (covers `[prefix_len, first offsetheader)`). Then existing per-slot seeks. `set_len(source_size)` unchanged.

Mixed jar (zopfli / unknown miss):

- Hits keep their codec and produce the original cdata.
- The miss entry is rebuilt (flate2 level 6, or STORE if that is the rebuild class).
- If the miss `compressed_size` changes, later `offsetheader` values cannot be kept: take today’s `write_rebuilt_jar` **whole-local rewrite** (do not invent a partial-offset patcher). Sibling **cdata** still uses recorded codecs (`parse_codec` becomes an enum: `zlib` / `flate2` / `stored`; `resolve_cdata` must not keep calling flate2-only `deflate_raw`).
- `source_*` may change. Do not require whole-file hash on a miss jar.

Do not fall back to jar-wide CleanMiss. Do not put `cdata_blob` on the miss.

`tests/corpus.rs` `proven_miss` today requires a non-`bit_identical_restore` jar to have **no** method-8 `cdata_codec` (0.2.3 CleanMiss). The implement PR **must rewrite** that assertion: a rebuild jar may mix method-8 hits and misses; do not require “exactly one miss” or “zero codecs”. No `source_*` on that jar. Also rewrite `jar_store_policy_miss_or_exotic_is_clean_miss` (`src/dehydrate.rs` ~1167) — that unit dies with jar-wide CleanMiss.

### 5. Slice: remaining skip-exact only (not first-local==prefix)

Do **not** change the gate to “accept `first offsetheader == prefix_len`”. That layout already slices on 0.2.3 after `cd_offset_to_zip_rel`.

`slice_from_archive` today `Err`s on homemade `None` (`src/exact.rs` ~144) and `attach_exact` **always** `remember_blob`s `slice.tail` on `Ok` (`src/dehydrate.rs` ~843–846). `ExactSlice` is `{ locals, tail }` with no homemade flag. The obvious “don’t Err on parse None” edit hits that `Ok` arm and **sets `tail_blob`** — the failure mode §B.1 names.

0.2.4 **must** change the return type, e.g. `ExactSlice { locals, tail: Option<Vec<u8>> }` or `homemade_ok: bool`. `attach_exact` sets `tail_blob` only when homemade parse is `Some` **and** `locals.len() == jar.entries.len()`. Phys CD→EOF bytes on homemade `None` are **not** stored as `tail_blob` (even if every listing slot hits). Overlap / last-wins still `Err` (skip-exact, no tail). Always-on **unit**: a listed homemade-`None` fixture has `tail_blob.is_none()` (no fat `source_*` gate required).

- **Leading pad (`min_off != 0`, `prefix_len == 0`):** do not `Err`. Read `[0, first offsetheader)` into `leading_pad_blob`. Then slice locals. Do not extend `prefix_len`. Do not zero-fill. Prefix + hole stays skip-exact.
- **Overlap / last-wins / encrypted / `NotZip`:** unchanged skip-exact / fail.
- Out-of-range `zip_index` fails `verify` / restore (`Format` / `HashMismatch`), not a panic.

`write_exact_entry`: if `offsetheader` is present, seek **there**. Never `prefix_len + offsetheader`.

### 6. Docs / contract (implement PR, not this plan-only PR)

This plan-only PR updates `PLAN.md`, the intended-model paragraphs in `DESIGN.md` / `AGENTS.md`, and the `tests/docs.rs` PLAN identity strings. It does **not** bump the crate, touch `src/`, or change restore. New `Option` fields (`offsetheader`, `data_start`, `zip_index`, `leading_pad_*`, `nestedindexes`) use the same `#[serde(default, skip_serializing_if = …)]` as `prefix_blob` (`src/manifest.rs` ~27–48) so 0.2.3 packs still deserialize.

| File | Implement PR |
|---|---|
| `Cargo.toml` / lock | Crate **0.2.4**. Pin zlib-rs (or equivalent). MSRV 1.80. |
| `AGENTS.md` | Current-tree line → crate **0.2.4**. Keep storage-efficiency / no-default-`cdata_blob` / 4 MiB groups / mix `569539 * 115 / 100` / `cdata_blob == 0` / MSRV **1.80** / `forbid(unsafe_code)`. **Replace** (do not keep) the sentence `Do **not** add a Java/zlib deflater, \`cdata_blob\` for misses, or \`raw_zip\` of healthy jars` with: do not add a Java subprocess / vendor `Deflater`; do not add `cdata_blob` for misses; do not `raw_zip` a listed jar; in-process zlib-rs raw-deflate hits are the 0.2.4 path. Update `tests/docs.rs` in the **same** implement PR. |
| `DESIGN.md` | Reconstruction + executable JAR: stencil walk; codec set; per-entry codecs; depth-1 `zip_index`; remaining skip-exact classes in §B. Keep `North star: **one CAS blob + ZIP index + zstd blocks**`. Nested STORE listable libs are `zip_index` on new packs when §2 applies. |
| `README.md` / `docs/library.md` | Do not tell agents to store `cdata_blob` or chase bit-identical hashes via dual copy. Keep `Crate **0.2.1** never writes \`cdata_blob\` (file or dir, any method)`. |
| `schemas/manifest.v1.schema.json` | Additive optional keys (`offsetheader`, `data_start`, `zip_index`, `nestedindexes`, `leading_pad_blob` / `leading_pad_size`). Add `$defs.nestedindex` (entries + tail + child prefix/leading_pad; **no** nested `nestedindexes`). Schema is `additionalProperties: false` on jar/entry. Widen `cdata_codec`. Keep `blob` required (hex\|null). No schema `oneOf`. Runtime refuse both-`Some`. `zip_index` OOB uses `.get()` → `Format` / `HashMismatch`, not `[]`. |
| `tests/docs.rs` | PLAN identity stays the 0.2.4 title strings. Implement PR updates the Java/zlib locked phrase as above. |
| `tests/corpus.rs` | Rewrite `proven_miss` (see §4). |

---

## Tests (must fail on 0.2.3, not log)

Do **not** restage a test that is already green on 21e73bb and call it the 0.2.4 gate. In-tree zip -A fats already have `tail_blob`.

1. **zlib-rs classic (always-on, not “Java-built”).** Bake a DEFLATE member whose raw cdata is a **fixed zlib bitstream** (bytes in the test, or built in-process with the zlib-rs pin; raw/nowrap, not `DeflateConfig::default()` zlib wrapper). The unit must call today’s `deflate::match_deflate` and assert `None` (not empty payload, not a miniz collision) so it fails on 0.2.3 — put that assert in `src/deflate.rs` (`deflate` is crate-private). After 0.2.4: `cdata_codec` is `deflate-raw:zlib:{1,6,9}`, no `cdata_blob`, Matt CLI, restored `source_size` and `source_sha256` match. **Java/Maven `source_*` is not promised until measured.** When `AYZENPACK_CORPUS_DIR` is set, the implement PR first counts method-8 / flate2 hits / zlib-rs `{1,6,9}` hits on lockfile lucene-* / jackson; put whole-file `source_*` **only** on jars where every slot hits. A remaining miss jar stays rebuild (siblings keep codecs). Do not call the zlib-rs fixture “Java-built”. Do not require a network fetch. zlib-rs 0.6.7’s Rust API is marked unstable — pin and use whatever raw/nowrap API that line actually exports; do not enable flate2’s `zlib-rs` feature. `deflate-raw:stored` must handle multi-block BTYPE 00 (plains > 65535; `raw_stored_deflate` in fixtures is u16-only).

2. **Leading-pad ZIP (always-on).** File starts with `PK` (`prefix_len == 0`) but first CD local is not at 0, so `min(zip_rel) != 0` on 21e73bb (`slice_from_archive` `Err`, `tail_blob.is_none()`, `source_sha256` mismatch). Do **not** use `launch.script` (that becomes `prefix_len` and slices). Use a **non-zero** decoy (`PK\x03\x04` garbage): `write_exact_jar` already `set_len(source_size)` (`src/rehydrate.rs` ~516), so a zero hole can pass `source_*` without `leading_pad_blob`. After 0.2.4: `leading_pad_blob` bytes equal `[0, first offsetheader)`, `tail_blob.is_some()`, `raw_zip_blob.is_none()`, every `cdata_blob` absent, Matt CLI, `source_*` match. Homemade-`None` is **not** this test. A mixed leading-pad + codec-miss jar takes `write_rebuilt_jar` (locals at 0, hole dropped); `source_*` may change.

3. **Depth-1 child zip_index + existing STORE zip-A hash (always-on).** Use `write_fat_spring_store_nested_zipa_jar` (already splices on 0.2.3). After 0.2.4: each STORE listable lib is `zip_index` + `blob: null`; `blake3(inner zip) ∉ blobs[]`; child stencil has `tail_blob`; `reconstruct_child_zip` bytes equal the original inner ZIP; outer `bit_identical_restore` and `source_*` still match; `verify` of the pack succeeds; no `raw_zip`; unique content = outer non-zip `Some(blob)` + nestedindexes file blobs. Both-`Some` refused. Cap predicate unit (65536 names) stays opaque. DEFLATE-wrapped Zip64 fat stays opaque. Rewrite helpers that assume a file `blob` (`assert_listed_no_dual`, `content_blob_ids`, restore_paths `BOOT-INF/lib` == `blake3(inner)`, `in_place_restore_paths_two_fats_share_nested_lib_blobs`). `two_jars_share_nested_lib_content_blob` uses `write_jar` DEFLATE nested and stays opaque. On 0.2.3 the STORE libs are opaque blobs (this `zip_index` assertion fails). Also add `--sort-inputs` jobs=1 vs jobs=N pack-bytes on a STORE-nested fat (today’s jobs-identity test is plain files only).

4. **Per-entry miss does not drop siblings.** One jar: a flate2- or zlib-reproducible DEFLATE file **plus** a bitstream that is **not** BTYPE 00 and not zlib/flate2 (handmade dynamic Huffman / zopfli-style). After 0.2.4: the hit keeps `cdata_codec`; the miss has no codec and no `cdata_blob`; restore functionally identical; miss entry rebuilt only. On 0.2.3 CleanMiss clears the hit. No `source_*` match. **Do not** use BTYPE 00 / `raw_stored_deflate` as a miss — after `deflate-raw:stored` those fixtures are **hits**. Rewrite **every** stored-block “miss” assert, including `src/deflate.rs` `stored_block_is_a_miss` (`match_deflate == None` on a local `raw_stored_block`) and every `raw_stored_deflate` / `write_deflate_miss_*` / `write_stored_block_deflate_*` caller:
   - `classify_stored_block_deflate_is_miss`
   - `codec_miss_rebuilds_valid_zip_keeping_extras`, `codec_miss_with_prefix_rebuilds`, `signed_rebuild_is_not_exact_restore`
   - `maven_empty_deflate_dir_does_not_force_cdata_blob` (`write_deflate_miss_plus_empty_deflate_dir`): after 0.2.4 both slots hit (stored file + `flate2:6` empty dir) → `bit_identical_restore`; still `cdata_blob == 0`
   - `class4_miss_plus_dir_cdata_rebuilds_empty_store_dir` (`write_deflate_miss_plus_dir_cdata`): file becomes a stored-codec **hit**; class-4 dir still forces rebuild (`metadata_rebuild`); do not assert `file.cdata_codec.is_none()`
   Add a new unknown-deflate (non-BTYPE-00) fixture for Test 4 / true `metadata_rebuild`. Mix `proven_miss` rewritten in the same PR.

5. **Mix / corpus** when present: keep `cdata_blob == 0` on every mix entry, `output_len <= 569539 * 115/100`, `raw_zip_blob` absent. Unique content blob count must not double. Assert `blake3(inner zip) ∉ blobs[]` **only** for STORE slots that took `zip_index`. Mix `spring-zip64-nested.jar` is DEFLATE-wrapped (`write_wrapped_zip64_jar`) and **stays opaque** — that whole-jar blob must remain (no exploded second copy). Do not require whole-file hash on a remaining codec-miss jar. Do not loosen the size gate. Rewrite `proven_miss` (sibling codecs allowed).

6. **Overlap / last-wins unchanged:** `unique_overlap_content_blobs_not_dual_copy`; `dup.txt` still skip-exact, no `raw_zip`.

7. **0.2.3 packs still read:** check in `testdata/v0.2.3-tiny.ayz` **or** dehydrate a tiny flate2 + opaque-nested jar with the unmodified 0.2.3 tree in the implement PR, then rehydrate that file on 0.2.4. Do **not** treat an in-memory `Manifest` (serde ignore-unknown) as the gate. This test stays green on 0.2.3 (it is a compat read, not a 0.2.4 feature gate).

8. **Skip-exact outer after explode (always-on).** One listed jar using `write_overlapping_local_zip` (same offset, two names) **or** the `dup.txt` last-wins homemade-count mismatch, **plus** one STORE listable inner zip (`write_stored_zip` or `inner_incompressible_jar`). On 0.2.3: skip-exact, opaque inner blob. After 0.2.4: inner is `zip_index` + `blob: null`, `blake3(inner) ∉ blobs[]`, no outer `tail_blob`, `write_jar` succeeds, `verify` succeeds, uncompressed inner bytes match `reconstruct_child_zip`. `source_*` may change. Proves there is no opaque-CAS fallback.

Matt CLI for (1)(2)(3): `dehydrate --recursive --sort-inputs --restore-paths`; `rehydrate --restore-paths` only. No `--overwrite`. No `-d`.

---

## Non-goals

- Dual `cdata_blob` + content
- `raw_zip` of listed jars
- Java process / vendor `Deflater` as a subprocess
- SQLite in the `.ayz`
- Renaming v1 JSON fields (`blob`, `local_header_offset`, `cdata_blob`, …)
- Unlimited nested explode / zip-bomb walks (cap depth 1, keep `--max-entry-bytes`)
- Merging #36 or #37 into this work
- Format v3 / per-blob zstd frames
- `--verbatim` / `--exact-cdata` CLI flag
- Requiring `source_*` match on zopfli / unknown-deflate miss
- This plan-only PR implementing any of the above

---

## Done when (implement PR)

- A zlib-rs-built classic (always-on) and, when corpus is present, a Java-built Maven classic restore with matching `source_size` and `source_sha256`
- Classic STORE nested zip-A (`write_fat_spring_store_nested_zipa_jar`) keeps matching `source_*` after `zip_index`. Leading-pad fat matches `source_*`. Zip64 DEFLATE-nested fat stays opaque (do not flip it to STORE).
- Nested libs are child `zip_index`es, not `raw_zip` / not a second whole-ZIP CAS
- `cdata_blob == 0` on new packs; unique uncompressed blob count does not double
- A zopfli / unknown-deflate miss still rebuilds **that entry only** (siblings keep codecs)
- Matt CLI: `dehydrate --recursive --sort-inputs --restore-paths`; `rehydrate --restore-paths` only (no `--overwrite` required)

---

## Skeptic review (plan)

Origin `skeptic-plan-review` was not reachable. Fresh adversarial Task subagents. Never skip sweep 1. Cap 3, then BLOCKED. Fix BLOCKING findings in this file. Stop at a clean plan or BLOCKED. Do not implement even if the plan is clean.

### Sweep 1 — REVISE (5 blockers, applied)

1. `first offsetheader == prefix_len` is the 0.2.3 splice case, not a 0.2.4 skip-exact fix. Remaining classes: homemade-`None` (exact-only) and leading-pad (record hole). Measure `slice_fail` before coding.
2. `commit_blob` before `attach_exact` would leave `blake3(inner.zip)` in `blobs[]`. Withhold CAS; explode while payload is in hand; opaque fallback if child skip-exact.
3. Specify `can_exact_cdata` / `resolve_cdata` / `write_exact_entry` / `write_rebuilt_jar` for `zip_index`; both-set refused; mixed → no `source_*`.
4. Rewrite corpus `proven_miss` (CleanMiss leftover).
5. Do not keep the AGENTS “Do not add a Java/zlib deflater” sentence; zlib-rs fixture is not “Java-built”.

### Sweep 2 — REVISE (7 blockers, applied)

1. Do not `commit_blob` inner files (that pushes outer `jar.entries`). `remember_blob` only; rows go on `nestedindexes[i].entries`.
2. Decide opaque vs explode before `--jobs` `spawn_file` / `remember_blob` of the combined ZIP. No commit-then-rollback.
3. `nestedindexes[]` is tail-bearing; `reconstruct_child_zip`; emit `zip_index` only if child would `bit_identical_restore`.
4. Homemade-`None`: set `tail_blob` only if every slot is a hit; `zip_index` is not `Unreproducible`.
5. Do not drop the fat hash gate. Always-on: existing STORE zip-A `source_*` after `zip_index`; Test 2 = leading-pad.
6. No schema `oneOf`. Runtime both-`Some` refuse. `blob: null` + `zip_index` is explode.
7. Child file-entry cap **65535** now; cap+1 stays opaque.

### Sweep 3 — REVISE (1 blocker, applied). Cap 3 reached — **BLOCKED** (no fourth skeptic)

1. `verify` (`src/lib.rs`) and `write_jar` must resolve `zip_index` via `reconstruct_child_zip`. Mix already `verify(&out)`. Skip-exact after explode has no opaque inner CAS. Folded. Should-fix folded: Test 1 must be a proven flate2 miss; rewrite blob-assuming helpers; name `--jobs` `Sequenced` deferred remember.

### Pre-loop 2 plan edits (this PR, before a new skeptic loop)

Closed holes a fourth skeptic would still hit: `slice_from_bytes` child probe; `verify` walks `nestedindexes` + `leading_pad_blob` + zip_index CRC; `write_exact` writes `leading_pad_blob`; homemade-`None` listed branch (drop tail unless all hits); Test 8 skip-exact outer + explode; Test 1 baked zlib miss; outer explode entry is `blob: None` without `commit_blob`.

### Loop 2 sweep 1 — REVISE (applied)

1. `deflate-raw:stored` hits `write_stored_block_deflate_zip`. Test 4 / existing miss tests must use a non-BTYPE-00 unknown deflate; stored-block becomes a hit.
2. Encrypted **child** is opaque; do not fail the outer. Encrypted **outer** still fails.

### Pre-sweep-2 hole close (this PR)

3. `--jobs`: `Sequenced::Exploded` (one seq, `wait_admit` inner-file sum, `remember_blob` only in `apply_sequenced`). Do not remember inners on the main thread while earlier seqs are in-flight.
4. Child probe must not propagate `archive_local_records` `Encrypted`; CRC-fail / unlistable / latch → opaque.
5. `NestedIndex` carries child `leading_pad_blob`. `reconstruct_child_zip` takes `get_blob`.
6. PLAN identity string `first local is not at ZIP offset 0` kept for `tests/docs.rs`.

### Loop 3 sweep 1 — REVISE (applied)

1. `jobs==1` (Matt CLI default) must probe/explode before `commit_blob`; `Sequenced::Exploded` is not the only path.
2. Rewrite **every** `raw_stored_deflate` caller (`maven_empty_deflate_dir_*`, `class4_miss_plus_dir_cdata_*`), not four named tests.
3. Mix `blake3(inner zip) ∉ blobs[]` only for STORE `zip_index` slots; Zip64 DEFLATE nested stays an opaque whole-jar blob.
4. Homemade-`None`: never `tail_blob`. Child listing `uncompressed_size` checked before `inflate_raw`. Java corpus `source_*` only after a hit-count. Test 7 is a real 0.2.3 pack. Prefix+hole stays skip-exact.

### Loop 3 sweep 2 — REVISE (applied)

1. One explode helper for `jobs==1` and `Sequenced::Exploded`: CAS child tail/headers/pads, not “inners only”. Same first-seen slot. `scan_from_bytes` (slice locals have no names/plains).
2. `ExactSlice` needs `homemade_ok` / `tail: Option`; `attach_exact` must not set `tail_blob` on the obvious “don’t Err” edit. Unit: homemade-`None` ⇒ `tail_blob.is_none()`.
3. Do not promise lucene/jackson whole-file `source_*` until method-8 / zlib-hit counts are measured. Always-on gate is the baked zlib bitstream.

### Loop 3 sweep 3 — ACCEPT

`NO BLOCKING FINDINGS`. Discretionary folds (not a fourth sweep): probe must not use `classify_local` on `blob == None` STORE rows or `Jar::bit_identical_restore()` before child tail is in hand; Test 2 non-zero decoy; Test 8 names one skip-exact helper; reconstruct seeks; `stored_block_is_a_miss` rewrite; serde `default` on new fields; DESIGN non-goal is depth > 1.
