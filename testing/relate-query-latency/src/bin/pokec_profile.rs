//! Pokec-shaped expansion profiling harness (LABS-1116).
//!
//! A dev tool (not CI-gated), the `plan_bench` precedent: runs the
//! traverse-bench pokec query groups against the synthetic pokec fixture
//! (`relate_test_support::pokec`) as one self-terminating process a
//! sampling profiler (samply) can record. Per group it prints wall time,
//! the total row count, and a checksum — the fixture is deterministic,
//! so identical checksums before and after a change are the correctness
//! cross-check for a perf edit.
//!
//! Usage: `cargo run --release -p relate-query-latency --bin
//! pokec_profile -- [small|medium]`.
//!
//! * `small` — 10,000 users / 121,716 friendships, opens in seconds.
//! * `medium` — 100,000 users / 1,768,164 friendships; the open cost is
//!   the population cost.
//! * Any other argument (or none) is a usage error; there is no default
//!   shape.
//! * No flags, no env vars beyond cargo's own.
//!
//! Record a profile: `samply record -s -n -o prof.json.gz -- <binary> small`.
//! Build with frame pointers for trustworthy stacks
//! (`RUSTFLAGS="-C force-frame-pointers=yes"`).
use std::collections::BTreeMap;
use std::time::Instant;

use relate::{Connection, Result, Value};
use relate_test_support::pokec::{open, PokecShape};

/// One measured group: its query, how many start ids (`1..=starts`) feed
/// the `$id` parameter, and how many rounds each start id runs. `None`
/// marks the queries that take no parameter — the param shape rides
/// here, never a sniff of the query text.
struct Group {
    name: &'static str,
    query: &'static str,
    rounds: u64,
    starts: Option<u64>,
}

fn main() -> Result<()> {
    let shape = match std::env::args().nth(1).as_deref() {
        Some("small") => PokecShape::SMALL,
        Some("medium") => PokecShape::MEDIUM,
        other => {
            eprintln!(
                "usage: pokec_profile [small|medium] — which pokec shape to open (got {other:?})"
            );
            std::process::exit(2);
        }
    };
    let db = open(shape)?;
    let mut conn = db.connect()?;

    // Start ids 1.. are the fixture's hub-heavy head (i^-0.4 weights),
    // the shape multi-hop expansion cost hangs on.
    let groups = [
        Group {
            name: "count",
            query: "MATCH (n:User) RETURN count(*)",
            rounds: 20,
            starts: None,
        },
        Group {
            name: "scan_agg_global",
            query: "MATCH (n:User) RETURN n.age, count(*) ORDER BY n.age",
            rounds: 20,
            starts: None,
        },
        Group {
            name: "expansion_3",
            query: "MATCH (a:User {id:$id})-[:Friend*3]->(b:User) RETURN count(b)",
            rounds: 2,
            starts: Some(64),
        },
        Group {
            name: "scan_expand_2hop_filter",
            query: "MATCH (a:User {id:$id})-[:Friend*2]->(b:User) WHERE b.age > 25 RETURN count(b)",
            rounds: 2,
            starts: Some(64),
        },
    ];
    for g in &groups {
        // One warm round of the group's own query — primes its plans and
        // caches so the measured rounds are steady-state. Propagated like
        // a measured round: a warm-up failure is a harness failure.
        warm(&mut conn, g)?;
        let t = Instant::now();
        let mut rows = 0i64;
        let mut checksum = 0i64;
        for r in 0..g.rounds {
            // starts == None runs the query once per round, unparametrized.
            let ids = match g.starts {
                Some(n) => 1..=n,
                None => 0..=0,
            };
            for s in ids {
                let out = run(&mut conn, g, s)?;
                rows += out;
                checksum = checksum
                    .wrapping_add(out.wrapping_mul(0x9e37_79b9))
                    .wrapping_add(r as i64)
                    .wrapping_add(s as i64);
            }
        }
        println!(
            "{}: {:.1} ms rows={} checksum={}",
            g.name,
            t.elapsed().as_millis(),
            rows,
            checksum
        );
    }
    Ok(())
}

/// One warm round of `g`'s own query — its first start id, or the single
/// unparametrized run. Not counted in the measurements.
fn warm(conn: &mut Connection, g: &Group) -> Result<()> {
    let first = g.starts.map_or(0, |n| n.min(1));
    run(conn, g, first).map(|_| ())
}

/// Run `g`'s query once at start id `s` (ignored when the group takes no
/// parameter) and return its one integer answer.
fn run(conn: &mut Connection, g: &Group, s: u64) -> Result<i64> {
    let out = match g.starts {
        Some(_) => {
            let mut params = BTreeMap::new();
            params.insert("id".to_string(), Value::Int(s as i64));
            conn.query_with_params(g.query, params)?
        }
        None => conn.query(g.query)?,
    };
    match out.rows.first().and_then(|row| row.first()) {
        Some(Value::Int(n)) => Ok(*n),
        other => panic!(
            "pokec_profile: {:#?} did not answer one integer (got {other:?})",
            g.query
        ),
    }
}
