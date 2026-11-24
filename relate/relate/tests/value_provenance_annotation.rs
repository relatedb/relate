//! ADR-0610's in-statement metadata channel, `@txMetadata` — under BOTH
//! gates it layers on: `cypher-annotations` (the annotation parses at
//! all) and `value-provenance` (the capture seeds the record). The two
//! spellings of one map (a positional map literal, all-named parameters),
//! the optional `type` key, merge order across annotations, the
//! never-captured rule for every other annotation name, and the channel's
//! equivalence with `CALL tx.setMetaData` across the three query
//! surfaces.
//!
//! This file is its own test binary: `RELATE_PREVIEW` is process-global
//! and read once per `Database::open` (ADR-0371), so the closed sides —
//! `cypher_annotations_closed.rs` (neither gate) and
//! `value_provenance_closed.rs` — must never share its process; the
//! discard side with `cypher-annotations` on and `value-provenance` off
//! lives in `cypher_annotations_gate.rs`, the binary that already opts
//! into exactly that set.

mod common;

use std::collections::BTreeMap;

use relate::{Database, TransactionRecord, Value};

/// Opt this process into both gates before the first open — the
/// channel needs the annotation grammar to parse and the record
/// machinery to seed.
fn gated() -> Database {
    common::set_preview_once("cypher-annotations,value-provenance");
    Database::open(":memory:").unwrap()
}

/// The bookmark sequence of one committed auto-commit write — the
/// commitTs its record carries.
fn sequence_of(bookmark: Option<&str>) -> u64 {
    bookmark
        .and_then(|b| b.strip_prefix("relate:"))
        .and_then(|n| n.parse().ok())
        .expect("the auto-commit write carries a bookmark")
}

/// The one retained record of the commit at `commit_ts`.
fn record_at(db: &Database, commit_ts: u64) -> TransactionRecord {
    db.transaction_records()
        .unwrap()
        .into_iter()
        .find(|record| record.commit_ts == commit_ts)
        .unwrap_or_else(|| panic!("the commit at {commit_ts} retained a record"))
}

/// The `by: app` metadata map, spelled once for the expectations below.
fn by_app() -> BTreeMap<String, Value> {
    BTreeMap::from([("by".to_string(), Value::String("app".into()))])
}

#[test]
fn the_positional_map_spelling_seeds_the_record() {
    let db = gated();
    let mut c = db.connect().unwrap();
    let result = c
        .query("@txMetadata({by: 'app', n: 2}) CREATE (n:P {k: 1}) RETURN id(n) AS id")
        .unwrap();
    let record = record_at(&db, sequence_of(result.bookmark.as_deref()));
    // The untyped channel's contract: the home type and the spelled map.
    assert_eq!(record.record_type.as_str(), "provenance");
    assert_eq!(
        record.metadata.as_ref(),
        Some(&BTreeMap::from([
            ("by".to_string(), Value::String("app".into())),
            ("n".to_string(), Value::Int(2)),
        ]))
    );
    assert!(record.changed_keys.is_some());
}

#[test]
fn the_named_spelling_seeds_the_record() {
    let db = gated();
    let mut c = db.connect().unwrap();
    let result = c
        .query(
            "@txMetadata(by: 'app', tags: ['a', 'b'], nested: {x: 1}) \
             CREATE (n:P {k: 1}) RETURN id(n) AS id",
        )
        .unwrap();
    let record = record_at(&db, sequence_of(result.bookmark.as_deref()));
    assert_eq!(record.record_type.as_str(), "provenance");
    assert_eq!(
        record.metadata.as_ref(),
        Some(&BTreeMap::from([
            ("by".to_string(), Value::String("app".into())),
            (
                "tags".to_string(),
                Value::List(vec![Value::String("a".into()), Value::String("b".into())].into())
            ),
            (
                "nested".to_string(),
                Value::Map(BTreeMap::from([("x".to_string(), Value::Int(1))]).into())
            ),
        ]))
    );
}

