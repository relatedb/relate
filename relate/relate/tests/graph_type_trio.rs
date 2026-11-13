//! The graph-type trio behind the ADR-0371 preview gates: optional
//! properties (`graph-type-opt-props`), property default values
//! (`graph-type-default-values`), and implied labels identifying node
//! element types -- `graph-type-implied-labels` (inheritance). Every
//! case below is authored locally for this suite, and every expectation
//! was recomputed by running these fixtures through this engine
//! (adjusted only where this engine's statement-is-the-transaction
//! commit boundary names itself).
//!
//! The gates are process-level (ADR-0371: `Database::open` resolves
//! `RELATE_PREVIEW` once), and an integration-test crate is its own
//! process -- set the three features before any store opens. The
//! ungated fail-closed errors are pinned in `tests/graph_type.rs` (a
//! separate, ungated process).

mod common;

use relate::Value;

use common::{connection, err_text, run, scalar_one, string};

/// The gated connection: the trio's three preview features, set before
/// any store opens (fail loudly rather than run half-gated, the same
/// contract as the hybrid-search fixture).
fn gated_connection() -> relate::Connection {
    common::set_preview_once(
        "graph-type-opt-props,graph-type-default-values,graph-type-implied-labels",
    );
    connection()
}

fn spec_of(c: &mut relate::Connection) -> Value {
    scalar_one(c, "SHOW CURRENT GRAPH TYPE")
}

/// The specification string (the caller pinned a `Value::String`).
fn spec_string(c: &mut relate::Connection) -> String {
    let Value::String(spec) = spec_of(c) else {
        panic!("expected a specification string");
    };
    spec.to_string()
}

// ---------------------------------------------------------------------
// Optional properties (`<property> :: ANY`)
// ---------------------------------------------------------------------

#[test]
fn optional_properties_set_show_and_drop() {
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET {
            (:Dock => {tonnage :: ANY}),
            ()-[:MOORED => {line1 :: ANY, line2 :: ANY, line3 :: ANY}]->()
        }",
    );
    let spec = spec_string(&mut c);
    assert!(spec.contains(":`Dock` => {`tonnage` :: ANY}"), "{spec}");
    assert!(
        spec.contains("[:`MOORED` => {`line1` :: ANY, `line2` :: ANY, `line3` :: ANY}]"),
        "{spec}"
    );

    // The key-constraint shorthand works on an optional property
    // (on ADD: the engine's ALTER verb does not accept constraint specs).
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE ADD { (:Quay => {code :: ANY IS KEY}) }",
    );
    let spec = spec_string(&mut c);
    assert!(spec.contains(":`Quay` => {`code` :: ANY}"), "{spec}");

    // Dropping restates the full definition or the bare label.
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE DROP { (:Quay =>), (:Dock =>), ()-[:MOORED =>]->() }",
    );
    // The Quay type's inline KEY constraint survives the element-type drop
    // as the body's own (undesignated) constraint; the element types are
    // gone.
    let spec = spec_string(&mut c);
    assert!(
        spec.contains("FOR (`n`:`Quay`) REQUIRE (`n`.`code`) IS KEY"),
        "{spec}"
    );
    // The rule keeps its Neo4j-generated hash name.
    assert!(spec.contains("CONSTRAINT `constraint_"), "{spec}");
    assert!(!spec.contains("=>"), "no element type remains: {spec}");
}

#[test]
fn an_optional_property_restricts_nothing() {
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:Dock => {tonnage :: ANY}) }",
    );
    // The ANY property type restricts nothing: any storable value fits,
    // and an absent property violates nothing (open graph type
    // semantics).
    run(
        &mut c,
        "CREATE (:Dock {tonnage: 42}), (:Dock {tonnage: 'many'})",
    );
    run(&mut c, "CREATE (:Dock)");
    let count = scalar_one(&mut c, "MATCH (n:Dock) RETURN count(n)");
    assert_eq!(count, Value::Int(3));
}

