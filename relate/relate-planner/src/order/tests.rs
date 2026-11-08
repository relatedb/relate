//! `order` tests: the endpoint ladder's measured levels (equality, the
//! range window, CONTAINS provenance, label rows), the degree scaling's
//! mean/tail policy, and the orientation and part-order decisions they
//! drive.

use relate_ir::Clause;
use relate_names::SmallName;

use super::*;
use crate::plan_with_stats;
use crate::stats::NoPatternStats;

mod seek;

/// One parse and leading-MATCH destructure — the front half every
/// endpoint consumer rides (the closure keeps the query alive for the
/// consumers' borrows).
fn with_leading_match<R>(cypher: &str, f: impl FnOnce(&PathPattern, Option<&Expr>) -> R) -> R {
    let query = relate_front::parse_and_lower(cypher).unwrap();
    let Clause::Match {
        patterns,
        where_clause,
        ..
    } = &query.parts[0].clauses[0]
    else {
        panic!("expected a leading MATCH");
    };
    f(&patterns[0], where_clause.as_ref())
}

fn label_stats(labels: &'static [(&'static str, u64)]) -> Stub {
    Stub {
        labels,
        ..Stub::EMPTY
    }
}

fn first_label(path: &PathPattern) -> String {
    path.nodes[0].labels[0].to_string()
}

struct Stub {
    labels: &'static [(&'static str, u64)],
    equals: &'static [(&'static str, &'static str, &'static str, u64)],
    ranges: &'static [(&'static str, &'static str, BinaryOp, i64, u64)],
    contains: &'static [(&'static str, &'static str, &'static str, ContainsCount)],
    rels: &'static [(&'static str, u64)],
}

/// One name-keyed table lookup — the idiom every stub's `label_rows`
/// and `rel_type_rows` delegates to (one spelling, never a per-struct
/// copy).
fn rows_of(table: &[(&str, u64)], key: &str) -> Option<u64> {
    table
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, rows)| *rows)
}

impl Stub {
    /// Every table empty — the base each literal spreads, so a site
    /// spells only the tables its test reads.
    const EMPTY: Stub = Stub {
        labels: &[],
        equals: &[],
        ranges: &[],
        contains: &[],
        rels: &[],
    };
}

impl PatternStats for Stub {
    fn label_rows(&self, label: &SmallName) -> Option<u64> {
        label.with_str(|text| rows_of(self.labels, text))
    }

    fn equality_rows(&self, label: &SmallName, property: &SmallName, value: &Expr) -> Option<u64> {
        let text = match value {
            Expr::String(text) => text.as_str(),
            _ => return None,
        };
        self.equals
            .iter()
            .find(|(l, p, v, _)| label == *l && property == *p && *v == text)
            .map(|(_, _, _, rows)| *rows)
    }

    fn range_rows(
        &self,
        label: &SmallName,
        property: &SmallName,
        op: BinaryOp,
        bound: &Expr,
    ) -> Option<u64> {
        let Expr::Int(bound) = bound else {
            return None;
        };
        self.ranges
            .iter()
            .find(|(l, p, o, b, _)| label == *l && property == *p && *o == op && *b == *bound)
            .map(|(_, _, _, _, rows)| *rows)
    }

    fn contains_rows(
        &self,
        label: &SmallName,
        property: &SmallName,
        needle: &Expr,
    ) -> Option<ContainsCount> {
        let text = match needle {
            Expr::String(text) => text.as_str(),
            _ => return None,
        };
        self.contains
            .iter()
            .find(|(l, p, n, _)| label == *l && property == *p && *n == text)
            .map(|(_, _, _, count)| *count)
    }

    fn rel_type_rows(&self, rel_type: &SmallName) -> Option<u64> {
        rel_type.with_str(|text| rows_of(self.rels, text))
    }
}

