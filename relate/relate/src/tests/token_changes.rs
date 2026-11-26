//! ADR-0687 end-to-end: the per-token last-changed table through the public
//! API. Each test drives real commits and reads what the publication
//! stamped — the mutation seams' reports (`Graph`'s changed-token set) folded
//! into the table at the stats generation the commit advanced to.

use crate::tests::token_id;
use crate::{Database, Shared};

/// The generation the store is at now.
fn generation(shared: &Shared) -> u64 {
    shared.stats_generation.current()
}

/// A node create reports its label ids and nothing else: the property keys
/// ride no token of their own (the label stamp covers the (label, key)
/// cardinality), so exactly one slot — the label's — is stamped.
#[test]
fn a_create_stamps_only_its_label_id() {
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    c.query("CREATE (:Person {name: 'ada'})").unwrap();
    let shared = db.shared();
    let person = token_id(&shared, "Person");
    let name = token_id(&shared, "name");
    let table = shared.lock_token_changes();
    assert_eq!(table.last_changed(person), generation(&shared));
    assert_eq!(table.last_changed(name), 0, "a create reports no key");
    assert_eq!(
        table.stamped_tokens(),
        vec![person],
        "one label, one stamped slot"
    );
}

/// A property set reports the interned key and the node's labels: the
/// (label, key) cardinalities a seek consult reads both moved.
#[test]
fn a_property_set_stamps_the_key_and_the_labels() {
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    c.query("CREATE (:Person {name: 'ada'})").unwrap();
    c.query("MATCH (n:Person) SET n.age = 41").unwrap();
    let shared = db.shared();
    let person = token_id(&shared, "Person");
    let name = token_id(&shared, "name");
    let age = token_id(&shared, "age");
    let stamped_at = generation(&shared);
    let table = shared.lock_token_changes();
    assert_eq!(table.last_changed(age), stamped_at);
    assert_eq!(table.last_changed(person), stamped_at);
    assert_eq!(table.last_changed(name), 0, "the set touched no other key");
}

/// The delete seams report through the ADR-chosen site: a detached node
/// delete hands its dying labels out of `Graph::delete_node`, and the
/// incident relationship's own delete hands its type id out of
/// `Graph::delete_rel` — the tokens no later walk can recover from the redo
/// op: the dying labels, the dying rel's type, and the dying versions'
/// property keys (ADR-0687 decision 7).
#[test]
fn deletes_stamp_the_dying_labels_and_the_rel_type() {
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    c.query("CREATE (:Person {name: 'ada'})-[:KNOWS {since: 2020}]->(:Person {name: 'bob'})")
        .unwrap();
    c.query("MATCH (n:Person {name: 'ada'}) DETACH DELETE n")
        .unwrap();
    let shared = db.shared();
    let person = token_id(&shared, "Person");
    let knows = token_id(&shared, "KNOWS");
    let name = token_id(&shared, "name");
    let since = token_id(&shared, "since");
    let deleted_at = generation(&shared);
    let table = shared.lock_token_changes();
    assert_eq!(table.last_changed(person), deleted_at);
    assert_eq!(table.last_changed(knows), deleted_at);
    assert_eq!(table.last_changed(name), deleted_at, "the dying node's key");
    assert_eq!(table.last_changed(since), deleted_at, "the dying rel's key");
}

/// A commit on other tokens leaves every earlier stamp where it was: the
/// table moves only the tokens its own writes reported.
#[test]
fn an_unrelated_commit_keeps_every_earlier_generation() {
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    c.query("CREATE (:Person)").unwrap();
    let shared = db.shared();
    let person = token_id(&shared, "Person");
    let created_at = generation(&shared);
    c.query("CREATE (:Other)").unwrap();
    let other = token_id(&shared, "Other");
    let other_at = generation(&shared);
    assert!(other_at > created_at, "each commit advances the generation");
    let table = shared.lock_token_changes();
    assert_eq!(table.last_changed(person), created_at, "untouched, kept");
    assert_eq!(table.last_changed(other), other_at);
    assert_eq!(table.stamped_tokens(), vec![person, other]);
}

/// Schema DDL moves the full-invalidation epoch and stamps no footprint: an
/// index and a constraint create and drop each move it once.
#[test]
fn schema_ddl_advances_the_epoch() {
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    c.query("CREATE INDEX person_name FOR (n:Person) ON (n.name)")
        .unwrap();
    let shared = db.shared();
    let after_create_index = shared.lock_token_changes().epoch();
    assert_eq!(after_create_index, 1, "the create moved the epoch");
    assert!(
        shared.lock_token_changes().stamped_tokens().is_empty(),
        "schema DDL stamps no footprint slot"
    );
    // The property's index is dropped before the constraint takes it — the
    // catalog refuses a constraint over an existing index (and the mirror).
    c.query("DROP INDEX person_name").unwrap();
    assert_eq!(shared.lock_token_changes().epoch(), after_create_index + 1);
    c.query("CREATE CONSTRAINT person_name_unique FOR (n:Person) REQUIRE n.name IS UNIQUE")
        .unwrap();
    assert_eq!(shared.lock_token_changes().epoch(), after_create_index + 2);
    c.query("DROP CONSTRAINT person_name_unique").unwrap();
    assert_eq!(shared.lock_token_changes().epoch(), after_create_index + 3);
}

/// A data commit never moves the epoch: only schema DDL does.
#[test]
fn data_commits_leave_the_epoch_alone() {
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    c.query("CREATE (:Person {name: 'ada'})").unwrap();
    c.query("MATCH (n:Person) SET n.age = 41").unwrap();
    c.query("MATCH (n:Person) DETACH DELETE n").unwrap();
    assert_eq!(db.shared().lock_token_changes().epoch(), 0);
}
