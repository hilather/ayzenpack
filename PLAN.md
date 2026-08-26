# PLAN: metadata-only exact packs (v0.1.9)

Base: current `main` at v0.1.8 (`374ed67`). Branch `cursor/metadata-only-exact-1c0c`. Do not tag. Do not merge.

Origin `matt-brewer/agent-skills` is not reachable (`origin` CLI unauthenticated; clones of `origin.cursor.com/matt-brewer/agent-skills` and `cursor.com/codebase/matt-brewer/agent-skills` failed). This file is the required written plan. Skeptic loops use fresh Task subagents with the verbatim prompts from the request. Implementation starts only after skeptic-plan-review reports **NO BLOCKING FINDINGS** (or 3 sweeps, then BLOCKED).

## Goal

v0.1.6–0.1.8 always CAS-put every local `cdata` as `cdata_blob`. STORE is already the content blob (same BLAKE3, extra `ref_count` only). DEFLATE `cdata` is already-deflated; zstd does not shrink it. That second copy is why new packs ballooned (~200MB → ~3GB).

**Default for new packs:** keep metadata + pack order; do **not** store a second payload copy. Rehydrate either (a) re-encodes a bit-identical raw-deflate stream, or (b) rebuilds a valid ZIP with patched sizes/offsets. Old packs that already have `cdata_blob` keep working.

No user flag. No `--exact-cdata`. MSRV 1.80, edition 2021, no `unsafe`, no edition-2024 deps. Prefer no new crate; `flate2` is already in `Cargo.lock` via `zip 2.4.2` (`flate2 1.1.9` + `miniz_oxide`). Add it as a **direct** pin `=1.1.9` so we call `DeflateEncoder` / `DeflateDecoder` without a new dependency graph.

## Verified against this tree (hypothesis vs code)

| Claim | Code |
|---|---|
| Exact splice writes prefix + local_header + cdata + descriptor + pad at `prefix + local_header_offset`, then tail at `source_size - tail.len` | `src/rehydrate.rs` `write_exact_jar` / `write_exact_entry` |
| `fill_exact_entry` always `remember_blob(local.cdata)` and sets `cdata_blob` (except empty dirs) | `src/dehydrate.rs:983–1007` — **this is the bloat** |
| STORE `cdata ==` uncompressed payload | `src/exact.rs` `stored_cdata_equals_payload` |
| `raw_zip_*` only when slice fails or CD/entry-count mismatch | `attach_exact` + `capture_zip_exact` |
| `Jar::exact_restore()` is `raw_zip \|\| tail_blob` | `src/manifest.rs:54–56` — **too coarse after this change** |
| Signed rebuild warning is `signed && !exact_restore()` | `src/dehydrate.rs:593–597` |
| Prefix / Spring / `zip -A` / Zip64 / decoy-PK are scan-time | `src/scan.rs`; must stay |
| `--restore-paths` is orthogonal | leave `DehydrateOptions.restore_paths` and rehydrate dest/mode/owner alone |
| Manifest v1, `skip_serializing_if = Option::is_none`, schema `additionalProperties: false` | new keys must be optional on the struct **and** listed in the schema |
| In-tree fixtures use the `zip` crate (same flate2/miniz_oxide backend) | `tests/fixtures.rs` `write_jar` defaults to Deflated |

Uncompressed entry bytes are **not** still in RAM at `attach_exact` (hash pipeline drops them after `commit_blob`). Trial-encode therefore inflates `local.cdata` with `flate2::write::DeflateDecoder` (raw deflate, same as ZIP), then re-encodes. Do not change the hash pipeline.

## Manifest (v1, optional only)

Crate version **0.1.9**. Manifest `version` stays 1. `examples/tiny.manifest.json` unchanged (no new required keys).

`Entry` gains one optional field, declared after `cdata_blob` (stable serde order):

| Key | Type | When present |
|-----|------|----------------|
| `cdata_codec` | string | DEFLATE byte-for-byte match; **never** together with `cdata_blob` on newly written packs |

Codec string (pinned, no negotiation): `deflate-raw:flate2:<level>` where `<level>` is `1`, `6`, or `9` (and `3` only if the GPBF hint selected it and it matched).

`skip_serializing_if = "Option::is_none"`. Schema `entry.properties` lists `cdata_codec` (`type: string`, pattern `^deflate-raw:flate2:[0-9]+$`). `additionalProperties: false` stays. `src/manifest.rs` tests: omit-when-none, field-order after `cdata_blob`, schema contains the key.

