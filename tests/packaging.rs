//! Packaging / release hardening (ratarmount-rs lessons). Unix-only scripts.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn run_script(rel: &str) {
    let path = repo_root().join(rel);
    let status = Command::new("bash")
        .arg(&path)
        .current_dir(repo_root())
        .status()
        .unwrap_or_else(|e| panic!("spawn {rel}: {e}"));
    assert!(
        status.success(),
        "{rel} failed with {status:?} (guards tag/Cargo mismatch, empty GitHub assets, Rocky dnf hardening)"
    );
}

#[test]
fn version_resolve_tag_must_match_cargo() {
    run_script("packaging/test-version-resolve.sh");
}

#[test]
fn release_asset_filter_skips_empty_files() {
    run_script("packaging/test-release-asset-filter.sh");
}

#[test]
fn rocky_dnf_hardening_in_packages_yml() {
    run_script("packaging/test-rocky-dnf-hardening.sh");
}
