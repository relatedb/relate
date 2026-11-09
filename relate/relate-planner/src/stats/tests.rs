//! The stats module's tests (split out of `mod.rs`, LABS-311 size gate):
//! ADR-0508's counting fake and the memo's once-each and transparency
//! checks.

use std::cell::RefCell;

use relate_ir::{BinaryOp, Expr};
use relate_names::{NamedToken, SmallName, Token};

use super::memo::StatsMemo;
use super::{ContainsCount, PatternStats};
use crate::plan::{plan_with_env, SeedScope};
use crate::plan_with_stats;

/// The statement every memo test plans: two disconnected patterns, so
/// the greedy `order_patterns` loop reprices the second pattern after
/// the first is chosen — its unbound `m:Post` start endpoint consults
/// `contains_rows` again. The consult set covers both memo key forms
/// (a parameter and literals) and two distinct questions on one
/// `(label, property)` — the `f.age > 30` and `f.age >= 25` windows —
/// so the once-each check proves questions, not just pairs, repeat.
const STMT: &str = "MATCH (a:Person)-[:KNOWS]->(f:Person), (m:Post)-[:HAS_TAG]->(t:Tag) \
                    WHERE a.name = $name AND a.name = 'Alice' \
                    AND f.age > 30 AND f.age >= 25 \
                    AND m.content CONTAINS 'graph' \
                    RETURN t";

/// Which value-keyed consult reached the inner stats.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Method {
    Equality,
    Range,
    Contains,
}

/// One consult as it reached the inner stats: the method beside the
/// typed `(label, property, operand)` it was asked with — the raw
/// `Expr` operand, not a rendering of it, so the once-each check
/// compares questions, not their string spellings.
#[derive(Clone, PartialEq, Debug)]
struct Consult {
    method: Method,
    label: SmallName,
    property: SmallName,
    operand: Expr,
}

/// ADR-0508's counting fake: answers the three value-keyed consults
/// from fixed tables while recording, per method, every typed
/// `(label, property, operand)` that reached it. The once-each check
/// reads the record; unmemoized planning through
/// [`crate::plan::plan_with_env`] repeats entries in it, so the
/// once-each assertion fails if the memo is bypassed.
struct Counting {
    consults: RefCell<Vec<Consult>>,
}

fn name_of(name: &SmallName) -> String {
    name.with_str(str::to_string)
}

impl Counting {
    fn record(&self, method: Method, label: &SmallName, property: &NamedToken, operand: &Expr) {
        self.consults.borrow_mut().push(Consult {
            method,
            label: label.clone(),
            property: property.0.clone(),
            operand: operand.clone(),
        });
    }
}

impl PatternStats for Counting {
    fn label_rows(&self, (label, _): &NamedToken) -> Option<u64> {
        match name_of(label).as_str() {
            "Person" => Some(1_000),
            "Post" => Some(20_000),
            "Tag" => Some(16_000),
            _ => None,
        }
    }

    fn equality_rows(
        &self,
        (label, _): &NamedToken,
        property: &NamedToken,
        value: &Expr,
    ) -> Option<u64> {
        self.record(Method::Equality, label, property, value);
        match (name_of(label).as_str(), name_of(&property.0).as_str()) {
            // Two different answers on the same (label, property): the
            // once-each check can tell the two `name` questions apart.
            ("Person", "name") => match value {
                Expr::Parameter(_) => Some(1),
                Expr::String(_) => Some(2),
                _ => None,
            },
            _ => None,
        }
    }

    fn range_rows(
        &self,
        (label, _): &NamedToken,
        property: &NamedToken,
        op: BinaryOp,
        bound: &Expr,
    ) -> Option<u64> {
        self.record(Method::Range, label, property, bound);
        match (name_of(label).as_str(), name_of(&property.0).as_str()) {
            ("Person", "age") => match op {
                BinaryOp::Gt => Some(400),
                BinaryOp::Gte => Some(500),
                _ => None,
            },
            _ => None,
        }
    }

    fn contains_rows(
        &self,
        (label, _): &NamedToken,
        property: &NamedToken,
        needle: &Expr,
    ) -> Option<ContainsCount> {
        self.record(Method::Contains, label, property, needle);
        match (name_of(label).as_str(), name_of(&property.0).as_str()) {
            ("Post", "content") => Some(ContainsCount::SampleScan {
                rows: 50,
                scan: 20_000,
            }),
            _ => None,
        }
    }

    fn rel_type_rows(&self, (rel_type, _): &NamedToken) -> Option<u64> {
        match name_of(rel_type).as_str() {
            "KNOWS" => Some(5_000),
            "HAS_TAG" => Some(60_000),
            _ => None,
        }
    }

    fn degree_tail(&self, (label, _): &NamedToken, (rel_type, _): &NamedToken) -> Option<u64> {
        match (name_of(label).as_str(), name_of(rel_type).as_str()) {
            ("Person", "KNOWS") => Some(9),
            _ => None,
        }
    }
}

