# PLAN: 0.2.4 stencil restore (ratarmount zip_index + codec recipes)

Base: `main` / tag `v0.2.3` at crate **0.2.3** / format **v2** (`21e73bb`). Branch `cursor/plan-stencil-restore-d2d9`. This PR is **plan-only**. Do not implement. Do not merge. Do not tag. Do not bump `Cargo.toml`. Do not change restore behavior in this PR.

Crate 0.2.3 no-nested-STORE-latch, crate 0.2.2 no-`raw_zip`-on-listed-jars, and crate 0.2.1 single-CAS / no-`cdata_blob` are **shipped and locked**. Do not reopen MixedExact, ExactWithExotic, `store_cdata`, grouping vs per-blob frames, format v3, or `raw_zip` of a listed jar. Do not merge #36 or #37 into this work.

Origin `matt-brewer/agent-skills` is not reachable (`origin auth status` is logged out; `origin repo clone` cannot auth). This file is written to that plan-function shape from the 0.2.3 PLAN (repro first, file-level work, tests that must fail, non-goals). Skeptic loops use fresh adversarial Task subagents. Never skip sweep 1. Fresh skeptic each sweep. Cap 3, then BLOCKED.

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

`min_off != 0` (`"first local header is not at zip offset 0"`) fires only when the first local is not at ZIP offset 0 after that convert — e.g. leading pad inside the zip portion (`offsetheader != prefix_len`). That is **not** the prefix / zip -A file-absolute layout. Do **not** encode `first offsetheader == prefix_len` as the 0.2.4 skip-exact fix; that is the in-tree splice case.

Matt's remaining fat skip-exact must be **measured** on 21e73bb (`slice_fail` string) before coding. Named remaining classes:

1. **Homemade `parse_central_directory` `None`** on a listed outer zip (`ZipArchive` has the outer listing, locals do not overlap). Today skip-exact. 0.2.4: attach `tail_blob` from `find_cd_bounds` physical CD→EOF + slot rows from `archive_local_records`. **Exact-only:** every slot must STORE / codec-hit / exact child `zip_index`. If any miss, stay skip-exact (`write_jar`) — `write_rebuilt_jar` / `patch_central_directory` cannot patch an unparseable CD. Not `raw_zip`.
2. **Leading pad** after the ZIP start that is not the detected prefix (`min_off != 0`). 0.2.4: keep those bytes as an explicit hole (`prefix_blob` extension or a pad blob before the first local). Do not silently zero-fill. Locals must not overlap.
3. **Overlap / last-wins / `dup.txt`:** stay skip-exact. Do not attach a broken tail.

If measurement finds no real zip -A fat in (1) or (2), drop skip-exact as a restore-hash gate for fats that already splice. Those fats then ride class A (zlib) + nested `zip_index`. Do not invent a first-local==prefix bug.

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

Each slot payload pointer is **ONE** of (writer mutex; schema `oneOf` / reject both):

1. CAS `blob` (uncompressed BLAKE3) + optional `cdata_codec` recipe
2. child `zip_index` (usize index into `jars[].nestedindexes[]`)

Never both. A child `zip_index` is not a second copy of the inner ZIP bytes. Inner classes CAS-dedup through the same BLOB records. Test: both-set is refused on write and on read.

Reconstruct walks in order: `prefix_blob`, then each slot (`local_header_*` at `offsetheader`, payload, `data_descriptor_*`, pad), then `tail_blob` (CD through EOF).

Old packs without `offsetheader`: restore stays `prefix_len + local_header_offset` (`write_exact_entry` today). New packs write **both**. 0.2.4 exact splice seeks `offsetheader` (file-absolute) and must **not** add `prefix_len` a second time. `detect_offset_mode` / `write_rebuilt_jar` keep taking **zip-rel** `local_header_offset`. Feeding `offsetheader` there makes zip -A look ZipRel (`cd_off == prefix_len == first_lh`) and corrupts rebuild.

Do not rename `blob` / `local_header_offset` / `cdata_blob`. Do not put SQLite in the `.ayz`.

### 2. Nested ZIP = child zip_index (depth 1)

