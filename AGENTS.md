# Agent contract

This file is mandatory. It is not a suggestion. Read it before changing pack format, dehydrate, rehydrate, corpus tests, or storage docs.

Cloud agents must use the in-repo `.cursor/skills` plan/skeptic skills (`review-plan`, `skeptic-plan-review`, `skeptic-code-review`) and `knowledge/*-skepticism/`. Origin `matt-brewer/agent-skills` is not required and is often unreachable.

## Storage efficiency is of the utmost importance

That is the product. Valid ZIP out of index + blobs is enough. Whole-file `source_*` hashes may change. Do not trade pack size for Java-zlib bit-identical files.

## Locked policy

1. Store **one** deduped copy of each payload. Dedup key is BLAKE3 of **uncompressed** entry bytes (same class across JARs is one blob). This is the data.
2. Keep **indexes** of where that blob belongs inside the original ZIP, in the ratarmount-rs sense: name, CD order, method, CRC, compressed/uncompressed sizes, GPBF, `local_header_offset`, local header / data-descriptor / pad metadata, jar tail (CD through EOF), prefix if any, blob hash. The original JAR is gone; the index points at the CAS blob, not at a sidecar copy of the source file.
3. **Never** store a second encoding of the same entry. No default `cdata_blob` next to the content blob. `ZipArchive` count ≠ homemade CD count is a parser bug, not a `raw_zip` case. Crate 0.2.2 never writes `raw_zip` of a listed jar. `raw_zip` only if `ZipArchive` never populated `entries[]` (`UnsupportedArchive` spanning / `NotZip`). Homemade parse failure / overlap on a listed jar is skip-exact (index + CAS / `write_jar`), never whole-zip CAS. Do not reintroduce dual copies. That is why packs went 200MB → ~3GB: uncompressed CAS + original deflate streams (zstd cannot shrink those).
4. **Zstd-compress the actual data in blocks.** Format v2 already does this: record-aligned zstd **groups** flushing at 4 MiB of uncompressed BLOB **record** bytes, final MANIFEST+END frame, uncompressed TOC. Do **not** switch to per-file/per-blob frames (resets the window, loses size). Do not store pre-deflated ZIP cdata as the CAS payload. Blobs in the frames are uncompressed entry bytes; zstd is the only pack compression.
5. Restore rebuilds a valid ZIP from index + blobs (STORE splice / flate2 codec hit if it happens / otherwise rebuild). Whole-file `source_*` hashes may change. That is acceptable. Do **not** add a Java/zlib deflater, `cdata_blob` for misses, or `raw_zip` of healthy jars just to keep file hashes.
6. Read old packs (v1, legacy `cdata_blob`, 0.1.6–0.1.8 dual copy) but **never write** that shape again.

The manifest is a ZIP-slot index (ratarmount-style pointers), not a second copy of file bytes.

## Forbidden

- Dual `cdata_blob` + content for the same entry.
- Per-file / per-blob zstd frames as the default.
- A Java-zlib bit-identical project (matching vendor `Deflater` bitstreams to keep `source_*`).
- Storing a whole listable jar as `raw_zip` because a codec missed or a sibling was exotic.
- Renaming JSON fields (`blob`, `local_header_offset`, `cdata_blob`, …) or bumping format off v2 to “fix” this.
- Edition-2024 dependencies. MSRV is **1.80**. `forbid(unsafe_code)` stays.

## Current tree vs this contract

Crate **0.2.3** / format **v2** groups uncompressed blobs in 4 MiB record-aligned zstd frames and **never writes** `cdata_blob` on STORE/DEFLATE (file or dir, any method). Crate **0.2.2** never writes `raw_zip` of a listed jar. `zip_archive_opens` must accept only the outer listing (rust zip may latch onto a STORE nested EOCD). CleanMiss rebuilds class-4 / mixed-exotic. Crate 0.2.0 leftover MixedExact / ExactWithExotic dual copies still read. Do not add new `cdata_blob` puts. Do not “fix” mix size by storing more cdata.

## Intended crate 0.2.4 (not this tree)

See [`PLAN.md`](PLAN.md). Format stays **v2** (additive keys). Do not implement in a plan-only PR.

- Stencil: ratarmount-rs columns (`offsetheader`, `data_start`, `ZipMemberTable`, `DurableZipMember` / `nestedindexes`). Offsets are a write recipe after the source file is gone. Keep `blob` / `local_header_offset` / `tail_blob` / `prefix_blob`.
- Slot payload is **one** of: CAS `blob` + optional `cdata_codec`, or a depth-1 child `zip_index`. Probe with `slice_from_bytes` **before** `jobs==1` `commit_blob` and before `--jobs` `spawn_file`. Encrypted **child** is opaque (do not fail the outer). Never `commit_blob` inners onto the outer listing. Never CAS `blake3(inner zip)` if that slot becomes `zip_index`. `--jobs`: one `Sequenced::Exploded` seq; `remember_blob` of inners only in `apply_sequenced` (first-seen / END digest stay jobs-invariant). Child stencil is tail-bearing (including child `leading_pad_blob`). Cap child file entries at **65535**. Opaque fallback if the child would not exact-restore. Homemade-`None` never gets `tail_blob`. `verify` / `write_jar` use `reconstruct_child_zip(index, get_blob)` (skip-exact outer has no opaque inner CAS). No SQLite in the `.ayz`.
- Codecs (no `cdata_blob`): STORE; `deflate-raw:zlib:{1,6,9}` via in-process zlib-rs (not a Java subprocess); existing `deflate-raw:flate2:{1,3,6,9}`; `deflate-raw:stored`. Cache by `(blob, compressed_size, flags, blake3(original cdata))` or re-verify on hit. A miss must not drop sibling codecs (today CleanMiss is jar-wide). `deflate-raw:stored` hits BTYPE 00; do not use that fixture as a miss.
- Do not treat `first offsetheader == prefix_len` as new work (0.2.3 already slices that). Remaining skip-exact: homemade-`None` (`tail_blob` only if every slot hits) and leading-pad (`leading_pad_blob`, do not extend `prefix_len`). Overlap stays skip-exact.
- `source_*` must match when every slot hits. A zopfli / unknown-deflate miss still rebuilds that entry only. Rewrite mix `proven_miss` so sibling codecs are allowed.

## Tests that must fail

A new pack of the mix / corpus **must fail CI** if:

- it writes `cdata_blob` on any mix entry (file or dir, any method) — no “documented exotic” exception;
- mix `.ayz` `output_len` exceeds `569539 * 115 / 100` (keep/strengthen that gate; do not loosen it).

Add an explicit `cdata_blob == 0` on every mix entry. Add a two-jar overlap unit whose unique **content** blob count equals unique uncompressed payloads, not 2× (index tails/headers are not a second encoding; `cdata_blob` is). Add the ExactWithExotic / class-4 dir fixture from `PLAN.md` so “put `cdata_blob` back” cannot be the hash fix.

Do **not** require whole-file hash match on Maven codec-miss jars. After 0.2.4, a zlib **hit** is not a miss: `source_*` must match. Mix `proven_miss` must allow sibling codecs on a rebuild jar.

## Docs and engineering

Behavior change ⇒ update `DESIGN.md` + tests in the same PR. README / `docs/library.md` must not tell agents to store `cdata_blob` or chase bit-identical hashes.

Implement `PLAN.md`. Do not merge. Do not tag unless asked.