#[test]
fn optional_properties_inherit_and_restrict() {
    // An ANY-typed parent property restricted by the child, over
    // implied-label inheritance.
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET {
            (:Pier1 => {tonnage :: ANY}),
            (:Pier2 => :Pier1 {tonnage :: STRING}),
            (:Pier3 => :Pier1 {tonnage :: ANY NOT NULL}),
            (:Pier4 => :Pier1 {tonnage :: STRING NOT NULL})
        }",
    );
    run(&mut c, "CREATE (:Pier1 {tonnage: true})");
    run(&mut c, "CREATE (:Pier1:Pier2 {tonnage: 'small'})");
    // Pier3/Pier4 require the property's existence. The node carries the
    // implied Pier1 so only the property rule can be violated (the
    // implied-label rule would otherwise be reported first, and its
    // selection order depends on the generated constraint names).
    let err = err_text(c.query("CREATE (:Pier1:Pier3)"));
    assert!(err.contains("requires properties"), "{err}");
    run(&mut c, "CREATE (:Pier1:Pier4 {tonnage: 'ok'})");
    // Pier2's restriction is enforced on its own nodes.
    let err = err_text(c.query("CREATE (:Pier1:Pier2 {tonnage: 42})"));
    assert!(err.contains("property type"), "{err}");
}

// ---------------------------------------------------------------------
// Property default values (`DEFAULT ON COMMIT`)
// ---------------------------------------------------------------------

#[test]
fn default_values_show_canonically() {
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET {
            (:Dock => {reg :: UUID NOT NULL DEFAULT ON COMMIT uuid()}),
            ()-[:MOORED => {log :: STRING DEFAULT ON COMMIT \"standby\"}]->()
        }",
    );
    let spec = spec_string(&mut c);
    assert!(
        spec.contains("`reg` :: UUID NOT NULL DEFAULT ON COMMIT uuid()"),
        "{spec}"
    );
    // The canonical string literal is single-quoted.
    assert!(
        spec.contains("`log` :: STRING DEFAULT ON COMMIT 'standby'"),
        "{spec}"
    );
}

#[test]
fn a_created_node_gets_the_default_at_commit_not_before() {
    // The default lands at the statement boundary -- the creating
    // statement's own RETURN still sees null; the next statement sees
    // the generated value.
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:Dock => {reg :: UUID NOT NULL DEFAULT ON COMMIT uuid()}) }",
    );
    let seen = scalar_one(&mut c, "CREATE (n:Dock {}) RETURN n.reg");
    assert_eq!(seen, Value::Null);
    let reg = scalar_one(&mut c, "MATCH (n:Dock) RETURN n.reg");
    let rendered = format!("{reg}");
    // The engine renders a UUID as `uuid('…')` (44 characters).
    assert!(
        rendered.starts_with("uuid('") && rendered.len() == 44,
        "a generated UUID value: {rendered}"
    );
}

#[test]
fn an_explicit_value_wins_and_an_explicit_null_takes_the_default() {
    // An explicit non-null value is kept; an explicit null is the same
    // as an absent property -- the default applies at commit.
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:Dock => {reg :: INTEGER DEFAULT ON COMMIT 42}) }",
    );
    let seen = scalar_one(&mut c, "CREATE (n:Dock {reg: 7}) RETURN n.reg");
    assert_eq!(seen, Value::Int(7));
    let seen = scalar_one(&mut c, "CREATE (n:Dock {reg: null}) RETURN n.reg");
    assert_eq!(seen, Value::Null);
    let after = scalar_one(&mut c, "MATCH (n:Dock) WHERE n.reg = 42 RETURN count(n)");
    assert_eq!(after, Value::Int(1));
}

