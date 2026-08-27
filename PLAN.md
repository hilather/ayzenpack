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

### B. zip -A fat skip-exact (no tail / no splice)

`slice_from_archive` (`src/exact.rs`) returns `Err` when `min(zip_rel_offset) != 0` (`"first local header is not at zip offset 0"`). Dehydrate treats that as skip-exact: no `tail_blob`, restore is `write_jar`.

`cd_offset_to_zip_rel` subtracts `prefix_len` only when `view_shift == 0 && prefix_len > 0` (the zip -A file-absolute case). After that subtract, a healthy zip -A fat whose first local **is** the prefix becomes `zip_rel == 0` and **does** slice.

In-tree on 21e73bb (do **not** treat these as the 0.2.4 skip-exact gate — they already have tail):

| Fixture | `tail_blob` | `raw_zip` | Notes |
|---|---|---|---|
| `write_fat_spring_zip64_zipa_jar` (DEFLATE nested) | some | none | `fat_spring_zip64_zipa_is_listed_raw_on_v021_no_dual_copy_now` |
| `write_fat_spring_store_nested_zipa_jar` | some | none | `store_nested_zipa_fat_is_outer_listing_tail_no_raw_zip` |
| `write_fat_spring_store_nested_jar` (unadjusted) | some | none | prefix shift, not zip -A |

Matt's "zip -A fats often skip-exact (first local not at ZIP offset 0)" is therefore **not** those helpers. Implementer must **measure** a real zip -A fat (or construct one) on 21e73bb and record the actual `slice_fail` string before coding. Candidates:

1. First local file-absolute ≠ 0 after conversion fails / is skipped (leading pad inside the zip portion; first `offsetheader` is not the ZIP start).
2. Homemade `parse_central_directory` `None` or count mismatch on a listed outer zip (today skip-exact, even with a correct `ZipArchive` listing).
3. A fat Matt actually used whose first CD local is the prefix **as a file-absolute stencil** and must be accepted **without** requiring zip-rel 0.

0.2.4 must accept a **complete stencil** when the first local is not at ZIP offset 0 **if that shift is the prefix / zip -A file-absolute layout**. Those fats must get `tail_blob` + slot rows so restore can splice instead of `write_jar`. Last-wins / overlap still skip-exact.

Nested `BOOT-INF/lib/*.jar` stay one opaque content blob on 0.2.3 (`nested_jar_not_exploded`, DESIGN "stay opaque"). A STORE nested lib is a complete ZIP. 0.2.4 treats that slot as a child `zip_index`, not a 30 MB blob and not `raw_zip`.

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
2. child `zip_index` (this member is itself a ZIP)

Never both. A child `zip_index` is not a second copy of the inner ZIP bytes. Inner classes CAS-dedup through the same BLOB records.

Reconstruct walks in order: `prefix_blob`, then each slot (`local_header_*` at `offsetheader`, payload, `data_descriptor_*`, pad), then `tail_blob` (CD through EOF).

Old packs without `offsetheader`: restore stays `prefix_len + local_header_offset` (`write_exact_entry` today). New packs write **both** so 0.2.4 exact splice can seek `offsetheader` directly (file-absolute) and must **not** add `prefix_len` a second time.

Do not rename `blob` / `local_header_offset` / `cdata_blob`. Do not put SQLite in the `.ayz`.

### 2. Nested ZIP = child zip_index (depth 1)

A STORE member whose uncompressed bytes are a **listable** ZIP (complete inner `PK\x03\x04`…`PK\x05\x06`, `ZipArchive` populates `entries[]`) is "write the child ZIP from its index", not "one opaque blob".