fn patterns_of(cypher: &str, stats: &dyn PatternStats) -> Vec<PathPattern> {
    let query = relate_front::parse_and_lower(cypher).unwrap();
    let plan = plan_with_stats(query, &mut |_| false, stats).unwrap();
    let Clause::Match { patterns, .. } = &plan.parts[0][0] else {
        panic!("expected a leading MATCH");
    };
    patterns.clone()
}

/// LABS-426 (the mean-to-tail flip): the tail-carrying stats — one
/// (label, rel-type) tail beside the mean's inputs, so the flip's
/// own arithmetic is pinned without the executor's graphs.
struct TailStub {
    labels: &'static [(&'static str, u64)],
    rels: &'static [(&'static str, u64)],
    tails: &'static [(&'static str, &'static str, u64)],
}

impl PatternStats for TailStub {
    fn label_rows(&self, label: &SmallName) -> Option<u64> {
        label.with_str(|text| rows_of(self.labels, text))
    }

    fn equality_rows(&self, _: &SmallName, _: &SmallName, _: &Expr) -> Option<u64> {
        None
    }

    fn contains_rows(&self, _: &SmallName, _: &SmallName, _: &Expr) -> Option<ContainsCount> {
        None
    }

    fn rel_type_rows(&self, rel_type: &SmallName) -> Option<u64> {
        rel_type.with_str(|text| rows_of(self.rels, text))
    }

    fn degree_tail(&self, label: &SmallName, rel_type: &SmallName) -> Option<u64> {
        self.tails
            .iter()
            .find(|(l, t, _)| *l == label && *t == rel_type)
            .map(|(_, _, tail)| *tail)
    }
}

/// The pattern's written first hop — parse only, no planning: the
/// written spelling is what both walk orientations evaluate against
/// (the reversed evaluation walks it from the end side), so the
/// parity pins need the un-reversed, un-flipped form.
fn first_hop(cypher: &str) -> (NodePattern, RelPattern) {
    with_leading_match(cypher, |path, _| {
        (path.nodes[0].clone(), path.rels[0].clone())
    })
}

/// LABS-426: an outgoing hop's fanout rides the (start-label,
/// rel-type) out-degree tail, not the mean — the label's mean (53)
/// hides the one 50k-degree supernode; the tail (32768, the log2
/// bucket top) does not. An incoming or undirected hop keeps the mean
/// (the histogram's key side is the far side), and a multi-label
/// start takes the min tail across its labels (every label's tail
/// bounds the intersection's max degree).
#[test]
fn apply_degree_rides_the_tail_on_an_outgoing_hop() {
    let stats = TailStub {
        labels: &[("P", 1000), ("A", 1000), ("B", 1000)],
        rels: &[("KNOWS", 53000)],
        tails: &[("P", "KNOWS", 32768), ("A", "KNOWS", 16384)],
    };
    // Outgoing, a small-seed seek: rows x tail, clamped by the type
    // total — 1 x 32768 (the mean would price the supernode at 53).
    let (start, rel) = first_hop("MATCH (p:P)-[:KNOWS]->(q:Q) RETURN q");
    assert!(seed_on_rel_start(&rel, false));
    assert_eq!(apply_degree(1, &start, &rel, true, &stats), 32_768);
    // Outgoing, a whole-label scan: the clamp holds the sum at the
    // type total — 53000, the mean's own scale, not 1000 x 32768.
    assert_eq!(apply_degree(1000, &start, &rel, true, &stats), 53_000);
    // Incoming: the mean — the histogram's key side is the far side.
    let (start, rel) = first_hop("MATCH (p:P)<-[:KNOWS]-(q:Q) RETURN q");
    assert!(!seed_on_rel_start(&rel, false));
    assert_eq!(apply_degree(1000, &start, &rel, false, &stats), 53_000);
    // Undirected: no start side in EITHER orientation (round 1,
    // F-1-1: the XOR coincidence answered true for reversed) — the
    // mean in both walks.
    let (start, rel) = first_hop("MATCH (p:P)-[:KNOWS]-(q:Q) RETURN q");
    assert!(!seed_on_rel_start(&rel, false));
    assert!(!seed_on_rel_start(&rel, true));
    assert_eq!(apply_degree(1000, &start, &rel, false, &stats), 53_000);
    assert_eq!(apply_degree(1000, &start, &rel, true, &stats), 53_000);
    // A multi-label start takes the min tail across its labels
    // (a small seed: 1 x 16384, clamped by the 53000 total).
    let (start, rel) = first_hop("MATCH (p:A:B)-[:KNOWS]->(q:Q) RETURN q");
    assert_eq!(apply_degree(1, &start, &rel, true, &stats), 16_384);
    // A label with no histogram entry keeps the mean (B alone: no
    // (B,KNOWS) tail).
    let (start, rel) = first_hop("MATCH (p:B)-[:KNOWS]->(q:Q) RETURN q");
    assert_eq!(apply_degree(1000, &start, &rel, true, &stats), 53_000);
    // The seed on the rel's END side (the reversed walk of a
    // written-outgoing hop): the histogram's key side is the far
    // side, so the mean stays even when the seed's label carries a
    // tail.
    let (start, rel) = first_hop("MATCH (p:P)-[:KNOWS]->(q:Q) RETURN q");
    assert!(!seed_on_rel_start(&rel, true));
    assert_eq!(apply_degree(1000, &start, &rel, false, &stats), 53_000);
    // The reversed walk of a written-INCOMING hop: the seed IS the
    // rel's start side — the tail rides again (a small seed).
    let (start, rel) = first_hop("MATCH (p:P)<-[:KNOWS]-(q:Q) RETURN q");
    assert!(seed_on_rel_start(&rel, true));
    assert_eq!(apply_degree(1, &start, &rel, true, &stats), 32_768);
}

