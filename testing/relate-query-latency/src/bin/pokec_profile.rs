//! Pokec-shaped expansion profiling harness bin (LABS-1116): the body is
//! [`relate_query_latency::pokec`]; the counting allocator is installed here
//! so the latency suite never pays its per-allocation atomic.
//!
//! Usage: `cargo run --release -p relate-query-latency --bin pokec_profile --
//! <small|medium> [--group NAME] [--sample DIR | --profile]` (`--help` lists
//! every flag; the flag table is in `docs/datasets.md`).
//!
//! * `small` — 10,000 users / 121,716 friendships, opens in seconds.
//! * `medium` — 100,000 users / 1,768,164 friendships; the open cost is
//!   the population cost.
//! * `--group NAME` runs one group (`--help` lists every name, drawn from
//!   the group catalogue; an unknown name is a usage error before the
//!   fixture opens); a timed line ends with `live_peak`, `live_end`,
//!   `rss_base`, `rss_peak`.
//! * `--sample DIR` runs the selected groups under the in-process profiler
//!   and writes `DIR/<group>/top.txt`, `folded.txt` and `flame.svg`; no
//!   samply needed.
//! * A timed run also prints one `<group>: plan ...` line per group, the
//!   PROFILE plan evidence (operator, `kernel` rung, read counts) of the
//!   group's query at start id 1, recorded before its timed rounds.
//! * `--profile` (needs `--group`) prints the `PROFILE` result of the
//!   group's query at start id 1 instead of timing it.
//! * `--profiled` times every round as `PROFILE <query>` (the memo
//!   counters of ADR-0654 collected), the A/B arm against an unprofiled
//!   run's `alloc_bytes`, `live_peak` and `rss_peak`.
//! * No env vars beyond cargo's own, except the engine's open-time settings the
//!   fixture's `Database::open` reads: `RELATE_EXPRESSION_MEMO_BUDGET=<MiB>`
//!   (ADR-0657) sizes or, at `0`, disables the memo tables for a sweep.
//!
//! Record a profile: `samply record -s -n -o prof.json.gz -- <binary> small`.
//! Build with frame pointers for trustworthy stacks
//! (`RUSTFLAGS="-C force-frame-pointers=yes"`).

use clap::Parser;
use relate::Result;
use relate_query_latency::cli::PokecArgs;

#[global_allocator]
static A: relate_query_latency::alloc::Counting = relate_query_latency::alloc::Counting;

#[derive(Parser)]
#[command(
    name = "pokec_profile",
    about = "Pokec expansion profiling harness (dev tool, not CI-gated)"
)]
struct Args {
    #[command(flatten)]
    pokec: PokecArgs,
}

fn main() -> Result<()> {
    relate_query_latency::pokec::run_groups(None, Args::parse().pokec.into_options())
}
