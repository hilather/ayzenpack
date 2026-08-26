use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use clap::{Parser, Subcommand};

use ayzenpack::stats::{fmt_bytes, format_stats_line, json_event};
use ayzenpack::{
    dehydrate, list, rehydrate, verify, AyzenpackError, DehydrateOptions, DehydrateSummary,
    Manifest, RehydrateOptions, Trailer,
};

#[derive(Parser)]
#[command(
    name = "ayzenpack",
    version,
    about = "Dehydrate / rehydrate JAR sets with BLAKE3 + zstd"
)]
pub struct Cli {
    /// No stderr progress
    #[arg(short, long, global = true)]
    quiet: bool,
    /// Extra stderr (each JAR path)
    #[arg(short, long, global = true)]
    verbose: bool,
    /// One JSON object per event on stderr
    #[arg(long, global = true)]
    json_logs: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Pack JARs into one .ayz archive
    #[command(visible_alias = "pack")]
    Dehydrate {
        #[arg(short, long)]
        output: PathBuf,
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        #[arg(long, default_value_t = 3)]
        level: i32,
        #[arg(long)]
        sort_inputs: bool,
        #[arg(short, long)]
        recursive: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value_t = 2_147_483_647)]
        max_entry_bytes: u64,
        #[arg(long)]
        strict: bool,
        #[arg(long)]
        fail_on_signed: bool,
        #[arg(long)]
        write_sidecar_manifest: Option<PathBuf>,
        #[arg(long)]
        pretty_manifest: bool,
        #[arg(long)]
        follow_symlinks: bool,
        #[arg(long)]
        exclude: Vec<String>,
        /// Hash worker threads. 1 = sequential (default). 0 = available parallelism
        #[arg(long, default_value_t = 1)]
        jobs: usize,
        /// Cap on uncompressed entry buffers in the hash pipeline (default 64 MiB)
        #[arg(long, default_value_t = 64 * 1024 * 1024)]
        max_inflight_bytes: u64,
    },
    /// Restore JARs from a .ayz archive
    #[command(visible_alias = "unpack")]
    Rehydrate {
        #[arg(short, long)]
        input: PathBuf,
        #[arg(short, long)]
        dir: PathBuf,
        #[arg(long)]
        cas_dir: Option<PathBuf>,
        #[arg(long)]
        keep_cas: bool,
        #[arg(long)]
        store_all: bool,
        #[arg(long, default_value_t = 6)]
        deflate_level: i32,
        #[arg(long)]
        clean: bool,
        #[arg(long)]
        overwrite: bool,
        #[arg(long)]
        only: Vec<String>,
    },
    /// Show archive contents
    List {
        #[arg(short, long)]
        input: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Re-hash blobs and check the manifest
    Verify {
        #[arg(short, long)]
        input: PathBuf,
    },
}

/// Binary-boundary error: `verify` integrity failures are exit 3; everything else is 1.
pub struct CliError {
    pub code: i32,
    err: anyhow::Error,
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.err)
    }
}

impl From<AyzenpackError> for CliError {
    fn from(err: AyzenpackError) -> Self {
        Self {
            code: 1,
            err: err.into(),
        }
    }
}

fn verify_err(err: AyzenpackError) -> CliError {
    CliError {
        code: verify_exit_code(&err),
        err: err.into(),
    }
}

fn verify_exit_code(err: &AyzenpackError) -> i32 {
    if is_integrity_error(err) {
        3
    } else {
        1
    }
}

/// `HashMismatch` and integrity `Format` → 3. I/O, `NotAyzenpack`, truncated trailer, JSON → 1.
fn is_integrity_error(err: &AyzenpackError) -> bool {
    match err {
        AyzenpackError::HashMismatch(_) => true,
        AyzenpackError::Format(msg) => format_is_integrity(msg),
        AyzenpackError::FormatOwned(msg) => format_is_integrity(msg),
        _ => false,
    }
}

fn format_is_integrity(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    if m.contains("truncated")
        || m.contains("trailer")
        || m.contains("missing end")
        || m.contains("missing manifest")
        || m.contains("unknown record")
        || m.contains("reserved record")
        || m.contains("blob after")
        || m.contains("multiple manifest")
        || m.contains("records after end")
        || m.contains("header format")
        || m.contains("invalid format version")
        || m.contains("record payload too large")
        || m.contains("manifest format")
        || m.contains("hex")
    {
        return false;
    }
    m.contains("hash")
        || m.contains("crc")
        || m.contains("digest")
        || m.contains("mismatch")
        || m.contains("sha256")
        || m.contains("blake3")
        || m.contains("missing blob")
}

