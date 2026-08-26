//! PR-17: bench scripts, budget gate, and workflow guards.
//!
//! Always-on cargo test stays offline and must not generate 50×1 MiB JARs.
//! The release CLI smoke lives in ci/bench.sh / bench.yml only.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BENCH_YML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/.github/workflows/bench.yml"
));
const CI_YML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/.github/workflows/ci.yml"
));
const BENCH_SH: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/ci/bench.sh"));
const COMPARE_PY: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/ci/compare-bench.py"));
const BUDGETS_JSON: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/ci/perf-budgets.json"));

const REQUIRED_RESULT_KEYS: &[&str] = &[
    "git_sha",
    "corpus_id",
    "bytes_in_jars",
    "archive_size",
    "bytes_unique_blobs",
    "unique_blob_count",
    "file_entry_count",
    "dehydrate_wall_ms",
    "rehydrate_wall_ms",
    "dehydrate_peak_rss_kb",
    "rehydrate_peak_rss_kb",
    "ratio_archive_to_jars",
    "ratio_unique_to_uncompressed",
];

fn python() -> Command {
    for bin in ["python3", "python"] {
        if Command::new(bin)
            .args(["-c", "import json,sys"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Command::new(bin);
        }
    }
    panic!("python3 is required for compare-bench.py tests");
}

fn compare_py() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/compare-bench.py")
}

fn budgets_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/perf-budgets.json")
}

fn at_limit_results() -> serde_json::Value {
    serde_json::json!({
        "git_sha": "test",
        "corpus_id": "test",
        "bytes_in_jars": 100,
        "archive_size": 70,
        "bytes_unique_blobs": 85,
        "unique_blob_count": 1,
        "file_entry_count": 1,
        "dehydrate_wall_ms": 60000,
        "rehydrate_wall_ms": 90000,
        "dehydrate_peak_rss_kb": 1048576,
        "rehydrate_peak_rss_kb": 1048576,
        "ratio_archive_to_jars": 0.70,
        "ratio_unique_to_uncompressed": 0.85
    })
}

