# PLAN: single-CAS + ZIP index (crate 0.2.1)

Base: `main` at v0.2.0 / format v2 (`4fb4fd3`). Branch `cursor/single-cas-zip-index-2545`. Do not merge. Do not tag. Do not bump the on-disk format.

This file is the **next** plan. The 0.2.0 format-v2 plan is shipped (grouped zstd + TOC). Do not reopen grouping vs per-blob frames.

Origin `matt-brewer/agent-skills` is not reachable from this environment (`origin` CLI is unauthenticated; `gh` cannot see `matt-brewer/agent-skills`; no `~/git/agent-skills` checkout). Skeptic loops use fresh adversarial Task subagents. Never skip the first sweep. Fresh skeptic each sweep. Cap 3, then BLOCKED.

This PR is **docs + agent hints + this plan**. Dehydrate writer changes land in a follow-up that implements this file. Do not treat 0.2.0 as already policy-compliant.

Constraints: MSRV 1.80, `forbid(unsafe_code)`, no edition-2024 deps, no zstd-framed crate. Manifest JSON field names stay (`blob`, `local_header_offset`, `cdata_blob`, `uncompressed_size`, …). Do not add a Java/zlib deflater.

---

## Locked storage policy

Storage efficiency is of the utmost importance. Do **not** chase Java-zlib bit-identical whole-file hashes.

1. Store **one** deduped copy of each payload. Dedup key is BLAKE3 of **uncompressed** entry bytes (same class across JARs is one blob). This is the data.
2. Keep **indexes** of where that blob belongs inside the original ZIP, in the ratarmount-rs sense: name, CD order, method, CRC, compressed/uncompressed sizes, GPBF, `local_header_offset`, local header / data-descriptor / pad metadata, jar tail (CD through EOF), prefix if any, blob hash. The original JAR is gone; the index points at the CAS blob, not at a sidecar copy of the source file.
3. **Never** store a second encoding of the same entry. No default `cdata_blob` next to the content blob. No `raw_zip` except a zip that cannot be sliced (spanning / parse failure / ZipArchive count ≠ CD count). Do not reintroduce dual copies. That is why packs went 200MB → ~3GB: uncompressed CAS + original deflate streams (zstd cannot shrink those).
4. **Zstd-compress the actual data in blocks.** Format v2 already does this: record-aligned zstd **groups** flushing at 4 MiB of uncompressed BLOB **record** bytes, final MANIFEST+END frame, uncompressed TOC. Do **not** switch to per-file/per-blob frames (resets the window, loses size). Do not store pre-deflated ZIP cdata as the CAS payload. Blobs in the frames are uncompressed entry bytes; zstd is the only pack compression.
5. Restore rebuilds a valid ZIP from index + blobs (STORE splice / flate2 codec hit if it happens / otherwise rebuild). Whole-file `source_*` hashes may change. That is acceptable. Do **not** add a Java/zlib deflater, `cdata_blob` for misses, or `raw_zip` of healthy jars just to keep file hashes.
6. Read old packs (v1, legacy `cdata_blob`, 0.1.6–0.1.8 dual copy) but **never write** that shape again.

---

## Why 2.8GB / ~3GB happened (verified)

Hypothesis confirmed on `main` 0.2.0:

| Layer | Status |
|-------|--------|
| Format v2 grouped zstd of **uncompressed** BLOB records, flush at 4 MiB (`src/format/writer.rs` `BLOB_FRAME_FLUSH`) | Already shipped. Keep it. No format bump. |
| One content blob per unique uncompressed payload (`remember_blob` / BLAKE3) | Already shipped. |
| Manifest as ZIP-slot index (`local_header_*`, `tail_blob`, `blob`, sizes, CRC, method) | Already shipped. Field names stay. |
| Default pack does **not** store `cdata_blob` on clean STORE / codec-hit / codec-miss jars | Already shipped (0.1.9 metadata-only). |
| **Leftover dual copy** | Still written. |

The leftover is `StorePolicy` in `src/dehydrate.rs`:

