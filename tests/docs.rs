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
