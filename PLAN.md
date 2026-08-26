# PLAN: do not latch ZipArchive onto nested STORE jars (crate 0.2.3)

Base: `main` / tag `v0.2.2` at crate **0.2.2** / format **v2** (`87ebeff`). Branch `cursor/fix-rehydrate-fat-jar-8517`. Do not merge. Do not tag. Do not bump the on-disk format.

This file is the **0.2.3 bugfix plan**. Crate 0.2.2 no-`raw_zip`-on-listed-jars and crate 0.2.1 single-CAS / no-`cdata_blob` are **shipped and locked**. Do not reopen MixedExact, ExactWithExotic, `store_cdata`, Java-zlib, grouping vs per-blob frames, format v3, or `raw_zip` of a listed jar.

Origin `matt-brewer/agent-skills` is not reachable (`origin repo clone` cannot auth). Skeptic loops use fresh adversarial Task subagents. Never skip sweep 1. Fresh skeptic each sweep. Cap 3, then BLOCKED.

Constraints: MSRV 1.80, `forbid(unsafe_code)`, no edition-2024 deps, no zstd-framed crate. Manifest JSON field names stay. Format stays **v2**. No new `cdata_blob` puts. No Java/zlib deflater. No `raw_zip` of a listed jar.

---

## Locked storage policy (unchanged)

Storage efficiency is of the utmost importance. Do **not** chase Java-zlib bit-identical whole-file hashes.

1. One CAS blob per unique **uncompressed** entry (BLAKE3 of those bytes).
2. Manifest is a ZIP-slot index (ratarmount-style pointers), not a second copy of file bytes.
3. **Never** store a second encoding of the same entry. No default `cdata_blob`. `ZipArchive` count ≠ homemade CD count is a **second bug**, not a `raw_zip` case. Crate 0.2.2 never writes `raw_zip` of a listed jar. Skip-exact (index + CAS / `write_jar`) when counts disagree.
4. Zstd in 4 MiB record-aligned BLOB groups (already v2). Do not switch to per-blob frames.
5. Restore rebuilds a valid ZIP from index + blobs. `source_*` may change.
6. Read old packs (v1, leftover `cdata_blob`, leftover `raw_zip`) but **never write** dual-copy.

---

## Repro (required; measured on 87ebeff, this VM)

Matt's flags, not a generic `-d` restore:

```
ayzenpack dehydrate --recursive --sort-inputs --restore-paths -o pack.ayz <dir-of-jars>
ayzenpack rehydrate --restore-paths -i pack.ayz
```

`--recursive` is only the input walk. `--sort-inputs` zeros `created_unix` and sorts/dedupes. `--overwrite` is **not** used and is not required: `--restore-paths` skips the overwrite guard, `prepare_restore_dest` unlinks an existing file, then `write_jar` / splice `File::create`.

| Input | Source size | Restored size | Zip listing | tail / raw_zip | Notes |
|---|---|---|---|---|---|
| Maven `spring-cloud-dataflow-server-2.11.5.jar` (sha1 `85236bf366869ea300bbc3bf198b7895090755ac`, starts `PK`, **no** prefix) | 133561923 | 133561757 | 418 → 418 | tail yes, raw no | Names + uncompressed bytes match. Whole-file hash changes. **Not the collapse.** |
| Same + official `launch.script` prepended, **no** `zip -A` | 133571151 | 133570985 | 418 → 418 | tail yes, raw no | Prefix 9228. **Not the collapse.** |
| Same + official `launch.script` + Info-ZIP **`zip -A`** | 133571151 | **1737752** | **scan 919** (outer is 418) | **tail no**, raw no | **≥10× collapse (77×).** Unique uncompressed ~3.9 MiB. Listing is `reactor-core-3.4.41.jar` (first STORE `BOOT-INF/lib`). |
| lucene-core 9.11.1 / groovy 4.0.24 (classic, no prefix) | 4.2 / 7.6 MiB | 4.2 / 7.6 MiB | counts match | tail yes | Small deflate-rebuild delta. **Do not treat as the bug.** |

Matt's 134 MB → 5.5 MB is this class: in-place overwrite of a short rebuild after scan latched onto a **STORE nested** member. 5.5 vs 1.7 depends on which inner jar rust zip binds. Do not require bit-identical 5.5.

In-tree `write_fat_spring_zip64_zipa_jar` **DEFLATEs** nested libs so `ZipView(prefix)` cannot latch onto an inner CD (comment in `tests/fixtures.rs`). That fixture stays green while real Spring STORE nested + `zip -A` collapses. Do not keep DEFLATE-nested as the only fat gate.