- Depth **1** only (Spring Boot `BOOT-INF/lib/*.jar`). Do not recurse into a child's nested ZIPs. Keep `--max-entry-bytes` on the outer entry **and** on each child entry.
- Child stencil uses the same columns (`DurableZipMember` + existing Entry fields + codecs). Stored on the outer jar as `nestedindexes[]` (ratarmount side-table name; JSON array, not SQL).
- Outer slot: `zip_index` set, `blob` null. Inner file slots: `blob` + optional `cdata_codec`. Inner STORE nested-zips stay opaque (depth cap).
- DEFLATE-wrapped nested jar (method 8) stays `blob` + codec. Do not inflate-then-explode.
- Encrypted inner zip: fail like today. Do not decrypt.
- Two fats sharing a nested lib: unique **content** blobs = unique uncompressed payloads (inner classes + non-zip outer files). Child tails / local headers are structural, not a second payload encoding. `cdata_blob == 0`.

0.2.3 readers ignore unknown keys and will not restore a 0.2.4 pack that replaced a nested blob with `zip_index`. That is acceptable (format v2, additive). 0.2.4 must keep reading 0.2.3 packs (opaque nested blob + flate2-only `cdata_codec`).

### 3. Codec recipes (no `cdata_blob`)

Closed set for JARs. Record **only** the codec id that hit the original cdata (already read during slice). Restore re-encodes. No `cdata_blob`.

| id | When |
|---|---|
| STORE (method 0, `csize == usize`, content `blob` matches local cdata) | already exists; no `cdata_codec` string |
| `deflate-raw:zlib:{1,6,9}` | Java / Maven / `jar` / Info-ZIP |
| `deflate-raw:flate2:{1,3,6,9}` | already exists |
| `deflate-raw:stored` | method 8, raw DEFLATE BTYPE 00 |

Anything else (bzip2, zopfli, encrypted, unknown deflate) stays **rebuild that entry**. No `cdata_blob`. No `raw_zip` of a listed jar.

**Pin zlib-rs in-process** (or equivalent safe Rust zlib that matches zlib output). Do **not** spawn Java. Do **not** enable flate2 `zlib` / `zlib-ng` (C backend; `Cargo.toml` already forbids this). Keep today's flate2 `rust_backend` / miniz path for `deflate-raw:flate2:*`. Two backends, two codec prefixes.

zlib-rs crate line advertises MSRV 1.75. Implement PR must pin a version that is **not** edition-2024 and that stays on 1.80 in CI (`cargo +1.80 test`). If the newest zlib-rs pulls edition-2024 deps, pin an older line or an equivalent in-process zlib. Do not relax MSRV.

Trial original cdata at pack time. Suggested order after STORE classification: zlib GPBF-hint ∩ `{1,6,9}`, then remaining zlib `{1,6,9}`, then existing flate2 `trial_levels`, then `deflate-raw:stored`. First bitstream match wins. Cache by `(blob, compressed_size, flags)` so a large tree is not 8 full deflates per class.

`cdata_codec` schema pattern today is `^deflate-raw:flate2:[1369]$`. Implement PR widens it. Serde does not `deny_unknown_fields`; 0.2.3 `parse_codec` will reject a zlib string if it ever sees one — 0.2.4 packs are not a 0.2.3 restore target.

### 4. Per-entry codecs (drop jar-level CleanMiss)

Today `jar_store_policy`: any `DeflateMiss` / `Unreproducible` → `CleanMiss` → **no** sibling `cdata_codec`. A zopfli neighbor wipes Maven hits.

0.2.4: a miss must **not** drop sibling codecs. `fill_exact_entry` records a codec on that entry iff that entry hit.

Whole-file exact splice (`source_*` checked) only when **every** slot can resolve cdata: STORE / recorded codec / empty STORE dir / child `zip_index` that itself exact-restores.

Mixed jar (zopfli / unknown miss):

- Hits keep their codec and produce the original cdata.
- The miss entry is rebuilt (flate2 level 6, or STORE if that is the rebuild class).
- If the miss `compressed_size` changes, later `offsetheader` values cannot be kept: compact from that point (same idea as `write_rebuilt_jar`) and patch CD / EOCD / Zip64 extras that already exist. Sibling **cdata** still uses recorded codecs.
- `source_*` may change. Do not require whole-file hash on a miss jar.

