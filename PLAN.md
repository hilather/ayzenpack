# PLAN: `--restore-paths` (v0.1.8)

Base: current `main` at v0.1.7 (`72dd971`). New branch off main. Not stacked on PR 28.

Origin `agent-skills` (plan function + skeptic workflow) was not reachable from this environment (`origin` CLI is unauthenticated; no `agent-skills` checkout; no Origin repo in the environment). This file is the required written plan plus a skeptic review. Implementation starts only after the skeptic section.

## Goal

Same flag on `dehydrate` and `rehydrate`: `--restore-paths`.

- **Dehydrate** records per-JAR filesystem metadata on `jars[]` (optional keys).
- **Rehydrate** writes each JAR back to that recorded path, restoring mode and (on Unix, when permitted) owner.
- Default rehydrate (`--dir` + basename `name`) stays unchanged, including for packs that contain restore metadata.

## Manifest (v1, optional fields)

Keep `source_path` as today (the CLI/scan path). Add dedicated fields so list / `--dir` rehydrate still use unique basename `name`:

| Key | Type | When present |
|-----|------|----------------|
| `restore_path` | string | `--restore-paths` dehydrate |
| `restore_mode` | u32 | when mode could be read |
| `restore_uid` | u32 | Unix only, when metadata available |
| `restore_gid` | u32 | Unix only, when metadata available |

Serde: `#[serde(default, skip_serializing_if = "Option::is_none")]` — same as `prefix_blob`.

JSON field order on `jars[]` (struct declaration order):

`name`, `source_path`, …, `signed`, **`restore_path`, `restore_mode`, `restore_uid`, `restore_gid`**, `prefix_blob`, `prefix_size`, `tail_*`, `raw_zip_*`, `entries`.

`examples/tiny.manifest.json` unchanged (no new required keys). Schema `additionalProperties: false` lists the four keys. Crate version **0.1.8**. Manifest `version` stays 1.

## Dehydrate

`DehydrateOptions.restore_paths: bool` (default false). CLI `--restore-paths`.

When the flag is on, for each input JAR after scan (same `path` already opened):

1. `restore_path`: `fs::canonicalize(path)` if it succeeds; otherwise store `path` as given (lossy UTF-8, same as `source_path`).
2. `restore_mode`:
   - Unix: `MetadataExt::mode()` (full `st_mode`).
   - Windows: `0o444` if readonly, else `0o644`.
   - Omit if `metadata` fails.
3. Unix: `restore_uid` / `restore_gid` from `MetadataExt`. Omit on Windows and if metadata fails.

Always still populate unique basename `name` so `--dir` rehydrate of the same pack works.

Without the flag: omit all four keys.

## Rehydrate

`RehydrateOptions.restore_paths: bool` (default false).

CLI clap:

- `--dir` is `Option<PathBuf>` with `required_unless_present = "restore_paths"`.
- `--restore-paths` is a bool flag on both subcommands.

If both `--dir` and `--restore-paths` are passed, **`--restore-paths` wins**; `dir` is unused (do not `create_dir_all` it).

Without the flag: existing behavior (`create_dir_all(dir)`, dest = `dir.join(jar.name)`, `--overwrite` / `--clean` unchanged).

With the flag:

1. Do **not** require `--dir`.
2. Before writing any JAR, if **any** selected jar lacks `restore_path` (or it is empty): error `Usage` whose text includes `pack was not created with --restore-paths`.
3. If `restore_path` is present but not absolute (`Path::is_absolute`): error (canonicalize failed at pack time and a relative path was stored).
4. Reject NUL in `restore_path` (`UnsafePath`). Absolute paths may contain `..`; we do not rewrite them. Dest-path symlink write-through is handled separately.
5. Dest = recorded `restore_path`.
6. Create missing **parent** directories only. Newly created parents get mode `0755` (Unix `DirBuilder` + chmod so umask does not leave 0700). Do **not** chown parents. Do **not** chmod existing parents.
7. If dest exists:
   - symlink → `remove_file` (do not `File::create` through it).
   - regular file → `remove_file` then create (overwrite; also handles dest `0444`).
   - directory → error.