No jar-level `rebuild` key. Infer restore mode from existing fields (below).

## Restore-mode inference (three paths)

Replace the boolean `exact_restore()` with two helpers used by dehydrate warning + rehydrate dispatch:

1. **`bit_identical_restore()`** — `raw_zip_blob` is set, **or** `tail_blob` is set and every file entry can resolve cdata without rebuilding:
   - `cdata_blob.is_some()` (old 0.1.6–0.1.8 packs), **or**
   - `cdata_codec.is_some()`, **or**
   - `method_code == 0` (STORE; use content `blob`), **or**
   - `is_dir` (empty cdata)
2. **`metadata_rebuild()`** — `tail_blob` is set, `raw_zip_blob` is absent, and some file entry is DEFLATE/`other` without `cdata_blob` and without `cdata_codec`.
3. **Else** — old 0.1.4/0.1.5 content archive → existing `write_jar` / `ZipWriter`.

`exact_restore()` becomes `bit_identical_restore()` (keep the name as a thin wrapper) so the signed warning `signed && !exact_restore()` already says **rebuild will break the signature** for metadata-rebuild jars. `raw_zip` and codec-hit / STORE / old-cdata packs stay “signed JAR” only.

**Resolution order for one entry’s payload bytes** (must match the request):

1. `cdata_blob` if present (old packs; also exotic methods)
2. else `cdata_codec` → load content `blob`, raw-deflate at the recorded level, compare length to `compressed_size`; mismatch is `HashMismatch`
3. else STORE (`method_code == 0`) or empty dir → content `blob` or `[]`
4. else rebuild fields (jar-level metadata rebuild)

If **any** file entry on a sliced jar is a codec miss, the **whole jar** is metadata-rebuild (one changed compressed size invalidates later `local_header_offset`s and the tail). On miss, do **not** store `cdata_codec` on the hits either — one canonical rebuild encoder for the jar.

## Dehydrate (`fill_exact_entry` / `attach_exact`)

`attach_exact` still slices via `capture_zip_exact`. `raw_zip_*` remains **last resort when the zip cannot be sliced**, never the deflate-miss path.

For each sliced local, still record `local_header_*`, `data_descriptor_hex`, `pad_*`, `local_header_offset` (needed for exact splice **and** rebuild patching).

**Do not** `remember_blob(local.cdata)` except:

| Entry | Action |
|---|---|
| dir with empty cdata | no `cdata_blob`, no codec (today’s skip) |
| STORE (`method_code == 0`) | omit `cdata_blob`; rehydrate uses `blob` |
| DEFLATE (`method_code == 8`) | inflate `local.cdata`; trial-encode; on hit set `cdata_codec` only; on miss set neither |
| `other` / inflate failure | **keep `cdata_blob`** for that entry so we do not invent a bitstream we cannot reproduce. If any entry on the jar is `other`/uninflateable, that jar stays bit-identical (has `cdata_blob`) and is **not** mixed into metadata-rebuild. |

Trial order (deterministic, first match wins):

1. GPBF bits 1–2 from local header bytes 6–7 (APPNOTE 4.4.4): `00→6`, `01→9`, `10→3`, `11→1`
2. then `6`, `9`, `1` (skip duplicates)

Encoder: `flate2::write::DeflateEncoder` + `Compression::new(level)` into a `Vec` (raw deflate, not zlib/gzip). Hit = byte-for-byte equality with `local.cdata`.

On a jar-wide miss (any DEFLATE entry failed the trial and was not `other`):

- leave those entries without `cdata_blob` / `cdata_codec`
- still store tail + local headers
- do **not** write `raw_zip`
- `source_blake3` / `source_sha256` / `source_size` stay the **original file** hashes/size (do not overwrite with a rebuilt file)

`--restore-paths` collection stays as-is, after scan, before `attach_exact`.

## Rehydrate

`restore_jars` dispatch:

```
if jar.bit_identical_restore() { write_exact_jar + verify_source_identity }
else if jar.metadata_rebuild() { write_rebuilt_jar  /* no source_* verify */ }
else { write_jar /* ZipWriter */ }
```

### Exact path (`write_exact_entry`)

Replace “`cdata_blob` or empty” with `resolve_cdata` using the resolution order above. Tail position and `set_len(source_size)` unchanged. Prefix + chmod rules unchanged (`apply_prefix_chmod` still respects `--restore-paths` + `restore_mode`).