#[test]
fn a_relationship_type_default_applies_on_create() {
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET {
            ()-[:MOORED => {log :: STRING DEFAULT ON COMMIT \"standby\"}]->()
        }",
    );
    let seen = scalar_one(&mut c, "CREATE ()-[r:MOORED]->() RETURN r.log");
    assert_eq!(seen, Value::Null);
    let after = scalar_one(&mut c, "MATCH ()-[r:MOORED]->() RETURN r.log");
    assert_eq!(after, string("standby"));
    // An explicit value is kept.
    let seen = scalar_one(
        &mut c,
        "CREATE ()-[r:MOORED {log: 'ahoy'}]->() RETURN r.log",
    );
    assert_eq!(seen, string("ahoy"));
}

#[test]
fn adding_the_identifying_label_applies_the_default() {
    // SET n:<identifying label> on a node without the property applies
    // the default; with the property it does not.
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:Tug => {callSign :: STRING NOT NULL DEFAULT ON COMMIT \"ready\"}) }",
    );
    run(&mut c, "CREATE (n:Barge {callSign: 'ahoy!'})");
    run(&mut c, "CREATE (n:Barge)");
    // The statement's own RETURN still shows the pre-existing value and
    // null; the defaulted value only shows after the boundary.
    let seen = rows_of(&mut c, "MATCH (n:Barge) SET n:Tug RETURN n.callSign");
    assert_eq!(seen, vec![string("ahoy!"), Value::Null]);
    let after = rows_of(&mut c, "MATCH (n:Tug:Barge) RETURN n.callSign");
    assert_eq!(after, vec![string("ahoy!"), string("ready")]);
}

#[test]
fn removing_the_property_in_the_same_statement_is_ignored() {
    // SET n:<label> together with REMOVE of the property (either order)
    // leaves the default to apply at the boundary.
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:Tug => {callSign :: STRING NOT NULL DEFAULT ON COMMIT \"ready\"}) }",
    );
    run(&mut c, "CREATE (n:Barge {callSign: 'ahoy!'})");
    let seen = scalar_one(
        &mut c,
        "MATCH (n:Barge) SET n:Tug REMOVE n.callSign RETURN n.callSign",
    );
    assert_eq!(seen, Value::Null);
    let after = scalar_one(&mut c, "MATCH (n:Tug:Barge) RETURN n.callSign");
    assert_eq!(after, string("ready"));
}

#[test]
fn a_later_removal_sticks_when_the_type_allows_null() {
    // Outside the creating statement, removing (or nulling) the
    // property sticks -- the default never backfills an untouched
    // label's nodes.
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET {
            ()-[:SAILED => {log :: STRING DEFAULT ON COMMIT \"standby\"}]->()
        }",
    );
    run(&mut c, "CREATE ()-[r:SAILED {log: 'ahoy!'}]->()");
    run(&mut c, "MATCH ()-[r:SAILED]->() SET r.log = null");
    let after = scalar_one(&mut c, "MATCH ()-[r:SAILED]->() RETURN r.log");
    assert_eq!(after, Value::Null);
    run(&mut c, "MATCH ()-[r:SAILED]->() REMOVE r.log");
    let after = scalar_one(&mut c, "MATCH ()-[r:SAILED]->() RETURN r.log");
    assert_eq!(after, Value::Null);
}

#[test]
fn default_values_on_existing_data_are_not_backfilled() {
    // Setting a default value never touches existing nodes; with a
    // null-allowing type the update succeeds over nodes missing the
    // property.
    let mut c = gated_connection();
    run(&mut c, "CREATE (:Dock {note: 'ahoy'})");
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:Dock => {note :: STRING DEFAULT ON COMMIT 'standby'}) }",
    );
    let value = scalar_one(&mut c, "MATCH (n:Dock) RETURN n.note");
    assert_eq!(value, string("ahoy"));
}