8. Do **not** require `--overwrite`. `--clean` / `--overwrite` do not apply to restore-path dests.
9. Write via the existing exact-restore or ZipWriter path. Keep `source_blake3` / `source_sha256` / `source_size` verify on exact jars.
10. Prefix `chmod 0755` runs **only when there is no recorded `restore_mode`**. Recorded mode wins.
11. After a successful write (+ exact verify):
    - `chmod` to `restore_mode` when present. Windows: `set_readonly` when `mode & 0o222 == 0`.
    - Unix: `std::os::unix::fs::chown` when uid/gid present. `EPERM` / `EACCES` → warning (`ayzenpack: warning: …`) and continue. Other chown errors fail.
12. `--only` still filters by `jar.name`.

## Windows

- Record/restore `restore_path` + readonly-mapped mode.
- Skip uid/gid on dehydrate and rehydrate.
- Tests that assert unix mode/owner are `#[cfg(unix)]`.

## Tests (`tests/restore_paths.rs` + clap help in `tests/cli.rs`)

- Dehydrate `--restore-paths` records abs path + mode (+ uid/gid on unix).
- Rehydrate `--restore-paths` writes to that path, overwrites, restores mode; unix uid/gid when the process is that user (same uid works without root; do not require root).
- Rehydrate `--restore-paths` on a pack without metadata errors clearly.
- Default rehydrate of a restore-paths pack still works via `--dir` + basename.
- Symlink dest is replaced; symlink target bytes unchanged.
- Missing parent dirs are created.
- `--dir` optional when the flag is set; `--dir` still required without it (clap exit 2).
- Both flags: dest is `restore_path`, not `--dir`.

`cargo test` and `cargo clippy --all-targets -- -D warnings`. No `unsafe`. MSRV 1.80.

## Docs

README + `docs/library.md` + YAML example mention the flag. `DESIGN.md` `tool_version` example follows `CARGO_PKG_VERSION` (0.1.8).

## Out of scope

- Manifest version bump.
- Changing `source_path` semantics when the flag is off.
- Jailing restore paths under `--dir`.
- Stacking on PR 28; tagging; merging.

---

## Skeptic review (self)

Origin skeptic workflow was not available. Review against the required failure modes.

### Destructive overwrite — **accepted, constrained**

`--restore-paths` rehydrate overwrites recorded paths without `--overwrite`. That is the product. Mitigations: flag is opt-in on **both** sides (old packs error instead of guessing `source_path`); default `--dir` rehydrate still refuses overwrite; dest directories are not replaced. Residual: a malicious/wrong pack can clobber `/etc/…` if the process can write there. No extra jail — that would break the feature.

### Symlink write-through — **must-fix in design, addressed**

`File::create` on a symlink writes through to the target. Plan: `symlink_metadata` + `remove_file` before create. Residual TOCTOU (symlink planted between unlink and create) is noted; v1 does not add `O_NOFOLLOW` (libc constant is OS-specific; no new deps; no `unsafe`). Test: dest symlink replaced, target untouched.

### Windows — **addressed**

No uid/gid. Mode is readonly mapping only. Unix-only tests behind `cfg(unix)`. `--dir` clap change is OS-agnostic.

### Missing metadata — **addressed**

Any jar without `restore_path` → hard error with the specified phrase. Partial metadata (path but no mode/owner) still writes bytes; chmod/chown only when keys exist. Prefix 0755 remains the fallback when mode is absent.

### Path traversal — **addressed with a deliberate hole**

Relative `restore_path` is rejected at rehydrate (covers failed canonicalize). NUL is rejected. `..` in an **absolute** path is allowed (canonical paths should not contain it; crafted packs might). We do not follow the dest symlink. Parent symlinks are followed (normal directory resolution; spec only forbids dest-file write-through).

### Other blockers considered

- **`create_dir_all(&opts.dir)`** at the top of `rehydrate` would create a junk directory when `--dir` is omitted. Must skip when `restore_paths`.
- **Readonly dest**: `File::create` can fail on `0444`. Plan unlinks first.
- **Umask vs parent 0755**: mkdir honors umask; plan chmod’s newly created parents to 0755.
- **Exact-restore verify vs chmod**: apply recorded mode after write/verify so a hash failure is not masked; skip prefix 0755 when `restore_mode` is set so 0644 Spring jars stay 0644.
- **Field-order / tiny example**: optional keys omitted by default; existing compact-order tests keep passing; add an order test when the new keys are `Some`.
- **PR 28**: branch from `main` only.

**Verdict: accept.** No reject-level holes left in the plan. Implement.