/// The path-multiset hop (`apply_path_degree`): the same fanout policy,
/// without the degree-sum clamp — rows that are paths revisit nodes, so
/// a chained multiplicity (4 paths x tail 4 = 16 on a 12-rel type)
/// outgrows the type total the distinct-seed hop clamps at.
#[test]
fn apply_path_degree_drops_the_degree_sum_clamp() {
    let stats = TailStub {
        labels: &[("P", 3)],
        rels: &[("KNOWS", 12)],
        tails: &[("P", "KNOWS", 4)],
    };
    let (start, rel) = first_hop("MATCH (p:P)-[:KNOWS]->(q:Q) RETURN q");
    assert_eq!(apply_degree(4, &start, &rel, true, &stats), 12);
    assert_eq!(apply_path_degree(4, &start, &rel, true, &stats), 16);
    // The mean arm carries no clamp in either form: 4 x 12 / 3.
    let (start, rel) = first_hop("MATCH (p:P)<-[:KNOWS]-(q:Q) RETURN q");
    assert_eq!(apply_degree(4, &start, &rel, false, &stats), 16);
    assert_eq!(apply_path_degree(4, &start, &rel, false, &stats), 16);
}

/// LABS-991: stats with an any-label tail per rel type (`""` keys the
/// any-type tail) beside the label and rel-type counts.
struct AnyTailStub {
    labels: &'static [(&'static str, u64)],
    rels: &'static [(&'static str, u64)],
    any_tails: &'static [(&'static str, u64)],
}

impl PatternStats for AnyTailStub {
    fn label_rows(&self, label: &SmallName) -> Option<u64> {
        label.with_str(|text| rows_of(self.labels, text))
    }

    fn equality_rows(&self, _: &SmallName, _: &SmallName, _: &Expr) -> Option<u64> {
        None
    }

    fn contains_rows(&self, _: &SmallName, _: &SmallName, _: &Expr) -> Option<ContainsCount> {
        None
    }

    fn rel_type_rows(&self, rel_type: &SmallName) -> Option<u64> {
        rel_type.with_str(|text| rows_of(self.rels, text))
    }

    fn any_degree_tail(&self, rel_type: Option<&SmallName>) -> Option<u64> {
        let key = rel_type.map_or(String::new(), |rel_type| rel_type.to_string());
        rows_of(self.any_tails, &key)
    }
}