#[test]
fn inherited_defaults_flow_down_and_child_declarations_override() {
    // An implicitly inherited default applies to the child's nodes; a
    // re-specified default wins.
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET {
            (:Pier1 => {tonnage :: INTEGER DEFAULT ON COMMIT 1}),
            (:Pier2 => :Pier1 {tonnage :: INTEGER})
        }",
    );
    let seen = scalar_one(&mut c, "CREATE (n:Pier1:Pier2 {}) RETURN n.tonnage");
    assert_eq!(seen, Value::Null);
    let after = scalar_one(&mut c, "MATCH (n:Pier2) RETURN n.tonnage");
    assert_eq!(after, Value::Int(1));
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE ALTER {
            (:Pier2 => :Pier1 {tonnage :: INTEGER DEFAULT ON COMMIT 2})
        }",
    );
    run(&mut c, "CREATE (n:Pier1:Pier2 {})");
    let after = scalar_one(
        &mut c,
        "MATCH (n:Pier2) WHERE n.tonnage = 2 RETURN count(n)",
    );
    assert_eq!(after, Value::Int(1));
}

#[test]
fn default_value_failures_fail_closed() {
    let mut c = gated_connection();
    // The default must conform to the declared value type.
    let err = err_text(c.query(
        "ALTER CURRENT GRAPH TYPE SET { (:Dock => {note :: STRING DEFAULT ON COMMIT 42}) }",
    ));
    assert!(
        err.contains("Invalid input 42 for default property value")
            && err.contains("Expected to be STRING"),
        "{err}"
    );
    // Null is not a default.
    let err = err_text(c.query(
        "ALTER CURRENT GRAPH TYPE SET { (:Dock => {note :: STRING DEFAULT ON COMMIT null}) }",
    ));
    assert!(
        err.contains("Invalid input null for default property value"),
        "{err}"
    );
    // A general expression is not a default.
    let err = err_text(c.query(
        "ALTER CURRENT GRAPH TYPE SET {
            (:Dock => {note :: STRING DEFAULT ON COMMIT toString(5 + $param)})
        }",
    ));
    assert!(
        err.contains("expected a literal or a type constructor expression"),
        "{err}"
    );
    // A map is not storable on a property even under ANY.
    let err = err_text(c.query(
        "ALTER CURRENT GRAPH TYPE SET {
            (:Dock => {tonnage :: ANY DEFAULT ON COMMIT {key: 'denied'}})
        }",
    ));
    assert!(err.contains("cannot be stored on a property"), "{err}");
    // uuid() takes no arguments in a default.
    let err = err_text(c.query(
        "ALTER CURRENT GRAPH TYPE SET {
            (:Dock => {reg :: UUID DEFAULT ON COMMIT uuid('familiar')})
        }",
    ));
    assert!(err.contains("uuid() takes no arguments"), "{err}");
}

#[test]
fn a_drop_must_restate_the_default_value() {
    // Dropping by the full definition without the default fails; with
    // it, and by the bare label, it drops.
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:Dock => {reg :: UUID NOT NULL DEFAULT ON COMMIT uuid()}) }",
    );
    let err =
        err_text(c.query("ALTER CURRENT GRAPH TYPE DROP { (:Dock => {reg :: UUID NOT NULL}) }"));
    assert!(
        err.contains("node element type specified incorrectly"),
        "{err}"
    );
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE DROP {
            (:Dock => {reg :: UUID NOT NULL DEFAULT ON COMMIT uuid()})
        }",
    );
    assert_eq!(spec_of(&mut c), string("{}"));
}

#[test]
fn two_declaring_parents_without_a_child_default_fail() {
    // Implicitly inheriting two defaults for the same property
    // (identical values included) fails at declaration time.
    let mut c = gated_connection();
    let err = err_text(c.query(
        "ALTER CURRENT GRAPH TYPE SET {
            (:Pier1 => {tonnage :: STRING DEFAULT ON COMMIT 'ready'}),
            (:Pier2 => {tonnage :: STRING DEFAULT ON COMMIT 'standby'}),
            (:Pier3 => :Pier1&:Pier2)
        }",
    ));
    assert!(
        err.contains("multiple default values coming from multiple implied node element types"),
        "{err}"
    );
}