Do not fall back to jar-wide CleanMiss. Do not put `cdata_blob` on the miss.

### 5. Slice / zip -A: accept prefix / file-absolute first local

`slice_from_archive` must attach tail + slot rows when the first local is not at ZIP offset 0 **if that shift is the prefix / zip -A file-absolute layout**.

Concrete gate (replace `min_off != 0` as a hard skip-exact):

- Classic / unadjusted: first local zip-rel 0 **or** first `offsetheader == prefix_len` (unadjusted prefix: first local sits at the prefix).
- zip -A: `view_shift == 0`, CD offsets file-absolute, first `offsetheader == prefix_len`. Accept even if the stored zip-rel mapping is not 0 (do not require a convert-to-0 step to keep the stencil).
- Leading pad after the ZIP start that is **not** the detected prefix: only accept if the stencil records that pad (prefix or an explicit leading hole) and locals do not overlap. Do not silently drop bytes.

Keep skip-exact (no tail, `write_jar`) for: overlapping locals, homemade count ≠ `ZipArchive::len()` on last-wins / `dup.txt`, encrypted, `NotZip` / listing never populated `entries[]`.

If a **listed** zip -A fat fails homemade parse (`None`) but `ZipArchive` has the outer listing and locals do not overlap: still attach tail from `find_cd_bounds` physical CD through EOF + slot rows from `archive_local_records`. That is skip-exact **today**; it is the named 0.2.4 fat path. It is **not** `raw_zip`. Overlap / last-wins stays skip-exact.

`write_exact_entry` today does `prefix_len + local_header_offset`. After this change, if `offsetheader` is present, seek **there** (file-absolute). Never `prefix_len + offsetheader`.

### 6. Docs / contract (implement PR, not this plan-only PR)

This plan-only PR updates `PLAN.md`, the intended-model paragraphs in `DESIGN.md` / `AGENTS.md`, and the `tests/docs.rs` PLAN identity strings. It does **not** bump the crate, touch `src/`, or change restore.

| File | Implement PR |
|---|---|
| `Cargo.toml` / lock | Crate **0.2.4**. Pin zlib-rs (or equivalent). MSRV 1.80. |
| `AGENTS.md` | Current-tree line → crate **0.2.4**. Keep every `tests/docs.rs` locked phrase (no default `cdata_blob`, never writes `cdata_blob` on STORE/DEFLATE, mix `569539 * 115 / 100`, `cdata_blob == 0` on every mix entry, MSRV **1.80**, `forbid(unsafe_code)`). Intended model already drafted here. |
| `DESIGN.md` | Reconstruction + executable JAR: stencil walk; codec set; per-entry codecs; depth-1 `zip_index`; zip -A file-absolute first local is a complete stencil. Keep `North star: **one CAS blob + ZIP index + zstd blocks**`. Nested libs are **not** opaque on new packs. |
| `README.md` / `docs/library.md` | Do not tell agents to store `cdata_blob` or chase bit-identical hashes via dual copy. Mention 0.2.4 stencil / zlib recipes only if a version sentence already exists. Keep `Crate **0.2.1** never writes \`cdata_blob\` (file or dir, any method)`. |
| `schemas/manifest.v1.schema.json` | Additive optional keys (`offsetheader`, `data_start`, `zip_index`, `nestedindexes`). Widen `cdata_codec` pattern. `additionalProperties: false` stays a **writer** check. |
| `tests/docs.rs` | PLAN identity: `# PLAN: 0.2.4 stencil restore` **and** `ratarmount zip_index + codec recipes` **and** `first local is not at ZIP offset 0`. |

---

## Tests (must fail on 0.2.3, not log)