/// LABS-991: a hop the walk model cannot price — an untyped rel, or a hop
/// leaving an unlabeled node — rides the any-label tail in the kernel
/// gate's [`apply_hop_bound`], unclamped so it compounds across a chain
/// (an alternation rides the any-type tail); every hop the model CAN price
/// keeps `apply_degree`'s price, and the shapes with no histogram to ride
/// (incoming, undirected, no stats) keep the row count.
#[test]
fn apply_hop_bound_prices_what_the_walk_model_leaves_unpriced() {
    let stats = AnyTailStub {
        labels: &[("P", 1000)],
        rels: &[("KNOWS", 53000)],
        any_tails: &[("KNOWS", 1024), ("", 2048)],
    };
    // Unlabeled seed side, typed: the walk model leaves the rows; the
    // bound is the type's any-label tail, and it compounds hop on hop.
    let (start, rel) = first_hop("MATCH ()-[:KNOWS]->(q) RETURN q");
    assert_eq!(apply_degree(1, &start, &rel, true, &stats), 1);
    assert_eq!(apply_hop_bound(1, &start, &rel, true, &stats), 1024);
    assert_eq!(apply_hop_bound(1024, &start, &rel, true, &stats), 1_048_576);
    // Untyped rel (labeled or not): the any-type tail.
    let (start, rel) = first_hop("MATCH (p:P)-->(q) RETURN q");
    assert_eq!(apply_degree(1, &start, &rel, true, &stats), 1);
    assert_eq!(apply_hop_bound(1, &start, &rel, true, &stats), 2048);
    // Incoming or undirected: no histogram side to ride — unchanged.
    let (start, rel) = first_hop("MATCH ()<-[:KNOWS]-(q) RETURN q");
    assert_eq!(apply_hop_bound(7, &start, &rel, false, &stats), 7);
    let (start, rel) = first_hop("MATCH ()-[:KNOWS]-(q) RETURN q");
    assert_eq!(apply_hop_bound(7, &start, &rel, false, &stats), 7);
    // An alternation rides the any-type tail, which bounds every type.
    let (start, rel) = first_hop("MATCH ()-[:KNOWS|LIKES]->(q) RETURN q");
    assert_eq!(apply_hop_bound(7, &start, &rel, true, &stats), 7 * 2048);
    // A priceable hop (labeled seed, typed, mean arm) keeps apply_degree's
    // price — the bound never replaces a price the model has.
    let (start, rel) = first_hop("MATCH (p:P)<-[:KNOWS]-(q) RETURN q");
    assert_eq!(
        apply_hop_bound(1000, &start, &rel, false, &stats),
        apply_degree(1000, &start, &rel, false, &stats)
    );
    // No histogram (the null stats): the rows pass through, as before.
    let (start, rel) = first_hop("MATCH ()-[:KNOWS]->(q) RETURN q");
    assert_eq!(apply_hop_bound(5, &start, &rel, true, &NoPatternStats), 5);
}

fn starts_with_label(path: &PathPattern, label: &str) -> bool {
    path.nodes
        .first()
        .is_some_and(|node| node.labels.iter().any(|got| got == label))
}