- `CleanExact` — STORE + codec hits only. Writes `cdata_codec` on hits. No `cdata_blob`. **Keep.**
- `CleanMiss` — at least one DEFLATE miss, no unreproducible sibling. No `cdata_blob` / no codec. Rebuild. **Keep.**
- `ExactWithExotic` — no miss, but a class-4 / unreproducible entry. Writes `cdata_blob` for that entry. **Stop.**
- `MixedExact` — a DEFLATE miss **and** an unreproducible sibling. Writes `cdata_blob` on **every** DEFLATE hit, DEFLATE miss, **and** unreproducible entry. **This is the balloon.** One exotic or inflate-fail sibling turns a healthy classpath JAR into uncompressed CAS **plus** a second copy of every pre-deflated member. zstd cannot shrink those streams. That is 200MB → ~3GB.

`raw_zip` is already only `ZipExact::Raw` (unlistable) or `Sliced` with `locals.len() != jar.entries.len()`. Do not widen it.

Maven/Java empty DEFLATE dirs (`03 00`, usize 0) are already `EmptyDir` / trial-encode, not class-4. Do not reclassify them as exotic.

Docs on `main` treated **metadata-only exact** / **codec-hit bit-identical** as the north star. That invited the MixedExact fallback (“keep hashes, store cdata”). This PR replaces that north star.

GPBF is already inside `local_header_hex` / `local_header_blob`. Do **not** add a JSON field named `gpbf`.

`unique_blob_count` counts **all** CAS objects (content + tail + prefix + large local headers + pads + leftover `cdata_blob`). A two-jar overlap pack is **not** `unique_blob_count == unique file contents`. Index blobs are allowed. Dual content encodings are not.

---

## Goal (follow-up implementation, crate 0.2.1)

Stay on **format v2**. Crate **0.2.1**. Writers never emit `cdata_blob`. Readers still resolve `cdata_blob` first (legacy).

One writer policy: **CleanExact** (every member STORE / empty-dir / codec-hit) or **CleanMiss** (anything else). Delete `MixedExact` and `ExactWithExotic`. No hybrid splice-plus-cdata. No `cdata_blob` “just for the exotic sibling.”

Dehydrate:

1. Write each unique content blob once (`hash_both` / `remember_blob`). Fill index fields as today.
2. Delete `store_cdata`. New packs omit `cdata_blob` for every class, including class-4.
3. If any member is `DeflateMiss` **or** `Unreproducible`, the whole jar is **CleanMiss**: write no `cdata_blob` and no `cdata_codec` (including on Maven empty DEFLATE dirs in that jar). Rebuild will re-encode empties.
4. If every member is STORE / empty-dir / `DeflateHit`, keep **CleanExact** (`cdata_codec` on hits only).
5. Empty Maven DEFLATE dirs stay codec/empty when the jar is CleanExact. They are not class-4. `Unreproducible` dir iff `uncompressed_size != 0` **or** (non-empty local cdata **and** not the Maven empty-DEFLATE case: method 8 + uncomp 0). Do not treat `03 00` empty dirs as exotic.
6. `raw_zip` only when unlistable: spanning, parse failure, or ZipArchive count ≠ CD count (today’s `ZipExact::Raw` **or** `Sliced` with `locals.len() != jar.entries.len()`). Do not delete the count-mismatch arm. Do not widen `raw_zip` to codec-miss or exotic-sibling jars that listed cleanly.

`Entry::can_exact_cdata` / `Jar::bit_identical_restore` must change in the same PR. Today a method-0 dir with payload (`uncompressed_size != 0`, fixture `DIRC`) still returns true, so dropping `cdata_blob` would take `write_exact_jar` → splice `[]` → `verify_source_identity` fail. Required predicate: a dir is exact-splice **iff** `uncompressed_size == 0` **and** `compressed_size == 0` (legacy `cdata_blob` / `cdata_codec` still win if present). Do not use a looser “method 0 && uncomp 0” — that would splice a method-0 dir with leftover local cdata. A payload dir without `cdata_blob` is **not** exact. After that, CleanMiss jars are `metadata_rebuild()`.

Rehydrate:

1. Legacy read order unchanged: `cdata_blob` → `cdata_codec` → STORE / empty-dir splice → rebuild.
2. Today’s `write_rebuilt_jar` is **not** sufficient. Both `write_rebuilt_jar` (method ∉ {0,8} hard error; dir with `cdata_codec`/`cdata_blob` hard error) **and** `resolve_cdata` (same method error; `allow_rebuild=false` on splice) must be extended. Do not `read_entry_content` on `blob: None` dirs.
3. File, method ∉ {0, 8}: emit STORE or DEFLATE from the content blob (flate2 `rebuild_level`). Patch **method**, uncompressed size (unchanged for files), crc (unchanged), and compressed size in local **and** CD. Files only. `source_*` will not match.
4. Class-4 dir (payload in the local record, no content blob): rebuild as an **empty STORE directory**. Set method 0, `uncompressed_size = 0`, `crc32 = 0`, compressed size 0. Patch those fields in the local header **and** the CD. Do not store its cdata. Do not `raw_zip` the jar. The `DIRC` bytes are discarded (they were never a content blob).

`src/exact.rs` `patch_local_compressed_size` / `patch_central_directory` / `patch_data_descriptor` today write **compressed size only** (CD also writes local offset). GPBF bit 3 makes the local helper return immediately — that early-return is **csize-only**. When extending, still write **method** (and crc/uncomp if they live in the local header or descriptor) even when bit 3 is set. Do **not** copy the early-return onto the new fields. Do **not** treat a csize-only call as the class-4 / exotic-method patch. Extend those helpers or add siblings. Update the `updates` tuple beyond `(zip_rel, csize)`.
5. Rebuild must accept (or the writer must omit) directory `cdata_codec`. Prefer omit: CleanMiss writes no codecs, so the existing “directory has cdata_codec” error stays valid for **new** packs. Still drop that error for **legacy** packs that rebuild after a strip, or ignore dir codecs when `allow_rebuild`.
6. Do **not** add a Java/zlib deflater. Do **not** add `cdata_blob` “for misses”. Do **not** `raw_zip` healthy jars to keep hashes.

Tests (must fail, not log):

1. Mix + hash may keep logging hashes. **Must fail** if **any** entry in the mix (file or dir, any method) has `cdata_blob`. Walk **every** entry — do not reuse `tests/corpus.rs`’s `if e.is_dir || e.method_code != 8 { continue }` counter (that hides method-0 / dir cdata). No “documented exotic” exception.
2. **Must fail** if mix `output_len` exceeds `569539 * 115 / 100`. Keep that gate; do not loosen it.
3. Unit: two-jar overlap (shared uncompressed payload). Distinct `entries[].blob` ids equal the unique contents (e.g. HELLO + A + B = 3). `cdata_blob` is `None` on those file entries. `unique_blob_count` is **not** ~2× content count (no second encoding). Do **not** require `unique_blob_count == 3` — tails / local-header blobs are index, not a violation.
4. Flip `class4_miss_plus_dir_cdata_keeps_cdata_blob`: no `cdata_blob`, no `raw_zip`, `metadata_rebuild()`, functional ZIP identity, **not** `assert_bit_identical`. Rename it. `assert_functional_identity` **skips dirs** — also assert the restored `marked/` local **and** CD are empty STORE (method 0, uncomp 0, crc 0, csize 0).
5. **New** ExactWithExotic fixture: STORE or codec-hit file + method-0 dir-with-payload (`DIRC`), **no** DEFLATE miss. Must dehydrate with `cdata_blob.is_none()` on every entry, `raw_zip_blob.is_none()`, `!bit_identical_restore()`, rehydrate succeeds, functional identity, **not** `assert_bit_identical`. Same empty-STORE dir header asserts as (4). This is the arm that today’s `can_exact_cdata` would mis-classify as splice.
6. Do **not** require whole-file hash match on Maven codec-miss jars.

Docs + tests on the behavior change. `AGENTS.md` / `DESIGN.md` / this file already state the policy; the code PR updates README reconstruction examples if any leftover “must be bit-identical” wording survives.

---

## This PR (docs + hints only)