---

## Why (verified)

`src/scan.rs` `layout_from_first_pk`:

1. `find_cd_first_local` finds the outer CD's first local (prefix length). **Same `first` for unadjusted and `zip -A`.**
2. `zip_archive_opens(view_shift = first)` — if `ZipArchive::new` returns `Ok`, that layout wins.
3. Else `zip_archive_opens(view_shift = 0)` (`zip -A` file-absolute).

`zip_archive_opens` treats **any** `ZipArchive::new` success as the outer zip. It does not compare `archive.len()` to the homemade outer CD count (`find_cd_bounds`).

zip 2.4.2 `find_central_directory`: if the recorded CD offset is not valid in the view (zip -A file-absolute offset vs a prefix-shifted view), it **keeps scanning earlier `PK\x05\x06`**. A STORE `BOOT-INF/lib/*.jar` is a complete zip. The next EOCD is that inner jar. `ZipArchive::new` succeeds with the **inner** listing.

Probe on the zip -A dataflow file:

| view_shift | `ZipArchive::len()` | first name | `header_start` | `BOOT-INF/lib` |
|---|---|---|---|---|
| 0 | 418 | `META-INF/` | 9228 (== prefix) | 318 |
| 9228 | **919** | `META-INF/` | 123115 | **0** |

Unadjusted prefix is the opposite (shift=prefix is correct, shift=0 latches). Today's try-order therefore **accepts the latch** on zip -A + STORE nested.

Consequences on 0.2.2 (working as specified, still wrong output):

- Scan stores 919 reactor-core entries, not 418 outer slots. Nested libs are not CAS blobs.
- `slice_from_archive`: homemade outer CD 418 ≠ 919 → skip-exact. No `tail_blob`, no `raw_zip` (0.2.2 policy).
- Restore is `write_jar` (`ZipWriter`) of the **inner** listing. Prefix is only the 9 KiB script. File size ≈ one nested jar.
- `--restore-paths` unlinks the 134 MB original and `File::create`s the short archive. That is why in-place looks like “truncate then a stub zip.” `File::create` is not the root cause; the listing is.

`write_jar` / Zip64 `large_file` / dropping outer members is **not** the bug on this repro. Do not “fix” ZipWriter unless a second repro shows it after scan lists the outer zip.

---

## Goal (crate 0.2.3)

Stay on **format v2**. Crate **0.2.3**.

### 1. `zip_archive_opens` must accept only the outer listing

**Do this:**

- After `ZipArchive::new` succeeds, accept the view only if `archive.len()` equals the **homemade outer** CD count from `find_cd_bounds` on the **raw file** (already Zip64-aware; EOCD comment-to-EOF so it cannot return a nested 919). This is **not** the `slice_from_archive` gate (that compares parse length to `archive.len()` after layout is chosen).
- Parse `None` / count mismatch → that view **fails** (try the other shift). Do not `raw_zip`. Do not skip-exact a healthy Spring zip -A jar just because the first try latched.
- Also reject when the first listed local is not at the prefix: `header_start + view_shift == prefix_len` (pass `prefix_len` into `zip_archive_opens`; `view_shift` alone cannot evaluate the `view_shift == 0` try). Do **both** count and `header_start` so a coincidental inner `len() == outer count` cannot pass.
- Apply this reject **only** inside `zip_archive_opens` (prefixed layout). Unprefixed scan returns on `PK` magic and never calls it (`dup.txt` last-wins stays skip-exact).
- Empty prefixed zip stays on today's empty / EOCD-extra path (`find_cd_first_local` is `None` when `entries == 0`). Do not require a dummy `by_index(0)`.
- Both views failing stays `None` → empty-EOCD / `NotZip`. No `raw_zip` fallback.

`find_cd_first_local` already knows file-absolute vs zip-relative (`local_name_eq(min_off)` vs `find_local_named`). Optionally try the matching shift first. **Not sufficient alone:** `zip_archive_opens` must still reject a latch, because today's order tries unadjusted first and that `Ok` is the bug.

Do **not** call `capture_zip_exact` / store `raw_zip` when the first try latches.

### 2. Restore stays index + CAS

After (1), zip -A + STORE nested Spring must:

- Scan `entries.len() ==` outer `ZipArchive::len()` (418 on dataflow).
- `tail_blob.is_some() && raw_zip_blob.is_none()` (homemade CD count agrees; attach_exact stores tail).
- Every entry `cdata_blob` absent.
- `--restore-paths` in-place (no `--overwrite`, no `-d`): restored **file size** in the same league as the source (not 134→5.5 / 133.6→1.7). `ZipArchive` names, CD order, and uncompressed file bytes match the source. Whole-file hash **may** change (rebuild / deflate).