#[test]
fn the_type_key_names_the_record_type_and_is_consumed() {
    let db = gated();
    let mut c = db.connect().unwrap();
    // The positional spelling: the key leaves the map, the family is the
    // named one, and changed keys are the provenance family's derivation
    // alone — a typed commit carries none.
    let result = c
        .query(
            "@txMetadata({type: 'audit-trail', why: 'run'}) CREATE (n:P {k: 1}) RETURN id(n) AS id",
        )
        .unwrap();
    let record = record_at(&db, sequence_of(result.bookmark.as_deref()));
    assert_eq!(record.record_type.as_str(), "audit-trail");
    assert_eq!(
        record.metadata.as_ref(),
        Some(&BTreeMap::from([(
            "why".to_string(),
            Value::String("run".into())
        )]))
    );
    assert_eq!(record.changed_keys, None);

    // The named spelling: same consumption, same family.
    let result = c
        .query(
            "@txMetadata(type: 'audit-trail', why: 'run2') CREATE (n:P {k: 2}) RETURN id(n) AS id",
        )
        .unwrap();
    let record = record_at(&db, sequence_of(result.bookmark.as_deref()));
    assert_eq!(record.record_type.as_str(), "audit-trail");
    assert_eq!(
        record.metadata.as_ref(),
        Some(&BTreeMap::from([(
            "why".to_string(),
            Value::String("run2".into())
        )]))
    );

    // The home type spelled explicitly: admitted as the default, where
    // the embedded typed twin reserves the name — the channel's home.
    let result = c
        .query(
            "@txMetadata({type: 'provenance', by: 'app'}) CREATE (n:P {k: 3}) RETURN id(n) AS id",
        )
        .unwrap();
    let record = record_at(&db, sequence_of(result.bookmark.as_deref()));
    assert_eq!(record.record_type.as_str(), "provenance");
    assert_eq!(record.metadata.as_ref(), Some(&by_app()));
    assert!(record.changed_keys.is_some());
}

#[test]
fn the_channel_rejections_are_the_value_domain_errors() {
    let db = gated();
    let mut c = db.connect().unwrap();
    for (query, needle) in [
        // A positional parameter that is not a map: the channel's
        // spelling rule, not the annotation grammar's (the parser
        // accepted it — the discard keeps it valid while the channel's
        // own gate is off, the closed binary's case).
        (
            "@txMetadata(5) CREATE (n:P {k: 1})",
            "positional parameter must be a map literal",
        ),
        // A non-string type key.
        (
            "@txMetadata({type: 2, by: 'app'}) CREATE (n:P {k: 1})",
            "type key must be a string, not INTEGER",
        ),
        // Another family's name through the canonical validation: the
        // same rejection the embedded typed twin raises.
        (
            "@txMetadata({type: 'Audit Trail'}) CREATE (n:P {k: 1})",
            "record type 'Audit Trail' must be a non-empty kebab-case name",
        ),
    ] {
        let error = c.query(query).expect_err(query).to_string();
        assert!(error.contains(needle), "{query}: {error}");
    }
    // Nothing seeded: the refused statements left no record at all.
    assert!(db.transaction_records().unwrap().is_empty());
}

#[test]
fn annotations_merge_in_clause_order_later_keys_winning() {
    let db = gated();
    let mut c = db.connect().unwrap();
    // One statement, three annotated clauses: the maps merge in clause
    // order, a shared key takes the later annotation's value, and the
    // last annotation's `type` owns the record's family.
    let result = c
        .query(
            "@txMetadata({a: 1, shared: 'first'}) CREATE (n:A {k: 1}) \
             @txMetadata(b: 2) CREATE (m:B {k: 1}) \
             @txMetadata({shared: 'last', type: 'audit-trail'}) CREATE (p:C {k: 1}) \
             RETURN id(p) AS id",
        )
        .unwrap();
    let record = record_at(&db, sequence_of(result.bookmark.as_deref()));
    assert_eq!(record.record_type.as_str(), "audit-trail");
    assert_eq!(
        record.metadata.as_ref(),
        Some(&BTreeMap::from([
            ("a".to_string(), Value::Int(1)),
            ("shared".to_string(), Value::String("last".into())),
            ("b".to_string(), Value::Int(2)),
        ]))
    );

    // The last channel call owns the type both ways: a later untyped
    // annotation returns the transaction to the home type, exactly like
    // the embedded channels' ordering rule.
    let result = c
        .query(
            "@txMetadata({type: 'audit-trail', x: 1}) CREATE (n:D {k: 1}) \
             @txMetadata({y: 2}) CREATE (m:E {k: 1}) RETURN id(m) AS id",
        )
        .unwrap();
    let record = record_at(&db, sequence_of(result.bookmark.as_deref()));
    assert_eq!(record.record_type.as_str(), "provenance");
    assert_eq!(
        record.metadata.as_ref(),
        Some(&BTreeMap::from([
            ("x".to_string(), Value::Int(1)),
            ("y".to_string(), Value::Int(2)),
        ]))
    );
    assert!(record.changed_keys.is_some());
}