Do **not** restage a test that is already green on 21e73bb and call it the 0.2.4 gate. In-tree zip -A fats already have `tail_blob`.

1. **Java-zlib classic (always-on).** In-tree ZIP whose DEFLATE cdata is zlib (not miniz). Build the fixture **in-process** with the same zlib-rs pin (no `java` subprocess). On 0.2.3: `cdata_codec` absent (flate2 miss) and/or CleanMiss; restore `source_sha256` ≠ source. After 0.2.4: `cdata_codec == "deflate-raw:zlib:{1,6,9}"` on that entry, no `cdata_blob`, Matt CLI (`dehydrate --recursive --sort-inputs --restore-paths`; `rehydrate --restore-paths` only), restored `source_size` and `source_sha256` match. If CI corpus already has a Maven classic (lucene / groovy), same assertions; do not require a network fetch.

2. **zip -A fat that skip-exacts on 21e73bb (always-on).** Construct (or record from a real file) a launch.script + Zip64 + STORE `BOOT-INF/lib` fat whose `slice_from_archive` on 0.2.3 is `Err` for first-local / prefix file-absolute (or homemade `None` on a listed outer). Assert the 0.2.3 skip-exact: `tail_blob.is_none()`, restore is `write_jar` or size/hash miss. After 0.2.4: `tail_blob.is_some()`, `raw_zip_blob.is_none()`, every `cdata_blob` absent, Matt CLI in-place, restored `source_size` and `source_sha256` match, restored size not 10× smaller (`restored_len * 10 >= source_len` inline). Do **not** flip `write_fat_spring_zip64_zipa_jar` to STORE. Do not use a fixture that already splices on 0.2.3 as this gate.

3. **Depth-1 child zip_index.** STORE `BOOT-INF/lib/foo.jar` that is a complete inner zip. After 0.2.4: that outer slot has `zip_index` and `blob == null`; inner classes appear as content blobs; no `raw_zip`; unique content blob count equals unique uncompressed payloads (two fats sharing one inner class → one class blob). On 0.2.3 the slot is an opaque `blob` (this test fails). Inner classes still dedup via the same CAS.

4. **Per-entry miss does not drop siblings.** One jar: flate2- or zlib-reproducible DEFLATE file **plus** a zopfli / BTYPE-unknown / `write_stored_block_deflate_zip` miss. After 0.2.4: the hit keeps `cdata_codec`; the miss has no codec and no `cdata_blob`; restore is functionally identical; miss entry is rebuilt only (siblings still emit original cdata). On 0.2.3 CleanMiss clears the hit. Do not require `source_*` match.

5. **Mix / corpus** when present: keep `cdata_blob == 0` on every mix entry, `output_len <= 569539 * 115/100`, `raw_zip_blob` absent. Unique **content** blob count must not double vs unique uncompressed payloads. Do not require whole-file hash on a remaining codec-miss jar. Do not loosen the size gate.

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

- A Java-built (zlib) classic JAR restores with matching `source_size` and `source_sha256`
- A zip -A fat (launch.script + Zip64 + STORE `BOOT-INF/lib`) that skip-exacts on 0.2.3 restores with matching `source_size` and `source_sha256` (no 10× shrink)
- Nested libs are child `zip_index`es, not `raw_zip` / not a second whole-ZIP CAS
- `cdata_blob == 0` on new packs; unique uncompressed blob count does not double
- A zopfli / unknown-deflate miss still rebuilds **that entry only** (siblings keep codecs)
- Matt CLI: `dehydrate --recursive --sort-inputs --restore-paths`; `rehydrate --restore-paths` only (no `--overwrite` required)

---

## Skeptic review (plan)

Origin `skeptic-plan-review` was not reachable. Fresh adversarial Task subagents. Never skip sweep 1. Cap 3, then BLOCKED. Fix BLOCKING findings in this file. Stop at a clean plan or BLOCKED. Do not implement even if the plan is clean.
