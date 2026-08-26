use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

use ayzenpack::{dehydrate, DehydrateOptions, DehydrateSummary};

#[derive(Parser)]
#[command(
    name = "ayzenpack",
    version,
    about = "Dehydrate / rehydrate JAR sets with BLAKE3 + zstd"
)]
pub struct Cli {
    #[arg(short, long, global = true)]
    quiet: bool,
    #[arg(short, long, global = true)]
    verbose: bool,
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
        // PR-18: jobs, max_inflight_bytes — do not add until then
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

pub fn run() -> Result<()> {
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
            };
            let summary = dehydrate(&opts)?;
            print_dehydrate_stats(&opts, &summary);
            Ok(())
        }
        Cmd::Rehydrate { .. } => bail!("rehydrate is not implemented yet"),
        Cmd::List { .. } => bail!("list is not implemented yet"),
        Cmd::Verify { .. } => bail!("verify is not implemented yet"),
    }
}

fn print_dehydrate_stats(opts: &DehydrateOptions, summary: &DehydrateSummary) {
    if opts.quiet {
        return;
    }
    let ratio = if summary.bytes_in_jars == 0 {
        0.0
    } else {
        summary.output_len as f64 / summary.bytes_in_jars as f64
    };
    eprintln!(
        "ayzenpack: {} jars, {} entries, {} unique blobs, {} → {} unique, zstd {} ({:.3} of jar bytes)",
        summary.jar_count,
        summary.entry_count,
        summary.unique_blob_count,
        fmt_bytes(summary.bytes_uncompressed_entries),
        fmt_bytes(summary.bytes_unique_blobs),
        fmt_bytes(summary.output_len),
        ratio,
    );
}

fn fmt_bytes(n: u64) -> String {
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