Today `commit_blob` / `remember_blob` runs **before** `attach_exact` (`src/dehydrate.rs`). The STORE nested jar is already in the zstd BLOB group. Nulling `entry.blob` later leaves `blake3(inner.zip)` in `blobs[]` next to exploded class blobs (the 200MB→3GB failure mode). `content_blob_ids` that only read `jars[].entries[].blob` can stay green while `blobs[]` still holds the whole inner ZIP.

A STORE member whose uncompressed bytes are a **listable** ZIP may become a child `zip_index` **only** if dehydrate **never CAS-es those combined bytes**:

- Detect the listable inner ZIP **while the payload is in hand** (scan / before `remember_blob`), explode depth 1, `commit_blob` each inner file, do not `remember_blob` the combined inner ZIP.
- Assert `blake3(inner zip) ∉ blobs[]` (always-on). Unique **content** count = outer non-zip `Some(blob)` + all `nestedindexes[].entries[].blob` (not `stats.unique_blob_count`, not tails / `local_header_blob`).
- **Opaque fallback (keep today's blob, no `zip_index`):** child `ZipArchive` does not populate `entries[]`; encrypted; overlap / last-wins / homemade count mismatch on the child; child entry exceeds `--max-entry-bytes`; child file-entry count exceeds a cap (implement: e.g. same order as `--max-entry-bytes` intent — a STORE zip-bomb of 100k tiny files; pick a named constant in the implement PR and test it). Depth 1 does not by itself stop that bomb.
- **Only explode when the child stencil is complete.** If the child would be skip-exact, keep the opaque blob so functional identity cannot lose the payload. Do not emit `zip_index` **and** the whole-inner blob.
- Depth **1** only (`BOOT-INF/lib/*.jar`). Do not recurse into a child's nested ZIPs. Keep `--max-entry-bytes` on the outer entry **and** on each child entry.
- Child stencil: same Entry columns + codecs, stored as `jars[].nestedindexes[]` (ratarmount name; JSON array, not SQL). `zip_index` is an index into that array, not a second inline stencil.
- Outer slot: `zip_index` set, `blob` null. Inner file slots: `blob` + optional `cdata_codec`. Inner STORE nested-zips stay opaque (depth cap).
- DEFLATE-wrapped nested jar (method 8) stays `blob` + codec. Do not inflate-then-explode.
- Encrypted inner zip: fail like today. Do not decrypt.
- `nested_jar_not_exploded` / corpus “nested JARs stay opaque” **flip** for STORE listable children that take the `zip_index` path. Kafka/lucene nested jars that are DEFLATE-wrapped stay opaque.

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

Trial original cdata at pack time. Suggested order after STORE classification: zlib GPBF-hint ∩ `{1,6,9}`, then remaining zlib `{1,6,9}`, then existing flate2 `trial_levels`, then `deflate-raw:stored`. First bitstream match wins. Cache by `(blob, compressed_size, flags)` so a large tree is not 8 full deflates per class.

`cdata_codec` schema pattern today is `^deflate-raw:flate2:[1369]$`. Implement PR widens it. Serde does not `deny_unknown_fields`; 0.2.3 `parse_codec` will reject a zlib string if it ever sees one — 0.2.4 packs are not a 0.2.3 restore target.

### 4. Per-entry codecs (drop jar-level CleanMiss)

Today `jar_store_policy`: any `DeflateMiss` / `Unreproducible` → `CleanMiss` → **no** sibling `cdata_codec`. A zopfli neighbor wipes Maven hits.

0.2.4: a miss must **not** drop sibling codecs. `fill_exact_entry` records a codec on that entry iff that entry hit.

Whole-file exact splice (`verify_source_identity` / `source_*` checked) only when **every** slot can resolve cdata: STORE with a content `blob`; recorded codec; empty STORE dir; child `zip_index` whose nested stencil `bit_identical_restore`s.

`Entry::can_exact_cdata` / `Jar::bit_identical_restore` / `resolve_cdata` / `write_exact_entry` / `write_rebuilt_jar` must change together (today a STORE slot with `csize == usize` is exact even when `blob` is null → `read_entry_content` → missing blob):

- `zip_index && blob` is an error.
- `zip_index` is exact iff the child stencil `bit_identical_restore`s. Payload is the reconstructed child ZIP bytes, never `read_entry_content` on the outer slot.
- Mixed outer **or** non-exact child → `metadata_rebuild` only. No `source_*` check. `write_rebuilt_jar` emits child-stencil bytes for a `zip_index` slot (not a content blob).
- `write_rebuilt_jar` today ignores dir `cdata_codec` (`allow_rebuild && is_dir`). After zlib empty-dir hits, a mixed rebuild can rewrite `03 00` as flate2:6. **Accept that** on a miss jar (hash may change). On an exact jar, dirs use the recorded codec and must not take that ignore path.

Mixed jar (zopfli / unknown miss):

- Hits keep their codec and produce the original cdata.
- The miss entry is rebuilt (flate2 level 6, or STORE if that is the rebuild class).
- If the miss `compressed_size` changes, later `offsetheader` values cannot be kept: compact from that point and patch CD / EOCD / Zip64 extras that already exist. Sibling **cdata** still uses recorded codecs.
- `source_*` may change. Do not require whole-file hash on a miss jar.

Do not fall back to jar-wide CleanMiss. Do not put `cdata_blob` on the miss.

`tests/corpus.rs` `proven_miss` today requires a non-`bit_identical_restore` jar to have **no** method-8 `cdata_codec` (0.2.3 CleanMiss). The implement PR **must rewrite** that assertion: sibling codecs allowed; only the miss entry lacks a codec; no `source_*` on that jar. Test 4 is not enough if mix CI still demands CleanMiss.

### 5. Slice: remaining skip-exact only (not first-local==prefix)

Do **not** change the gate to “accept `first offsetheader == prefix_len`”. That layout already slices on 0.2.3 after `cd_offset_to_zip_rel`.

Implementer records the `slice_fail` string on 21e73bb for any fat that still skip-exacts, then:

- **Homemade `None`, listed outer, no overlap:** attach tail + `archive_local_records`. Exact-only (see §B.1). Any miss → stay skip-exact.
- **Leading pad (`min_off != 0`):** keep hole bytes in the stencil; then slice. Do not zero-fill.
- **Overlap / last-wins / encrypted / `NotZip`:** unchanged skip-exact / fail.

`write_exact_entry`: if `offsetheader` is present, seek **there**. Never `prefix_len + offsetheader`.

### 6. Docs / contract (implement PR, not this plan-only PR)

This plan-only PR updates `PLAN.md`, the intended-model paragraphs in `DESIGN.md` / `AGENTS.md`, and the `tests/docs.rs` PLAN identity strings. It does **not** bump the crate, touch `src/`, or change restore.

| File | Implement PR |
|---|---|
| `Cargo.toml` / lock | Crate **0.2.4**. Pin zlib-rs (or equivalent). MSRV 1.80. |
| `AGENTS.md` | Current-tree line → crate **0.2.4**. Keep storage-efficiency / no-default-`cdata_blob` / 4 MiB groups / mix `569539 * 115 / 100` / `cdata_blob == 0` / MSRV **1.80** / `forbid(unsafe_code)`. **Replace** (do not keep) the sentence `Do **not** add a Java/zlib deflater, \`cdata_blob\` for misses, or \`raw_zip\` of healthy jars` with: do not add a Java subprocess / vendor `Deflater`; do not add `cdata_blob` for misses; do not `raw_zip` a listed jar; in-process zlib-rs raw-deflate hits are the 0.2.4 path. Update `tests/docs.rs` in the **same** implement PR. |
| `DESIGN.md` | Reconstruction + executable JAR: stencil walk; codec set; per-entry codecs; depth-1 `zip_index`; remaining skip-exact classes in §B. Keep `North star: **one CAS blob + ZIP index + zstd blocks**`. Nested STORE listable libs are `zip_index` on new packs when §2 applies. |
| `README.md` / `docs/library.md` | Do not tell agents to store `cdata_blob` or chase bit-identical hashes via dual copy. Keep `Crate **0.2.1** never writes \`cdata_blob\` (file or dir, any method)`. |
| `schemas/manifest.v1.schema.json` | Additive optional keys (`offsetheader`, `data_start`, `zip_index`, `nestedindexes`). Widen `cdata_codec`. Writer mutex: `blob` xor `zip_index`. `additionalProperties: false` stays a writer check — add the keys or the writer check fails. |
| `tests/docs.rs` | PLAN identity stays the 0.2.4 title strings. Implement PR updates the Java/zlib locked phrase as above. |
| `tests/corpus.rs` | Rewrite `proven_miss` (see §4). |

---

## Tests (must fail on 0.2.3, not log)

Do **not** restage a test that is already green on 21e73bb and call it the 0.2.4 gate. In-tree zip -A fats already have `tail_blob`.

1. **zlib-rs classic (always-on, not “Java-built”).** In-tree ZIP whose DEFLATE cdata is zlib-rs raw (not miniz). Build the fixture **in-process** with the same pin (no `java` subprocess). This is a self-roundtrip that 0.2.3 cannot hit. On 0.2.3: `cdata_codec` absent and/or CleanMiss; restore `source_sha256` ≠ source. After 0.2.4: `cdata_codec` is `deflate-raw:zlib:{1,6,9}`, no `cdata_blob`, Matt CLI, restored `source_size` and `source_sha256` match. **Java/Maven gate:** when `AYZENPACK_CORPUS_DIR` is set, the same `source_*` assertions on corpus lucene/groovy (or another Java-built classic already in the lock). Do not call the zlib-rs fixture “Java-built”. Do not require a network fetch.

2. **Skip-exact class that is actually `Err` on 21e73bb (always-on).** Not an in-tree fat that already has `tail_blob`. Either (a) homemade-`None` listed outer (exact-only after 0.2.4; `source_*` match only if every slot hits), or (b) leading-pad fat with recorded hole bytes. 0.2.3 fail: `tail_blob.is_none()` and `source_sha256` mismatch. `restored_len * 10 >= source_len` does **not** fail 0.2.3 `write_jar` of a correct listing — do not use it as the 0.2.3 fail. After 0.2.4: `tail_blob.is_some()`, `raw_zip_blob.is_none()`, every `cdata_blob` absent, Matt CLI. Do **not** flip `write_fat_spring_zip64_zipa_jar` to STORE.

3. **Depth-1 child zip_index (no whole-inner CAS).** STORE `BOOT-INF/lib/foo.jar` complete inner zip that takes the §2 explode path. After 0.2.4: outer slot `zip_index` set and `blob == null`; `blake3(inner zip) ∉ blobs[]`; inner classes are content blobs; no `raw_zip`; unique content = outer non-zip `Some(blob)` + nestedindexes file blobs (two fats sharing one inner class → one class blob). On 0.2.3 the slot is an opaque `blob`. Both-set `zip_index`+`blob` refused. Opaque fallback still covered (overlap child / DEFLATE-wrapped nested).

4. **Per-entry miss does not drop siblings.** One jar: flate2- or zlib-reproducible DEFLATE file **plus** `write_stored_block_deflate_zip` miss. After 0.2.4: the hit keeps `cdata_codec`; the miss has no codec and no `cdata_blob`; restore functionally identical; miss entry rebuilt only. On 0.2.3 CleanMiss clears the hit. No `source_*` match. Mix `proven_miss` rewritten in the same PR.

5. **Mix / corpus** when present: keep `cdata_blob == 0` on every mix entry, `output_len <= 569539 * 115/100`, `raw_zip_blob` absent. Unique content blob count must not double (`blake3(inner zip)` absent). Do not require whole-file hash on a remaining codec-miss jar. Do not loosen the size gate. Rewrite `proven_miss` (sibling codecs allowed).

6. **Overlap / last-wins unchanged:** `unique_overlap_content_blobs_not_dual_copy`; `dup.txt` still skip-exact, no `raw_zip`.

7. **0.2.3 packs still read:** a flate2-codec pack and an opaque-nested 0.2.3 pack rehydrate on 0.2.4.

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
- A zip -A fat (launch.script + Zip64 + STORE `BOOT-INF/lib`) that **already splices** on 0.2.3 keeps matching `source_*` after zlib + `zip_index` (no 10× shrink). A fat that still skip-exacts on 0.2.3 (measured homemade-`None` or leading-pad) gets tail + `source_*` only on the exact-only path.
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
