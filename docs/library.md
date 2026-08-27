# Library quick start

ayzenpack is a Rust library. The `ayzenpack` binary is clap over four functions:

```rust
pub fn dehydrate(opts: &DehydrateOptions) -> Result<DehydrateSummary>;
pub fn rehydrate(opts: &RehydrateOptions) -> Result<()>;
pub fn list(input: &Path) -> Result<Manifest>;
pub fn verify(input: &Path) -> Result<()>;
```

No clap in the lib. No process-global flags. Quiet / verbose / json-logs are fields on the options structs — copy them in from your own CLI, or leave the defaults.

`#![forbid(unsafe_code)]` is on `lib.rs`.

Restore policy (see [DESIGN.md](https://github.com/hilather/ayzenpack/blob/main/DESIGN.md)): Priorities: (1) lean pack (2) complete rehydrate (3) class-level dedup. `source_*` must match iff `bit_identical_restore`. Outer exact is a file seek-walk (no outer `Vec`). Leftover junk after N complete CD records with `N == ZipArchive::len()` is homemade_ok + `tail_blob` (exact when every slot hits). Remaining homemade-`None` never gets `tail_blob`. Arm 1 homemade-`None` with captured headers is stencil seek + synthetic CD (FileAbs iff `prefix_size > 0`; locals-region identity; original-file `source_*` not required). Arm 2 csize-changing skip-exact is concat + synthetic CD. Equal-offset last-wins with matching homemade count is exact splice (pad of the unreferenced second local kept; unique content 1). Arm 3 ZipWriter STOREs `method_code == 0` / `zip_index` over uncompressed payload (`read_entry_content` / `reconstruct_child_zip`); never `resolve_cdata` (range overlap / ZipArchive count mismatch). Prefix+hole **(A)** `[non-PK prefix][hole][first CD local]` is already `prefix_blob` covering bash+hole (`find_cd_first_local`); not skip-exact. Prefix+hole **(B)** `prefix_len > 0 && min(zip_rel) != 0` after convert is a dead defensive `Err`; keep it; do not call (B) absorbed. PK-start hole is `leading_pad_blob` + arm 1 (do not extend `prefix_len`). Listing oracle: prefixed synthetic-CD dest is `ZipArchive::new(File)` (outer names); prefixed arm 3 dest is FileAbs `ZipArchive::new(File)` vs source `scan_jar`; prefixed source is `scan_jar` / `ZipView`; do not rewrite mix `assert_functional_identity`. Nested STORE is `zip_index` + shared class blobs — never CAS `blake3(inner zip)`. Closed codec set: STORE; `deflate-raw:zlib:{1,3,6,9}`; `deflate-raw:flate2:{1,3,6,9}`; `deflate-raw:stored`. A miss rebuilds that slot only. Do not store `cdata_blob`. Do not chase bit-identical hashes on a miss. Corpus lucene/jackson `source_*` stays gated on `AYZENPACK_CORPUS_DIR` until every method-8 slot is a measured hit (100% `miss=0` / `exact=true`); enablement is [`ci/download-corpus.sh`](https://github.com/hilather/ayzenpack/blob/main/ci/download-corpus.sh) then `cargo test --test corpus corpus_lucene_jackson_source_identity_only_when_every_slot_hits`. Mix gates (`cdata_blob == 0`, `output_len <= 569539 * 115 / 100`) stay.

---

## Pack and restore

```rust
use std::path::PathBuf;
use ayzenpack::{dehydrate, list, rehydrate, verify, DehydrateOptions, RehydrateOptions};

fn main() -> ayzenpack::Result<()> {
    let summary = dehydrate(&DehydrateOptions {
        output: PathBuf::from("libs.ayz"),
        inputs: vec![PathBuf::from("app.jar"), PathBuf::from("lib")],
        recursive: true,
        sort_inputs: true,
        level: 3,
        jobs: 0,
        fail_on_signed: true,
        exclude: vec!["*.sources.jar".into(), "*.javadoc.jar".into()],
        write_sidecar_manifest: Some(PathBuf::from("libs.ayz.manifest.json")),
        pretty_manifest: true,
        ..DehydrateOptions::default()
    })?;

    eprintln!(
        "{} jars, {} unique blobs, ratio {:.3}",
        summary.jar_count, summary.unique_blob_count, summary.dedup_ratio
    );

    verify("libs.ayz".as_ref())?;

    let manifest = list("libs.ayz".as_ref())?;
    assert_eq!(manifest.format, "ayzenpack-manifest");

    rehydrate(&RehydrateOptions {
        input: PathBuf::from("libs.ayz"),
        dir: PathBuf::from("restored"),
        overwrite: true,
        ..RehydrateOptions::default()
    })
}
```

`Cargo.toml`:

```toml
[dependencies]
ayzenpack = { git = "https://github.com/hilather/ayzenpack" }
```

---

## `DehydrateOptions`

`Default` is the same as the CLI defaults. Set only what you care about.

| Field | Default | Notes |
|-------|---------|--------|
| `output` | empty (required) | overwritten if the file exists |
| `inputs` | `[]` (required) | files, or directories when `recursive` |
| `recursive` | `false` | `*.jar,*.zip,*.war,*.ear`, case-insensitive |
| `sort_inputs` | `false` | also forces header `created_unix` to `0` |
| `level` | `3` | zstd 1..=19 |
| `max_entry_bytes` | `2_147_483_647` | zip-bomb cap |
| `strict` | `false` | warnings become errors; does **not** promote signed-JAR |
| `fail_on_signed` | `false` | abort if `META-INF/*.SF` + `*.RSA`/`*.DSA`/`*.EC` |
| `dry_run` | `false` | hash and count; write nothing |
| `write_sidecar_manifest` | `None` | extra JSON next to the archive |
| `pretty_manifest` | `false` | pretty-print **sidecar only**; archive MANIFEST is always compact |
| `follow_symlinks` | `false` | walkdir `follow_links` |
| `exclude` | `[]` | glob 0.3; match CLI path **or** basename; `*` does not cross `/` |
| `quiet` / `verbose` / `json_logs` | `false` | stderr behaviour |
| `jobs` | `1` | `0` = `available_parallelism` |
| `max_inflight_bytes` | 64 MiB | uncompressed buffers in the hash pipeline |
| `restore_paths` | `false` | record `restore_path` / mode / uid / gid on each jar |

`DehydrateSummary` mirrors manifest `stats`, plus `output_len` and `signed_jars`.

---

## `RehydrateOptions`

| Field | Default | Notes |
|-------|---------|--------|
| `input` | empty (required) | `.ayz` |
| `dir` | empty (required unless `restore_paths`) | created if missing; unused when `restore_paths` |
| `cas_dir` | `None` | tempfile, deleted on success |
| `keep_cas` | `false` | keep that tempfile |
| `store_all` | `false` | ZIP STORE instead of DEFLATE. Skip-exact already STOREs `method_code == 0` / `zip_index` without this flag; method-8 files DEFLATE at `deflate_level`. `source_*` may change. |
| `deflate_level` | `6` | 0..=9 |
| `clean` | `false` | unlink dest names we will write (not the whole dir) |
| `overwrite` | `false` | refuse to clobber existing JARs (ignored when `restore_paths`) |
| `only` | `[]` | jar `name`s from the manifest |
| `restore_paths` | `false` | write to recorded `restore_path`; `--dir` unused. Sibling tmp then `replace_file`; dest is not unlinked first |

---

## Load a YAML job file

There is no YAML parser inside ayzenpack. Job state is yours: a file, a CI secret, a struct. Deserialize it, then fill the options.

Starter: [`examples/ayzenpack.yaml`](https://github.com/hilather/ayzenpack/blob/main/examples/ayzenpack.yaml).

```toml
# your binary / build tool
[dependencies]
ayzenpack = { git = "https://github.com/hilather/ayzenpack" }
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"
```

```rust
use std::path::{Path, PathBuf};
use ayzenpack::{dehydrate, rehydrate, DehydrateOptions, RehydrateOptions};
use serde::Deserialize;

#[derive(Deserialize)]
struct JobFile {
    dehydrate: DehydrateJob,
    rehydrate: Option<RehydrateJob>,
}

#[derive(Deserialize)]
struct DehydrateJob {
    output: PathBuf,
    inputs: Vec<PathBuf>,
    #[serde(default)]
    recursive: bool,
    #[serde(default)]
    sort_inputs: bool,
    #[serde(default = "default_level")]
    level: i32,
    #[serde(default)]
    jobs: usize,
    #[serde(default)]
    fail_on_signed: bool,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    write_sidecar_manifest: Option<PathBuf>,
    #[serde(default)]
    pretty_manifest: bool,
}

#[derive(Deserialize)]
struct RehydrateJob {
    input: PathBuf,
    dir: PathBuf,
    #[serde(default)]
    overwrite: bool,
}

fn default_level() -> i32 { 3 }

fn load_dehydrate(path: &Path) -> anyhow::Result<DehydrateOptions> {
    let job: JobFile = serde_yaml::from_str(&std::fs::read_to_string(path)?)?;
    let d = job.dehydrate;
    Ok(DehydrateOptions {
        output: d.output,
        inputs: d.inputs,
        recursive: d.recursive,
        sort_inputs: d.sort_inputs,
        level: d.level,
        jobs: d.jobs,
        fail_on_signed: d.fail_on_signed,
        exclude: d.exclude,
        write_sidecar_manifest: d.write_sidecar_manifest,
        pretty_manifest: d.pretty_manifest,
        ..DehydrateOptions::default()
    })
}

fn run_job(path: &Path) -> anyhow::Result<()> {
    let job: JobFile = serde_yaml::from_str(&std::fs::read_to_string(path)?)?;
    dehydrate(&load_dehydrate(path)?)?;
    if let Some(r) = job.rehydrate {
        rehydrate(&RehydrateOptions {
            input: r.input,
            dir: r.dir,
            overwrite: r.overwrite,
            ..RehydrateOptions::default()
        })?;
    }
    Ok(())
}
```

`list()` is the other “state load”: it reads the MANIFEST out of the archive (v2: last zstd frame via the TOC; v1: full payload decode). Use that when the catalog *is* the state, not a YAML file you authored.

```rust
let manifest = ayzenpack::list(std::path::Path::new("libs.ayz"))?;
for jar in &manifest.jars {
    println!("{}  {} entries  signed={}", jar.name, jar.entries.len(), jar.signed);
}
```

---

## GitHub Actions

Same options, as workflow YAML rather than a job file:

```yaml
name: pack-classpath
on:
  push:
    paths: ["vendor/**", "app.jar"]

jobs:
  pack:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install --path .
      - name: Dehydrate
        run: |
          ayzenpack dehydrate -o dist/libs.ayz \
            --sort-inputs --recursive --jobs 0 \
            --fail-on-signed \
            --exclude '*.sources.jar' \
            --write-sidecar-manifest dist/libs.ayz.manifest.json \
            --pretty-manifest \
            vendor/ app.jar
      - run: ayzenpack verify -i dist/libs.ayz
      - uses: actions/upload-artifact@v4
        with:
          name: libs.ayz
          path: dist/libs.ayz
```

Rocky RPM install in CI is the other path — see [`.github/workflows/packages.yml`](https://github.com/hilather/ayzenpack/blob/main/.github/workflows/packages.yml).

---

## Errors and exit codes

Library: `AyzenpackError` + `Result<T>`. Operational errors that involve a file include the path.

The **binary** maps those to:

| Code | When |
|------|------|
| 0 | ok |
| 1 | runtime error; also integrity failures on non-`verify` commands |
| 2 | clap usage |
| 3 | `verify` only: blob / SHA-256 / CRC / END mismatch |

I/O, `NotAyzenpack`, truncated trailer, and JSON parse on `verify` stay **1**.