#[test]
fn restricting_a_type_under_an_inherited_default_must_keep_it_valid() {
    // The child's restriction must not invalidate the inherited
    // default.
    let mut c = gated_connection();
    let err = err_text(c.query(
        "ALTER CURRENT GRAPH TYPE SET {
            (:Pier1 => {tonnage :: INTEGER | FLOAT DEFAULT ON COMMIT 1}),
            (:Pier2 => :Pier1 {tonnage :: FLOAT})
        }",
    ));
    assert!(
        err.contains("Invalid input 1 for default property value")
            && err.contains("Expected to be FLOAT"),
        "{err}"
    );
}

#[test]
fn a_node_with_two_declaring_labels_fails_without_an_explicit_value() {
    // Creating a node whose labels each declare a default for the same
    // property fails unless the property is explicitly set.
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET {
            (:Pier1 => {tonnage :: STRING NOT NULL DEFAULT ON COMMIT 'ready'}),
            (:Pier2 => {tonnage :: STRING NOT NULL DEFAULT ON COMMIT 'standby'})
        }",
    );
    let err = err_text(c.query("CREATE (:Pier1:Pier2)"));
    assert!(
        err.contains("more than one default value for the property"),
        "{err}"
    );
    // An explicit value settles it.
    run(&mut c, "CREATE (:Pier1:Pier2 {tonnage: 'agreed'})");
}

// ---------------------------------------------------------------------
// Implied labels identifying node element types (inheritance)
// ---------------------------------------------------------------------

#[test]
fn implicit_inheritance_declares_and_enforces_the_implied_label() {
    // The child inherits the parent's rules; its nodes must carry the
    // parent's implied labels and properties.
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET {
            (:Tug => :Barge&Cutter {flag1 :: STRING, flag2 :: ANY NOT NULL, flag3 :: INTEGER NOT NULL}),
            (:Dinghy => :Tug)
        }",
    );
    run(
        &mut c,
        "CREATE (:Tug:Barge:Cutter {flag1: 'port', flag2: 'keel', flag3: 7}), (:Tug:Barge:Cutter:Dinghy {flag1: 'port', flag2: 'keel', flag3: 8})",
    );
    // Missing an inherited implied label fails.
    let err =
        err_text(c.query("CREATE (:Tug:Barge:Dinghy {flag1: 'port', flag2: 'keel', flag3: 9})"));
    assert!(err.contains("must also have label"), "{err}");
    // Missing an inherited property fails.
    let err = err_text(c.query("CREATE (:Tug:Barge:Cutter {flag1: 'port', flag3: 9})"));
    assert!(err.contains("requires properties"), "{err}");
}

#[test]
fn a_child_restriction_is_enforced_beyond_the_parent_rules() {
    // A further restriction on the child applies to the child's own
    // nodes.
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET {
            (:Tug => {flag :: INTEGER | FLOAT}),
            (:Escort => :Tug&Barge {flag :: FLOAT NOT NULL, flag2 :: BOOLEAN})
        }",
    );
    let err = err_text(c.query("CREATE (:Tug:Barge:Escort {flag: 1})"));
    assert!(err.contains("property type"), "{err}");
    run(
        &mut c,
        "CREATE (:Tug:Barge:Escort {flag: 1.5, flag2: false})",
    );
}

#[test]
fn inheritance_chains_and_multiple_parents() {
    // Chained inheritance and the diamond.
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET {
            (:Tug => :Barge),
            (:Cutter => :Tug),
            (:Dinghy => :Cutter)
        }",
    );
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE ADD {
            (:Light1 => {flag1 :: STRING, flag2 :: ANY NOT NULL, flag4 :: FLOAT | INTEGER}),
            (:Light2 => {flag2 :: INTEGER, flag3 :: ANY NOT NULL, flag4 :: ANY NOT NULL}),
            (:Escort => :Light1&:Light2 {flag3 :: BOOLEAN NOT NULL, flag4 :: FLOAT NOT NULL})
        }",
    );
    // The effective intersection for flag2 on Escort is INTEGER NOT NULL.
    let err = err_text(c.query(
        "CREATE (:Light1:Light2:Escort {flag1: 'port', flag3: true, flag4: 1.5, flag2: 'no'})",
    ));
    assert!(err.contains("property type"), "{err}");
    run(
        &mut c,
        "CREATE (:Light1:Light2:Escort {flag1: 'port', flag2: 2, flag3: true, flag4: 1.5})",
    );
}

