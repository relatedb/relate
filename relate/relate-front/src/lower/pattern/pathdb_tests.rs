//! The lowering evidence for the 42 PathDB timeout classes (SF0.1 spike,
//! 10 s watchdog):
//! every timeout query is parsed through the real front end and its MATCH
//! patterns' lowered `PatternElement` shape is asserted exactly, so the
//! frontier-NFA kernel's IR coverage report cites shapes that were
//! lowered, not translated by hand.

use super::*;

/// Normalized shape of one lowered pattern element: label count for a
/// node, static type-list length for a plain rel hop, `(min, max,
/// inner shapes)` for a quantified element.
#[derive(Debug, Clone, PartialEq)]
enum Seg {
    Node(usize),
    Rel(usize),
    Quant(u32, Option<u32>, Vec<Seg>),
}

/// One PathDB timeout query: spike qid, template name, statement text
/// (with the spike's `@SEED@` placeholder), and the expected lowered
/// shape — one `Vec<Seg>` per UNION part.
struct TimeoutCase {
    qid: u32,
    template: &'static str,
    query: &'static str,
    expected: Vec<Vec<Seg>>,
}

impl TimeoutCase {
    fn new(qid: u32, template: &'static str, query: &'static str, expected: Vec<Vec<Seg>>) -> Self {
        Self {
            qid,
            template,
            query,
            expected,
        }
    }

    /// The statement with `@SEED@` bound to a literal — pattern
    /// lowering is store-free, the value never matters.
    fn bound(&self) -> String {
        self.query.replace("@SEED@", "1")
    }
}

/// Bare/alternated quantifier over one rel hop: `A+`/`A*`/`(A|B)+`/
/// `(A|B)*` — one flat `Quantified` between the two boundary nodes.
fn flat(min: u32, types: usize) -> Vec<Seg> {
    vec![
        Seg::Node(1),
        Seg::Quant(min, None, vec![Seg::Node(0), Seg::Rel(types), Seg::Node(0)]),
        Seg::Node(0),
    ]
}

/// Two-step quantified group: `(A.B)+`/`(A.B)*` — one `Quantified`
/// over `[node, rel, node, rel, node]`.
fn two_step_group(min: u32) -> Vec<Seg> {
    vec![
        Seg::Node(1),
        Seg::Quant(
            min,
            None,
            vec![
                Seg::Node(0),
                Seg::Rel(2),
                Seg::Node(0),
                Seg::Rel(2),
                Seg::Node(0),
            ],
        ),
        Seg::Node(0),
    ]
}

/// Quantified segment then a bounded context hop: `(A+.B)`/`(A*.B)`.
fn quant_then_rel(min: u32, tail: usize) -> Vec<Seg> {
    vec![
        Seg::Node(1),
        Seg::Quant(min, None, vec![Seg::Node(0), Seg::Rel(1), Seg::Node(0)]),
        Seg::Node(0),
        Seg::Rel(tail),
        Seg::Node(0),
    ]
}

/// Bounded context hop then a quantified segment: `(A.B+)`/`(A.B*)`.
fn rel_then_quant(min: u32, head: usize) -> Vec<Seg> {
    vec![
        Seg::Node(1),
        Seg::Rel(head),
        Seg::Node(0),
        Seg::Quant(min, None, vec![Seg::Node(0), Seg::Rel(1), Seg::Node(0)]),
        Seg::Node(0),
    ]
}

/// A plain single-hop arm (the non-recursive side of a UNION).
fn plain(types: usize) -> Vec<Seg> {
    vec![Seg::Node(1), Seg::Rel(types), Seg::Node(0)]
}

fn shape(elements: &[mdb::PatternElement]) -> Vec<Seg> {
    elements
        .iter()
        .map(|element| match element {
            mdb::PatternElement::Node(node) => Seg::Node(node.labels.len()),
            mdb::PatternElement::Relationship(rel) => Seg::Rel(rel.types.len()),
            mdb::PatternElement::Quantified(q) => Seg::Quant(q.min, q.max, shape(&q.elements)),
        })
        .collect()
}

/// The MATCH path patterns of every UNION part, in order.
fn parts_patterns(query: &mdb::Query) -> Vec<Vec<&mdb::PathPattern>> {
    query
        .parts
        .iter()
        .map(|part| {
            part.clauses
                .iter()
                .filter_map(|clause| match clause {
                    mdb::Clause::Match { patterns, .. } => Some(patterns),
                    _ => None,
                })
                .flatten()
                .collect()
        })
        .collect()
}