#[test]
fn no_stats_reorders_q22_and_q27_and_keeps_equal_scores() {
    let stats = NoPatternStats;
    let q22 = patterns_of(
        "MATCH (p:Person)-[:workAt]->(o:Organisation), \
         (c:Comment)-[:replyOfPost]->(post:Post), \
         (c)-[:commentHasCreator]->(p) \
         WHERE o.name = 'Linxair' RETURN p",
        &stats,
    );
    assert!(starts_with_label(&q22[0], "Organisation"));
    assert_eq!(
        q22[1].nodes[0].variable.as_ref().map(|v| v.to_string()),
        Some("p".to_string())
    );
    assert_eq!(
        q22[2].nodes[0].variable.as_ref().map(|v| v.to_string()),
        Some("c".to_string())
    );

    let q27 = patterns_of(
        "MATCH (c:Comment)-[:commentHasCreator]->(p:Person), \
         (p)-[:personIsLocatedIn]->(l:Place), \
         (c)-[:commentHasTag]->(t:Tag) \
         WHERE l.name = 'Toronto' AND t.name = 'Winston_Churchill' \
         RETURN c",
        &stats,
    );
    assert!(starts_with_label(&q27[0], "Place"));
    assert_eq!(
        q27[1].nodes[0].variable.as_ref().map(|v| v.to_string()),
        Some("p".to_string())
    );
    assert_eq!(
        q27[2].nodes[0].variable.as_ref().map(|v| v.to_string()),
        Some("c".to_string())
    );

    let tied = patterns_of(
        "MATCH (a:A)-[:R]->(b:B), (c:C)-[:S]->(d:D) RETURN a",
        &stats,
    );
    assert_eq!(
        tied[0].nodes[0].variable.as_ref().map(|v| v.to_string()),
        Some("a".to_string())
    );
    assert_eq!(
        tied[1].nodes[0].variable.as_ref().map(|v| v.to_string()),
        Some("c".to_string())
    );
    assert_eq!(tied[0].rels[0].direction, relate_ir::Direction::Outgoing);
}

/// The reversal's evaluation-order gate: a pattern whose element
/// predicate references a peer pattern variable keeps its written
/// orientation — the reference is legal in that order, and the strict
/// evaluator rejects it once the walk order is reversed (ADR-0335).
/// Without such a reference the bound-endpoint reversal still applies.
#[test]
fn peer_referenced_predicate_keeps_the_written_orientation() {
    let stats = NoPatternStats;
    let referencing = patterns_of(
        "MATCH (q:Question)<-[:COMMENTED_ON]-(first:Comment), \
         (u2:User)-[:COMMENTED]->(c2:Comment {seen: u2.name})-[:COMMENTED_ON]->(q) \
         RETURN u2.name AS name",
        &stats,
    );
    assert_eq!(
        referencing[1].nodes[0]
            .variable
            .as_ref()
            .map(|v| v.to_string()),
        Some("u2".to_string())
    );
    assert_eq!(
        referencing[1].nodes[1]
            .variable
            .as_ref()
            .map(|v| v.to_string()),
        Some("c2".to_string()),
        "the written order binds u2 before the referencing predicate"
    );

    let plain = patterns_of(
        "MATCH (q:Question)<-[:COMMENTED_ON]-(first:Comment), \
         (u2:User)-[:COMMENTED]->(c2:Comment {seen: 'x'})-[:COMMENTED_ON]->(q) \
         RETURN u2.name AS name",
        &stats,
    );
    assert_eq!(
        plain[1].nodes[0].variable.as_ref().map(|v| v.to_string()),
        Some("q".to_string()),
        "the bound-endpoint reversal still applies without a peer reference"
    );
}

#[test]
fn label_and_index_counts_seed_q19_and_q28() {
    let stats = Stub {
        labels: &[
            ("Comment", 2_000_000),
            ("Tag", 16_000),
            ("Place", 1_460),
            ("Tagclass", 71),
            ("Person", 9_892),
        ],
        equals: &[
            ("Place", "name", "Manila", 1),
            ("Tagclass", "name", "BritishRoyalty", 1),
        ],
        contains: &[(
            "Tag",
            "name",
            "Copernicus",
            ContainsCount::SampleScan {
                rows: 4,
                scan: 16_000,
            },
        )],
        rels: &[
            ("commentHasTag", 2_000_000),
            ("commentIsLocatedIn", 2_000_000),
            ("personIsLocatedIn", 9_892),
            ("hasType", 16_080),
            ("hasInterest", 229_166),
        ],
        ..Stub::EMPTY
    };
    let q19 = patterns_of(
        "MATCH (c:Comment)-[:commentHasTag]->(t:Tag), \
         (c)-[:commentIsLocatedIn]->(l:Place) \
         WHERE t.name CONTAINS 'Copernicus' RETURN l",
        &stats,
    );
    assert!(starts_with_label(&q19[0], "Tag"));
    assert_eq!(
        q19[1].nodes[0].variable.as_ref().map(|v| v.to_string()),
        Some("c".to_string())
    );

    let q28 = patterns_of(
        "MATCH (t:Tag)-[:hasType]->(tc:Tagclass), \
         (p:Person)-[:hasInterest]->(t), \
         (p)-[:personIsLocatedIn]->(l:Place) \
         WHERE tc.name = 'BritishRoyalty' AND l.name = 'Manila' RETURN p",
        &stats,
    );
    assert!(starts_with_label(&q28[0], "Place"));
    assert!(q28[1].rels[0].types.iter().any(|ty| ty == "hasInterest"));
    assert!(q28[2].rels[0].types.iter().any(|ty| ty == "hasType"));
}

