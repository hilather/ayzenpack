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
        AGENTS.contains("Do **not** add a Java/zlib deflater, `cdata_blob` for misses, or `raw_zip` of healthy jars"),
        "AGENTS.md must forbid Java-zlib, cdata_blob-for-misses, and raw_zip of healthy jars"
    );
    assert!(
        AGENTS.contains("but **never write** that shape again"),
        "AGENTS.md must say never write legacy dual-copy again"
    );
    assert!(
        AGENTS.contains("**never writes** `cdata_blob` on STORE/DEFLATE"),
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
        PLAN.contains("# PLAN: single-CAS + ZIP index (crate 0.2.1)")
            && PLAN.contains("Writers never emit `cdata_blob`")
            && PLAN.contains("Delete `store_cdata`"),
        "PLAN.md must be the 0.2.1 single-CAS plan, not the shipped 0.2.0 format-v2 plan"
    );
    assert!(
        !README.contains("## Reconstruction guarantee")
            && !README.contains("restore **bit-identical** files")
            && !README.contains("bit-identical restore is the guarantee"),
        "README must not tell agents that bit-identical restore is the guarantee"
    );
    assert!(
        README.contains("Crate **0.2.1** never writes `cdata_blob`"),
        "README must say 0.2.1 never writes leftover cdata_blob"
    );
}