Do not add a Java deflater, `cdata_blob`, or `raw_zip` to keep `source_*`.

Do not change `write_jar` / `File::create` / `prepare_restore_dest` unless a post-fix repro still truncates. In-place unlink+create of the **correct** listing is enough.

### 3. Docs / contract (same PR)

| File | Change |
|---|---|
| `Cargo.toml` / lock | Crate **0.2.3** |
| `AGENTS.md` | Current-tree line: crate **0.2.3** / format **v2**. Keep every `tests/docs.rs` locked phrase (no default `cdata_blob`, never writes `cdata_blob` on STORE/DEFLATE, mix `569539 * 115 / 100`, `cdata_blob == 0` on every mix entry, MSRV **1.80**, `forbid(unsafe_code)`). |
| `DESIGN.md` | Executable JAR section: `ZipArchive::new` success is **not** enough. Unadjusted / zip -A open must match homemade outer CD count **and** first `header_start` (0 vs prefix). rust zip may latch onto a STORE nested EOCD when the view's CD offset is wrong. Nested `BOOT-INF/lib/*.jar` stay opaque. Keep `North star: **one CAS blob + ZIP index + zstd blocks**`. |
| `README.md` | Do not tell agents to store `cdata_blob` or chase bit-identical hashes. Keep `Crate **0.2.1** never writes \`cdata_blob\` (file or dir, any method)` (docs.rs). Mention crate 0.2.3 only if a version sentence already exists to update. |
| `tests/docs.rs` | **Replace the 3-way PLAN AND.** New conjuncts, copied **byte-for-byte** from this file: `# PLAN: do not latch ZipArchive onto nested STORE jars (crate 0.2.3)` **and** `zip_archive_opens` must accept only the outer listing **and** `` rust zip may latch onto a STORE nested EOCD ``. |
| `tests/fixtures.rs` | Keep DEFLATE-nested Zip64 fixture. **Add** STORE-nested + launch.script + `zip -A` (classic u32 CD, like dataflow). Comment must say DEFLATE-nested hides the latch. |

---

## Tests (must fail, not log)

1. **STORE-nested complete inner zip + official launch.script + `zip -A` (always-on).** New **classic-u32** helper (do not change `write_fat_spring_zip64_zipa_jar` to STORE; fix its docstring — it claims STORE but DEFLATEs). Outer method **0** members must be a **complete inner zip** (own `PK\x05\x06`), not a random blob named `*.jar`. **0.2.2 latch proof (must be true on the fixture before the detector change):** `ZipArchive::new(ZipView(file, prefix_len))` is `Ok` and `len() ≠` homemade outer CD count (names omit outer `App.class` / `BOOT-INF/lib`). After the fix: `prefix_len ==` script length, **`view_shift == 0`**, `ZipArchive::len() ==` homemade outer count, names include every `BOOT-INF/lib/` member. `raw_zip_blob.is_none()`, `tail_blob.is_some()`, every `cdata_blob` absent.

2. **Matt's flags, in-place.** Directory of jars (Test 1 fixture with **≥2** STORE `BOOT-INF/lib/` complete inner zips + one classic ZipWriter jar). `dehydrate --recursive --sort-inputs --restore-paths`. `rehydrate --restore-paths` onto copies of the original paths (no `--overwrite`, no `-d`). Restored fat: `ZipArchive::len()` equal, names + order + uncompressed file bytes equal; file size not &lt; 50% of source (a 1-lib latch can stay inside 15%). Classic: counts/bytes equal; small size delta OK. `raw_zip` still absent.

3. **Unadjusted prefix + same complete STORE inner zip** (no zip -A): `view_shift == prefix_len`, outer listing, tail, no `raw_zip`. An inverted `header_start` gate must not make this `NotZip`.

4. **Existing** `write_fat_spring_zip64_zipa_jar` (DEFLATE nested, Zip64): still `tail_blob.is_some() && raw_zip_blob.is_none()`, functional identity. Do not loosen.

5. **Mix / corpus** when present: keep `cdata_blob == 0`, `output_len <= 569539 * 115/100`, `raw_zip_blob` absent, `spring-zip64-nested` bit-identical + no `raw_zip`. Do not require whole-file hash on Maven codec-miss jars.

6. **Overlap / last-wins** unchanged: `unique_overlap_content_blobs_not_dual_copy`; `dup.txt` still skip-exact (2 vs 1), no `raw_zip`. Prefixed last-wins is out of scope (no fixture).

