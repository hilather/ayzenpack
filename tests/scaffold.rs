//! PR-0 identity guards: crate name, no unsafe, no deferred deps.

#[test]
fn crate_is_named_ayzenpack() {
    // Guards renaming the crate back to jded.
    assert_eq!(env!("CARGO_PKG_NAME"), "ayzenpack");
    assert_ne!(env!("CARGO_PKG_NAME"), "jded");
}

#[test]
fn lib_forbids_unsafe_code() {
    // Guards dropping #![forbid(unsafe_code)] from the crate root.
    let lib = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    assert!(
        lib.contains("#![forbid(unsafe_code)]"),
        "src/lib.rs must forbid unsafe_code"
    );
}

#[test]
fn cargo_toml_has_no_deferred_deps() {
    // Guards pulling indicatif/rayon/proptest before their PRs.
    let toml = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
    for dep in ["indicatif", "rayon", "proptest"] {
        assert!(!toml.contains(dep), "PR-0 Cargo.toml must not pull {dep}");
    }
}
