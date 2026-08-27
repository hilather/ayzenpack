//! README and schema identity guards (PR-13).

const README: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"));
const SCHEMA: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/schemas/manifest.v1.schema.json"
));
const EXAMPLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/tiny.manifest.json"
));
const AGENTS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/AGENTS.md"));
const DESIGN: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/DESIGN.md"));
const PLAN: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/PLAN.md"));
const LIBRARY: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/library.md"));

#[test]
fn readme_contains_ayzenpack_dehydrate_example() {
    // Guards docs still saying jded / .jded, or dropping the two-command example.
    assert!(
        README.contains("ayzenpack dehydrate"),
        "README must show `ayzenpack dehydrate`"
    );
    assert!(
        README.contains("ayzenpack dehydrate -o libs.ayz app.jar lib/*.jar"),
        "README must include the dehydrate example"
    );
    assert!(
        README.contains("ayzenpack rehydrate -i libs.ayz -d restored/"),
        "README must include the rehydrate example"
    );
    assert!(README.contains("AYZP"), "README must mention magic AYZP");
    assert!(
        README.contains(".ayz"),
        "README must mention the .ayz extension"
    );
    assert!(
        README.contains("cargo install --path ."),
        "README must document install"
    );
    assert!(
        README.contains("pack") && README.contains("unpack"),
        "README must document pack/unpack aliases"
    );
    assert!(
        README.contains("--fail-on-signed"),
        "README must document --fail-on-signed"
    );
    assert!(
        README.contains("--verbatim"),
        "README must say --verbatim is not in v1"
    );
    assert!(
        README.contains("MIT OR Apache-2.0") || README.contains("MIT license"),
        "README must document dual license"
    );
    assert!(!README.contains("jded"), "README must not say jded");
    assert!(!README.contains(".jded"), "README must not say .jded");
    assert!(
        README.contains("Rocky Linux 8") && README.contains("Rocky Linux 9"),
        "README must document Rocky 8 and 9 packages"
    );
}

#[test]
fn schema_const_format_is_ayzenpack_manifest() {
    // Guards schema/example identity drift and jded-manifest regression.
    assert!(
        SCHEMA.contains("\"const\": \"ayzenpack-manifest\""),
        "schema format const must be ayzenpack-manifest"
    );
    assert!(
        EXAMPLE.contains("\"format\": \"ayzenpack-manifest\""),
        "example format must be ayzenpack-manifest"
    );
    assert!(!SCHEMA.contains("jded"), "schema must not say jded");
    assert!(!EXAMPLE.contains("jded"), "example must not say jded");
    assert!(!SCHEMA.contains("jded-manifest"));
    assert!(!EXAMPLE.contains("jded-manifest"));
}