Do **not** require whole-file hash match on rebuild / Maven codec-miss. Do **not** add `cdata_blob`.

Further Matt CLI coverage (Zip64 fixture + few-MiB classic in one `--restore-paths` pack, two nearby zip-A fats sharing nested lib bytes, listed-pack manifest shape) lives in `PLAN-coverage.md`. STORE-nested in-place is fat-only (`in_place_restore_paths_store_nested_zipa_must_not_collapse`); do not restage the tiny `a.jar` overwrite-without-`--overwrite` test.

---

## Out of scope

- Format v3 / renaming JSON fields / per-blob zstd frames.
- Java zlib / `Deflater` bitstream matching.
- `--verbatim` / `--exact-cdata`.
- Exploding nested JARs (they stay opaque).
- Rewriting `write_jar` / adding a restore tempfile “just in case” without a post-scan-fix repro.
- Adding a `raw_zip` success path for listed jars.
- Merging, tagging, publish.

---

## Skeptic review (plan)

Origin `skeptic-plan-review` was not reachable. Fresh adversarial Task subagents. Never skip sweep 1. Cap 3, then BLOCKED.

### Sweep 1 — REVISE (1 blocker, applied)

1. **Tests 1–2 can stay green on 0.2.2.** `inner zip bytes, not exploded` still allows incompressible non-zip payloads named `*.jar`. Then today's first try returns `Err`, falls through to `view_shift == 0`, and the 0.2.3 gates never see a latch. Dataflow still collapses. **Required:** new **classic-u32** helper (do not flip `write_fat_spring_zip64_zipa_jar` to STORE; fix that helper's docstring — it says STORE but DEFLATEs). Outer method 0 must wrap a **complete inner zip** (own `PK\x05\x06`). Tests 1–2 must use that helper. Assert the 0.2.2 latch: `ZipArchive::new(ZipView(file, prefix_len))` is `Ok` and `len() ≠` homemade outer CD count (or names omit outer `App.class` / `BOOT-INF/lib`).

Should-fix folded in: pass `prefix_len` into `zip_archive_opens` (or check `header_start + view_shift == prefix_len`); homemade count from `find_cd_bounds` on the raw file, not described as the `slice_from_archive` gate; do not apply the reject to unprefixed scan (`dup.txt`); both views failing stays `None` → empty-EOCD / `NotZip`, no `raw_zip`; Test 3 uses the same complete STORE inner zip without `zip -A`.

### Sweep 2 — ACCEPT (fresh skeptic; NO BLOCKING FINDINGS)

Should-fix for implement (not blockers):

- Homemade count is `find_cd_bounds(...).3` only. After `zip -A`, `recorded_cd_offset` is file-absolute — do not treat it as prefix/`header_start`.
- Latch proof lives in `src/scan.rs` (or tests duplicate `ZipView`). `ZipArchive::new(File)` is `view_shift == 0` and lists the **outer** zip on `zip -A` — that does not prove the latch.
- Test 2 size-within-15% cannot catch a 1-lib latch (inner ≈ source). Require ≥2 `BOOT-INF/lib/` STORE members; listing/uncompressed-bytes are the latch catch. Optional lower bound: restored size not &lt; 50% of source.
- `tests/docs.rs` `PLAN.contains` must use the exact substrings already in this file (not the table’s double-backtick padding).
- Compute homemade count in `layout_from_first_pk` (has `file_len`) and pass it into `zip_archive_opens`. `ZipArchive<ZipView<&mut File>>` holds `file`; a second `find_cd_bounds` will not compile.
- Do not add a sentinel-EOCD Zip64+STORE helper using only in-tree `adjust_self_extracting_offsets` (does not patch Zip64 EOCD/locator → both views latch → `NotZip`). Keep Test 4 on the existing DEFLATE Zip64 fixture.

### Sweep 3 — ACCEPT (fresh skeptic; NO BLOCKING FINDINGS)

Should-fix for implement (not blockers): homemade count is `.3` only; latch proof is `ZipArchive::new(ZipView(file, prefix_len))` not `ZipArchive::new(File)`; after-fix `len()` is the **chosen** view; do not use `archive.offset()` in place of `header_start` (`offset()` is 0 on a correct zip -A open); docs.rs exact substrings; Test 2 ≥2 STORE libs (50% size is backup); no Zip64+STORE helper on `adjust_self_extracting_offsets`.

**Plan locked.** Three sweeps (REVISE, ACCEPT, ACCEPT). Implement from this file.
