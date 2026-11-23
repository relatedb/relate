//! Round-1 harness for the vortex property-store evaluation.
//!
//! Kernels are named O1..O20 in `workload-mapping-findings.md`; each reports
//! ns/op, allocs/op, B/op with a load stamp. Measurements are serialized —
//! one kernel at a time on a shared box (quiet-box protocol).

mod bench;
mod ops;
mod store;

#[global_allocator]
static ALLOC: bench::Counting = bench::Counting;

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("all");
    let out_dir = PathBuf::from(
        std::env::var("VORTEX_LAB_OUT").unwrap_or_else(|_| "/tmp/relate-vortex-lab".to_string()),
    );
    std::fs::create_dir_all(&out_dir)?;

    // The LDBC SF0.1 CSV the query-latency suite already has on disk.
    let csv = PathBuf::from(
        std::env::var("VORTEX_LAB_CSV").unwrap_or_else(|_| {
            "target/query-latency/data/social_network-sf0.1-CsvBasic-StringDateFormatter/dynamic/person_0_0.csv"
                .to_string()
        }),
    );

    println!("vortex-lab: out={} load={:.1}", out_dir.display(), bench::loadavg1());

    match cmd {
        "point" => {
            ops::point::run(&csv, &out_dir, 100_000)?;
        }
        "seek" => {
            let n: usize = args
                .get(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or(150_000);
            ops::seek::run("flat", n, 4, &out_dir, 20_000)?;
        }
        "filter" => {
            ops::filter::run(&out_dir, 20_000)?;
        }
        "agg" => {
            ops::agg::run(&out_dir, 20_000)?;
        }
        "join" => {
            ops::join::run(&out_dir, 20_000)?;
        }
        "sets" => {
            ops::sets::run(&out_dir, 20_000)?;
        }
        "all" => {
            ops::point::run(&csv, &out_dir, 100_000)?;
            ops::seek::run("flat", 150_000, 4, &out_dir, 20_000)?;
            ops::filter::run(&out_dir, 20_000)?;
            ops::agg::run(&out_dir, 20_000)?;
            ops::join::run(&out_dir, 20_000)?;
            ops::sets::run(&out_dir, 20_000)?;
        }
        other => {
            eprintln!("unknown command {other}: point | seek | filter | agg | join | sets | all");
            std::process::exit(2);
        }
    }
    Ok(())
}
