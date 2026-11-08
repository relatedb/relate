//! The deterministic synthetic pokec-shaped store (`pokec`): the
//! in-memory fixture the integration, small-perf and validation suites
//! open (owner decision 2026-11-07). The real SNAP soc-pokec data never
//! enters this repository (the nothing-vendored rule, ADR-0243) — the
//! fixture reproduces the *shape* the traversal and kernel work is
//! measured against: one `:User` label (`id`, `completion_percentage`,
//! `gender`, `age`), one directed `:Friend` relationship type, a
//! heavy-tailed degree distribution with hubs, ages almost always young
//! (the `age > 50` filter class stays a small-answer query), and the two
//! RANGE indexes the benchmark store carries (`user_id` on `User(id)`,
//! `user_age` on `User(age)`).
//!
//! Determinism is the fixture's contract: one [`Rng`] stream (LABS-934)
//! drives every draw, in a fixed order, so the same `PokecShape` opens a
//! graph with the same ids, properties and edges on every run and every
//! machine — a test that pins a row today pins it tomorrow. Two shapes
//! with the same `nodes`/`rels` but different seeds are different graphs;
//! tests that compare graphs must say which seed they mean.
//!
//! The store is `:memory:` and populated through the public facade
//! (`query_with_params` over `UNWIND $rows` batches), never through
//! internal graph APIs: what the fixture builds is exactly what an
//! application could build, and the population path itself is exercised
//! wherever a suite opens the fixture.

use std::collections::BTreeMap;

use relate::{Connection, Database, Value};

use crate::rng::Rng;

/// One synthetic pokec shape: how many `:User` nodes, how many directed
/// `:Friend` relationships, and the seed that pins every draw.
///
/// Constructing a custom shape is fine (perf sweeps over sizes); the
/// presets are the shapes other suites compare against, so a test that
/// wants comparability names a preset instead of hand-rolling numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PokecShape {
    /// `:User` nodes, ids `1..=nodes`.
    pub nodes: usize,
    /// directed `:Friend` relationships: distinct ordered pairs, no
    /// self-loops.
    pub rels: usize,
    /// the [`Rng`] seed: the whole graph is a function of
    /// `(nodes, rels, seed)`.
    pub seed: u64,
}

impl PokecShape {
    /// The unit-test shape: small enough that every assertion opens its
    /// own store in milliseconds, large enough that hubs, filters and
    /// multi-hop expansion all mean something.
    pub const TINY: PokecShape = PokecShape {
        nodes: 200,
        rels: 1_000,
        seed: 0x706f_6b65_6301,
    };

    /// traverse-bench small parity: 10,000 users, 121,716 friendships —
    /// the counts of the benchmark's `small` variant, on a synthetic
    /// graph.
    pub const SMALL: PokecShape = PokecShape {
        nodes: 10_000,
        rels: 121_716,
        seed: 0x706f_6b65_6302,
    };

    /// traverse-bench medium parity: 100,000 users, 1,768,164 friendships.
    /// Perf runs only — the open cost is the population cost.
    pub const MEDIUM: PokecShape = PokecShape {
        nodes: 100_000,
        rels: 1_768_164,
        seed: 0x706f_6b65_6303,
    };
}

/// How many `UNWIND $rows` maps one population statement carries. Bounds
/// the single statement's expression build (ADR-0042's per-value cap)
/// while keeping the statement count — and the per-statement plan and
/// commit overhead — out of the picture.
const BATCH: usize = 2_500;

/// The friendship stream's seed mix-in, so the two streams (users,
/// friendships) never alias one another's draws.
const FRIEND_STREAM: u64 = 0x9e37_79b9_7f4a_7c15;