#[test]
fn every_other_annotation_name_is_never_captured() {
    let db = gated();
    let mut c = db.connect().unwrap();
    // An unrecognized annotation carrying a map: checked and discarded
    // (CIP-281), never captured — the commit's record is the plain
    // write's: the family's own type, no metadata, its changed keys.
    let result = c
        .query("@showHint({by: 'app'}) @other(level: 2) CREATE (n:P {k: 1}) RETURN id(n) AS id")
        .unwrap();
    let record = record_at(&db, sequence_of(result.bookmark.as_deref()));
    assert_eq!(record.record_type.as_str(), "provenance");
    assert_eq!(record.metadata, None);
    assert!(record.changed_keys.is_some());
}

#[test]
fn explain_never_seeds_the_channel() {
    let db = gated();
    let mut c = db.connect().unwrap();
    // A plan-only statement compiles and renders, never executes — its
    // annotations seed nothing (the seed applies on the run path, after
    // the EXPLAIN early return).
    c.query("EXPLAIN @txMetadata({by: 'app'}) CREATE (n:P {k: 1})")
        .unwrap();
    let result = c.query("CREATE (n:Q {k: 1}) RETURN id(n) AS id").unwrap();
    let record = record_at(&db, sequence_of(result.bookmark.as_deref()));
    assert_eq!(record.metadata, None);
}

#[test]
fn a_refused_annotated_statement_seeds_nothing() {
    // The seed applies after the admission gates, the last point where
    // the statement is guaranteed to run — so a statement the gates
    // refuse (a referenced parameter the caller did not supply,
    // ADR-0308) seeds nothing, and its annotations cannot ride the
    // next commit's record.
    let db = gated();
    let mut c = db.connect().unwrap();
    let error = c
        .query("@txMetadata({by: 'app'}) CREATE (n:P {k: $k}) RETURN id(n) AS id")
        .expect_err("the missing parameter refuses the statement");
    assert!(
        error.to_string().contains("Expected parameter(s): k"),
        "{error}"
    );
    // The refused statement's seed went with its statement: the next
    // commit's record carries no metadata.
    let result = c.query("CREATE (n:P {k: 1}) RETURN id(n) AS id").unwrap();
    let record = record_at(&db, sequence_of(result.bookmark.as_deref()));
    assert_eq!(record.metadata, None);
}

#[test]
fn an_annotated_read_seeds_every_run_not_just_the_first() {
    // The two spellings normalize to one plan-cache template (only the
    // map's literal differs), so the second run would be a cache hit if
    // an annotated statement were cacheable — the normalizer's `@` bail
    // keeps every run parsing, so every run's seed lands on the cell
    // ahead of the next boundary.
    let db = gated();
    let mut c = db.connect().unwrap();
    c.query("@txMetadata({run: 'first'}) RETURN 1").unwrap();
    c.query("@txMetadata({run: 'second'}) RETURN 1").unwrap();
    let result = c.query("CREATE (n:P {k: 1}) RETURN id(n) AS id").unwrap();
    let record = record_at(&db, sequence_of(result.bookmark.as_deref()));
    assert_eq!(
        record.metadata.as_ref(),
        Some(&BTreeMap::from([(
            "run".to_string(),
            Value::String("second".into())
        )]))
    );
}

#[test]
fn the_channel_answers_as_the_call_channel_does() {
    // The one equivalence the contract records: the same write seeded
    // through the in-statement annotation and through `CALL
    // tx.setMetaData` — two fresh stores, so commitTs, lsn and the
    // element ids are comparable — answer identically across
    // `provenance()`, `history()` and `db.transactions`.
    let annotated = gated();
    let mut a = annotated.connect().unwrap();
    let called = gated();
    let mut b = called.connect().unwrap();

    a.query("@txMetadata({agent: 'a'}) CREATE (n:Doc {price: 1}) RETURN id(n) AS id")
        .unwrap();
    b.query("CALL tx.setMetaData({agent: 'a'}) CREATE (n:Doc {price: 1}) RETURN id(n) AS id")
        .unwrap();

    let query = |c: &mut relate::Connection, statement: &str| c.query(statement).unwrap().rows;
    for statement in [
        "MATCH (element:Doc) RETURN provenance(element, 'price') AS p",
        "MATCH (element:Doc) RETURN history(element, 'price') AS h",
        "CALL db.transactions()",
    ] {
        let from_annotation = query(&mut a, statement);
        let from_call = query(&mut b, statement);
        assert_eq!(
            from_annotation, from_call,
            "{statement}: the two channels diverged"
        );
    }
}