/// ADR-0508's once-each check: every distinct value-keyed consult
/// reaches the inner stats exactly once per statement — the greedy
/// order loop's repricing repeats are answered from the memo — and
/// all three memoized methods actually ran.
#[test]
fn value_keyed_consults_reach_the_inner_stats_once_each() {
    let stats = Counting {
        consults: RefCell::default(),
    };
    let query = relate_front::parse_and_lower(STMT).unwrap();
    plan_with_stats(query, &mut |_| false, &stats).unwrap();
    // The expected set, compared order-free: every question exactly once
    // — a repeat (or a miss) fails here, not just a count mismatch.
    let expected = vec![
        Consult {
            method: Method::Equality,
            label: "Person".into(),
            property: "name".into(),
            operand: Expr::Parameter("name".into()),
        },
        Consult {
            method: Method::Range,
            label: "Person".into(),
            property: "age".into(),
            operand: Expr::Int(30),
        },
        Consult {
            method: Method::Range,
            label: "Person".into(),
            property: "age".into(),
            operand: Expr::Int(25),
        },
        Consult {
            method: Method::Contains,
            label: "Post".into(),
            property: "content".into(),
            operand: Expr::String("graph".into()),
        },
    ];
    let mut recorded: Vec<Consult> = stats.consults.borrow().clone();
    assert_eq!(
        recorded.len(),
        expected.len(),
        "consults that reached the inner stats: {recorded:?}"
    );
    for want in &expected {
        let before = recorded.len();
        recorded.retain(|got| got != want);
        assert_eq!(
            recorded.len(),
            before - 1,
            "expected exactly one {want:?}; remaining: {recorded:?}"
        );
    }
}

/// A name beside the unresolved token, the form an unplanned pattern holds.
fn named(name: &str) -> NamedToken {
    (name.into(), Token::Unresolved)
}

/// The memo forwards every method it does not memoize: each counter
/// read (`label_rows`, `rel_type_rows`, `degree_tail`) returns the inner
/// stats' answer unchanged — the forwarding half of the wrapper's
/// contract, pinned against a future method silently growing a second
/// policy.
#[test]
fn unmemoized_methods_forward_the_inner_answers() {
    let stats = Counting {
        consults: RefCell::default(),
    };
    let memo = StatsMemo::new(&stats);
    assert_eq!(memo.label_rows(&named("Person")), Some(1_000));
    assert_eq!(memo.rel_type_rows(&named("KNOWS")), Some(5_000));
    assert_eq!(memo.degree_tail(&named("Person"), &named("KNOWS")), Some(9));
    assert_eq!(memo.label_rows(&named("Missing")), None);
    assert_eq!(memo.degree_tail(&named("Missing"), &named("KNOWS")), None);
}

/// ADR-0508's transparency check: the memoized plan is identical to
/// the unmemoized one — same statement planned straight through
/// [`plan_with_env`] against the same stats.
#[test]
fn memoized_planning_matches_unmemoized() {
    let stats = Counting {
        consults: RefCell::default(),
    };
    let memoized = plan_with_stats(
        relate_front::parse_and_lower(STMT).unwrap(),
        &mut |_| false,
        &stats,
    )
    .unwrap();
    let raw = plan_with_env(
        relate_front::parse_and_lower(STMT).unwrap(),
        SeedScope::default(),
        &mut |_| false,
        &stats,
    )
    .unwrap();
    // No `USE` clause, so the entry-point `use_target` lift is `None`
    // on both sides and the debug renders compare plan-shape for
    // plan-shape.
    assert_eq!(format!("{memoized:?}"), format!("{raw:?}"));
}

/// Counts consults by the token they carry: a resolved plan hands every
/// consult an id, an unresolved one hands the name alone.
#[derive(Default)]
struct IdCounting {
    name_calls: std::cell::Cell<u32>,
    id_calls: std::cell::Cell<u32>,
}

impl IdCounting {
    fn tally(&self, token: Token) -> Option<u64> {
        let calls = match token {
            Token::Id(_) => &self.id_calls,
            Token::Unresolved | Token::Absent => &self.name_calls,
        };
        calls.set(calls.get() + 1);
        Some(10)
    }
}

impl PatternStats for IdCounting {
    fn label_rows(&self, (_, token): &NamedToken) -> Option<u64> {
        self.tally(*token)
    }

    fn equality_rows(&self, (_, token): &NamedToken, _: &NamedToken, _: &Expr) -> Option<u64> {
        self.tally(*token)
    }

    fn contains_rows(
        &self,
        (_, token): &NamedToken,
        _: &NamedToken,
        _: &Expr,
    ) -> Option<ContainsCount> {
        self.tally(*token).map(ContainsCount::TextIndex)
    }

    fn rel_type_rows(&self, (_, token): &NamedToken) -> Option<u64> {
        self.tally(*token)
    }

    fn degree_tail(&self, (_, label): &NamedToken, _: &NamedToken) -> Option<u64> {
        self.tally(*label)
    }
}

/// Resolves every name to an id derived from its length.
struct LenResolver;

impl relate_names::TokenResolver for LenResolver {
    fn resolve(&self, name: &str) -> relate_names::Token {
        relate_names::Token::Id(relate_names::TokenId(name.len() as u32))
    }
}

#[test]
fn resolved_plan_consults_counters_by_id_only() {
    let stmt = "MATCH (a:Person)-[:KNOWS]->(f:Person) RETURN f";
    let stats = IdCounting::default();
    crate::plan_with_tokens(
        relate_front::parse_and_lower(stmt).unwrap(),
        &mut |_| false,
        &stats,
        &LenResolver,
    )
    .unwrap();
    assert!(stats.id_calls.get() > 0, "no id-keyed consult happened");
    assert_eq!(stats.name_calls.get(), 0, "name-keyed consults leaked");

    // Without resolution the same statement rides the name forms.
    let stats = IdCounting::default();
    plan_with_stats(
        relate_front::parse_and_lower(stmt).unwrap(),
        &mut |_| false,
        &stats,
    )
    .unwrap();
    assert_eq!(stats.id_calls.get(), 0);
    assert!(stats.name_calls.get() > 0);
}