#[test]
fn named_variable_length_stays_written() {
    let stats = label_stats(&[("B", 1)]);
    let paths = patterns_of("MATCH (a)-[r*]->(b:B) RETURN a", &stats);
    assert_eq!(
        paths[0].nodes[0].variable.as_ref().map(|v| v.to_string()),
        Some("a".to_string())
    );
    assert_eq!(paths[0].rels[0].direction, relate_ir::Direction::Outgoing);
}

#[test]
fn unbound_part_reverses_toward_the_rare_label() {
    let stats = label_stats(&[("Hub", 200_000), ("Target", 10)]);
    let patterns = patterns_of("MATCH (h:Hub)-[:R]->(t:Target) RETURN h", &stats);
    assert_eq!(first_label(&patterns[0]), "Target");
    assert_eq!(
        patterns[0].nodes.last().unwrap().labels[0].to_string(),
        "Hub"
    );
    assert_eq!(
        patterns[0].rels[0].direction,
        relate_ir::Direction::Incoming
    );
}

// A typed hop scales both directions to the same edge count. The seed
// is still the rarer label: that is the scan ADR-0218 exists to avoid.
#[test]
fn degree_tie_still_seeds_the_rare_label() {
    let stats = Stub {
        labels: &[("Hub", 200_000), ("Target", 10)],
        rels: &[("R", 100)],
        ..Stub::EMPTY
    };
    let patterns = patterns_of("MATCH (h:Hub)-[:R]->(t:Target) RETURN h", &stats);
    assert_eq!(first_label(&patterns[0]), "Target");
}

/// The compound's flip side (ADR-0474, the GBL q8 shape): a sampled
/// CONTAINS seed whose 1,000-row label scan dwarfs its 1-row output
/// ties the rival's degree-scaled walk (both land on the 1,000 rels),
/// and the tie-break's seed comparison prices the sampled seed at its
/// scan — so the 10-row label scan keeps the start the raw selectivity
/// (1 survivor) would have stolen.
#[test]
fn sampled_contains_scan_floor_flips_the_seed_to_the_cheaper_scan() {
    let stats = Stub {
        labels: &[("A", 10), ("B", 1_000)],
        contains: &[(
            "B",
            "x",
            "needle",
            ContainsCount::SampleScan {
                rows: 1,
                scan: 1_000,
            },
        )],
        rels: &[("R", 1_000)],
        ..Stub::EMPTY
    };
    let patterns = patterns_of(
        "MATCH (a:A)<-[:R]-(b:B) WHERE b.x CONTAINS 'needle' RETURN a",
        &stats,
    );
    assert_eq!(first_label(&patterns[0]), "A");
}

