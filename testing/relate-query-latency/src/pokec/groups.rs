//! The pokec group catalogue: each measured group's query, what it binds,
//! and how many rounds it runs — the static table the runner walks.

use std::collections::BTreeMap;

use relate::Value;

/// What a group's query binds: the single shape rides here, never a sniff
/// of the query text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Binds {
    /// No parameter: one unparametrized run per round.
    None,
    /// `$id` over the start ids `1..=N`.
    Start(u64),
    /// `$id` over the start ids `1..=N` and `$to`, the start id plus
    /// [`PAIR_OFFSET`]: the two endpoints of a shortest-path pair group.
    Pair(u64),
    /// `$ids`: `len` distinct ints, one unparametrized-schedule run per round.
    /// [`BOUND_HIT_AGES`] of them are fixture ages, the rest decoys past
    /// every age, so the answer is the users in that age band.
    IntListOf(i64),
    /// `$names`: `len` distinct strings (one the real `man`, so the answer is
    /// the men), each decoy zero-padded to at least `width` digits. `width`
    /// sizes the list's total string bytes.
    StringListOf { len: i64, width: usize },
    /// `$p`: the text `abc`, the row-invariant input of the hoist baseline.
    Text,
}

/// Members of a bound-list group's list: well past the membership index's
/// build gate, the size of a client's `WHERE n.x IN $ids` batch.
const BOUND_LIST_LEN: i64 = 2000;
/// The fixture ages (`1..=BOUND_HIT_AGES`) the `$ids` list holds; ages run
/// `1..=100`, young-heavy, so this keeps roughly the young half.
const BOUND_HIT_AGES: i64 = 25;

/// One measured group: its query, what it binds, and how many rounds each
/// start id runs.
pub(super) struct Group {
    pub(super) name: &'static str,
    pub(super) query: &'static str,
    pub(super) rounds: u64,
    pub(super) binds: Binds,
}

/// `len` distinct ints: the fixture ages `1..=BOUND_HIT_AGES`, then decoys.
fn int_list(len: i64) -> Value {
    let ids = (1..=len).map(|i| Value::Int(if i <= BOUND_HIT_AGES { i } else { 1000 + i }));
    Value::List(ids.collect::<Vec<_>>().into())
}

/// `len` distinct strings, the real gender `man` mid-list, each decoy
/// zero-padded to `width` digits.
fn string_list(len: i64, width: usize) -> Value {
    let names = (0..len).map(|i| {
        let name = if i == len / 2 {
            "man".to_owned()
        } else {
            format!("decoy-{i:0>width$}")
        };
        Value::String(name.into())
    });
    Value::List(names.collect::<Vec<_>>().into())
}

/// The distance, in ids, from a pair group's start to its `$to` end: a
/// mid-range user on both fixture shapes (both hold at least 10,000
/// users), so each pair is a hub-head start against an arbitrary user.
pub(super) const PAIR_OFFSET: u64 = 5000;

/// One run of a measured group: the start id (`None` for an unparametrized
/// shape) and the parameters it binds.
pub(super) struct Run {
    pub(super) start: Option<u64>,
    pub(super) params: BTreeMap<String, Value>,
}

impl Binds {
    /// The runs one round performs — every start id with the parameters it
    /// binds, resolved by one match so the schedule and its bindings cannot
    /// drift. Bounded (at most 64 runs) and rebuilt per round; the bound
    /// lists ride an `Arc`, so a run's parameters clone for free.
    pub(super) fn runs(self) -> Vec<Run> {
        let int = |n: u64| Value::Int(i64::try_from(n).expect("start ids are small"));
        match self {
            Binds::None => vec![Run {
                start: None,
                params: BTreeMap::new(),
            }],
            Binds::Start(n) => (1..=n)
                .map(|s| Run {
                    start: Some(s),
                    params: BTreeMap::from([("id".to_owned(), int(s))]),
                })
                .collect(),
            Binds::Pair(n) => (1..=n)
                .map(|s| Run {
                    start: Some(s),
                    params: BTreeMap::from([
                        ("id".to_owned(), int(s)),
                        ("to".to_owned(), int(s + PAIR_OFFSET)),
                    ]),
                })
                .collect(),
            // Ages past 100 never occur, so the decoys start at 1000.
            Binds::IntListOf(len) => vec![Run {
                start: None,
                params: BTreeMap::from([("ids".to_owned(), int_list(len))]),
            }],
            Binds::StringListOf { len, width } => vec![Run {
                start: None,
                params: BTreeMap::from([("names".to_owned(), string_list(len, width))]),
            }],
            Binds::Text => vec![Run {
                start: None,
                params: BTreeMap::from([("p".to_owned(), Value::String("abc".into()))]),
            }],
        }
    }
}