#[test]
fn inheritance_failures_fail_closed() {
    let mut c = gated_connection();
    // Cycles.
    let err =
        err_text(c.query("ALTER CURRENT GRAPH TYPE SET { (:Tug => :Barge), (:Barge => :Tug) }"));
    assert!(err.contains("circular inheritance"), "{err}");
    // The self-reference keeps its historical own-set rejection
    // ("identifying label repeated in implied label set"): both errors
    // fail closed; the wording is an engine-consistent choice.
    let err = err_text(c.query("ALTER CURRENT GRAPH TYPE SET { (:Tug => :Tug) }"));
    assert!(
        err.contains("circular inheritance")
            || err.contains("identifying label repeated in implied label set"),
        "{err}"
    );
    // A conflicting child declaration.
    let err = err_text(c.query(
        "ALTER CURRENT GRAPH TYPE SET {
            (:Tug => {flag :: STRING}),
            (:Barge => :Tug {flag :: INTEGER})
        }",
    ));
    assert!(
        err.contains("is not allowed as it is either in conflict with or more permissive"),
        "{err}"
    );
    // A loosening child declaration.
    let err = err_text(c.query(
        "ALTER CURRENT GRAPH TYPE SET {
            (:Tug => {flag :: STRING NOT NULL}),
            (:Barge => :Tug {flag :: STRING})
        }",
    ));
    assert!(
        err.contains("is not allowed as it is either in conflict with or more permissive"),
        "{err}"
    );
    // An implicitly conflicting diamond.
    let err = err_text(c.query(
        "ALTER CURRENT GRAPH TYPE SET {
            (:Tug => {flag :: STRING}),
            (:Barge => {flag :: INTEGER}),
            (:Cutter => :Tug&Barge)
        }",
    ));
    assert!(
        err.contains("coming from the intersection of property definitions"),
        "{err}"
    );
}

#[test]
fn unrenderable_restated_drop_defaults_fail_typed_not_panic() {
    // Stellar round 1 (the false-invariant expect): a DROP body never
    // runs the graph-type body validator, so an unrenderable restated
    // default (a function call no default shape admits) must surface as
    // the exact-match typed error, never a renderer panic.
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE ADD { (:Sailor => {id :: UUID DEFAULT ON COMMIT uuid(), name :: STRING}) }",
    );
    let err = err_text(c.query(
        "ALTER CURRENT GRAPH TYPE DROP { (:Sailor => {id :: UUID DEFAULT ON COMMIT rand()}) }",
    ));
    assert!(
        err.contains("node element type specified incorrectly"),
        "expected the exact-match typed error, got: {err}"
    );
}

#[test]
fn an_inherited_node_element_type_cannot_be_dropped() {
    // Dropping the parent without the child fails.
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:Tug => {flag :: STRING}), (:Barge => :Tug) }",
    );
    let err = err_text(c.query("ALTER CURRENT GRAPH TYPE DROP { (:Tug =>) }"));
    assert!(
        err.contains("node element type is still referenced and cannot be dropped"),
        "{err}"
    );
    // Dropping both, in one command, works.
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE DROP { (:Barge =>), (:Tug =>) }",
    );
    assert_eq!(spec_of(&mut c), string("{}"));
}

#[test]
fn adding_a_type_on_an_implied_label_is_inheritance() {
    // ADD of a type whose label another type already implies turns the
    // implication into inheritance -- allowed behind the gate (the
    // ungated store fails closed; pinned in `tests/graph_type.rs`).
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:Tug => :Barge {flag1 :: STRING}) }",
    );
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE ADD { (:Barge => {flag2 :: INTEGER}) }",
    );
    // Barge's rules now apply to Tug's nodes: a nullable type allows an
    // absent property, but a wrong-typed one fails.
    run(&mut c, "CREATE (:Tug:Barge)");
    let err = err_text(c.query("CREATE (:Tug:Barge {flag2: 'no'})"));
    assert!(err.contains("property type"), "{err}");
    run(&mut c, "CREATE (:Tug:Barge {flag1: 'port', flag2: 1})");
}

