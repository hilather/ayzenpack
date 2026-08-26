# PLAN: ayzenpack crate 0.2.0 / format v2

Base: `cursor/metadata-only-exact-1c0c` (PR #30, v0.1.9 metadata-only exact). Branch `cursor/format-v2-0-2-0-1cf2`. Do not push onto #30. Do not overwrite #30 `PLAN.md` on that branch. Do not merge. Do not tag.

This file is the **locked plan** that survived 3 skeptic-plan-review sweeps (7 then 4 blockers; sweep 3 = NO BLOCKING FINDINGS). Do not reopen grouping vs per-blob. Implement this. After the diff exists, run skeptic-code-review on `git diff cursor/metadata-only-exact-1c0c...HEAD` until NO BLOCKING FINDINGS or 3 sweeps.

Origin `matt-brewer/agent-skills` is not reachable from this environment. Skeptic loops use fresh adversarial Task subagents.

Constraints: MSRV 1.80, `forbid(unsafe_code)`, no zstd-framed, no new edition-2024 deps. Keep 0.1.9 metadata-only exact (no default `cdata_blob`). Manifest JSON field names stay (`blob`, `uncompressed_size`, `local_header_offset`, …). Seek prefix + `local_header_offset`.

---

## Goal

New packs are **format v2** / crate **0.2.0**: record-aligned zstd **groups** (flush at 4 MiB uncompressed BLOB record bytes), a **final zstd frame of MANIFEST + END only**, an uncompressed **TOC** (`AYZPTOC2`), and trailer `toc_len` in bytes 56–63. `list()` seeks the last frame via the TOC. v1 files still read and rehydrate. `--jobs` remains hash-only. END digest is still BLAKE3 of concat(first-seen hashes).

Do **not** default to per-blob zstd frames.

---

## On-disk v2

```
[FileHeader version=2, magic AYZP{2}\0\0\0 derived from header.version]
[zstd frames: record-aligned BLOB groups, flush at 4 MiB uncompressed BLOB record bytes]
[final zstd frame: MANIFEST + END only]
[uncompressed TOC AYZPTOC2]
[64-byte trailer version=2; bytes 56-63 = toc_len u64le]
```

- `payload_bytes` = zstd-only (sum of frames).
- `file_len` = `header_total` + `payload_bytes` + `toc_len` + 64.
- Offsets in TOC are **payload-relative** (0 = first zstd byte after header).
- Never copy `Trailer.manifest_len` (uncompressed JSON) into TOC. Test those two lengths differ.

### TOC

```
"AYZPTOC2" n:u32le
n × { blake3[32], zstd_off:u64le, zstd_len:u64le, rec_off:u64le }
manifest_zstd_off:u64le  manifest_zstd_len:u64le
```

Encoded length = `28 + n*56`.

TOC fill: pending `{blake3, zstd_off, rec_off}` during a frame; back-fill `zstd_len` on `end_frame` **after** `Encoder::finish` + `BufWriter::flush`. Same for `manifest_*`.

### Empty pack

Close the blob frame before the manifest even if `n_blobs=0`, **or** document 1-frame empty v2 (MANIFEST+END only; no preceding empty blob frame). Do **not** append MANIFEST onto a partial blob frame.

This implementation: **1-frame empty v2**. `start_ayz_file` leaves the writer Idle. The first BLOB starts a blob frame. `finish` always ends any open blob frame, then starts a **new** frame for MANIFEST+END.

---

## AyzWriter

```
enum Idle(BufWriter<File>) | Frame(Encoder<'static, BufWriter<File>>)
```

Store `zstd_level`, pending rows, finished TOC rows, running `payload_bytes`. Next `zstd_off` = `payload_bytes`. `include_checksum(false)` on **every** Encoder.

Change `start_ayz_file` / `remember_blob` / `finish_ayz_file` / `write_ayz_file`. Keep `PendingAyz` + Windows replace.

Flush rule (record-aligned): if the current frame already has BLOB record bytes and adding the next BLOB record (`1+32+8+data.len()`) would exceed 4 MiB, `end_frame` then start a new frame. A single record larger than 4 MiB occupies its own frame.

`write_ayz_file` writes v2 when `header.version == 2` (default via `FileHeader::new`). Keep `write_ayz_file_v1` as a path that **actually writes version 1** (not v2 dehydrate labeled v1).

---

## Header / trailer

- Write magic `AYZP{ver}\0\0\0` from `header.version`. Accept write versions `{1,2}`.
- Read: `magic[0..4]==AYZP`, `magic[4]∈{1,2}`, `magic[5..8]==0`, `header.version == u32::from(magic[4]) == trailer.version`.
- Drop `magic == FILE_MAGIC` equality.
- Reject skew with a **dedicated error** (`VersionSkew`). A v1 file is still an ayzenpack file (`NotAyzenpack` is wrong).
- Move the “byte 2 unsupported” header test to **byte 3**.

Trailer v2: bytes 56–63 = `toc_len` u64le. v1: `toc_len = 0`.

---

## `toc_len` read invariant (checked-sub)

```
expected_toc = file_len - 64 - header_total - payload_bytes
trailer.toc_len == expected_toc
```

- v1: `toc_len == 0`
- v2: `toc_len == 28+n*56` and `>= 28`
- `header_total` = `12+header_len` **or** `stream_position` after `read_header`. Never raw `trailer.header_len` alone if that is wrong.
- Update `verify_finished_ayz`: `file_len == header_total + payload_bytes + toc_len + 64`.
- `format_corrupt`: truncated TOC, `toc_len` too big, v2 `toc_len=0`, v1 `toc_len≠0`.

---

## Readers

Shared helper for version agreement + `expected_toc` + `take(payload_bytes)`.

- v1: `.single_frame()` OK.
- v2: multi-frame, **no** `single_frame`.
- `read_ayz_file` and `spill_to_cas` (the **fourth** decoder at `src/rehydrate.rs` ~112–128, **not** a `read_ayz_file` caller) both use the helper and decode **all** blob+manifest frames.
- `list()`: v1 full decode; v2 TOC seek `header_total+manifest_zstd_off`, decode last frame only, then `read_records` / `manifest_from_records` (frame is `REC_MANIFEST`+json+`REC_END`, not raw JSON).

---

## Tests

Keep the #30 suite. Add:

- v2 last frame is MANIFEST+END only
- `list` last-frame (corrupt first blob frame; `list` still works)
- v1 still rehydrates (`write_ayz_file_v1`)
- mix `output_len <= 569539 * 115 / 100`
- synthetic two blob frames
- `manifest_zstd_len != trailer.manifest_len`
- `toc_len` corrupt (truncated / too big / v2=0 / v1≠0)
- magic / JSON / trailer skew
- payload-relative origin (`zstd_off == 0` for the first blob)
- `record.rs` one-frame test: two small blobs share a frame (still forbid per-blob default)
- Keep corpus mix + whole-file hash. Log `unique_blob_count`.

---

## Docs

`DESIGN.md`, `README.md`, `docs/library.md`, `tests/docs.rs`. Crate version 0.2.0. Manifest schema stays v1.

---

## Done

PR open (not merged, not tagged). `cargo test` green. `clippy -D warnings`. PR body has sweep counts and mix size vs 569539.