Codec-hit: encode content blob; if encoded bytes ≠ `compressed_size`, `HashMismatch` (corrupt pack / wrong codec). Then splice at original offsets. `source_*` still verified.

STORE without `cdata_blob`: write content blob as cdata (length must match `compressed_size` == `uncompressed_size`).

Old `cdata_blob`: unchanged.

### Metadata rebuild (`write_rebuilt_jar`) — the miss path

Do **not** seek to original `local_header_offset`s. Write a new zip portion, then the patched tail.

Canonical miss encoder: `deflate-raw:flate2:6` for every `method_code == 8` file. STORE / dirs stay stored/empty.

**Offset mode** (prefix / `zip -A`): from the first CD record in `tail` vs `entries[0].local_header_offset` and `prefix_size`:

- if `cd_local_off == local_header_offset` → ZIP-relative (Spring unadjusted)
- if `prefix_size > 0 && cd_local_off == prefix_size + local_header_offset` → file-absolute (`zip -A`)
- else → `Format` (do not guess)

**Per entry (CD order = `jar.entries`):**

1. Load original local header (`hex` or blob).
2. New cdata = STORE/`dir` ? content-or-empty : `deflate_raw(content, 6)`.
3. Patch sizes in the local header **in place**:
   - GPBF bit 3 set: leave the three local size fields as they were (usually 0 / Zip64 sentinels); patch the data descriptor’s compressed-size field (u32 or u64 depending on original descriptor length 12/16/20/24, same layout `split_descriptor` already understands).
   - else: write `compressed_size` at local +18 (u32, or `0xFFFFFFFF` if a Zip64 extra already carries the 64-bit size) and patch the Zip64 extra payload’s compressed-size slot (APPNOTE order: uncomp, comp, … only the fields that were present).
4. If the new compressed size **does not fit the original Zip64 layout** (32-bit fields, no extra, size ≥ 4 GiB — should not happen for class files): error `Format` with the jar/entry name rather than silently emitting a corrupt ZIP. Do not insert new extra fields (that would change header length and every later offset, and CD extra size).
5. Drop original `pad_*` on rebuild (pads were alignment to the *old* next local).
6. Track running zip-relative write offset.

**Patch tail in place** (length unchanged):

- Walk CD records in lockstep with `jar.entries`. Patch compressed size at CD+20 (and Zip64 extra). Patch local-header offset at CD+42 (and Zip64 extra) using the offset mode above.
- If a Zip64 EOCD is present (locator magic immediately before classic EOCD, as `find_zip64_cd_bounds` already requires): patch `cd_offset` (and locator’s Zip64-EOCD offset = `new_cd_start + original_cd_size`).
- Classic EOCD: if recorded `cd_off` is `u32::MAX`, leave it; else write the new CD start (zip-rel or file-abs). `cd_size` unchanged.
- Archive comment bytes after EOCD stay.

Write: prefix (existing `write_prefix`) + packed locals + patched tail. `set_len` to that sum. **Do not** call `verify_source_identity`. Unix prefix `chmod 0755` still applies when `apply_prefix_chmod`.

Rebuild output must:

- open with `zip::ZipArchive`
- same Unicode names and CD order
- same uncompressed entry bytes
- same DOS timestamps / extras / GPBF (except the size fields we patched)
- **not** equal `source_blake3` (test asserts inequality or at least does not require match)

## Tests

Offline `cargo test` / `cargo clippy --all-targets -- -D warnings`.

New / updated in `tests/roundtrip.rs` (and manifest unit tests):