/// The endpoint weights' tail: node `i` (1-based) weighs
/// `FRIEND_WEIGHT_SCALE / (i²)^(1/5)`, i.e. `i^-0.4` over an integer grid.
/// The exponent is 2/5 so the weight is an **integer 5th root** ([`iroot5`])
/// and the table is byte-identical on every platform — a float `powf` here
/// is libm-dependent, and one unit of drift in the cumulative total changes
/// the modulus every draw reads, reshuffling the whole graph (rust-review,
/// round 3: the documented "same graph on every machine" contract).
///
/// The exponent is calibrated against the real store the fixture stands in
/// for: traverse-bench small measures **max degree 426** over a directed
/// mean of 12.17, because the tail is what multi-hop expansion cost hangs
/// on. `1.1` (the first cut) put ~10% of every relationship into node 1 — a
/// max degree of ~10,000 and 2-hop expansions answering 7,476 rows where
/// the real store answers a few hundred. The integer `0.4` measures max
/// degree 290, 14 users over 50 (the real store's 12) and 180 distinct
/// 2-hop neighbours from one node — the same shape, and
/// `the_max_degree_stays_in_band` keeps it there.
const FRIEND_WEIGHT_SCALE: u64 = 1_000_000;

/// Open a `:memory:` store holding the shape's synthetic pokec graph:
/// the `:User` nodes, the two RANGE indexes, then the `:Friend`
/// relationships (each endpoint matched through the `user_id` index,
/// exactly as the benchmark's import statements do). A store whose counts
/// do not match the shape is never handed out — that is a fixture bug,
/// and it panics with both numbers.
pub fn open(shape: PokecShape) -> relate::Result<Database> {
    // The shape's own invariants, checked once here so every draw below
    // has a proven capacity: the relationship loop terminates only while
    // distinct ordered pairs remain, which `nodes * (nodes - 1)` bounds.
    assert!(
        shape.nodes >= 1 && shape.rels <= shape.nodes * shape.nodes.saturating_sub(1),
        "pokec fixture: shape {} nodes / {} rels cannot hold that many distinct pairs",
        shape.nodes,
        shape.rels
    );
    let db = Database::open(":memory:")?;
    let mut conn = db.connect()?;
    for chunk in users(shape).chunks(BATCH) {
        unwind(
            &mut conn,
            chunk,
            "CREATE (:User {id: r.id, completion_percentage: r.completion_percentage, \
             gender: r.gender, age: r.age})",
        )?;
    }
    conn.query("CREATE INDEX user_id FOR (n:User) ON (n.id)")?;
    conn.query("CREATE INDEX user_age FOR (n:User) ON (n.age)")?;
    for chunk in friends(shape).chunks(BATCH) {
        unwind(
            &mut conn,
            chunk,
            "MATCH (a:User {id: r.f}), (b:User {id: r.t}) CREATE (a)-[:Friend]->(b)",
        )?;
    }
    let got_users = count(&mut conn, "MATCH (n:User) RETURN count(n)")?;
    let got_rels = count(&mut conn, "MATCH ()-[e:Friend]->() RETURN count(e)")?;
    assert_eq!(
        (got_users, got_rels),
        (shape.nodes as i64, shape.rels as i64),
        "pokec fixture: the populated store does not match the shape"
    );
    Ok(db)
}

/// The synthetic users: `(id, completion_percentage, gender, age)` for
/// `id` in `1..=nodes`, drawn in id order from the one stream.
fn users(shape: PokecShape) -> Vec<Value> {
    let mut rng = Rng(shape.seed);
    let mut rows = Vec::with_capacity(shape.nodes);
    for id in 1..=shape.nodes as u64 {
        // Ages mirror the real skew: almost everyone young, so the
        // `age > 50` filter class stays a small-answer query (the real
        // small variant has 12 of 10,000 users over 50). One age draw,
        // offset by the old-user coin: two arms drawing separately would
        // silently desynchronize the stream if either range moved.
        let old = rng.below(800) == 0;
        let age = 1 + rng.below(50) + if old { 50 } else { 0 };
        rows.push(map([
            ("id", Value::Int(id as i64)),
            ("completion_percentage", Value::Int(rng.below(101) as i64)),
            (
                "gender",
                Value::String(if rng.below(2) == 0 { "man" } else { "woman" }.into()),
            ),
            ("age", Value::Int(age as i64)),
        ]));
    }
    rows
}