pub fn run() -> std::result::Result<(), CliError> {
    let Cli {
        quiet,
        verbose,
        json_logs,
        cmd,
    } = Cli::parse();
    match cmd {
        Cmd::Dehydrate {
            output,
            inputs,
            level,
            sort_inputs,
            recursive,
            dry_run,
            max_entry_bytes,
            strict,
            fail_on_signed,
            write_sidecar_manifest,
            pretty_manifest,
            follow_symlinks,
            exclude,
            jobs,
            max_inflight_bytes,
        } => {
            let opts = DehydrateOptions {
                output,
                inputs,
                recursive,
                sort_inputs,
                level,
                max_entry_bytes,
                strict,
                fail_on_signed,
                dry_run,
                write_sidecar_manifest,
                pretty_manifest,
                follow_symlinks,
                exclude,
                quiet,
                verbose,
                json_logs,
                jobs,
                max_inflight_bytes,
            };
            let summary = dehydrate(&opts)?;
            print_dehydrate_stats(&opts, &summary);
            Ok(())
        }
        Cmd::Rehydrate {
            input,
            dir,
            cas_dir,
            keep_cas,
            store_all,
            deflate_level,
            clean,
            overwrite,
            only,
        } => {
            let opts = RehydrateOptions {
                input,
                dir,
                cas_dir,
                keep_cas,
                store_all,
                deflate_level,
                clean,
                overwrite,
                only,
                quiet,
                verbose,
                json_logs,
            };
            rehydrate(&opts)?;
            Ok(())
        }
        Cmd::List { input, json } => {
            let manifest = list(&input)?;
            if json {
                print_list_json(&manifest)?;
            } else {
                let trailer = read_trailer_file(&input)?;
                print_human_list(&manifest, &trailer);
            }
            Ok(())
        }
        Cmd::Verify { input } => verify(&input).map_err(verify_err),
    }
}

fn print_list_json(manifest: &Manifest) -> std::result::Result<(), CliError> {
    let s = serde_json::to_string_pretty(manifest).map_err(|e| CliError {
        code: 1,
        err: anyhow!(e),
    })?;
    println!("{s}");
    Ok(())
}

fn read_trailer_file(path: &Path) -> std::result::Result<Trailer, CliError> {
    let mut file = File::open(path).map_err(|source| AyzenpackError::Io {
        source,
        path: Some(path.to_path_buf()),
    })?;
    ayzenpack::format::read_trailer(&mut file).map_err(CliError::from)
}

/// Human table (name, entries, signed, source_size) plus footer from trailer + manifest.
fn print_human_list(manifest: &Manifest, trailer: &Trailer) {
    println!(
        "{:<32} {:>7} {:>6} {:>12}",
        "NAME", "ENTRIES", "SIGNED", "SIZE"
    );
    for jar in &manifest.jars {
        println!(
            "{:<32} {:>7} {:>6} {:>12}",
            jar.name,
            jar.entries.len(),
            jar.signed,
            jar.source_size
        );
    }
    println!();
    println!(
        "{} jars, {} unique blobs, {} unique",
        trailer.jar_count,
        trailer.blob_count,
        fmt_bytes(manifest.stats.bytes_unique_blobs)
    );
}

fn print_dehydrate_stats(opts: &DehydrateOptions, summary: &DehydrateSummary) {
    if opts.json_logs {
        json_event(&serde_json::json!({
            "event": "stats",
            "jars": summary.jar_count,
            "entries": summary.entry_count,
            "unique_blobs": summary.unique_blob_count,
            "bytes_uncompressed_entries": summary.bytes_uncompressed_entries,
            "bytes_unique_blobs": summary.bytes_unique_blobs,
            "zstd_bytes": summary.output_len,
            "bytes_in_jars": summary.bytes_in_jars,
            "dedup_ratio": summary.dedup_ratio,
        }));
        return;
    }
    if opts.quiet {
        return;
    }
    eprintln!(
        "{}",
        format_stats_line(
            summary.jar_count,
            summary.entry_count,
            summary.unique_blob_count,
            summary.bytes_uncompressed_entries,
            summary.bytes_unique_blobs,
            summary.output_len,
            summary.bytes_in_jars,
        )
    );
}