1. **STORE uses content blob, no second object.** Stored fixture: every file entry has `cdata_blob.is_none()`, `cdata_codec.is_none()`, and `blob` equals the only payload CAS id. `blobs[].ref_count` is not doubled for cdata. Bit-identical restore.
2. **Codec hit is bit-identical.** `write_jar` / mixed stored+deflated / Spring / zip-A / Zip64 / data-descriptor (STORE) / zipalign (STORE) fixtures already in this file must still `assert_bit_identical`. Manifest shows `cdata_codec` on deflated file entries and **no** `cdata_blob`. `source_*` verify stays.
3. **Codec miss rebuilds a valid ZIP.** Hand-built ZIP whose DEFLATE cdata is a raw stored-block (`01` + len + `~len` + payload) so miniz_oxide levels 1/3/6/9 cannot match. After pack: no `cdata_blob`, no `cdata_codec`, no `raw_zip_*`, `tail_blob` present. Rehydrate: `ZipArchive` opens; names/order/uncompressed bytes match; file bytes ≠ source; pack `output_len` and `bytes_unique_blobs` are not ~1:1 with the jar size (unique blobs ≈ uncompressed entries + small headers/tail).
4. **Old-style `cdata_blob` fixture.** Craft a pack with `format::write_ayz_file`: content blob + **separate** cdata blob + tail + local header hex + `cdata_blob` set, `cdata_codec` absent. Rehydrate is bit-identical. (Also keep a swapped-`cdata_blob` hash-fail test by crafting, not by dehydrating a new pack.)
5. **Prefix / Spring / zip-A / Zip64** existing tests stay bit-identical (zip-crate deflate → codec hit).
6. **Signed + rebuild:** miss-style signed-looking jar warns `rebuild will break the signature` (stderr or `signed && !exact_restore()`). Codec-hit signed fixture stays bit-identical and does **not** use that warning.
7. **`strip_exact_fields`** also clears `cdata_codec`.
8. **`verify()`** does not require a blob for `cdata_codec`; still requires `cdata_blob` when present.
9. **`two_jars_share_nested_lib_cdata_blob`:** assert shared **content** `blob` + same `cdata_codec`, not `cdata_blob`. Rename.

`tests/prop_roundtrip.rs` still requires full-file equality (generator uses `zip` crate → codec hit).

`tests/docs.rs` `--verbatim` line stays; README/DESIGN text about “always store cdata_blob” must change.

## Docs

- `DESIGN.md`: reconstruction is metadata-only by default; `cdata_blob` is legacy / exotic; `cdata_codec`; miss = rebuild, **do not** claim `source_blake3` match; `tool_version` example `0.1.9`; memory line no longer says peak includes a `cdata` copy for new packs.
- `README.md` Reconstruction guarantee: same facts. No new CLI flag table row.
- `docs/library.md` only if it describes exact/cdata storage (it currently does not).
- Schema as above.

## Corpus (after implementation, not in default `cargo test`)

`ci/download-corpus.sh` → `.corpus`, `AYZENPACK_CORPUS_DIR=.corpus`. Dehydrate the lockfile artifacts + copies. Report in the PR:

- total jar bytes (`bytes_in_jars`)
- unique blob bytes (`bytes_unique_blobs`)
- `.ayz` size (`output_len`)
- hypothetical cdata-full size if cheap (sum of compressed_size over file entries + unique uncompressed + tails/headers — or dehydrate a one-off that still puts cdata, test-only, **not** a CLI flag)
- codec hit/miss rate (deflate entries with `cdata_codec` vs without)

`.ayz` must sit in the pre-0.1.6 ballpark (near `zstd(unique uncompressed)`), not near `sum(jar sizes)`.

Rehydrate corpus; verify entry contents. Bit-identical only where `cdata_codec` or STORE/`cdata_blob`.

Existing `tests/corpus.rs` overlap tests (env-gated) should still pass; update any assertion that requires `cdata_blob` or whole-file equality for Maven JARs (Java zlib ≠ miniz_oxide is likely a **miss**. That is expected; rebuild must still be a valid ZIP).

## Out of scope

- Manifest version bump
- `--exact-cdata` / `--verbatim`
- Recursively exploding nested JARs
- New deflate backend (zlib-ng / libz) to raise Maven hit rate
- Inserting Zip64 extras on rebuild
- Tagging, merging

## Implementation order

1. `flate2 = "=1.1.9"` in `Cargo.toml`; `cdata_codec` + schema + manifest tests; crate `0.1.9`.
2. `deflate_raw` / `inflate_raw` / GPBF hint helper (unit-tested) in a small module (`src/deflate.rs` or `exact.rs`).
3. Change `fill_exact_entry` (no default cdata put; trial; other-method escape).
4. `resolve_cdata` + exact path; then `write_rebuilt_jar` + dispatch + signed warning.
5. Tests + docs.
6. `cargo test`, `cargo clippy -D warnings`.
7. Download corpus; measure; put numbers in the PR.

## Residual risks (accepted)

- Maven/Java deflate will usually **miss** with miniz_oxide. Product is still correct (rebuild). Hit-rate is a metric, not a gate.
- Rebuild does not preserve zipalign padding (pads dropped). Miss fixtures are not aligned.
- Rebuild refuses rather than invent Zip64 extras if a size crosses 4 GiB without an existing extra.
- Residual: a crafted pack with `cdata_codec` but wrong content still fails encode-size/`source_*` checks on the exact path.