impl Group {
    /// The runs one round performs (see [`Binds::runs`]).
    pub(super) fn runs(&self) -> Vec<Run> {
        self.binds.runs()
    }

    /// The run of the warm-up and `PROFILE` runs: a group always runs once.
    pub(super) fn first_run(&self) -> Run {
        self.runs()
            .into_iter()
            .next()
            .expect("a group runs at least once")
    }
}

/// A datetime-shaped text expression of the integer expression `$x`, built by
/// the constructor `$f`: `$base + $x / 3600` is the year, `$x / 60 % 60` the
/// minute and `$x % 60` the second, so every `$x` in a run of 3600 * 9000
/// values spells a distinct text. The `memmemo_*` groups feed it to
/// `datetime` (the hashed memo's one consumer) and, as the control with the
/// same parse cost and no memo, to `localdatetime`.
macro_rules! ts {
    ($f:literal, $base:literal, $x:literal) => {
        concat!(
            $f,
            "(toString(",
            $base,
            " + ",
            $x,
            " / 3600) + '-01-01T00:' + substring(toString(100 + (",
            $x,
            " / 60) % 60), 1) + ':' + substring(toString(100 + ",
            $x,
            " % 60), 1) + 'Z')"
        )
    };
}

/// Start ids 1.. are the fixture's hub-heavy head (i^-0.4 weights), the
/// shape multi-hop expansion cost hangs on.
pub(super) const GROUPS: [Group; 36] = [
    Group {
        name: "count",
        query: "MATCH (n:User) RETURN count(*)",
        rounds: 20,
        binds: Binds::None,
    },
    Group {
        name: "scan_agg_global",
        query: "MATCH (n:User) RETURN n.age, count(*) ORDER BY n.age",
        rounds: 20,
        binds: Binds::None,
    },
    Group {
        name: "expansion_3",
        query: "MATCH (a:User {id:$id})-[:Friend*3]->(b:User) RETURN count(b)",
        rounds: 2,
        binds: Binds::Start(64),
    },
    Group {
        name: "expansion_3_unrolled",
        query: "MATCH (a:User {id:$id})-[:Friend]->()-[:Friend]->()-[:Friend]->(b:User) RETURN count(b)",
        rounds: 2,
        binds: Binds::Start(64),
    },
    Group {
        name: "scan_expand_2hop_filter",
        query: "MATCH (a:User {id:$id})-[:Friend*2]->(b:User) WHERE b.age > 25 RETURN count(b)",
        rounds: 2,
        binds: Binds::Start(64),
    },
    Group {
        name: "shortest_pair",
        query: "MATCH p = SHORTEST 1 (a:User {id:$id})-[:Friend*..6]-(b:User {id:$to}) \
                RETURN count(p)",
        rounds: 2,
        binds: Binds::Pair(64),
    },
    Group {
        name: "shortest_path_fn",
        query: "MATCH p = shortestPath((a:User {id:$id})-[:Friend*..6]-(b:User {id:$to})) \
                RETURN count(p)",
        rounds: 2,
        binds: Binds::Pair(64),
    },
    Group {
        name: "shortest_expr",
        query: "MATCH (a:User {id:$id}), (b:User {id:$to}) \
                RETURN count(shortestPath((a)-[:Friend*..6]-(b)))",
        rounds: 2,
        binds: Binds::Pair(64),
    },
    Group {
        name: "all_shortest_paths",
        query: "MATCH p = allShortestPaths((a:User {id:$id})-[:Friend*..6]-(b:User {id:$to})) \
                RETURN count(p)",
        rounds: 2,
        binds: Binds::Pair(64),
    },
    Group {
        name: "shortest_postfix",
        query: "MATCH p = SHORTEST 1 (a:User {id:$id})-[:Friend]-{1,6}(b:User {id:$to}) \
                RETURN count(p)",
        rounds: 2,
        binds: Binds::Pair(64),
    },
    Group {
        name: "shortest_fan",
        query: "MATCH p = SHORTEST 1 (a:User {id:$id})-[:Friend*..3]-(b:User) RETURN count(p)",
        rounds: 2,
        binds: Binds::Start(16),
    },
    Group {
        name: "shortest_all_pairs",
        query: "MATCH (a:User), (b:User) \
                WHERE a.id IN range($id, $id + 7) AND b.id IN range($id + 100, $id + 107) \
                MATCH p = SHORTEST 1 (a)-[:Friend*..6]-(b) RETURN count(p)",
        rounds: 2,
        binds: Binds::Start(16),
    },
    // The expression-memo A/B groups (ab-harness): each scans a fixture
    // column through one expression class, so a memo tier's saving or cost
    // shows per class. The `memo_site_*` groups run a loop-invariant call
    // at each of the three wired sites (filter, projection, ordering key).
    Group {
        name: "memo_prop_read",
        query: "MATCH (n:User) WHERE n.completion_percentage > 50 RETURN count(n)",
        rounds: 10,
        binds: Binds::None,
    },
    Group {
        name: "memo_date_call",
        query: "MATCH (n:User) WHERE date('2024-05-05').year = 2024 AND n.age > 25 \
                RETURN count(n)",
        rounds: 10,
        binds: Binds::None,
    },
    Group {
        name: "memo_datetime_call",
        query: "MATCH (n:User) WHERE datetime('2024-05-05T10:00:00Z').hour = 10 AND n.age > 25 \
                RETURN count(n)",
        rounds: 10,
        binds: Binds::None,
    },
    // A literal `date('...')` folds at lowering, so these two feed the call a
    // per-row value (two distinct strings across the column).
    Group {
        name: "memo_date_row",
        query: "MATCH (n:User) \
                WHERE date(CASE n.gender WHEN 'man' THEN '2024-05-05' ELSE '2023-01-01' END).year \
                = 2024 RETURN count(n)",
        rounds: 10,
        binds: Binds::None,
    },
    Group {
        name: "memo_datetime_row",
        query: "MATCH (n:User) \
                WHERE datetime(CASE n.gender WHEN 'man' THEN '2024-05-05T10:00:00Z' \
                ELSE '2023-01-01T09:00:00Z' END).hour = 10 RETURN count(n)",
        rounds: 10,
        binds: Binds::None,
    },
    Group {
        name: "memo_labels",
        query: "MATCH (n:User) WHERE 'User' IN labels(n) RETURN count(n)",
        rounds: 10,
        binds: Binds::None,
    },
    Group {
        name: "memo_in_list",
        query: "MATCH (n:User) WHERE n.age IN [20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30] \
                RETURN count(n)",
        rounds: 10,
        binds: Binds::None,
    },
    Group {
        name: "memo_regex",
        query: "MATCH (n:User) WHERE n.gender =~ 'm.*' RETURN count(n)",
        rounds: 10,
        binds: Binds::None,
    },
    Group {
        name: "memo_hub_star",
        query: "MATCH (a:User)-[:Friend]->(b:User), (a)<-[:Friend]-(b) \
                WHERE b.gender =~ 'm.*' \
                RETURN count(b)",
        rounds: 2,
        binds: Binds::None,
    },
    Group {
        name: "memo_site_where",
        query: "MATCH (n:User) WHERE n.age > toInteger(date('2024-05-05').year / 100) \
                RETURN count(n)",
        rounds: 10,
        binds: Binds::None,
    },
    Group {
        name: "memo_site_project",
        query: "MATCH (n:User) RETURN sum(n.age + date('2024-05-05').month)",
        rounds: 10,
        binds: Binds::None,
    },
    Group {
        name: "memo_site_order",
        query: "MATCH (n:User) WITH n ORDER BY n.age + date('2024-05-05').month, n.id LIMIT 1 \
                RETURN n.id",
        rounds: 10,
        binds: Binds::None,
    },
    // The bound-list membership groups (the IN index): `x IN $list` over a
    // 2000-member client list, ints over the indexed `age` and strings over
    // the stored `gender`.
    Group {
        name: "in_param_ints",
        query: "MATCH (n:User) WHERE n.age IN $ids RETURN count(n)",
        rounds: 10,
        binds: Binds::IntListOf(BOUND_LIST_LEN),
    },
    Group {
        name: "in_param_strings",
        query: "MATCH (n:User) WHERE n.gender IN $names RETURN count(n)",
        rounds: 10,
        binds: Binds::StringListOf { len: BOUND_LIST_LEN, width: 0 },
    },
    // The memory groups (`memmemo_*`): they stress what a memo tier retains,
    // not how fast it answers. Read the `live_peak` column (the counting
    // allocator's high-water live bytes over the statement, minus the live
    // bytes before it) and `rss_peak` (run one group per process); the
    // `_control` twin of a group runs the same statement shape on an
    // expression no memo wraps, so the difference is the memo's cost.
    //
    // Group 1: 300,000 distinct datetime texts, far past what the 8 MiB
    // per-table ceiling holds, so the table fills, stops inserting and keeps
    // missing.
    Group {
        name: "memmemo_datetime_distinct",
        query: concat!(
            "UNWIND range(1, 300000) AS i WITH i WHERE ",
            ts!("datetime", "1000", "i"),
            ".year >= 1000 RETURN count(i)"
        ),
        rounds: 3,
        binds: Binds::None,
    },
    Group {
        name: "memmemo_datetime_control",
        query: concat!(
            "UNWIND range(1, 300000) AS i WITH i WHERE ",
            ts!("localdatetime", "1000", "i"),
            ".year >= 1000 RETURN count(i)"
        ),
        rounds: 3,
        binds: Binds::None,
    },
    // Group 2: a distinct regex per row (10,000 patterns against the
    // 8-pattern cache): every probe evicts and recompiles.
    Group {
        name: "memmemo_regex_distinct",
        query: "MATCH (n:User) WHERE n.gender =~ ('man|u' + toString(n.id)) RETURN count(n)",
        rounds: 3,
        binds: Binds::None,
    },
    // Group 3: large bound lists. Ints and strings at the index's 65,536
    // member cap; then 40,000 38-byte strings (1.5 MiB), just over the
    // index's 1 MiB string-byte cap, which must decline to the scan.
    Group {
        name: "memmemo_in_ints_64k",
        query: "MATCH (n:User) WHERE n.age IN $ids RETURN count(n)",
        rounds: 3,
        binds: Binds::IntListOf(65_536),
    },
    Group {
        name: "memmemo_in_strings_64k",
        query: "MATCH (n:User) WHERE n.gender IN $names RETURN count(n)",
        rounds: 3,
        binds: Binds::StringListOf { len: 65_536, width: 0 },
    },
    Group {
        name: "memmemo_in_strings_overcap",
        query: "MATCH (n:User) WHERE n.gender IN $names RETURN count(n)",
        rounds: 3,
        binds: Binds::StringListOf { len: 40_000, width: 32 },
    },
    // Group 4: several memo sites in one statement over a hub expansion: the
    // regex cache, the 64k IN index and a per-edge distinct datetime, so the
    // sites share the 16 MiB per-query budget.
    Group {
        name: "memmemo_sites_combined",
        query: concat!(
            "MATCH (a:User)-[:Friend]->(b:User), (a)<-[:Friend]-(b) ",
            "WHERE b.gender =~ 'm.*' AND b.age IN $ids AND ",
            ts!("datetime", "1000", "(a.id * 7 + b.id)"),
            ".year >= 1000 RETURN count(b)"
        ),
        rounds: 2,
        binds: Binds::IntListOf(65_536),
    },
    // Group 5: three WITH stages, each with its own datetime memo over its
    // own distinct texts (100,000 per stage), the same budget under three
    // tables. The harness holds one connection, so concurrent statements are
    // not measured.
    Group {
        name: "memmemo_with_chain",
        query: concat!(
            "UNWIND range(1, 100000) AS i WITH i WHERE ",
            ts!("datetime", "1000", "i"),
            ".year >= 1000 WITH i WHERE ",
            ts!("datetime", "2000", "i"),
            ".year >= 1000 WITH i WHERE ",
            ts!("datetime", "3000", "i"),
            ".year >= 1000 RETURN count(i)"
        ),
        rounds: 3,
        binds: Binds::None,
    },
    // Group 6: the row-invariant baseline a future hoist compares against:
    // the same constant sub-expression in WHERE and RETURN over every row.
    Group {
        name: "memmemo_invariant_heavy",
        query: "MATCH (n:User) WHERE toUpper($p) + toString(1 + 2) = 'ABC3' AND n.age > 0 \
                RETURN sum(size(toUpper($p) + toString(1 + 2)))",
        rounds: 10,
        binds: Binds::Text,
    },
    Group {
        name: "memmemo_invariant_heavy_x4",
        query: "MATCH (n:User) WHERE toUpper($p) + toString(1 + 2) = 'ABC3' \
                AND size(toUpper($p) + toString(1 + 2)) = 4 AND n.age > 0 \
                RETURN sum(size(toUpper($p) + toString(1 + 2)) \
                + size(toLower(toUpper($p)) + toString(1 + 2)))",
        rounds: 10,
        binds: Binds::Text,
    },
];