#[test]
fn show_renders_only_the_child_declarations() {
    // The canonical specification keeps the child's own declarations
    // only -- inherited definitions are not inlined.
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET {
            (:Tug => :Barge&Cutter {flag1 :: STRING, flag2 :: ANY NOT NULL, flag3 :: INTEGER NOT NULL}),
            (:Dinghy => :Cutter),
            (:Ferry => :Tug&Dinghy {flag1 :: STRING NOT NULL, flag4 :: BOOLEAN})
        }",
    );
    let spec = spec_string(&mut c);
    assert!(spec.contains(":`Dinghy` => :`Cutter`"), "{spec}");
    // Ferry re-specifies flag1 and adds flag4 only: the inherited
    // flag2/flag3 (from Tug) and flag1's nullable parent form are not
    // inlined. The parents render in the canonical sorted order.
    assert!(
        spec.contains(
            ":`Ferry` => :`Dinghy`&`Tug` {`flag1` :: STRING NOT NULL, `flag4` :: BOOLEAN}"
        ),
        "{spec}"
    );
    assert!(
        !spec.contains(":`Ferry` => :`Dinghy`&`Tug` {`flag1` :: STRING NOT NULL, `flag2`"),
        "the inherited flag2 is not inlined: {spec}"
    );
}

#[test]
fn show_as_graph_targets_the_parent_element_type_node() {
    // An implied label that identifies a node element type has its
    // IMPLIES relationship go to the NodeElementType node, not a
    // separate NodeLabel node.
    let mut c = gated_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:Tug => :Barge {flag1 :: STRING}), (:Dinghy => :Tug) }",
    );
    let value = scalar_one(
        &mut c,
        "SHOW CURRENT GRAPH TYPE AS GRAPH YIELD nodes RETURN nodes",
    );
    let Value::List(nodes) = value else {
        panic!("expected a node list");
    };
    let mut element_labels = Vec::new();
    for node in nodes.iter() {
        let Value::Node(entity) = node else {
            panic!("expected a node, got {node:?}")
        };
        if entity.labels.iter().any(|l| l == "NodeElementType") {
            let Some(Value::String(label)) = entity.properties.get("label") else {
                panic!("element-type node misses its label: {entity:?}");
            };
            element_labels.push(label.to_string());
        }
    }
    // Tug and Dinghy are element types; Barge (identifying no node
    // element type here) stays a NodeLabel node.
    element_labels.sort();
    assert_eq!(element_labels, vec!["Dinghy", "Tug"], "{nodes:?}");
}

/// The first column of every row, as a flat value list.
fn rows_of(c: &mut relate::Connection, query: &str) -> Vec<Value> {
    common::rows(c, query)
        .into_iter()
        .map(|row| row.into_iter().next().expect("one column"))
        .collect()
}

// ---------------------------------------------------------------------
// ADR-0388 review (LABS-386): the depth cap and persisted replay
// ---------------------------------------------------------------------

#[test]
fn deep_inheritance_chains_fail_typed_not_abort() {
    // A flat comma-list chain of element types never trips the parser's
    // nesting-depth guard, so without the fold's own cap a ~25k-link
    // body would overflow the 8 MiB stacks of the HTTP/MCP/CLI threads
    // and abort the process. The cap fails the statement typed.
    let mut query = String::from("ALTER CURRENT GRAPH TYPE SET { (:N0 => :N1)");
    for i in 1..5000 {
        query.push_str(&format!(", (:N{i} => :N{})", i + 1));
    }
    query.push_str(" }");
    let mut c = gated_connection();
    let err = err_text(c.query(&query));
    assert!(
        err.contains("maximum inheritance depth of 500 exceeded"),
        "expected the typed depth error, got: {err}"
    );
}