fn write_json(path: &Path, value: &serde_json::Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn run_compare(results: &Path, budgets: &Path, baseline: Option<&Path>) -> std::process::Output {
    let mut cmd = python();
    cmd.arg(compare_py()).arg(results).arg(budgets);
    if let Some(b) = baseline {
        cmd.arg("--baseline").arg(b);
    }
    cmd.output().expect("spawn compare-bench.py")
}

#[test]
fn perf_budgets_match_design() {
    let v: serde_json::Value = serde_json::from_str(BUDGETS_JSON).expect("perf-budgets.json");
    assert_eq!(v["dehydrate_wall_ms_max"].as_u64(), Some(60000));
    assert_eq!(v["rehydrate_wall_ms_max"].as_u64(), Some(90000));
    assert_eq!(v["dehydrate_peak_rss_kb_max"].as_u64(), Some(1048576));
    assert_eq!(v["rehydrate_peak_rss_kb_max"].as_u64(), Some(1048576));
    let archive = v["ratio_archive_to_jars_max"].as_f64().unwrap();
    let unique = v["ratio_unique_to_uncompressed_max"].as_f64().unwrap();
    assert!((archive - 0.70).abs() < 1e-9, "archive ratio max {archive}");
    assert!((unique - 0.85).abs() < 1e-9, "unique ratio max {unique}");
}

#[test]
fn compare_bench_passes_at_limit() {
    let dir = tempfile::tempdir().unwrap();
    let results = dir.path().join("results.json");
    write_json(&results, &at_limit_results());
    let out = run_compare(&results, &budgets_path(), None);
    assert!(
        out.status.success(),
        "equal to max must pass, stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn compare_bench_fails_when_over_budget() {
    let dir = tempfile::tempdir().unwrap();
    let results = dir.path().join("results.json");
    let mut over = at_limit_results();
    over["dehydrate_wall_ms"] = serde_json::json!(60001);
    write_json(&results, &over);
    let out = run_compare(&results, &budgets_path(), None);
    assert!(
        !out.status.success(),
        "over-budget must fail, stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("OVER") || combined.to_ascii_lowercase().contains("exceed"),
        "failure should name the over-budget metric, got: {combined}"
    );

    // Ratio over max also fails; vs-main baseline must not flip a pass into a fail.
    let mut ratio_over = at_limit_results();
    ratio_over["ratio_archive_to_jars"] = serde_json::json!(0.71);
    write_json(&results, &ratio_over);
    let baseline = dir.path().join("main.json");
    let mut better_main = at_limit_results();
    better_main["ratio_archive_to_jars"] = serde_json::json!(0.10);
    write_json(&baseline, &better_main);
    let out = run_compare(&results, &budgets_path(), Some(&baseline));
    assert!(
        !out.status.success(),
        "ratio over budget must fail even with a baseline"
    );

    let under = at_limit_results();
    write_json(&results, &under);
    let out = run_compare(&results, &budgets_path(), Some(&baseline));
    assert!(
        out.status.success(),
        "under/at budget must pass when worse than main (delta is informational)"
    );
}

#[cfg(unix)]
#[test]
fn bench_script_emits_required_keys() {
    // Tiny synthetic only: never 50×1 MiB in cargo test. Fake-sized copies + cargo_bin.
    let dir = tempfile::tempdir().unwrap();
    let out_json = dir.path().join("bench-results.json");
    let work = dir.path().join("work");
    let bin = assert_cmd::cargo::cargo_bin("ayzenpack");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("ci/bench.sh");
    let out = Command::new("bash")
        .arg(&script)
        .env("AYZENPACK_BIN", &bin)
        .env("BENCH_OUT", &out_json)
        .env("BENCH_WORKDIR", &work)
        .env("BENCH_SYNTHETIC_COPIES", "2")
        .env("BENCH_SYNTHETIC_BYTES", "1024")
        .env("BENCH_SKIP_CORPUS", "1")
        .env("GITHUB_SHA", "testsha")
        .output()
        .expect("spawn ci/bench.sh");
    assert!(
        out.status.success(),
        "bench.sh dry-run failed, stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let text = fs::read_to_string(&out_json).expect("bench-results.json");
    let v: serde_json::Value = serde_json::from_str(&text).expect("results JSON");
    for key in REQUIRED_RESULT_KEYS {
        assert!(
            v.get(*key).is_some(),
            "bench-results.json missing {key}: {text}"
        );
    }
    assert!(v["bytes_in_jars"].as_u64().unwrap() > 0);
    assert!(v["archive_size"].as_u64().unwrap() > 0);
    // Stored synthetic copies share one content blob + one tail (cdata == blob).
    assert_eq!(v["unique_blob_count"].as_u64(), Some(2));
}

#[test]
fn bench_yml_timeout_release_cli_not_test_release() {
    assert!(
        BENCH_YML.contains("timeout-minutes: 30"),
        "bench job timeout-minutes must be 30"
    );
    assert!(
        BENCH_YML.contains("ubuntu-latest"),
        "bench.yml is Linux only"
    );
    assert!(
        !BENCH_YML.to_ascii_lowercase().contains("windows"),
        "bench.yml must not run on Windows"
    );
    assert!(
        BENCH_YML.contains("cargo build --release"),
        "bench.yml must cargo build --release for the CLI"
    );
    assert!(
        !BENCH_YML.contains("cargo test --release"),
        "bench.yml must not cargo test --release under panic=abort"
    );
    assert!(
        !BENCH_YML.contains("cargo test --locked"),
        "bench.yml must not run cargo test (50×1 MiB is CLI-only)"
    );
    assert!(
        BENCH_YML.contains("python3 ci/compare-bench.py"),
        "bench.yml must gate with python3 ci/compare-bench.py"
    );
    assert!(
        BENCH_YML.contains("ci/bench.sh"),
        "bench.yml must run ci/bench.sh"
    );
    assert!(
        BENCH_YML.contains("upload-artifact"),
        "bench.yml must upload bench-results.json"
    );
}

#[test]
fn bench_yml_actions_are_sha_pinned() {
    for line in BENCH_YML.lines() {
        let t = line.trim();
        if let Some(rest) = t
            .strip_prefix("- uses:")
            .or_else(|| t.strip_prefix("uses:"))
        {
            let spec = rest.trim();
            let sha = spec
                .split_whitespace()
                .next()
                .and_then(|s| s.rsplit_once('@'))
                .map(|(_, sha)| sha)
                .unwrap_or("");
            assert_eq!(sha.len(), 40, "action must be pinned by 40-char SHA: {t}");
            assert!(
                sha.chars().all(|c| c.is_ascii_hexdigit()),
                "action pin is not hex: {t}"
            );
        }
    }
}

#[test]
fn always_on_ci_does_not_run_fifty_mib_bench() {
    // Guards sneaking 50×1 MiB into every PR's cargo test via ci.yml.
    assert!(!CI_YML.contains("bench.sh"));
    assert!(!CI_YML.contains("50×1"));
    assert!(!CI_YML.contains("1048576"));
    assert!(
        BENCH_SH.contains("BENCH_SYNTHETIC_COPIES:-50")
            || BENCH_SH.contains("COPIES:-50")
            || BENCH_SH.contains("50"),
        "ci/bench.sh default copies must be 50"
    );
    assert!(
        BENCH_SH.contains("1048576"),
        "ci/bench.sh default payload must be 1 MiB (1048576)"
    );
    assert!(
        COMPARE_PY.contains("ratio_archive_to_jars_max"),
        "compare-bench.py must gate archive/input ratio"
    );
}