/// The arrow spelling of a lowered rel's direction tag.
fn arrows(direction: mdb::Direction) -> (&'static str, &'static str) {
    match direction {
        mdb::Direction::Outgoing => ("-[", "]->"),
        mdb::Direction::Incoming => ("<-", "]-"),
        mdb::Direction::Both => ("-[", "]-"),
    }
}

/// Human-readable dump of a lowered element sequence (type names,
/// variables, direction tags, hop bounds) — the report's evidence
/// lines, printed by the shape test under `--nocapture`.
fn describe(elements: &[mdb::PatternElement]) -> String {
    let bounds = |min: u32, max: Option<u32>| {
        format!(
            "{min}..{}",
            max.map(|m| m.to_string()).unwrap_or_else(|| "inf".into())
        )
    };
    elements
        .iter()
        .map(|element| match element {
            mdb::PatternElement::Node(node) => format!(
                "({}{})",
                node.variable
                    .as_ref()
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                node.label_names()
                    .map(|label| format!(":{label}"))
                    .collect::<String>(),
            ),
            mdb::PatternElement::Relationship(rel) => {
                let (open, close) = arrows(rel.direction);
                format!(
                    "{open}{}[{} {}]{close}",
                    rel.variable
                        .as_ref()
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                    rel.type_names()
                        .map(|t| t.to_string())
                        .collect::<Vec<_>>()
                        .join("|"),
                    bounds(rel.min_hops, rel.max_hops),
                )
            }
            mdb::PatternElement::Quantified(q) => {
                format!("({})^{}", describe(&q.elements), bounds(q.min, q.max),)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The recorded IR facts shared by all 42: every hop, at any nesting
/// depth, is undirected over a non-empty static type list.
fn assert_undirected_static(elements: &[mdb::PatternElement], qid: u32) {
    for element in elements {
        match element {
            mdb::PatternElement::Node(_) => {}
            mdb::PatternElement::Relationship(rel) => {
                assert_eq!(
                    rel.direction,
                    mdb::Direction::Both,
                    "qid {qid}: hop direction"
                );
                assert!(!rel.types.is_empty(), "qid {qid}: static type list");
            }
            mdb::PatternElement::Quantified(q) => {
                assert_undirected_static(&q.elements, qid);
            }
        }
    }
}

/// The structural gate a v1 frontier-NFA compiler needs on one path
/// pattern (design note §4): default pool (no mode, no selector), no
/// pattern-level WHERE, at most one top-level `Quantified` element,
/// and every rel hop — top level or inside the quantifier — a plain
/// one-hop over a static type list with no inline `WHERE` and no
/// nested quantifier.
fn frontier_compilable(pattern: &mdb::PathPattern) -> bool {
    if pattern.mode.is_some()
        || pattern.selector.is_some()
        || pattern.where_clause.is_some()
        || pattern.is_shortest_path_function
    {
        return false;
    }
    if pattern
        .elements
        .iter()
        .filter(|element| matches!(element, mdb::PatternElement::Quantified(_)))
        .count()
        > 1
    {
        return false;
    }
    elements_compilable(&pattern.elements)
}

/// The per-element vocabulary of [`frontier_compilable`], applied at
/// both scan depths: a node carries no inline `WHERE`; a rel is a
/// plain one-hop over a static type list (no inline `WHERE`, not
/// var-length, `max_hops == Some(1)`); a quantified group has no
/// subpath variable and no group `WHERE`, and its inner sequence must
/// itself pass the alternation-stating inner scan. The node/rel
/// alternation of the surrounding sequence is a property of the
/// sequence, not of one element — each scan states it where it is a
/// fact.
fn compilable_element(element: &mdb::PatternElement) -> bool {
    match element {
        mdb::PatternElement::Node(node) => node.where_clause.is_none(),
        mdb::PatternElement::Relationship(rel) => {
            !rel.var_length
                && rel.where_clause.is_none()
                && !rel.types.is_empty()
                && rel.max_hops == Some(1)
        }
        mdb::PatternElement::Quantified(q) => {
            q.subpath_variable.is_none()
                && q.where_clause.is_none()
                && inner_alternation_compilable(&q.elements)
        }
    }
}

/// The top-level element scan of [`frontier_compilable`]: every element
/// passes the shared [`compilable_element`] vocabulary check. The top
/// level's node/rel alternation is the parser's own contract and is not
/// re-checked here (inside a quantified group it is, see
/// [`inner_alternation_compilable`]).
fn elements_compilable(elements: &[mdb::PatternElement]) -> bool {
    elements.iter().all(compilable_element)
}

/// The inner scan of a quantified group: the node/rel alternation is
/// stated explicitly — a `Node` at an even inner index, a
/// `Relationship` at an odd one; a `Node` at an odd index, a
/// `Relationship` at an even one, or a nested `Quantified` is a broken
/// alternation and declines — and every surviving element must pass the
/// shared [`compilable_element`] vocabulary check.
fn inner_alternation_compilable(elements: &[mdb::PatternElement]) -> bool {
    for (i, inner) in elements.iter().enumerate() {
        match inner {
            mdb::PatternElement::Node(_) if i % 2 == 0 => {}
            mdb::PatternElement::Relationship(_) if i % 2 == 1 => {}
            _ => return false,
        }
        if !compilable_element(inner) {
            return false;
        }
    }
    true
}

fn timeout_cases() -> Vec<TimeoutCase> {
    vec![
        // 2 bare quantifiers (exists family, cyclic type system)
        TimeoutCase::new(9, "knows+", "MATCH p = (x:Person)-[:knows*1..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(1, 1)]),
        TimeoutCase::new(12, "knows*", "MATCH p = (x:Person)-[:knows*0..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(0, 1)]),
        // 16 NFA-frontier: 4 two-step groups + 12 per-hop alternations
        TimeoutCase::new(37, "(likes.hasCreator)+", "MATCH p = (x:Person)((a)-[:likeComment|likePost]-(b)-[:commentHasCreator|postHasCreator]-(c)){1,}(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![two_step_group(1)]),
        TimeoutCase::new(38, "(hasCreator.likes)+", "MATCH p = (x:Comment)((a)-[:commentHasCreator|postHasCreator]-(b)-[:likeComment|likePost]-(c)){1,}(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![two_step_group(1)]),
        TimeoutCase::new(39, "(likes.hasCreator)*", "MATCH p = (x:Person)((a)-[:likeComment|likePost]-(b)-[:commentHasCreator|postHasCreator]-(c)){0,}(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![two_step_group(0)]),
        TimeoutCase::new(40, "(hasCreator.likes)*", "MATCH p = (x:Comment)((a)-[:commentHasCreator|postHasCreator]-(b)-[:likeComment|likePost]-(c)){0,}(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![two_step_group(0)]),
        TimeoutCase::new(47, "(hasCreator|isLocatedIn)+", "MATCH p = (x:Comment)-[:commentHasCreator|postHasCreator|commentIsLocatedIn|organisationIsLocatedIn|personIsLocatedIn|postIsLocatedIn*1..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(1, 6)]),
        TimeoutCase::new(48, "(studyAt|isLocatedIn)+", "MATCH p = (x:Person)-[:studyAt|commentIsLocatedIn|organisationIsLocatedIn|personIsLocatedIn|postIsLocatedIn*1..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(1, 5)]),
        TimeoutCase::new(49, "(studyAt|hasInterest)+", "MATCH p = (x:Person)-[:studyAt|hasInterest*1..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(1, 2)]),
        TimeoutCase::new(50, "(workAt|hasInterest)+", "MATCH p = (x:Person)-[:workAt|hasInterest*1..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(1, 2)]),
        TimeoutCase::new(51, "(studyAt|knows)+", "MATCH p = (x:Person)-[:studyAt|knows*1..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(1, 2)]),
        TimeoutCase::new(52, "(workAt|knows)+", "MATCH p = (x:Person)-[:workAt|knows*1..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(1, 2)]),
        TimeoutCase::new(53, "(hasCreator|isLocatedIn)*", "MATCH p = (x:Comment)-[:commentHasCreator|postHasCreator|commentIsLocatedIn|organisationIsLocatedIn|personIsLocatedIn|postIsLocatedIn*0..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(0, 6)]),
        TimeoutCase::new(54, "(studyAt|isLocatedIn)*", "MATCH p = (x:Person)-[:studyAt|commentIsLocatedIn|organisationIsLocatedIn|personIsLocatedIn|postIsLocatedIn*0..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(0, 5)]),
        TimeoutCase::new(55, "(studyAt|hasInterest)*", "MATCH p = (x:Person)-[:studyAt|hasInterest*0..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(0, 2)]),
        TimeoutCase::new(56, "(workAt|hasInterest)*", "MATCH p = (x:Person)-[:workAt|hasInterest*0..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(0, 2)]),
        TimeoutCase::new(57, "(studyAt|knows)*", "MATCH p = (x:Person)-[:studyAt|knows*0..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(0, 2)]),
        TimeoutCase::new(58, "(workAt|knows)*", "MATCH p = (x:Person)-[:workAt|knows*0..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(0, 2)]),
        // 24 scan+segment: bounded context around a recursive segment
        TimeoutCase::new(67, "(knows+).studyAt", "MATCH p = (x:Person)-[:knows*1..]-()-[:studyAt]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![quant_then_rel(1, 1)]),
        TimeoutCase::new(68, "(knows+).isLocatedIn", "MATCH p = (x:Person)-[:knows*1..]-()-[:commentIsLocatedIn|organisationIsLocatedIn|personIsLocatedIn|postIsLocatedIn]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![quant_then_rel(1, 4)]),
        TimeoutCase::new(69, "(knows+).hasInterest", "MATCH p = (x:Person)-[:knows*1..]-()-[:hasInterest]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![quant_then_rel(1, 1)]),
        TimeoutCase::new(70, "(knows+).likes", "MATCH p = (x:Person)-[:knows*1..]-()-[:likeComment|likePost]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![quant_then_rel(1, 2)]),
        TimeoutCase::new(73, "(knows*).studyAt", "MATCH p = (x:Person)-[:knows*0..]-()-[:studyAt]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![quant_then_rel(0, 1)]),
        TimeoutCase::new(74, "(knows*).isLocatedIn", "MATCH p = (x:Person)-[:knows*0..]-()-[:commentIsLocatedIn|organisationIsLocatedIn|personIsLocatedIn|postIsLocatedIn]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![quant_then_rel(0, 4)]),
        TimeoutCase::new(75, "(knows*).hasInterest", "MATCH p = (x:Person)-[:knows*0..]-()-[:hasInterest]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![quant_then_rel(0, 1)]),
        TimeoutCase::new(76, "(knows*).likes", "MATCH p = (x:Person)-[:knows*0..]-()-[:likeComment|likePost]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![quant_then_rel(0, 2)]),
        TimeoutCase::new(86, "knows.(knows+)", "MATCH p = (x:Person)-[:knows]-()-[:knows*1..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![rel_then_quant(1, 1)]),
        TimeoutCase::new(87, "hasCreator.(knows+)", "MATCH p = (x:Comment)-[:commentHasCreator|postHasCreator]-()-[:knows*1..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![rel_then_quant(1, 2)]),
        TimeoutCase::new(88, "hasMember.(knows+)", "MATCH p = (x:Forum)-[:hasMember]-()-[:knows*1..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![rel_then_quant(1, 1)]),
        TimeoutCase::new(92, "knows.(knows*)", "MATCH p = (x:Person)-[:knows]-()-[:knows*0..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![rel_then_quant(0, 1)]),
        TimeoutCase::new(93, "hasCreator.(knows*)", "MATCH p = (x:Comment)-[:commentHasCreator|postHasCreator]-()-[:knows*0..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![rel_then_quant(0, 2)]),
        TimeoutCase::new(94, "hasMember.(knows*)", "MATCH p = (x:Forum)-[:hasMember]-()-[:knows*0..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![rel_then_quant(0, 1)]),
        TimeoutCase::new(103, "(knows+)|isLocatedIn", "MATCH p = (x:Person)-[:knows*1..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100 UNION MATCH p = (x:Person)-[:commentIsLocatedIn|organisationIsLocatedIn|personIsLocatedIn|postIsLocatedIn]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(1, 1), plain(4)]),
        TimeoutCase::new(104, "(knows+)|studyAt", "MATCH p = (x:Person)-[:knows*1..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100 UNION MATCH p = (x:Person)-[:studyAt]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(1, 1), plain(1)]),
        TimeoutCase::new(105, "(knows+)|hasInterest", "MATCH p = (x:Person)-[:knows*1..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100 UNION MATCH p = (x:Person)-[:hasInterest]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(1, 1), plain(1)]),
        TimeoutCase::new(106, "(knows+)|likes", "MATCH p = (x:Person)-[:knows*1..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100 UNION MATCH p = (x:Person)-[:likeComment|likePost]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(1, 1), plain(2)]),
        TimeoutCase::new(109, "(knows*)|isLocatedIn", "MATCH p = (x:Person)-[:knows*0..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100 UNION MATCH p = (x:Person)-[:commentIsLocatedIn|organisationIsLocatedIn|personIsLocatedIn|postIsLocatedIn]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(0, 1), plain(4)]),
        TimeoutCase::new(110, "(knows*)|studyAt", "MATCH p = (x:Person)-[:knows*0..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100 UNION MATCH p = (x:Person)-[:studyAt]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(0, 1), plain(1)]),
        TimeoutCase::new(111, "(knows*)|hasInterest", "MATCH p = (x:Person)-[:knows*0..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100 UNION MATCH p = (x:Person)-[:hasInterest]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(0, 1), plain(1)]),
        TimeoutCase::new(112, "(knows*)|likes", "MATCH p = (x:Person)-[:knows*0..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100 UNION MATCH p = (x:Person)-[:likeComment|likePost]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![flat(0, 1), plain(2)]),
        TimeoutCase::new(124, "knows|(knows+)", "MATCH p = (x:Person)-[:knows]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100 UNION MATCH p = (x:Person)-[:knows*1..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![plain(1), flat(1, 1)]),
        TimeoutCase::new(130, "knows|(knows*)", "MATCH p = (x:Person)-[:knows]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100 UNION MATCH p = (x:Person)-[:knows*0..]-(y) WHERE x.ID = @SEED@ RETURN p LIMIT 100", vec![plain(1), flat(0, 1)]),
    ]
}

/// Every one of the 42 timeout queries, parsed and lowered for real:
/// the exact per-UNION-part `PatternElement` shape is asserted, the
/// shared IR facts (undirected hops over static type lists, default
/// match mode) are checked, and a full descriptor per query is
/// printed for the coverage report (`--nocapture`).
#[test]
fn pathdb_timeout_classes_lower_to_expected_ir() {
    for case in timeout_cases() {
        let query = crate::parse_and_lower(&case.bound())
            .unwrap_or_else(|error| panic!("qid {} ({}): {error}", case.qid, case.template));
        for part in &query.parts {
            for clause in &part.clauses {
                if let mdb::Clause::Match { match_mode, .. } = clause {
                    assert_eq!(
                        *match_mode,
                        mdb::MatchMode::DifferentRelationships,
                        "qid {}: default pool",
                        case.qid
                    );
                }
            }
        }
        let parts = parts_patterns(&query);
        let lowered: Vec<Vec<Seg>> = parts
            .iter()
            .map(|patterns| {
                assert_eq!(
                    patterns.len(),
                    1,
                    "qid {}: one path pattern per MATCH",
                    case.qid
                );
                assert_undirected_static(&patterns[0].elements, case.qid);
                shape(&patterns[0].elements)
            })
            .collect();
        assert_eq!(
            lowered, case.expected,
            "qid {} ({}) lowered IR shape",
            case.qid, case.template
        );
        let dump = parts
            .iter()
            .map(|patterns| describe(&patterns[0].elements))
            .collect::<Vec<_>>()
            .join(" UNION ");
        println!("qid {} [{}] => {}", case.qid, case.template, dump);
    }
}

/// The coverage verdict: a v1 frontier-NFA compiler with flat-
/// quantifier, quantified-group, and concatenation-context support
/// (design note §4) compiles every MATCH arm of all 42 timeout
/// classes — the corpus contains no nested quantifier, no
/// alternation-of-segments single pattern, and no rel predicates.
#[test]
fn pathdb_timeout_classes_are_frontier_compilable() {
    let cases = timeout_cases();
    assert_eq!(cases.len(), 42);
    for case in cases {
        let query = crate::parse_and_lower(&case.bound())
            .unwrap_or_else(|error| panic!("qid {} ({}): {error}", case.qid, case.template));
        let compilable = parts_patterns(&query)
            .iter()
            .all(|patterns| patterns.iter().all(|pattern| frontier_compilable(pattern)));
        assert!(
            compilable,
            "qid {} ({}) declined by the v1 structural gate",
            case.qid, case.template
        );
    }
}