/// The synthetic friendships: `rels` distinct ordered `(f, t)` pairs over
/// `1..=nodes`, no self-loops, both endpoints drawn from one
/// heavy-tailed (Zipf-style) distribution so hubs exist and most nodes
/// stay low-degree — the property multi-hop expansion cost hangs on.
fn friends(shape: PokecShape) -> Vec<Value> {
    // The draw weights: node i (1-based) weighs the constant's integer
    // expression, accumulated into the u64 table the draws binary-search.
    // The weight's shape and its calibration live on the constant — never
    // restated here, where they would drift the moment it moves. The sum
    // grows like `SCALE · n^0.6` (a few 10^10 at any shape a machine
    // holds), so the accumulation cannot overflow — and says so instead of
    // capping silently.
    let mut cumulative = Vec::with_capacity(shape.nodes);
    let mut total: u64 = 0;
    for i in 1..=shape.nodes as u64 {
        total = total
            .checked_add(weight(i))
            .expect("the cumulative pokec weight fits u64 for every shape");
        cumulative.push(total);
    }
    let mut rng = Rng(shape.seed ^ FRIEND_STREAM);
    let mut seen = std::collections::HashSet::with_capacity(shape.rels);
    let mut rows = Vec::with_capacity(shape.rels);
    while rows.len() < shape.rels {
        let f = draw(&cumulative, &mut rng);
        let t = draw(&cumulative, &mut rng);
        // One word pair per attempt: redraws stay on the same stream.
        if f == t || !seen.insert((f, t)) {
            continue;
        }
        rows.push(map([
            ("f", Value::Int(f as i64)),
            ("t", Value::Int(t as i64)),
        ]));
    }
    rows
}

/// Node `i`'s (1-based) draw weight: `FRIEND_WEIGHT_SCALE / (i²)^(1/5)`,
/// integer throughout so the table cannot differ across platforms. Every
/// weight is at least `FRIEND_WEIGHT_SCALE / iroot5(nodes²)`, strictly
/// positive for every node a shape can hold — the accumulation and the
/// draw both rely on that, so no floor guards it.
fn weight(i: u64) -> u64 {
    FRIEND_WEIGHT_SCALE / iroot5(i * i)
}