/// The compound's keep side (ADR-0474, the GBL q19 shape): the scan
/// floor raises the selective sampled seed from its 10-row scaled
/// output to its 40-row label scan, and that still beats the rival
/// pattern's 200-row degree-scaled walk — the genuinely selective
/// seed keeps the anchor the floor gave it.
#[test]
fn sampled_contains_scan_floor_keeps_a_selective_seed_over_a_pricier_walk() {
    let stats = Stub {
        labels: &[("C", 200), ("B", 40), ("P", 30)],
        contains: &[(
            "B",
            "x",
            "needle",
            ContainsCount::SampleScan { rows: 2, scan: 40 },
        )],
        rels: &[("T", 200), ("L", 200)],
        ..Stub::EMPTY
    };
    let patterns = patterns_of(
        "MATCH (c:C)-[:T]->(b:B), (c)-[:L]->(p:P) \
         WHERE b.x CONTAINS 'needle' RETURN p",
        &stats,
    );
    assert_eq!(first_label(&patterns[0]), "B");
    assert_eq!(
        patterns[1].nodes[0]
            .variable
            .as_ref()
            .map(|v| v.to_string()),
        Some("c".to_string())
    );
}

/// The TEXT-indexed measurement carries no scan floor (ADR-0474): the
/// index seeks, so the exact hit count prices the seed alone and the
/// 1-hit index-served side keeps the start over the 10-row label scan
/// — the compound never prices a scan the index did the work to skip.
#[test]
fn text_indexed_contains_keeps_the_exact_count_without_a_scan_floor() {
    let stats = Stub {
        labels: &[("A", 10), ("B", 1_000)],
        contains: &[("B", "x", "needle", ContainsCount::TextIndex(1))],
        rels: &[("R", 1_000)],
        ..Stub::EMPTY
    };
    let patterns = patterns_of(
        "MATCH (a:A)<-[:R]-(b:B) WHERE b.x CONTAINS 'needle' RETURN a",
        &stats,
    );
    assert_eq!(first_label(&patterns[0]), "B");
}

#[test]
fn selective_written_first_end_stays_first() {
    let stats = label_stats(&[("Hub", 200_000), ("Target", 10)]);
    let patterns = patterns_of("MATCH (t:Target)<-[:R]-(h:Hub) RETURN t", &stats);
    assert_eq!(first_label(&patterns[0]), "Target");
}

#[test]
fn equal_cardinalities_keep_written_order() {
    let stats = label_stats(&[("A", 5), ("B", 5)]);
    let patterns = patterns_of("MATCH (a:A)-[:R]->(b:B) RETURN a", &stats);
    assert_eq!(first_label(&patterns[0]), "A");
}

#[test]
fn no_stats_keeps_written_order() {
    let patterns = patterns_of("MATCH (h:Hub)-[:R]->(t:Target) RETURN h", &NoPatternStats);
    assert_eq!(first_label(&patterns[0]), "Hub");
}

#[test]
fn no_stats_keeps_written_part_order() {
    let patterns = patterns_of(
        "MATCH (h:Hub)-[:R]->(t:Target), (x:Tiny)-[:S]->(y:Tiny) RETURN x",
        &NoPatternStats,
    );
    assert_eq!(first_label(&patterns[0]), "Hub");
    assert_eq!(first_label(&patterns[1]), "Tiny");
}

#[test]
fn bound_node_still_wins_over_cardinality() {
    let stats = label_stats(&[("Hub", 200_000), ("Target", 10)]);
    let query =
        relate_front::parse_and_lower("MATCH (h:Hub) WITH h MATCH (h)-[:R]->(t:Target) RETURN t")
            .unwrap();
    let plan = plan_with_stats(query, &mut |_| false, &stats).unwrap();
    let Clause::Match { patterns, .. } = &plan.parts[0][2] else {
        panic!("expected the second MATCH");
    };
    assert_eq!(
        patterns[0].nodes[0]
            .variable
            .as_ref()
            .map(|v| v.to_string()),
        Some("h".to_string())
    );
}

#[test]
fn cheaper_independent_part_runs_first() {
    let stats = label_stats(&[("Hub", 200_000), ("Target", 10), ("Tiny", 3)]);
    let patterns = patterns_of(
        "MATCH (h:Hub)-[:R]->(t:Target), (x:Tiny)-[:S]->(y:Tiny) RETURN x",
        &stats,
    );
    assert_eq!(first_label(&patterns[0]), "Tiny");
}