#[test]
fn agents_md_locks_single_cas_and_zstd_blocks() {
    // Full sentences, not keyword soup. An inverted stub must fail.
    assert!(
        AGENTS.contains("Storage efficiency is of the utmost importance"),
        "AGENTS.md must say storage efficiency is of the utmost importance"
    );
    assert!(
        AGENTS.contains(
            "Dedup key is BLAKE3 of **uncompressed** entry bytes (same class across JARs is one blob)"
        ),
        "AGENTS.md must require one CAS blob per unique uncompressed payload"
    );
    assert!(
        AGENTS.contains("**Never** store a second encoding of the same entry"),
        "AGENTS.md must forbid a second encoding of the same entry"
    );
    assert!(
        AGENTS.contains("No default `cdata_blob` next to the content blob"),
        "AGENTS.md must forbid default cdata_blob beside the content blob"
    );
    assert!(
        AGENTS.contains(
            "record-aligned zstd **groups** flushing at 4 MiB of uncompressed BLOB **record** bytes"
        ),
        "AGENTS.md must require zstd in 4 MiB record-aligned groups"
    );
    assert!(
        AGENTS.contains("Do **not** switch to per-file/per-blob frames"),
        "AGENTS.md must forbid per-file zstd frames"
    );
    assert!(
        AGENTS.contains(
            "The manifest is a ZIP-slot index (ratarmount-style pointers), not a second copy of file bytes"
        ),
        "AGENTS.md must say the manifest is a ZIP-slot index, not a second copy"
    );
    assert!(
        AGENTS.contains("Do not add a Java subprocess / vendor `Deflater`")
            && AGENTS.contains("do not add `cdata_blob` for misses")
            && AGENTS.contains("do not `raw_zip` a listed jar")
            && AGENTS.contains("In-process zlib-rs raw-deflate hits are the 0.2.4 path"),
        "AGENTS.md must forbid Java subprocess / cdata_blob-for-misses / raw_zip of a listed jar; zlib-rs is the 0.2.4 path"
    );
    assert!(
        AGENTS.contains("but **never write** that shape again"),
        "AGENTS.md must say never write legacy dual-copy again"
    );
    assert!(
        AGENTS.contains("**never writes** `cdata_blob` on STORE/DEFLATE (file or dir, any method)"),
        "AGENTS.md must say 0.2.1 never writes leftover cdata_blob"
    );
    assert!(
        AGENTS.contains("Do not add new `cdata_blob` puts"),
        "AGENTS.md must forbid adding more cdata_blob writes"
    );
    assert!(
        AGENTS.contains("569539 * 115 / 100")
            && AGENTS.contains("`cdata_blob == 0` on every mix entry"),
        "AGENTS.md must keep the mix size gate and explicit cdata_blob == 0"
    );
    assert!(
        AGENTS.contains("MSRV is **1.80**") && AGENTS.contains("`forbid(unsafe_code)`"),
        "AGENTS.md must state MSRV 1.80 and forbid(unsafe_code)"
    );
    assert!(
        DESIGN.contains("North star: **one CAS blob + ZIP index + zstd blocks**"),
        "DESIGN.md storage north star must be index + single CAS + zstd blocks"
    );
    assert!(
        !DESIGN.contains("New packs default to **metadata-only** exact restore"),
        "DESIGN.md must not treat metadata-only exact as the north star"
    );
    assert!(
        PLAN.contains("# PLAN: 0.2.4 stencil restore (ratarmount zip_index + codec recipes)")
            && PLAN.contains("ratarmount zip_index + codec recipes")
            && PLAN.contains("first local is not at ZIP offset 0"),
        "PLAN.md must be the 0.2.4 stencil restore plan"
    );
    assert!(
        !README.contains("## Reconstruction guarantee")
            && !README.contains("restore **bit-identical** files")
            && !README.contains("bit-identical restore is the guarantee"),
        "README must not tell agents that bit-identical restore is the guarantee"
    );
    assert!(
        README.contains("Crate **0.2.1** never writes `cdata_blob` (file or dir, any method)"),
        "README must say 0.2.1 never writes leftover cdata_blob"
    );
}

