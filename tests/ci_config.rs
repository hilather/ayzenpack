//! PR-15 guards: always-on CI stays offline (no Maven, no cargo test --release).

const CI_YML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/.github/workflows/ci.yml"
));

const PACKAGES_YML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/.github/workflows/packages.yml"
));

#[test]
fn ci_yml_does_not_curl_maven() {
    // Corpus downloads belong in corpus.yml, not the always-on workflow.
    let lower = CI_YML.to_ascii_lowercase();
    assert!(
        !lower.contains("repo1.maven.org"),
        "ci.yml must not contain curl to repo1.maven.org"
    );
    assert!(
        !(lower.contains("curl") && lower.contains("maven")),
        "ci.yml must not curl Maven"
    );
}

#[test]
fn ci_yml_does_not_cargo_test_release() {
    // [profile.release] panic = abort makes cargo test --release a landmine.
    assert!(
        !CI_YML.contains("cargo test --release"),
        "ci.yml must not contain cargo test --release"
    );
}

#[test]
fn ci_yml_linux_fmt_clippy_offline_locked_test() {
    // Guards dropping fmt/clippy -D, --locked, or going back online after fetch.
    assert!(
        CI_YML.contains("cargo fmt --all -- --check"),
        "linux job must run rustfmt"
    );
    assert!(
        CI_YML.contains("cargo clippy --all-targets --locked -- -D warnings"),
        "linux job must run clippy --all-targets --locked -- -D warnings"
    );
    assert!(
        CI_YML.contains("cargo test --locked"),
        "ci.yml must cargo test --locked"
    );
    assert!(
        CI_YML.contains("cargo fetch"),
        "ci.yml must cargo fetch before offline steps"
    );
    assert!(
        CI_YML.contains("CARGO_NET_OFFLINE"),
        "subsequent steps after fetch must set CARGO_NET_OFFLINE"
    );
}

#[test]
fn ci_yml_has_windows_test_and_msrv_1_80() {
    // Windows check from PR-0 is upgraded here; MSRV must not float on stable-only.
    assert!(
        CI_YML.contains("windows-latest"),
        "ci.yml must include a Windows job"
    );
    assert!(
        CI_YML.contains("toolchain: \"1.80\"") || CI_YML.contains("toolchain: '1.80'"),
        "MSRV job must pin toolchain 1.80 (quoted so YAML is not 1.8)"
    );
    assert!(
        CI_YML.contains("cargo check --locked") || CI_YML.contains("cargo test --locked"),
        "MSRV job must cargo check --locked (or test)"
    );
}

#[test]
fn packages_yml_builds_rocky_8_and_9() {
    assert!(
        PACKAGES_YML.contains("rockylinux/rockylinux:8"),
        "packages.yml must build Rocky Linux 8"
    );
    assert!(
        PACKAGES_YML.contains("rockylinux/rockylinux:9"),
        "packages.yml must build Rocky Linux 9"
    );
    assert!(
        !PACKAGES_YML.contains("cargo test --release"),
        "packages.yml must not cargo test --release (panic=abort)"
    );
    assert!(
        PACKAGES_YML.contains("test-version-resolve.sh"),
        "packages.yml must resolve VERSION from tag vs Cargo.toml"
    );
}
