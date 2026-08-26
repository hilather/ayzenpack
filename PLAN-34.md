# PLAN: stop claiming ZIP rebuild breaks the JAR signature (#34)

Base: `main` at crate **0.2.1** / format v2 (`0751510`). Branch `cursor/signed-jar-warning-0a05`.
Do **not** merge. Do **not** tag. Do **not** bump the crate version. Do **not** touch 0.2.2 `raw_zip` / `slice_zip` work. Do **not** overwrite `PLAN.md` (0.2.1 single-CAS lock; `tests/docs.rs` guards it).

Origin `matt-brewer/agent-skills` is not reachable (`origin` CLI unauthenticated; clone of `https://origin.cursor.com/matt-brewer/agent-skills.git` failed). Same fallback as 0.2.0 / 0.2.1: fresh adversarial Task subagents. **Never skip sweep 1.** Fresh skeptic each sweep. Cap 3, then BLOCKED.

Constraints: MSRV 1.80, `forbid(unsafe_code)`, no edition-2024 deps, no format bump, no JSON field renames, no `cdata_blob` / `raw_zip` writer changes.

---

## Repro (captured on current main, required before this plan)

On `0751510`, `src/dehydrate.rs` after `attach_exact`:

```
if jar.signed && !jar.exact_restore() {
    warn(..., "signed JAR {} (rebuild will break the signature)");
} else if jar.signed {
    warn(..., "signed JAR {}");
}
```

`--fail-on-signed` still returns earlier (`scanned.signed`) with `AyzenpackError::Usage("signed JAR {jar_name}")`. `warn()` never consults `opts.strict`.

Fixture: stored-block DEFLATE + `META-INF/FOO.SF` (same shape as `tests/roundtrip.rs` `signed_rebuild_is_not_exact_restore`). Binary: `/workspace/target/debug/ayzenpack` built from that main.

| Command | Exit | Observed stderr |
|---------|------|-----------------|
| `dehydrate -o rebuild.ayz signed-miss.jar` | 0 | `ayzenpack: warning: signed JAR signed-miss.jar (rebuild will break the signature)` |
| `dehydrate --fail-on-signed -o fail.ayz signed-miss.jar` | 1 | `ayzenpack: signed JAR signed-miss.jar` (no extra clause; Usage abort) |
| `dehydrate --strict -o strict.ayz signed-miss.jar` | 0 | same extra-clause **warning**; pack succeeds (strict does **not** promote) |
| `dehydrate -o signed-py.ayz signed.jar` (zipfile-deflated / codec-hit) | 0 | `ayzenpack: warning: signed JAR signed.jar` (no extra clause) |

If that extra clause had not appeared, stop. It did.

---

## Why the extra clause is wrong

`jarsigner` / `META-INF/*.SF` + `*.RSA`/`*.DSA`/`*.EC` digest **uncompressed** entry bytes (via `MANIFEST.MF`), not the deflate stream. Rebuild keeps names, CD order, and those bytes. The signature block should still verify.

What **does** change on rebuild is the whole-file hash (`source_blake3` / `source_sha256` of the `.jar`). That is expected (AGENTS.md / DESIGN reconstruction). It is not a broken JAR signature.

`DESIGN.md` Signed JARs currently says `.SF` “digest compressed or stored bytes”. That line is false. Fix it with this change.

---

## Locked expected behavior (Matt)

- **KEEP** a warning that the JAR is signed, for **both** exact and rebuild: `signed JAR <name>`
- **DROP** `(rebuild will break the signature)` in **all** cases
- `--fail-on-signed` stays an explicit abort (`signed JAR {name}`)
- `--strict` still does **not** promote the signed notice
- Not this issue: silent signed JARs, re-signing, Java-zlib bit-identical hashes, restoring leftover `cdata_blob`

---

## Code (smallest change)

`src/dehydrate.rs` only, the signed-notice branch after `attach_exact` (today ~586–593). Fold into one warn:

```
if jar.signed {
    warn(opts, &format!("signed JAR {}", jar.name));
}
```

Do **not** change:

- the earlier `fail_on_signed` Usage return (`signed JAR {jar_name}`)
- `warn()` / `opts.strict` / `opts.quiet` / `opts.json_logs`
- `looks_signed`, `attach_exact`, `exact_restore`, `raw_zip`, writers, format, crate version

`json_logs` reuses the same `msg` string; one format covers text and JSON.

---

## Docs

Keep `PLAN.md` (0.2.1) and `AGENTS.md` as-is.

**`DESIGN.md`**

- Reconstruction “Otherwise rebuild” bullet: drop “Signed JARs on this path use the existing “rebuild will break the signature” warning.” Keep `source_*` may change.
- Signed JARs section: replace the compressed/stored-bytes claim. State that `.SF` / MANIFEST digest **uncompressed** entry bytes, not the deflate stream. Rebuild keeps those bytes, so jarsigner should still verify. Whole-file `source_*` may change. Still warn `signed JAR <name>` for exact and rebuild. `--fail-on-signed` aborts. `--strict` does not promote. No re-sign. Do not store `cdata_blob` / `raw_zip` of a healthy jar to keep a file hash.
- Security table “Signed JAR silently broken”: replace the cell so it does **not** say “splice may keep bytes; rebuild will not” (that is the old compressed-bytes model). Detect + warn `signed JAR <name>`; jarsigner should still verify after rebuild; keep `--fail-on-signed`. Title may stay.

**`README.md` Signed JARs**

- Drop “Rebuild changes compressed sizes and will break them.”
- Drop “Content-mode rebuild of an old archive can still break a signature.”
- Do **not** leave splice as the only “signatures survive” story. State the DESIGN fact: `.SF` / MANIFEST digest **uncompressed** entry bytes; rebuild keeps those bytes; whole-file `source_*` may change. Still warn; `--fail-on-signed`; `--strict` does not promote; no second payload copy; no re-sign.

`docs/library.md` only documents `--fail-on-signed` / `fail_on_signed`. Leave it unless a sentence repeats the lie (it does not today).

---

## Tests

1. **Regression (required):** CLI dehydrate of the stored-block signed rebuild fixture (`write_stored_block_deflate_zip(..., "META-INF/FOO.SF", ...)`).
   - success
   - stderr contains `signed JAR signed-miss.jar` (or the basename used)
   - stderr does **not** contain `rebuild will break the signature`
2. **`--fail-on-signed`:** still failure / exit 1 / stderr contains `signed JAR <name>` and not the extra clause.
3. **`--strict`:** still success; stderr still contains `signed JAR <name>`; not an error.
4. Keep existing `fail_on_signed_exits_error` (strict packs; flag aborts). On the existing no-flag CLI success of `write_signed_jar` (exact/codec-hit), **require** stderr contains `signed JAR` and does **not** contain `rebuild will break the signature`.
5. `signed_rebuild_is_not_exact_restore` **keeps** `!exact_restore()`. Only the comment that names “rebuild-breaks-signature warning path” changes.
6. `tests/docs.rs`:
   - DESIGN + README must **not** contain `rebuild will break the signature`
   - DESIGN must **not** claim `.SF` digests “compressed or stored bytes”
   - README must **not** contain “will break them” or “can still break a signature”
   - DESIGN must state uncompressed entry bytes (via MANIFEST) for `.SF`

Do not add jarsigner / JDK verification. Do not require whole-file hash match on rebuild.

---

## Out of scope

- Silent signed JARs
- Re-signing
- Java-zlib / bit-identical `source_*`
- Restoring leftover `cdata_blob`
- `raw_zip` / `slice_zip` / crate 0.2.2
- Format bump, field renames, version bump
- Replacing `PLAN.md`

---

## Skeptic review (plan)

- Sweep 1: ACCEPT, no blockers. Folded five SHOULD items (README fact + wording lock, security-table cell, exact-path extra-clause reject, keep `!exact_restore()`).
- Sweep 2 / 3: pending.

---

## Done

- Repro captured (this file)
- Warning is `signed JAR <name>` only
- `--fail-on-signed` and `--strict` unchanged
- Docs no longer claim `.SF` digests compressed bytes or that rebuild breaks jarsigner
- PR against `main` with `Fixes #34`, not merged, not tagged
- `cargo test` green
