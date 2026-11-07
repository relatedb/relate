use std::path::PathBuf;

use clap::Parser;
use relate_datasets::harness::default_bolt_bin;
use relate_datasets::memory_shape::{default_store_uri, format_report, run, MemoryShapeConfig};
use relate_datasets::shapes::{IndexCounts, DEFAULT_LOGICAL_TARGET_BYTES, DEFAULT_NODE_ATTRIBUTES};

#[derive(Parser, Debug)]
#[command(
    name = "memory-shape-loadtest",
    about = "Measure the relate serve process's RSS against logical graph bytes for each shape"
)]
struct Args {
    /// Directory for this run. Each shape is stored at `<uri>/<shape>`, and
    /// disk MB is the full size of that directory. Default:
    /// `/tmp/memory-shape-loadtest-<unix seconds>`.
    #[arg(long)]
    uri: Option<PathBuf>,
    /// Path to the `relate` product binary (its `serve --bolt` child is
    /// measured). Default: sibling of this executable, then
    /// target/release/relate.
    #[arg(long)]
    bolt_bin: Option<PathBuf>,
    /// Report JSON path
    #[arg(long, default_value = "target/memory-shape-loadtest.json")]
    out: PathBuf,
    /// Logical ceiling for each filled shape, in MB (1024×1024 bytes). The
    /// load stops at a fixed count. Reaching this ceiling first fails the run.
    #[arg(long, default_value_t = DEFAULT_LOGICAL_TARGET_BYTES / (1024 * 1024))]
    logical_mb: u64,
    /// Integer properties on every node. The first is `id`.
    #[arg(long, default_value_t = DEFAULT_NODE_ATTRIBUTES)]
    attributes: u32,
    /// String attributes per node, each with a TEXT index. Added on top of
    /// `--attributes`.
    #[arg(long, default_value_t = 0)]
    text_indexes: u32,
    /// Point attributes per node, each with a POINT index. Added on top of
    /// `--attributes`.
    #[arg(long, default_value_t = 0)]
    point_indexes: u32,
    /// Integer attributes per node that carry a RANGE index. Must be at most
    /// `--attributes`.
    #[arg(long, default_value_t = 0)]
    range_indexes: u32,
}

fn main() {
    let args = Args::parse();
    let Some(logical_target_bytes) = args.logical_mb.checked_mul(1024 * 1024) else {
        eprintln!("logical-mb overflows");
        std::process::exit(1);
    };
    let cfg = MemoryShapeConfig {
        bolt_bin: args.bolt_bin.unwrap_or_else(default_bolt_bin),
        store: args.uri.unwrap_or_else(default_store_uri),
        out: Some(args.out.clone()),
        logical_target_bytes,
        attributes: args.attributes,
        indexes: IndexCounts {
            text: args.text_indexes,
            point: args.point_indexes,
            range: args.range_indexes,
        },
    };
    match run(&cfg) {
        Ok(report) => {
            print!("{}", format_report(&report));
            println!("wrote {}", args.out.display());
        }
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}
