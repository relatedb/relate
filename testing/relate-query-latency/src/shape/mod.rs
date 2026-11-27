//! The shape matrix (REL8-2242 step 3): the same variable-length / QPP
//! query timed over many graph and query shapes, so a change to the
//! walker (the QPP memo) is judged on more than the social-network Q3/Q4.
//!
//! One [`Cell`] is one point of the matrix: the start distinctness, the
//! out-degree per hop, the hop bound, the typing, the topology, the
//! direction and the consumer. The graph a cell needs is [`graph`]'s; the
//! timing, memory and report are [`run`]'s. The matrix is a baseline cell
//! plus one-axis-at-a-time variations and a few crossings — the full cross
//! product (thousands of cells) would not finish in minutes.

pub mod graph;
pub mod run;

/// How many distinct start nodes the driver rows share: the axis the memo
/// pays back on (repeated starts) or must not regress on (all unique).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Starts {
    One,
    Ten,
    Thousand,
    /// One start per driver row.
    Unique,
}

impl Starts {
    pub const ALL: [Starts; 4] = [Starts::One, Starts::Ten, Starts::Thousand, Starts::Unique];

    pub fn name(self) -> &'static str {
        match self {
            Starts::One => "d1",
            Starts::Ten => "d10",
            Starts::Thousand => "d1k",
            Starts::Unique => "dall",
        }
    }

    /// The distinct start count for `persons` driver rows (never more
    /// than the rows).
    pub fn distinct(self, persons: usize) -> usize {
        match self {
            Starts::One => 1,
            Starts::Ten => 10,
            Starts::Thousand => 1000,
            Starts::Unique => persons,
        }
        .min(persons)
    }
}

/// The hop shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hops {
    /// `*1..2`
    OneTwo,
    /// `*1..3`
    OneThree,
    /// `*2..4`
    TwoFour,
    /// A quantified path pattern with inner nodes, `{1,3}`.
    Qpp,
}

impl Hops {
    pub const ALL: [Hops; 4] = [Hops::OneTwo, Hops::OneThree, Hops::TwoFour, Hops::Qpp];

