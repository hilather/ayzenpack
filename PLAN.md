# PLAN: no raw_zip dual-copy on listed jars (crate 0.2.2)

Base: `main` at crate **0.2.1** / format **v2** (`0751510`). Branch `cursor/fix-raw-zip-dual-copy-5024`. Do not merge. Do not tag. Do not bump the on-disk format.

This file is the **0.2.2 bugfix plan**. Crate 0.2.1 single-CAS / no-`cdata_blob` is **shipped and locked**. Do not reopen MixedExact, ExactWithExotic, `store_cdata`, Java-zlib, grouping vs per-blob frames, or format v3.

Origin `matt-brewer/agent-skills` is not reachable (`gh` 404; no `~/git/agent-skills`). Skeptic loops use fresh adversarial Task subagents. Never skip sweep 1. Fresh skeptic each sweep. Cap 3, then BLOCKED.

Constraints: MSRV 1.80, `forbid(unsafe_code)`, no edition-2024 deps, no zstd-framed crate. Manifest JSON field names stay. Format stays **v2**. No new `cdata_blob` puts. No Java/zlib deflater.

---

## Locked storage policy (unchanged)

Storage efficiency is of the utmost importance. Do **not** chase Java-zlib bit-identical whole-file hashes.

1. One CAS blob per unique **uncompressed** entry (BLAKE3 of those bytes).
2. Manifest is a ZIP-slot index (ratarmount-style pointers), not a second copy of file bytes.
3. **Never** store a second encoding of the same entry. No default `cdata_blob`. **`raw_zip` is not a fallback for a slice/count mismatch.** `ZipArchive` count ≠ homemade CD count is a **second bug**, not a `raw_zip` case.
4. Zstd in 4 MiB record-aligned BLOB groups (already v2). Do not switch to per-blob frames.
5. Restore rebuilds a valid ZIP from index + blobs. `source_*` may change.
6. Read old packs (v1, leftover `cdata_blob`, leftover `raw_zip`) but **never write** the 0.2.1 whole-zip-plus-CAS shape.

---

## Why 2.8 GB still happens on 0.2.1 (verified)

`src/dehydrate.rs` `attach_exact` (0.2.1):

```
ZipExact::Sliced(slice) if slice.locals.len() == jar.entries.len() => { tail + index; OK }
ZipExact::Raw(zip) => remember_blob(entire zip) + raw_zip_blob
ZipExact::Sliced(_) => read_zip_after_prefix + remember_blob(entire zip) + raw_zip_blob
```

Scan already stored **uncompressed** entry CAS blobs via `zip::ZipArchive::by_index`. Slice walks a **second** CD parser (`exact.rs` `parse_central_directory`). Same file, same `detect_zip_layout`.

zip 2.4.2 then collapses CD rows by name (`IndexMap` last-wins). `archive.len()` can be **less** than the number of `PK\x01\x02` records. In-tree proof: `tests/roundtrip.rs` `duplicate_entry_names_in_one_jar_all_restored` (`dup.txt` twice). Homemade `slice_zip` still emits two locals. 0.2.1 takes the third arm and stores the **whole JAR** again.

Any `slice_zip` `Err` (except Encrypted) becomes `ZipExact::Raw` and does the same.

Uncompressed CAS + full already-deflated jars ≈ 2.8 GB. zstd cannot shrink those streams.

0.2.1 `PLAN.md` / `DESIGN.md` / `AGENTS.md` / `README.md` **bless** `ZipArchive` count ≠ CD count as a `raw_zip` case. That is papering over the IndexMap / dual-parser bug.

---

## Goal (crate 0.2.2)

Stay on **format v2**. Crate **0.2.2**.

### 1. One listing for entries + locals

On a jar we successfully scanned, the entry list **is** `jar.entries` (ZipArchive `by_index` order, including last-wins collapse).

**Do this:**

