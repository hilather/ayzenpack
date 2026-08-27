# PLAN-coverage: in-place restore-paths fat + classic (crate 0.2.3)

Superseded as the product plan by [`PLAN.md`](PLAN.md) (crate **0.2.4** stencil restore). This file remains the **0.2.3** coverage note for Matt’s locked in-place tests. Do not merge. Do not tag. Do not restage #36 / #37.

Base: PR `cursor/fix-rehydrate-fat-jar-8517` (detector fix already in tree). The 0.2.3 latch product fix shipped in `21e73bb`. These tests must be small, named after the failure, and use in-tree fixtures. No new size-cap constants. No `output_len * 115/100`. No corpus unless files are already present.

Origin `matt-brewer/agent-skills` is not reachable. Skeptic loops use fresh adversarial Task subagents. Never skip sweep 1. Fresh skeptic each sweep. Cap 3, then BLOCKED.

---

## Already covered (do not duplicate)

- `fat_spring_zip64_zipa_is_listed_raw_on_v021_no_dual_copy_now` — listed Zip64+prefix+zip-A has no `raw_zip`; dest-dir restore of a **splice** (`tail_blob` present)
- mix: no `raw_zip` / no `cdata_blob` on members (`569539 * 115/100` stays in AGENTS / corpus.rs — do not add a second copy here)
- `overlapping_locals_listed_jar_has_no_raw_zip`
- `duplicate_entry_names_in_one_jar_all_restored` last-wins no `raw_zip`
- `tests/inputs.rs`: `--recursive` walks, picks jar/zip/war/ear
- `sort_inputs` byte-identical
- `restore_paths.rs` tiny `a.jar` in-place **without** `--overwrite`
- dest-dir rehydrate requires `--overwrite` if the dest exists
- #36 signed warning, #37 Dataflow pair — other PRs

`--restore-paths` skips the overwrite guard (`prepare_restore_dest` creates parents and does not unlink; writers emit sibling tmp then `replace_file`). Hang new in-place tests off that same rule. Do not add `--overwrite`.

---

## Reproduce first (required)

Matt’s flags only:

```
ayzenpack dehydrate --recursive --sort-inputs --restore-paths -o pack.ayz <dir>
ayzenpack rehydrate --restore-paths -i pack.ayz
```

No `--overwrite`. No `-d`.

Use `write_fat_spring_zip64_zipa_jar` (0.2.2 fixture: official launch.script + Zip64 + zip-A + nested `BOOT-INF/lib`). Also a classic no-prefix JAR with a few MiB of stored payload.

Record source vs restored: file size, `ZipArchive` file-entry count (`!is_dir()`), uncompressed bytes per name, `raw_zip_blob`.

On 0.2.2 (`87ebeff`), Matt CLI in-place (this VM, `--restore-paths` only):

| Input | Source | Restored | File entries | Notes |
|---|---|---|---|---|
| `write_fat_spring_zip64_zipa_jar` | 535863 | **535863** | 6 → 6 | Names unchanged. **No collapse.** Do not invent a Zip64 truncate. |
| STORE-nested + zip-A (2×2 KiB complete inners) | 13945 | **9372** | 3 → **1** (`com/Lib0.class`) | Latch. Size only ~1.5×; `* 10` would stay green. Entry map vs source is the catch. |
| Maven dataflow + launch.script + zip -A | 133571151 | 1737752 | 418 → inner | 77×. The 134→5.5 class. |

The Zip64 DEFLATE-nested fixture **hides** the latch. The STORE-nested latch is the 134→5.5 class; keep a small in-tree STORE-nested in-place test that fails on unfixed 0.2.2.

On current 0.2.3 tree, STORE-nested + zip-A **splices** (`tail_blob` present). Every in-tree fat splices. Still run Matt CLI (1) on the Zip64 fixture.

---

## Tests to add

Matt CLI identity (1) and manifest (4) are **not** the 0.2.2 latch. They must still land. Only the STORE-nested zip-A in-place test is required to fail on unfixed 0.2.2.

### 1. Matt CLI in-place (`restore_paths.rs`) — Zip64 + classic

**Fact (do not invent a Zip64 truncate):** `write_fat_spring_zip64_zipa_jar` DEFLATEs nested libs, so rust zip does not latch. Dest-dir already splices on 0.2.2 (`fat_spring_zip64_zipa_is_listed_raw_on_v021_no_dual_copy_now`). In-place unlink+create uses the same listing. Zip64+classic in-place stays green on unfixed 0.2.2. Reproduce must record those sizes. Do not name this test `must_not_collapse`.

One directory containing:

- (a) `write_fat_spring_zip64_zipa_jar` (reuse 0.2.2 fixture — do not flip it to STORE)
- (b) a classic no-prefix JAR with real payload (a few MiB stored, enough to notice)

`dehydrate --recursive --sort-inputs --restore-paths -o pack.ayz <dir>`
`rehydrate --restore-paths -i pack.ayz` only (NO `--overwrite`, NO `-d`).

Name: `in_place_restore_paths_zip64_fat_and_classic`.

Snapshot each source **before** rehydrate. For **both** dests vs **that** source:

- same ZipArchive **file**-entry count (`!is_dir()`), not `ZipArchive::len()` unless the fixture has no dirs
- same uncompressed bytes per name
- dest file size in the same league as source — `assert!(restored_len * 10 >= source_len)` inline; **no** named constant
- sidecar / pack: `raw_zip_blob` none on both jars
- whole-file hash **may** change

Classic half stays in the same test (one pack, two dests) so `--recursive` is actually exercised.

Keep tiny `a.jar` overwrite-without-`--overwrite`. Do not restage it.

### 2. Rebuild dest-dir, not only splice

**Fact on this 0.2.3 tree:** Zip64 zip-A, STORE zip-A, and STORE unadjusted all splice (`tail_blob` present). There is no in-tree listable fat that is skip-exact. Overlap / `dup.txt` are not fats. Do not add a fake skip-exact fat. Do not add a dest-dir rebuild test. Test 2 is a **comment on (1)** only.

The STORE-nested zip-A fixture is skip-exact on **unfixed 0.2.2** (wrong listing). After the detector it splices. A dest-dir STORE-nested test on this tree is a splice, not a rebuild — do not claim otherwise.

### 3. Two nearby fats, one pack, restore-paths in-place

Two **zip-A** prefixed fats (`write_fat_spring_store_nested_zipa_jar` style, not the unadjusted helper). Unadjusted prefix makes `ZipArchive::new(File)` latch on **both** the pre-rehydrate snapshot and the dest, so dest==“source” is two inner listings (false green). Pin zip-A so `ZipArchive::new(File)` is the outer view.

They share the same nested lib **bytes** (one CAS blob). Plant those bytes (do not rely on stock helpers accidentally sharing `App.class`). `dehydrate --recursive --sort-inputs --restore-paths`. `rehydrate --restore-paths` only (NO `--overwrite`, NO `-d`).

Snapshot each source’s file-entry names (`!is_dir()`) and uncompressed bytes **before** rehydrate (in-memory or a copy — after restore the path is the dest). Each dest must equal **that** source map — not dest↔dest and not dest↔pack listing (0.2.2 can latch both fats onto the same inner zip). Each source map must contain that jar’s outer `BOOT-INF/lib/…` planted member, not the inner `META-INF` / `com/Lib*.class` listing.

- unique content blobs = distinct `Some(blob)` on pack-wide `!is_dir()` entries (not `stats.unique_blob_count`, not tails / `local_header_blob`) **<** sum of file entries (`!is_dir()`)
- the planted `BOOT-INF/lib` entry’s `blob` equals BLAKE3(planted inner-jar bytes) on both jars (not “any shared blob” such as `App.class`)
- `raw_zip_blob` none
- size not 10× smaller (inline `* 10`)

Do not explode nested JARs. Do not require whole-file hash.

### 4. Manifest shape (small, always-on)

On the pack from (1) or a one-jar dehydrate of the Zip64 fixture:

- every file entry (`!is_dir()`) has `blob`
- directory entries may have `blob == null`
- `raw_zip_blob` absent / `raw_zip_size` unset (sum of present `raw_zip_size` == 0)
- no `cdata_blob`

---

## STORE-nested in-place (the 134→5.5 class; must fail unfixed 0.2.2)

The 0.2.2 Zip64 fixture DEFLATEs nested libs so rust zip does not latch. The **134→5.5** bug is STORE-nested + zip-A + `--restore-paths`.

Rename existing `restore_paths_in_place_keeps_store_nested_zipa_entries` to `in_place_restore_paths_store_nested_zipa_must_not_collapse`. Fat only (drop the tiny classic sibling). Matt flags, no `--overwrite`, no `-d`.

Snapshot the source file-entry map (`!is_dir()`, uncompressed bytes per name) **before** rehydrate into memory or a copy — after restore the path is the dest. Dest must match **that** map, not dest↔pack listing (unfixed sidecar is the inner listing). That is the latch catch: a 2-lib in-tree stub that binds one inner zip is only ~2× smaller, so inline `restored_len * 10 >= source_len` will not fail it. Do not add a 50% named constant. Do not drop the source snapshot. `raw_zip` none. Do not require `tail_blob` here (dest-dir splice test already does). The source map must include outer `BOOT-INF/lib/` members, not the inner `META-INF` / `com/Lib*.class` listing.

---

## Out of scope

- #36 / #37
- Corpus / mix `115/100` (already on main)
- Flipping `write_fat_spring_zip64_zipa_jar` to STORE
- `write_jar` / `File::create` rewrite unless a Zip64 in-place collapse is reproduced
- Merge, tag

---

## Skeptic review (plan)

Fresh adversarial Task subagents. Never skip sweep 1. Cap 3, then BLOCKED.

| Sweep | Verdict | Folded |
|---|---|---|
| 1 | REVISE | Zip64+classic is Matt CLI identity, not `must_not_collapse`. dest==**source** snapshot on two fats. |
| 2 | REVISE | Pin zip-A (unadjusted `ZipArchive::new(File)` is a false source). In-memory snapshot. Planted `BOOT-INF/lib` + BLAKE3. |
| 3 | ACCEPT | Nits: thin zip-A writer with payload hook; count `Some(blob)` on `!is_dir()` only; no tiny classic on the STORE test. |