| File | Change |
|------|--------|
| `AGENTS.md` | **Create.** Standing agent contract. Short. Mandatory rules. Policy 1–6. Forbidden list. Test gates. MSRV / `forbid(unsafe_code)` / no edition-2024. |
| `DESIGN.md` | Storage / reconstruction north star = index + single CAS + zstd blocks + efficiency. Codec-hit splice is a bonus, not the goal. |
| `PLAN.md` | This file (replaces shipped 0.2.0 plan). |
| `README.md` | Reconstruction no longer leads with bit-identical. Efficiency + index + rebuild. Keep `tests/docs.rs` strings (`--verbatim`, dehydrate examples, Rocky, license). |
| `docs/library.md` | No `cdata_blob` / bit-identical instruction today. Leave unless a sentence appears that contradicts the policy. |
| `tests/docs.rs` | Guard `AGENTS.md` exists and contains the locked phrases so a later agent cannot delete the contract. |

Do **not** bump `Cargo.toml` to 0.2.1 here. Do **not** edit `src/dehydrate.rs` here. MixedExact leftover stays until the 0.2.1 code PR. `AGENTS.md` must say current 0.2.0 still writes `cdata_blob` on MixedExact / class-4 so agents do not assume the writer is already clean — and must say they **must not** add more `cdata_blob` writes or a Java-zlib project.

---

## Out of scope

- Format v3 / renaming JSON fields / per-blob zstd frames.
- Java zlib / `Deflater` bitstream matching.
- `--verbatim` / `--exact-cdata` flags.
- Exploding nested JARs.
- Merging, tagging, crate 0.2.1 publish.
- Implementing dehydrate in this PR.

---

## Skeptic review (plan)

Origin `skeptic-plan-review` was not reachable. Fresh adversarial Task subagents. Never skip sweep 1. Cap 3, then BLOCKED.

### Sweep 1 — REVISE (3 blockers, applied)

1. **`can_exact_cdata` + ExactWithExotic.** Method-0 dir with payload still looks exact. Deleting `store_cdata` would splice `[]` and fail `verify_source_identity`. Plan now requires the predicate change, collapses ExactWithExotic → CleanMiss, deletes the hybrid sentence, adds a no-miss class-4 fixture.
2. **Rebuild rejects dirs with `cdata_codec`.** “Already written” rebuild is not sufficient. CleanMiss writes no codecs; rebuild + `resolve_cdata` must still be extended for method ∉ {0,8} and class-4 empty-dir patch (method / uncomp / crc / sizes in local+CD).
3. **“Documented exotic” exception.** Would hide class-4 (method 0) `cdata_blob` writes. Dropped. Mix must fail on any `cdata_blob`. ExactWithExotic test is required so the hash-fix is not “put `cdata_blob` back.”

Should-fix from sweep 1 folded in: name both `write_rebuilt_jar` and `resolve_cdata`; class-4 dir patches uncomp/crc/method; no hybrid.

### Sweep 2 — ACCEPT (fresh skeptic; no blockers)

Should-fix folded in: name csize-only patch helpers and extend them; keep raw_zip count-mismatch arm; mix `cdata_blob` walk must not skip dirs/non-8; class-4 tests assert empty STORE dir in local+CD (`assert_functional_identity` skips dirs); exact dir iff both sizes are 0.

### Sweep 3 — ACCEPT (fresh skeptic; no blockers)

Should-fix folded in: Maven empty-DEFLATE (`03 00`) is not Unreproducible; GPBF bit 3 early-return stays csize-only and must not skip method/crc/uncomp patches.

**Plan locked.** Three sweeps, last two ACCEPT. Implement dehydrate only from this file. This PR stays docs + hints + plan.

### Skeptic code review (docs + hints + this file)

- Sweep 1 — **REJECT**: `tests/docs.rs` was keyword soup; an inverted AGENTS.md still passed. Tightened to full policy sentences. README now discloses 0.2.0 leftover `cdata_blob`. DESIGN security table no longer says “exact restore keeps bytes.”
- Sweep 2 — **ACCEPT** (fresh skeptic). Residual: polarity-blind `contains` (quoting then contradicting still passes). Not the old hole.