#[test]
fn docs_lock_leftover_junk_hash_policy_and_class_dedup() {
    // Full sentences, not keyword soup. An inverted stub must fail.
    assert!(
        DESIGN.contains("**Priorities (in order):**")
            && DESIGN.contains("1. **Lean pack.**")
            && DESIGN.contains("2. **Complete rehydrate.**")
            && DESIGN.contains("3. **Class-level dedup.**"),
        "DESIGN.md must list priorities 1/2/3: lean pack, complete rehydrate, class-level dedup"
    );
    assert!(
        DESIGN.contains("Never CAS `blake3(inner zip)` when the slot is `zip_index`"),
        "DESIGN.md must forbid CAS of blake3(inner zip) on zip_index"
    );
    assert!(
        DESIGN.contains(
            "**Leftover-junk CD:** N complete CD records + trailing junk with `N == ZipArchive::len()` is homemade_ok + `tail_blob`"
        ),
        "DESIGN.md must document leftover-junk as homemade_ok + tail_blob"
    );
    assert!(
        DESIGN.contains(
            "Remaining homemade-`None` (true parse failure, truncated/malformed CD) **never** gets `tail_blob`"
        ) && DESIGN.contains("Never attach tail while homemade parse is `None`")
            && DESIGN.contains(
                "Range overlap, ZipArchive count mismatch, and slice `Err` are other skip-exact reasons"
            )
            && DESIGN.contains(
                "Equal-offset last-wins with matching homemade count is exact splice"
            )
            && !DESIGN.contains(
                "Remaining homemade-`None` (true parse failure, truncated/malformed CD, overlap, prefix+hole)"
            )
            && !DESIGN.contains("Overlap, prefix+hole, and slice `Err` are other skip-exact reasons")
            && !DESIGN.contains("prefix+hole stays skip-exact"),
        "DESIGN.md must say remaining homemade-None never gets tail_blob; range overlap/count mismatch are other skip-exact; equal-offset last-wins is exact splice; prefix+hole is not skip-exact (A)"
    );
    assert!(
        DESIGN.contains(
            "∀ jar (bit_identical_restore: STORE splice / codec-hit / leftover-junk exact / legacy cdata_blob / raw_zip):"
        ) && !DESIGN.contains("/ zip_index / leftover-junk exact"),
        "DESIGN.md forall must require source_* iff bit_identical_restore, not on skip-exact zip_index"
    );
    assert!(
        DESIGN.contains("Outer exact (`write_exact_jar`) is a **file seek-walk**")
            && DESIGN.contains("Arm 1 homemade-`None` with captured local headers")
            && DESIGN.contains("stencil seek + synthetic CD")
            && !DESIGN.contains("Synthetic CD is parked"),
        "DESIGN.md must say outer exact is a file seek-walk and arm 1 is stencil seek + synthetic CD"
    );
    assert!(
        DESIGN.contains("### Restore hash policy")
            && DESIGN.contains("`source_*` **must** match iff `Jar::bit_identical_restore()`"),
        "DESIGN.md must document restore hash policy: match iff bit_identical_restore"
    );
    assert!(
        DESIGN.contains("Corpus lucene/jackson `source_*` stays gated")
            && DESIGN.contains("mix `.ayz` `output_len <= 569539 * 115 / 100`")
            && DESIGN.contains("`cdata_blob == 0` on every mix entry"),
        "DESIGN.md must keep mix gates and gated corpus lucene/jackson source_*"
    );
    assert!(
        DESIGN.contains("Skip-exact arm 2")
            && DESIGN.contains("write_skip_exact_concat")
            && DESIGN.contains("Never put recorded `offsetheader`")
            && DESIGN.contains(
                "Arm 3 (no captured headers: overlap / ZipArchive count mismatch / slice `Err`) uses `write_jar` ZipWriter that STOREs `method_code == 0` / `zip_index` over uncompressed payload (`read_entry_content` / `reconstruct_child_zip`); never `resolve_cdata`"
            )
            && DESIGN.contains("Skip-exact arm 1")
            && DESIGN.contains("stencil seek + synthetic CD")
            && !DESIGN.contains("Remaining skip-exact uses `write_jar` ZipWriter")
            && !DESIGN.contains("arm 2-until-concat"),
        "DESIGN.md must document arm 1 seek, arm 2 concat + synthetic CD, and ZipWriter as arm 3 never resolve_cdata"
    );
    assert!(
        README.contains("Priorities: (1) lean pack (2) complete rehydrate (3) class-level dedup")
            && README.contains("`source_*` **must** match iff `bit_identical_restore`")
            && README.contains("Never CAS `blake3(inner zip)` on a `zip_index` slot")
            && README.contains(
                "Leftover junk after N complete CD records with `N == ZipArchive::len()` is homemade_ok + `tail_blob` (exact when every slot hits)"
            )
            && README.contains("Remaining homemade-`None` never gets `tail_blob`")
            && README.contains("Arm 1 homemade-`None` with captured headers is stencil seek + synthetic CD")
            && README.contains("Arm 2 csize-changing skip-exact is concat + synthetic CD")
            && README.contains("Arm 3 stays `ZipWriter` STORE")
            && !README.contains("Synthetic CD is parked"),
        "README must document priorities, hash match iff bit_identical_restore, leftover-junk vs homemade-None, arm 1 synthetic CD"
    );
    assert!(
        README.contains("STORE listable nested `BOOT-INF/lib/*.jar` become depth-1 `zip_index`")
            && !README.contains("Nested `BOOT-INF/lib/*.jar` entries are not exploded")
            && !README.contains("Nested JARs are opaque blobs; they are not exploded"),
        "README must document nested STORE as zip_index + shared class blobs, not opaque whole-ZIP CAS"
    );
    assert!(
        README.contains("https://github.com/hilather/ayzenpack/blob/main/DESIGN.md")
            && README.contains("https://github.com/hilather/ayzenpack/blob/main/AGENTS.md")
            && README.contains("https://github.com/hilather/ayzenpack/blob/main/docs/library.md"),
        "README must use absolute HTTPS links for DESIGN, AGENTS, and library docs"
    );
    assert!(
        README.contains("Do not store `cdata_blob` or `raw_zip` a healthy jar to keep hashes")
            && !README.contains("## Reconstruction guarantee")
            && !README.contains("bit-identical restore is the guarantee"),
        "README must not tell agents to store cdata_blob or chase bit-identical hashes"
    );
    assert!(
        LIBRARY.contains("Priorities: (1) lean pack (2) complete rehydrate (3) class-level dedup")
            && LIBRARY.contains("`source_*` must match iff `bit_identical_restore`")
            && LIBRARY.contains("Outer exact is a file seek-walk")
            && LIBRARY.contains(
                "Leftover junk after N complete CD records with `N == ZipArchive::len()` is homemade_ok + `tail_blob` (exact when every slot hits)"
            )
            && LIBRARY.contains("Remaining homemade-`None` never gets `tail_blob`")
            && LIBRARY.contains("Arm 1 homemade-`None` with captured headers is stencil seek + synthetic CD")
            && LIBRARY.contains("Arm 2 csize-changing skip-exact is concat + synthetic CD")
            && LIBRARY.contains("Arm 3 ZipWriter STOREs")
            && !LIBRARY.contains("Synthetic CD is parked")
            && LIBRARY.contains("never CAS `blake3(inner zip)`")
            && LIBRARY.contains("Do not store `cdata_blob`")
            && LIBRARY.contains("Do not chase bit-identical hashes on a miss")
            && LIBRARY.contains("https://github.com/hilather/ayzenpack/blob/main/DESIGN.md")
            && LIBRARY.contains(
                "https://github.com/hilather/ayzenpack/blob/main/examples/ayzenpack.yaml"
            ),
        "docs/library.md must document priorities, leftover-junk exact vs homemade-None, seek-walk, arm 1 synthetic CD, and absolute HTTPS links"
    );
}