- Build exact locals from that **same ZipArchive listing**: same `detect_zip_layout` + `ZipView`, `by_index` in order.
- `ZipFile::header_start` (zip 2.4.2; also `data_start` / `central_header_start`) is the **offset source only**. Local **extent** is still today’s `read_local` rule: header + cdata + data-descriptor + pad through the **next** `header_start` or the CD start. Do not drop descriptor/pad; CleanExact splice would fail `verify_source_identity`.
- Convert coordinates the same way as `cd_offset_to_zip_rel`: when `view_shift == 0` and `prefix_len > 0` (`zip -A`), crate `header_start` is file-absolute; manifest `local_header_offset` stays zip-relative. Mix includes `spring-zipa-slf4j.jar`.
- Then `locals.len() == jar.entries.len()` by construction whenever both opens succeed. `attach_exact` zips by index. **Do not** call `ZipArchive::len() == locals.len()` a regression gate (tautology).
- `ZipFile::central_header_start` is the CD row, not the local. Do not store it as `local_header_offset`.
- **Do not call `capture_zip_exact` / `slice_zip` from `attach_exact`.** Those helpers’ `Err(_) => Raw` path is how whole-zip CAS comes back. Collect `header_start` from the ZipArchive walk, convert zip-A coordinates, then use today’s `read_local` with `next = next header_start or CD start`. Tail from `find_cd_bounds` only.

`parse_central_directory` is the store-tail **count gate** (and Zip64 offset resolve). Rebuild **patching** is `patch_central_directory` / `patch_eocd_cd_start` — do not conflate them. Parse must **not** trigger `raw_zip`. Treat `parse_central_directory` returning `None` the same as a count mismatch (next section).

### 2. Tail is stored only when homemade CD count == entries

After building ZipArchive locals, read the tail (CD through EOF) via `find_cd_bounds` as today.

**Store `tail_blob` and fill the index if and only if** `parse_central_directory(tail)` is `Some(records)` **and** `records.len() == jar.entries.len()`.

Otherwise (homemade CD count ≠ ZipArchive listing, or parse `None`):

- That leftover disagreement is the **second bug** (IndexMap last-wins is the proven case).
- **Do not** `raw_zip`.
- **Do not** store that tail (`write_rebuilt_jar` patches the first `N` CD records, leaves extras, does not rewrite EOCD entry count → desync).
- Skip exact attach (`tail_blob` / `raw_zip_blob` unset). Restore = existing no-tail `write_jar` (ZipWriter). `duplicate_entry_names_in_one_jar_all_restored` already accepts last-wins.

**Mix / Spring / Maven / Zip64 fat / `zip -A` are not allowed to take this skip-exact path.** Those jars must still get a tail whose homemade CD count equals `jar.entries.len()`, and `spring-zip64-nested.jar` must still splice (`bit_identical_restore()` via tail + STORE/`cdata_codec`, **not** via `raw_zip`). If 0.2.1 hid a walker disagreement behind `raw_zip` on that fat, **fix `find_cd_bounds` / Zip64 / prefix** in this PR so the counts agree. Walker fix is mandatory for those fixtures, not optional. Keep `tests/corpus.rs` `spring-zip64-nested` `bit_identical_restore` + whole-file hash asserts. Add `raw_zip_blob.is_none()` on every mix member. Do **not** loosen mix asserts to “skip-exact / write_jar is fine” for those members — that is how `raw_zip` sneaks back.

Skip-exact is reserved for last-wins / extra CD rows (`dup.txt`) and for listed jars whose locals cannot be read as a consistent slice (overlapping distinct-name locals — Test 2). It is **not** for healthy Spring / Zip64 / `zip -A` jars.

### 3. Never write `raw_zip` when `jar.entries` is populated

`attach_exact`:

- **Tail stored (counts agree):** `fill_exact_entry` + `remember_blob` the **tail only**. No whole-zip blob.
- **Skip-exact (counts disagree or parse `None`):** no tail, no `raw_zip`. Content blobs already in CAS.
- **Cannot read locals after scan** (including overlapping `header_start`s / `next <= current`, first local not zip-rel 0, `read_local` failure): skip exact the same way. No `raw_zip`. Do not map this to `ZipExact::Raw`. Detect overlap on the ZipArchive `header_start` list — do not rely on leftover `slice_zip` for that.
- **`ZipExact::Raw` / `remember_blob(entire zip)`:** delete these arms for listed jars.
- Encrypted: still error. No `raw_zip`.

True multi-disk spanning (`disk_number != disk_with_central_directory`) fails `ZipArchive::new` (`UnsupportedArchive`) **before** `entries[]` exists. Keep that as a dehydrate error. Do not add an unlistable success path that packs garbage as `raw_zip`. Empty zip (`len()==0`) is **listable**: tail-only, no `raw_zip` (today’s `empty_zip_is_tail_only`).

`attach_exact` must not call `capture_zip_exact`. If `capture_zip_exact` remains for unit tests, dehydrate must not consume `ZipExact::Raw`. Do not “ignore Raw after calling it” as the design — that still reads the whole zip into RAM.

New packs of normal / Spring / Maven / Zip64 members: `raw_zip_blob == None`. Readers still accept legacy `raw_zip`. Schema fields stay.

### 4. Docs / contract (same PR)

| File | Change |
|------|--------|
| `Cargo.toml` / lock | Crate **0.2.2** |
| `AGENTS.md` | Policy 3: drop “ZipArchive count ≠ CD count” as a `raw_zip` exception. Say that mismatch is a parser bug. `raw_zip` only if `ZipArchive` never populated `entries[]` (`UnsupportedArchive` spanning / `NotZip`). Homemade parse `None` / `read_local` failure / `slice_zip` `Err` on a **listed** jar = skip-exact, never whole-zip CAS. Crate 0.2.2 never writes `raw_zip` of a listed jar. **Keep** the exact `tests/docs.rs` strings that still apply, including `No default \`cdata_blob\` next to the content blob`, `**never writes** \`cdata_blob\` on STORE/DEFLATE (file or dir, any method)`, `Do not add new \`cdata_blob\` puts`, `569539 * 115 / 100`, `\`cdata_blob == 0\` on every mix entry`, `MSRV is **1.80**`, `forbid(unsafe_code)`. |
| `DESIGN.md` | **Delete** “unlistable = spanning / parse failure / ZipArchive count ≠ CD count”. Count mismatch **and** homemade parse failure on a listed jar are parser/skip-exact bugs, not `raw_zip`. `raw_zip` only when listing never produced `entries[]`. Keep north-star sentence `North star: **one CAS blob + ZIP index + zstd blocks**`. |
| `README.md` | **Delete** the sentence “If a zip cannot be sliced (spanning / parse failure / count mismatch) … `raw_zip_blob`”. Do not leave “parse failure” in any `raw_zip` exception list. Listed jars: index + CAS, or skip-exact `write_jar`. **Keep** `Crate **0.2.1** never writes \`cdata_blob\` (file or dir, any method)` (docs.rs locks it). Keep `--verbatim`, dehydrate examples, Rocky, license. |
| `tests/docs.rs` | **Replace the 3-way PLAN AND**, not just the title. Today it requires `# PLAN: single-CAS + ZIP index (crate 0.2.1)` **and** `Writers never emit \`cdata_blob\`` **and** `Delete \`store_cdata\``. This 0.2.2 file does not contain the last two. New conjuncts must be copied **byte-for-byte** from this file: `# PLAN: no raw_zip dual-copy on listed jars (crate 0.2.2)` **and** `Never write \`raw_zip\` when \`jar.entries\` is populated` **and** `` `ZipArchive` count ≠ homemade CD count is a **second bug** `` (backticks around `ZipArchive` — a bare `ZipArchive count ≠` string is **not** in this file). |
| `src/exact.rs` module docs | Stop saying CD/entry-count mismatch yields `Raw`. |

