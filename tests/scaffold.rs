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
fn cargo_toml_has_indicatif() {
    // Guards shipping progress without the deferred crate this PR is supposed to add.
    let toml = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
    assert!(
        toml.contains("indicatif"),
        "PR-12 Cargo.toml must add indicatif"
    );
}

#[test]
fn cargo_toml_has_no_deferred_deps() {
    // Guards pulling rayon before PR-18. proptest is already a dev-dep from PR-19.
    let toml = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
    assert!(!toml.contains("rayon"), "Cargo.toml must not pull rayon yet");
    let (runtime, dev) = toml
        .split_once("[dev-dependencies]")
        .expect("Cargo.toml has [dev-dependencies]");
    assert!(
        !runtime.contains("proptest"),
        "proptest must not be a runtime dependency"
    );
    assert!(
        dev.contains("proptest"),
        "proptest must be a [dev-dependencies] crate"
    );
}