#[test]
fn docs_lock_synthetic_cd_hash_policy_fileabs_and_corpus() {
    // Full sentences, not keyword soup. An inverted stub must fail.
    assert!(
        DESIGN.contains("**must not** require original-file match")
            && DESIGN.contains("Locals-region identity")
            && DESIGN.contains("FileAbs iff `prefix_size > 0`")
            && DESIGN.contains("Homemade-`None` arm 1 is stencil-faithful skip-exact"),
        "DESIGN.md hash policy must require locals-region + FileAbs listing on homemade-None, not original-file source_*"
    );
    assert!(
        DESIGN.contains("### FileAbs listing oracle")
            && DESIGN.contains("**`scan_jar` / `ZipView(prefix)`**")
            && DESIGN.contains("**Do not** rewrite `assert_functional_identity`")
            && DESIGN.contains("when `jar.tail_blob.is_none() && jar.prefix_size.unwrap_or(0) > 0`"),
        "DESIGN.md must document FileAbs listing oracle per (arm, prefix) without rewriting mix assert_functional_identity"
    );
    assert!(
        DESIGN.contains("### Corpus lucene/jackson `source_*`")
            && DESIGN.contains("AYZENPACK_CORPUS_DIR=/path/to/corpus cargo test --test corpus")
            && DESIGN.contains("ci/download-corpus.sh")
            && DESIGN.contains("only when every printed line has `miss=0` and `exact=true`")
            && DESIGN.contains(
                "https://github.com/hilather/ayzenpack/blob/main/ci/download-corpus.sh"
            ),
        "DESIGN.md must document AYZENPACK_CORPUS_DIR enablement with absolute HTTPS links; not always-on until 100% hits"
    );
    assert!(
        AGENTS.contains("Crate **0.2.7** / format **v2**")
            && AGENTS.contains("locals-region identity + FileAbs listing")
            && AGENTS.contains("`AYZENPACK_CORPUS_DIR`; not always-on until 100% hits")
            && AGENTS.contains("Equal-offset last-wins with matching homemade count is exact splice")
            && !AGENTS.contains("Crate **0.2.6** / format **v2**"),
        "AGENTS.md current-tree must be crate 0.2.7 with homemade-None locals-region + FileAbs listing"
    );
    assert!(
        DESIGN.contains(
            "Prefix+hole **(A)** `[non-PK prefix][hole][first CD local]` is already `prefix_blob` covering bash+hole (`find_cd_first_local`); not skip-exact"
        ) && DESIGN.contains(
            "Prefix+hole **(B)** `prefix_len > 0 && min(zip_rel) != 0` after convert is a dead defensive `Err`"
        ) && DESIGN.contains("do not call (B) absorbed")
            && DESIGN.contains("Do not extend `prefix_len` on PK-start files")
            && DESIGN.contains("Do not invent a bash-vs-hole splitter")
            && AGENTS.contains(
                "Prefix+hole **(A)** `[non-PK prefix][hole][first CD local]` is already `prefix_blob` covering bash+hole (`find_cd_first_local`); not skip-exact; do not split bash vs hole"
            )
            && AGENTS.contains(
                "Prefix+hole **(B)** `prefix_len > 0 && min(zip_rel) != 0` after convert is a dead defensive `Err` at first-local-not-at-0; keep it; do not call (B) absorbed"
            )
            && !AGENTS.contains("prefix+hole stays skip-exact arm 3"),
        "DESIGN/AGENTS must split prefix+hole (A) absorbed prefix_blob vs (B) kept dead Err"
    );
    assert!(
        README.contains("locals-region identity")
            && README.contains("FileAbs listing")
            && README.contains("AYZENPACK_CORPUS_DIR")
            && README.contains("ci/download-corpus.sh")
            && README.contains(
                "when `!tail && prefix`, dest `ZipArchive::new(File)` vs source `scan_jar`"
            ),
        "README must document homemade-None locals-region + FileAbs listing and corpus enablement"
    );
    assert!(
        LIBRARY.contains("locals-region identity")
            && LIBRARY.contains("FileAbs iff `prefix_size > 0`")
            && LIBRARY.contains("prefixed source is `scan_jar` / `ZipView`")
            && LIBRARY.contains("AYZENPACK_CORPUS_DIR")
            && LIBRARY
                .contains("https://github.com/hilather/ayzenpack/blob/main/ci/download-corpus.sh"),
        "docs/library.md must document FileAbs listing oracle and gated corpus enablement"
    );
}

#[test]
fn signed_jar_docs_do_not_claim_rebuild_breaks_jarsigner() {
    assert!(
        !DESIGN.contains("rebuild will break the signature")
            && !README.contains("rebuild will break the signature"),
        "DESIGN/README must not claim ZIP rebuild breaks the JAR signature"
    );
    assert!(
        !DESIGN.contains("compressed or stored bytes"),
        "DESIGN.md must not say .SF digests compressed or stored bytes"
    );
    assert!(
        !DESIGN.contains("those signatures will not verify"),
        "DESIGN.md must not say rebuild signatures will not verify"
    );
    assert!(
        !README.contains("will break them") && !README.contains("can still break a signature"),
        "README must not say rebuild breaks signatures"
    );
    assert!(
        DESIGN.contains("digest uncompressed entry bytes")
            && DESIGN.contains("MANIFEST.MF")
            && README.contains("digest uncompressed entry bytes")
            && README.contains("MANIFEST.MF"),
        "DESIGN and README must say .SF / MANIFEST digest uncompressed entry bytes"
    );
}
