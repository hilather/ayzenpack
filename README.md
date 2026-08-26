# ayzenpack

Dehydrate many JARs into one zstd content-addressed archive and rehydrate them.

Archive files use magic `AYZP` and the recommended extension `.ayz`.

## Install

From a clone of this repository:

```text
cargo install --path .
```

Requires Rust 1.80 or later.

## Usage

```text
ayzenpack dehydrate -o libs.ayz app.jar lib/*.jar
ayzenpack rehydrate -i libs.ayz -d restored/
```

Aliases: `pack` = `dehydrate`, `unpack` = `rehydrate`. Also `list` and `verify`.

```text
ayzenpack pack -o libs.ayz --sort-inputs --recursive vendor/
ayzenpack unpack -i libs.ayz -d restored/ --overwrite
ayzenpack list -i libs.ayz
ayzenpack verify -i libs.ayz
```

`dehydrate` (alias `pack`) writes a `.ayz` archive and overwrites `-o` if it exists.

Progress (per JAR, by entry count) and the final stats line go to **stderr**. `-q` / `--quiet` disables progress. stdout stays empty on success so the binary is pipe-safe.

```text
ayzenpack: 12 jars, 8401 entries, 912 unique blobs, 148.2 MiB → 41.7 MiB unique, zstd 18.4 MiB (0.124 of jar bytes)
```

`--json-logs` writes one JSON object per event on stderr (not stdout).

## Reconstruction guarantee

Rehydrate restores **functional identity**, not ZIP bit-identity.

Guaranteed:

- Uncompressed bytes of every file entry match the source.
- Entry names and central-directory order match (Unicode names from the ZIP).
- CRC-32 of uncompressed bytes matches the source header CRC.
- Valid DOS last-modified times are preserved. Invalid pairs, including the common JAR `0,0`, fall back to 1980-01-01 rather than aborting.

Not guaranteed (rebuilt JAR bytes need not equal source JAR bytes):

- Deflate bitstream
- Extra fields (dropped in v1; Android zipalign / alignment is not preserved)
- Data descriptors, GPBF bit 11, raw name encoding

`--verbatim` is **not** in v1. There is no flag to request bit-identical ZIP reconstruction.

Rebuilt JARs use deflate for file entries and store for directories, unless `--store-all`.

## Signed JARs

Rebuild **will not verify signatures**. `META-INF/*.SF` plus `*.RSA` / `*.DSA` / `*.EC` digest compressed or stored bytes; rewriting DEFLATE invalidates those signatures. `ayzenpack` does not re-sign.

`dehydrate` warns (listing jar names) and still packs. Pass `--fail-on-signed` to abort instead. `--strict` does not promote the signed notice by itself.

## Archive and manifest

The container is one file: an uncompressed header (`AYZP` + version 1), one zstd frame of length-prefixed BLOB / MANIFEST / END records, and an uncompressed 64-byte trailer (`AYZPTLR1`).

Dedup key is **BLAKE3** of uncompressed entry bytes. SHA-256 of the same bytes is recorded for integrity, never used as the CAS key.

The archive MANIFEST is compact JSON with `"format": "ayzenpack-manifest"`. See `schemas/manifest.v1.schema.json` and `examples/tiny.manifest.json`. Field names in those files are the v1 contract; keep them in lockstep.

## Commands

Global flags: `-q` / `--quiet` (no stderr progress), `-v` / `--verbose`, `--json-logs`.

### dehydrate / pack

```text
ayzenpack dehydrate -o <OUT> [OPTIONS] <INPUTS>...
```

| Flag | Meaning |
|------|---------|
| `-o, --output` | required output path (typically `*.ayz`). Overwrites if it exists. |
| `-r, --recursive` | if an input is a directory, add `*.jar,*.zip,*.war,*.ear` (case-insensitive) |
| `--sort-inputs` | sort input paths for deterministic archives |
| `--level <1-19>` | zstd level, default **3** |
| `--strict` | warnings → errors (does not promote the signed-JAR notice) |
| `--fail-on-signed` | error if a JAR looks signed |
| `--dry-run` | stats only; write nothing |
| `--exclude <GLOB>` | repeatable; matches CLI path or basename (`*` does not cross `/`) |
| `--jobs <N>` | hash workers; default **1** (sequential). `0` = available parallelism |
| `--max-inflight-bytes` | cap on uncompressed entry buffers in the hash pipeline, default **64 MiB** |

`--jobs` hashes in parallel; BLOB records stay in first-seen (scan) order so `--sort-inputs` archives are byte-identical at any `--jobs`.

Shell-expanded globs are the caller’s job. Directories are not recursed unless `--recursive`. Duplicate basenames become `a.jar`, `a__2.jar`, `a__3.jar`.

### rehydrate / unpack

```text
ayzenpack rehydrate -i <ARCHIVE> -d <DIR> [OPTIONS]
```

| Flag | Meaning |
|------|---------|
| `-i, --input` | required `.ayz` |
| `-d, --dir` | required output directory (created) |
| `--store-all` | write ZIP entries stored (no deflate) |
| `--overwrite` | default: fail if the target JAR exists |
| `--only <NAME>` | repeatable; only those jar `name`s |

### list and verify

```text
ayzenpack list -i libs.ayz
ayzenpack list -i libs.ayz --json
ayzenpack verify -i libs.ayz
```

`list` prints a table (name, entries, signed, size). `--json` prints the full pretty MANIFEST on stdout.

`verify` re-hashes blobs and checks the manifest. Integrity mismatches exit 3; unreadable / not-an-archive errors exit 1.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Dual license: MIT OR Apache-2.0.