---

## Tests (must fail, not log)

1. **Former 0.2.1 mismatch arm = `dup.txt`.** Fixture already has two CD records named `dup.txt`; ZipArchive last-wins so `scan`/`entries.len()` is 1 and homemade `parse_central_directory` / 0.2.1 `slice_zip` locals is 2. Prove that inequality in-test (call the homemade parse / 0.2.1-style slice count). After the fix: `raw_zip_blob.is_none()`, every `cdata_blob` absent, `bytes_unique_blobs` / `output_len` must **not** include a second copy of the zip portion (compare to `source_size` — unique blobs stay on the order of one payload + small index, not `source_size` + payload). Restore still matches scanner-visible entries (existing last-wins assert).

2. **Always-on `ZipExact::Raw` kill.** Tests 1/`dup.txt` only hit today’s **third** arm (`Sliced` count mismatch). `Raw` is `slice_zip` `Err` (overlap, first local ≠ 0, parse `None`, descriptor fail, EOCD vs parse count). An implementer can delete only the third arm, keep `ZipExact::Raw` + `remember_blob(entire zip)`, and Test 1 stays green. **Required always-on fixture:** a jar ZipArchive **lists** (`entries[]` populated) that **today** `capture_zip_exact` returns `ZipExact::Raw` — overlapping local offsets with **distinct names** (not `dup.txt`). After the fix: `raw_zip_blob.is_none()`, `tail_blob.is_none()` (do not store a broken tail), no whole-zip blob, restore via `write_jar`. Overlapping-local is mandatory, not optional. Do **not** use spanning (`ZipArchive::new` fails before listing).

3. **In-tree Zip64 / `zip -A` / Spring / descriptor / zipalign (always-on, no corpus).** `bit_identical_restore()` is **true for `raw_zip`** — that flag is not a dual-copy gate. These fixtures (`write_wrapped_zip64_jar`, `write_wrapped_jar_adjusted`, official launch.script, data-descriptor, zipalign pad) **must** assert `tail_blob.is_some() && raw_zip_blob.is_none()`. Tighten existing `tail_blob || raw_zip` and `pad_zeros || raw_zip` accepts in `tests/roundtrip.rs`. Corpus skip without `AYZENPACK_CORPUS_DIR` is OK for the 569539 Maven-mix size gate only.

4. **Mix / corpus** (`tests/corpus.rs`, when corpus is present): `raw_zip_blob` absent on **every** mix member. Keep `cdata_blob == 0`. Keep `output_len <= 569539 * 115/100`. **Keep** `spring-zip64-nested.jar` `bit_identical_restore()` + whole-file hash **and** `raw_zip_blob.is_none()` (the flag alone is not enough). Hash-mismatch members must still be `metadata_rebuild()` (they have a tail). Do not loosen those asserts. If a mix member would skip-exact, that is a walker bug — fix it, do not change the assert.

5. **Overlap:** `unique_overlap_content_blobs_not_dual_copy` / HELLO+A+B=3 still holds. `unique_blob_count` is unique contents plus index blobs, **not** jars+contents.

6. **Second-bug gate (not tautological).** Expose homemade parse via `pub(crate)` (or a thin helper). On named fixtures, assert `parse_central_directory(tail).len()` (and EOCD `entry_count` when parse succeeds) **versus** `ZipArchive::len()` / `jar.entries.len()`:
   - plain ZipWriter, Maven empty DEFLATE dir, class-4 dir, Spring prefix, `zip -A`, Zip64 fat: **must be equal** (walker must agree) **and** `tail_blob.is_some() && raw_zip_blob.is_none()`.
   - `dup.txt` last-wins: **must be 2 vs 1** (documents IndexMap). Must **not** `raw_zip` or store `tail_blob`.
   Treat parse `None` as mismatch (fail the “must be equal” fixtures).

