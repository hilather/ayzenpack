//! Dedup ratio, human stats line, and stderr progress (indicatif).
//!
//! Progress and JSON logs go to stderr so stdout stays pipe-safe.

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use serde_json::Value;

/// `1 - unique/uncompressed`. Zero when there are no uncompressed bytes (avoid div-by-zero).
pub fn dedup_ratio(bytes_unique_blobs: u64, bytes_uncompressed_entries: u64) -> f64 {
    if bytes_uncompressed_entries == 0 {
        0.0
    } else {
        1.0 - (bytes_unique_blobs as f64 / bytes_uncompressed_entries as f64)
    }
}

pub fn fmt_bytes(n: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const KIB: f64 = 1024.0;
    let x = n as f64;
    if x >= MIB {
        format!("{:.1} MiB", x / MIB)
    } else if x >= KIB {
        format!("{:.1} KiB", x / KIB)
    } else {
        format!("{n} B")
    }
}

/// DESIGN stderr last line. Ratio is zstd bytes / on-disk jar bytes, not the inverted dedup ratio.
pub fn format_stats_line(
    jar_count: u64,
    entry_count: u64,
    unique_blob_count: u64,
    bytes_uncompressed_entries: u64,
    bytes_unique_blobs: u64,
    zstd_bytes: u64,
    bytes_in_jars: u64,
) -> String {
    let ratio = if bytes_in_jars == 0 {
        0.0
    } else {
        zstd_bytes as f64 / bytes_in_jars as f64
    };
    format!(
        "ayzenpack: {jar_count} jars, {entry_count} entries, {unique_blob_count} unique blobs, {} → {} unique, zstd {} ({ratio:.3} of jar bytes)",
        fmt_bytes(bytes_uncompressed_entries),
        fmt_bytes(bytes_unique_blobs),
        fmt_bytes(zstd_bytes),
    )
}

pub fn json_event(value: &Value) {
    eprintln!("{value}");
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LogMode {
    Quiet,
    Human,
    Json,
}

/// Per-JAR entry progress on stderr. `-q` hides it; `--json-logs` emits NDJSON instead.
pub struct PackProgress {
    mode: LogMode,
    bar: ProgressBar,
}

impl PackProgress {
    pub fn new(quiet: bool, json_logs: bool) -> Self {
        let mode = if json_logs {
            LogMode::Json
        } else if quiet {
            LogMode::Quiet
        } else {
            LogMode::Human
        };
        let bar = if mode == LogMode::Human {
            let pb = ProgressBar::with_draw_target(Some(0), ProgressDrawTarget::stderr());
            if let Ok(style) = ProgressStyle::with_template("{msg} [{bar:32}] {pos}/{len} entries")
            {
                pb.set_style(style.progress_chars("=>-"));
            }
            pb
        } else {
            ProgressBar::hidden()
        };
        Self { mode, bar }
    }

    pub fn start_jar(&self, name: &str, entries: u64) {
        if self.mode != LogMode::Human {
            return;
        }
        self.bar.set_length(entries);
        self.bar.set_position(0);
        self.bar.set_message(name.to_string());
    }

    pub fn inc_entry(&self) {
        self.bar.inc(1);
    }

    pub fn finish_jar(&self, name: &str, entries: u64) {
        match self.mode {
            LogMode::Quiet => {}
            LogMode::Json => {
                json_event(&serde_json::json!({
                    "event": "jar_done",
                    "name": name,
                    "entries": entries,
                }));
            }
            LogMode::Human => {
                self.bar.set_position(entries);
                // Pipes/tests are not a TTY; keep a stable per-JAR line on stderr.
                let line = format!("ayzenpack: {name}: {entries} entries");
                if self.bar.is_hidden() {
                    eprintln!("{line}");
                } else {
                    self.bar.println(line);
                }
            }
        }
    }
}

impl Drop for PackProgress {
    fn drop(&mut self) {
        self.bar.finish_and_clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_ratio_zero_when_no_uncompressed_bytes() {
        // Guards inverted ratio or NaN from unique/0.
        assert_eq!(dedup_ratio(0, 0), 0.0);
        assert_eq!(dedup_ratio(10, 0), 0.0);
    }

    #[test]
    fn dedup_ratio_formula() {
        // Guards unique/uncompressed (inverted) instead of 1 - unique/uncompressed.
        assert_eq!(dedup_ratio(25, 100), 0.75);
        assert_eq!(dedup_ratio(100, 100), 0.0);
        assert_eq!(dedup_ratio(0, 50), 1.0);
    }

    #[test]
    fn stats_line_order_is_uncompressed_then_unique() {
        // Guards swapping the arrow so unique looks larger than uncompressed.
        let line = format_stats_line(12, 8401, 912, 2048, 1024, 512, 4096);
        assert!(
            line.starts_with("ayzenpack: 12 jars, 8401 entries, 912 unique blobs, "),
            "{line}"
        );
        assert!(line.contains("2.0 KiB → 1.0 KiB unique"), "{line}");
        assert!(line.contains("zstd 512 B (0.125 of jar bytes)"), "{line}");
    }
}