/// The integer 5th root of `n` (floor) — the platform-independent
/// stand-in for `n.powf(0.2)`. Total and exact over every `u64`: the
/// upper bound doubles until its 5th power no longer fits or reaches `n`,
/// and the search compares through `checked_pow`, where a power that
/// overflows `u64` is **greater** than `n` (a saturated comparison would
/// read it as equal and answer too large — the near-`u64::MAX` case
/// `the_integer_fifth_root_is_exact` pins).
fn iroot5(n: u64) -> u64 {
    let mut hi = 1u64;
    while hi.checked_pow(5).is_some_and(|p| p < n) {
        hi *= 2;
    }
    let mut lo = 0u64;
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if mid.checked_pow(5).is_some_and(|p| p <= n) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// One endpoint draw: a uniform word binary-searched into the cumulative
/// weight table, answering a 1-based node id. The table carries its own
/// length and total, so neither is a parameter: `partition_point` over a
/// non-empty table answers `0..len`, and `pick` is drawn below the last
/// entry, so the index needs no clamp.
fn draw(cumulative: &[u64], rng: &mut Rng) -> u64 {
    let total = *cumulative
        .last()
        .expect("a shape's cumulative table is never empty");
    let pick = rng.below(total);
    let idx = cumulative.partition_point(|w| *w <= pick);
    debug_assert!(idx < cumulative.len());
    idx as u64 + 1
}

/// Run one `UNWIND $rows AS r <statement>` over the chunk's rows.
fn unwind(conn: &mut Connection, rows: &[Value], statement: &str) -> relate::Result<()> {
    let mut params = BTreeMap::new();
    params.insert("rows".to_string(), Value::List(rows.to_vec().into()));
    conn.query_with_params(&format!("UNWIND $rows AS r {statement}"), params)?;
    Ok(())
}

/// One `count(...)` scalar, as `i64` — the fixture's own reads are all
/// counts. Engine failures stay engine failures (`?`); a count that is not
/// one integer is a fixture bug and panics.
fn count(conn: &mut Connection, cypher: &str) -> relate::Result<i64> {
    match conn.query(cypher)?.rows.first() {
        Some(row) => match row.first() {
            Some(Value::Int(n)) => Ok(*n),
            other => panic!("pokec fixture: {cypher} did not answer one integer (got {other:?})"),
        },
        None => panic!("pokec fixture: {cypher} answered no rows"),
    }
}

/// One row map from key/value pairs — the parameter rows' one spelling.
fn map<'a>(pairs: impl IntoIterator<Item = (&'a str, Value)>) -> Value {
    Value::Map(
        pairs
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect::<BTreeMap<_, _>>()
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two opens of the same shape are the same graph at query level:
    /// the whole deterministic contract, pinned by rows rather than by
    /// internals.
    #[test]
    fn the_same_shape_opens_the_same_graph() {
        let probe = "MATCH (s:User {id: 7})-[:Friend]->(n:User) RETURN n.id ORDER BY n.id";
        let rows = |db: Database| db.connect().unwrap().query(probe).unwrap().rows;
        assert_eq!(
            rows(open(PokecShape::TINY).unwrap()),
            rows(open(PokecShape::TINY).unwrap())
        );
    }

    /// A different seed is a different graph — the seed is
    /// load-bearing, not decoration.
    #[test]
    fn a_different_seed_is_a_different_graph() {
        let other = PokecShape {
            seed: PokecShape::TINY.seed + 1,
            ..PokecShape::TINY
        };
        let probe = "MATCH ()-[e:Friend]->() RETURN sum(id(startNode(e))) AS from_sum";
        let mut a = open(PokecShape::TINY).unwrap().connect().unwrap();
        let mut b = open(other).unwrap().connect().unwrap();
        assert_ne!(a.query(probe).unwrap().rows, b.query(probe).unwrap().rows);
    }

    /// The shape invariants: counts, property domains, no self-loops, and
    /// the `age > 50` class small like the real data. The degree tail has
    /// its own test (`the_max_degree_stays_in_band`).
    #[test]
    fn the_shape_holds() {
        let mut conn = open(PokecShape::TINY).unwrap().connect().unwrap();
        assert_eq!(
            count(&mut conn, "MATCH (n:User) RETURN count(n)").unwrap(),
            200
        );
        assert_eq!(
            count(&mut conn, "MATCH ()-[e:Friend]->() RETURN count(e)").unwrap(),
            1_000
        );
        assert_eq!(
            count(
                &mut conn,
                "MATCH (n:User) WHERE n.gender IN ['man', 'woman'] RETURN count(n)"
            )
            .unwrap(),
            200
        );
        assert_eq!(
            count(
                &mut conn,
                "MATCH (n:User) WHERE n.age < 1 OR n.age > 100 RETURN count(n)"
            )
            .unwrap(),
            0
        );
        // No self-loops, spelled as a predicate rather than as a repeated
        // pattern variable: the repeated-variable spelling `(a)-[:Friend]->(a)`
        // currently over-matches on the engine (LABS-1125), and this
        // fixture's own contract must not ride on that bug's fix.
        assert_eq!(
            count(
                &mut conn,
                "MATCH (a:User)-[:Friend]->(b:User) WHERE a = b RETURN count(*)"
            )
            .unwrap(),
            0
        );
    }

    /// The heavy tail stays in the band the real store occupies: a hub
    /// several times the mean degree, but no node absorbing the graph.
    /// The real traverse-bench small store measures max 426 over a
    /// directed mean of 12.17 (35x); a tail exponent that drifts toward 1
    /// puts a single node on ~10,000 edges and inflates every expansion
    /// answer by an order of magnitude, which is what this pins.
    #[test]
    fn the_max_degree_stays_in_band() {
        let mut conn = open(PokecShape::TINY).unwrap().connect().unwrap();
        let max_deg = count(
            &mut conn,
            "MATCH (n:User)-[:Friend]->(m:User) WITH n, count(m) AS deg RETURN max(deg)",
        )
        .unwrap();
        let mean_deg = PokecShape::TINY.rels as f64 / PokecShape::TINY.nodes as f64;
        let ratio = max_deg as f64 / mean_deg;
        assert!(
            (5.0..=60.0).contains(&ratio),
            "max degree {max_deg} is {ratio:.1}x the mean {mean_deg} — outside the real store's band"
        );
    }

    /// The integer 5th root is exact floor for every input the search can
    /// meet, the boundary powers included, and it keeps answering past the
    /// point a fixed upper bound would have clamped.
    #[test]
    fn the_integer_fifth_root_is_exact() {
        for (n, want) in [(0, 0), (1, 1), (31, 1), (32, 2), (2u64.pow(50), 1024)] {
            assert_eq!(iroot5(n), want, "iroot5({n})");
        }
        // The property, over values a shape can produce and values far past
        // any fixed bound: the root's 5th power is at most `n`, and the next
        // integer's is past it.
        for n in [
            2u64,
            33,
            1_000,
            1_000_000,
            2u64.pow(40),
            2u64.pow(50),
            u64::MAX,
        ] {
            let r = iroot5(n);
            assert!(
                r.checked_pow(5).is_some_and(|p| p <= n),
                "iroot5({n}) = {r} overshoots"
            );
            assert!(
                (r + 1).checked_pow(5).is_none_or(|p| p > n),
                "iroot5({n}) = {r} undershoots"
            );
        }
    }

    /// The indexes exist and are ONLINE — the fixture must resolve
    /// endpoint `MATCH`es the way the benchmark store does. Named
    /// columns, never a positional scan of the catalogue row.
    #[test]
    fn the_indexes_are_online() {
        let mut conn = open(PokecShape::TINY).unwrap().connect().unwrap();
        let online = conn
            .query("SHOW INDEXES YIELD name, state WHERE state = 'ONLINE' RETURN name")
            .unwrap();
        let names: Vec<String> = online
            .rows
            .iter()
            .map(|row| match &row[0] {
                Value::String(s) => s.to_string(),
                other => panic!("SHOW INDEXES YIELD name answered {other:?}"),
            })
            .collect();
        assert!(
            names.contains(&"user_id".to_string()),
            "online indexes: {names:?}"
        );
        assert!(
            names.contains(&"user_age".to_string()),
            "online indexes: {names:?}"
        );
    }

    /// `MEDIUM` exists for perf runs, not for the default suite: opening
    /// it is the population cost of 100,000 users / 1,768,164
    /// friendships. Ignored so it never slows the suite, runnable on
    /// demand by a perf sweep (`--ignored --nocapture`), which is the
    /// consumer the preset exists for.
    #[test]
    #[ignore = "perf runs only: opens the 100k/1.77M shape"]
    fn medium_opens_for_perf_runs() {
        let t = std::time::Instant::now();
        let db = open(PokecShape::MEDIUM).unwrap();
        eprintln!(
            "pokec fixture: open(MEDIUM) took {:.1}s",
            t.elapsed().as_secs_f64()
        );
        let mut conn = db.connect().unwrap();
        assert_eq!(
            count(&mut conn, "MATCH (n:User) RETURN count(n)").unwrap(),
            PokecShape::MEDIUM.nodes as i64
        );
    }

    /// The recorded baseline for the perf suites: opening SMALL (the
    /// traverse-bench small counts). The assertion is a coarse ceiling —
    /// an order of magnitude, not a regression gate — and the
    /// measurement is printed for the record.
    #[test]
    fn small_opens_in_seconds() {
        let t = std::time::Instant::now();
        let _db = open(PokecShape::SMALL).unwrap();
        let secs = t.elapsed().as_secs_f64();
        eprintln!("pokec fixture: open(SMALL) took {secs:.1}s");
        assert!(
            secs < 60.0,
            "open(SMALL) took {secs:.1}s — population regressed"
        );
    }
}