    pub fn name(self) -> &'static str {
        match self {
            Hops::OneTwo => "h1-2",
            Hops::OneThree => "h1-3",
            Hops::TwoFour => "h2-4",
            Hops::Qpp => "qpp1-3",
        }
    }

    /// The `(low, high)` repetition bounds.
    pub fn bounds(self) -> (u32, u32) {
        match self {
            Hops::OneTwo => (1, 2),
            Hops::OneThree | Hops::Qpp => (1, 3),
            Hops::TwoFour => (2, 4),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Typing {
    Typed,
    Untyped,
    /// Typed relationship, the far end constrained to a label alternation.
    LabelAlt,
}

impl Typing {
    pub const ALL: [Typing; 3] = [Typing::Typed, Typing::Untyped, Typing::LabelAlt];

    pub fn name(self) -> &'static str {
        match self {
            Typing::Typed => "typed",
            Typing::Untyped => "untyped",
            Typing::LabelAlt => "labelalt",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Topology {
    /// Divergent trees, one per start: out-degree `f`, no sharing (with
    /// `f = 1` the Q3 chain City->State->Country shape).
    Tree,
    /// A small dense core every start enters: paths revisit nodes.
    Cycles,
    /// The dense core with a skewed (cubic) start frequency: a few hot
    /// starts carry most driver rows.
    Hubs,
}

impl Topology {
    pub const ALL: [Topology; 3] = [Topology::Tree, Topology::Cycles, Topology::Hubs];

    pub fn name(self) -> &'static str {
        match self {
            Topology::Tree => "tree",
            Topology::Cycles => "cycles",
            Topology::Hubs => "hubs",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Directed,
    Undirected,
}

impl Direction {
    pub fn name(self) -> &'static str {
        match self {
            Direction::Directed => "dir",
            Direction::Undirected => "undir",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Consumer {
    /// Every row returned.
    Full,
    /// `LIMIT 10`: early halt.
    Limit,
    /// `count(*)`.
    Count,
}

impl Consumer {
    pub const ALL: [Consumer; 3] = [Consumer::Full, Consumer::Limit, Consumer::Count];

    pub fn name(self) -> &'static str {
        match self {
            Consumer::Full => "full",
            Consumer::Limit => "limit",
            Consumer::Count => "count",
        }
    }
}

/// One matrix point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub starts: Starts,
    pub fanout: u32,
    pub hops: Hops,
    pub typing: Typing,
    pub topology: Topology,
    pub direction: Direction,
    pub consumer: Consumer,
}

impl Cell {
    /// The cell every axis variation departs from.
    pub const BASELINE: Cell = Cell {
        starts: Starts::Ten,
        fanout: 3,
        hops: Hops::OneThree,
        typing: Typing::Typed,
        topology: Topology::Tree,
        direction: Direction::Directed,
        consumer: Consumer::Count,
    };

    /// The stable cell id: every axis, so a report row is self-describing
    /// and two runs join on it.
    pub fn id(&self) -> String {
        format!(
            "{}.f{}.{}.{}.{}.{}.{}",
            self.topology.name(),
            self.fanout,
            self.starts.name(),
            self.hops.name(),
            self.typing.name(),
            self.direction.name(),
            self.consumer.name()
        )
    }

    /// The statement. Driver rows `p:P` reach their start `c:C` over `AT`;
    /// the walk under test runs from `c`. Nothing is parameterized: the
    /// shape, not the parameter path, is under test.
    pub fn query(&self) -> String {
        let (lo, hi) = self.hops.bounds();
        let (left, right) = match self.direction {
            Direction::Directed => ("-", "->"),
            Direction::Undirected => ("-", "-"),
        };
        let walk = match (self.hops, self.typing) {
            (Hops::Qpp, typing) => {
                let rel = if typing == Typing::Untyped { "" } else { ":E" };
                let end = if typing == Typing::LabelAlt {
                    "b:V1|V2"
                } else {
                    "b"
                };
                format!(" ((a){left}[{rel}]{right}({end})){{{lo},{hi}}} ")
            }
            (_, Typing::Untyped) => format!("{left}[*{lo}..{hi}]{right}"),
            (_, _) => format!("{left}[:E*{lo}..{hi}]{right}"),
        };
        let target = match (self.hops, self.typing) {
            (Hops::Qpp, _) => "(t)",
            (_, Typing::LabelAlt) => "(t:V1|V2)",
            _ => "(t)",
        };
        let tail = match self.consumer {
            Consumer::Full => "RETURN p.id AS p, t.id AS t",
            Consumer::Limit => "RETURN p.id AS p, t.id AS t LIMIT 10",
            Consumer::Count => "RETURN count(*) AS n",
        };
        format!("MATCH (p:P)-[:AT]->(c:C){walk}{target} {tail}")
    }

    /// The walker's estimated path visits per start: the paths up to the
    /// hop bound. A budget gate, not a prediction: an undirected hop sees
    /// both directions, a dense core shares nodes.
    pub fn visits_per_start(&self) -> f64 {
        let (_, hi) = self.hops.bounds();
        let degree = f64::from(self.fanout)
            * match self.direction {
                Direction::Directed => 1.0,
                Direction::Undirected => 2.0,
            };
        (1..=hi).map(|h| degree.powi(h as i32)).sum()
    }
}

/// The matrix: the baseline, each axis varied alone, and the crossings
/// that matter for the memo (start distinctness against fan-out, and the
/// hub-skewed starts).
pub fn matrix() -> Vec<Cell> {
    let b = Cell::BASELINE;
    let mut cells = vec![b];
    cells.extend(Starts::ALL.map(|starts| Cell { starts, ..b }));
    cells.extend([1, 3, 10, 50].map(|fanout| Cell { fanout, ..b }));
    cells.extend(Hops::ALL.map(|hops| Cell { hops, ..b }));
    cells.extend(Typing::ALL.map(|typing| Cell { typing, ..b }));
    cells.extend(Topology::ALL.map(|topology| Cell { topology, ..b }));
    cells.push(Cell {
        direction: Direction::Undirected,
        ..b
    });
    cells.extend(Consumer::ALL.map(|consumer| Cell { consumer, ..b }));
    // Crossings: the repeated-start win and the unique-start regression
    // guard, at low and high fan-out, on the Q3 chain and on a QPP.
    for starts in [Starts::One, Starts::Unique] {
        for fanout in [1, 10] {
            cells.push(Cell {
                starts,
                fanout,
                topology: Topology::Cycles,
                ..b
            });
            cells.push(Cell {
                starts,
                fanout,
                hops: Hops::Qpp,
                ..b
            });
        }
    }
    cells.push(Cell {
        topology: Topology::Hubs,
        fanout: 10,
        consumer: Consumer::Full,
        ..b
    });
    cells.sort_by_key(Cell::id);
    cells.dedup();
    cells
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_ids_are_unique_and_the_baseline_is_in_it() {
        let cells = matrix();
        let mut ids: Vec<String> = cells.iter().map(Cell::id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), cells.len(), "two cells share an id");
        assert!(cells.contains(&Cell::BASELINE));
    }

    #[test]
    fn the_baseline_query_is_the_typed_directed_count() {
        assert_eq!(
            Cell::BASELINE.query(),
            "MATCH (p:P)-[:AT]->(c:C)-[:E*1..3]->(t) RETURN count(*) AS n"
        );
    }

    #[test]
    fn qpp_and_alternation_spellings() {
        let qpp = Cell {
            hops: Hops::Qpp,
            typing: Typing::LabelAlt,
            direction: Direction::Undirected,
            ..Cell::BASELINE
        };
        assert!(qpp.query().contains("((a)-[:E]-(b:V1|V2)){1,3} (t)"));
    }

    #[test]
    fn distinct_starts_never_exceed_the_driver_rows() {
        assert_eq!(Starts::Thousand.distinct(200), 200);
        assert_eq!(Starts::Unique.distinct(2000), 2000);
        assert_eq!(Starts::One.distinct(2000), 1);
    }
}