Fill `fill_exact_entry` **only after** the store-tail count gate passes. Last-wins: one local = last `dup.txt`; zipping `entries[i]` with `locals[i]` is correct on the store-tail path. On skip-exact, do not fill from a disagreeing homemade slice.

Do **not** require whole-file hash match on Maven codec-miss jars. Do **not** add `cdata_blob` to keep hashes.

---

## Out of scope

- Format v3 / renaming JSON fields / per-blob zstd frames.
- Java zlib / `Deflater` bitstream matching.
- `--verbatim` / `--exact-cdata`.
- Exploding nested JARs.
- Reopening 0.2.1 CleanExact / CleanMiss.
- Adding a `raw_zip` success path for `NotZip` / `UnsupportedArchive`.
- Merging, tagging, publish.

---

## Skeptic review (plan)

Origin `skeptic-plan-review` was not reachable. Fresh adversarial Task subagents. Never skip sweep 1. Cap 3, then BLOCKED.

### Sweep 1 — REVISE (2 blockers, applied)

1. **Test 5 was tautological.** `ZipArchive::len() == slice.locals.len()` after building locals from ZipArchive cannot fail. zip 2.4.2 `IndexMap` last-wins vs homemade CD is the real second bug (`dup.txt`). Gate is now homemade `parse_central_directory(tail).len()` vs `entries.len()`, including parse `None`. Do not store a disagreeing tail (`write_rebuilt_jar` leaves extra CD rows).
2. **Skip-exact vs mix asserts.** Skip-exact leaves neither `bit_identical_restore` nor `metadata_rebuild`. `spring-zip64-nested` hard-requires `bit_identical_restore()`. Plan now **keeps** those corpus asserts and **forbids** skip-exact on mix/Spring/Zip64/`zip -A`; walker must make homemade CD count match. Skip-exact is only last-wins / extra CD rows (`dup.txt`).

Should-fix folded in: `header_start` is offset-only (extent still header+cdata+descriptor+pad to next local/CD); `zip -A` file-abs → zip-rel; Test 2 is not spanning; `tests/docs.rs` 3-way PLAN AND is replaced in full, AGENTS/README locked phrases kept.

### Sweep 2 — REVISE (3 blockers, applied)

1. **Tests 1/`dup.txt` only kill the third arm.** `ZipExact::Raw` (`slice_zip` `Err`) could remain and every always-on test stay green. Added mandatory overlapping-local (distinct names) fixture that is `Raw` on 0.2.1; in-tree Zip64/`zip -A`/Spring/descriptor/zipalign must assert `tail_blob.is_some() && raw_zip_blob.is_none()` (`bit_identical_restore` is true for `raw_zip`).
2. **DESIGN/README still blessed “parse failure ⇒ `raw_zip`”.** Table now deletes that exception. Listed-jar parse/`read_local`/`slice_zip` `Err` = skip-exact. `raw_zip` only when listing never produced `entries[]`.
3. **docs.rs conjunct was not a substring.** Lock `` `ZipArchive` count ≠ homemade CD count is a **second bug** `` (backticks around `ZipArchive`), not a bare `ZipArchive count ≠` string.

Should-fix folded in: do not call `capture_zip_exact` from `attach_exact`; parse is count gate not patcher; `central_header_start` is not a local offset; keep `No default cdata_blob…`; tighten `|| raw_zip` accepts; expose parse `pub(crate)`.

### Sweep 3 — ACCEPT (fresh skeptic; NO BLOCKING FINDINGS)

Should-fix for implement (not blockers): Test 6 lives in `src/` or a thin `pub` helper — do not clone a second CD walker in `tests/`. Build `CdRecord` from the ZipArchive walk (`header_start` after zip-A convert + crc/sizes), not homemade CD rows. Parse the **source** tail from `find_cd_bounds` for Test 6. Do not add a new unlistable `raw_zip` success path.

**Plan locked.** Three sweeps (REVISE, REVISE, ACCEPT). Implement from this file.

---
