//! CIP-275 behind the `graph-type-show-variants` preview gate (ADR-0371):
//! `SHOW CURRENT GRAPH TYPE AS CONSOLIDATED | AS FLATTENED`, enabled side.
//! The closed side is pinned in `tests/graph_type.rs`, its own ungated
//! process (the gate is process-level: `Database::open` resolves
//! `RELATE_PREVIEW` once).
//!
//! Every example below is the CIP's own example graph type, and each
//! expectation was checked against the CIP's table, then recomputed by
//! running the fixture through this engine. The engine's existing
//! canonical rendering is the one place the CIP's hand-written tables
//! differ from, and the variants reuse it unchanged:
//!
//! - an identifying label is rendered with its `=>` wherever it is
//!   written (an edge endpoint, a constraint target);
//! - a property set is ordered by its rendered text, so `` `p1` `` sorts
//!   before `` `p` `` in the specification string (the graph view lists
//!   the properties by name);
//! - constraints sort identified labels first, then the rest, each by
//!   label, so a flattened type's inlined constraints precede the
//!   non-identifying labels' own;
//! - `DEFAULT ON COMMIT` is the engine's spelling of the CIP's `DEFAULT`.
//!
//! The one place the CIP's table departs from its own rule is E6's
//! flattened string, which omits the undesignated constraints that `A`
//! and `C` inherit from `B` and `D`; the rule (and E3/E4) inline them,
//! and so does this engine.
//!
//! Specifications are compared without backticks, without the generated
//! constraint names (the CIP tables skip them too) and on one line, the
//! shape the CIP prints.

mod common;

use std::collections::HashMap;

use relate::Value;

use common::show_variants::{printed_spec, show_spec};
use common::{err_text, rows, run, scalar_one};

fn graph_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.to_string(),
        other => panic!("expected a string, got {other:?}"),
    }
}

fn text_list(value: Option<&Value>) -> String {
    let Some(Value::List(items)) = value else {
        panic!("expected a list, got {value:?}");
    };
    items.iter().map(graph_text).collect::<Vec<_>>().join(" ; ")
}

/// The `AS GRAPH` view as two ordered line lists, one line per virtual
/// node / relationship, endpoints named by label (`*` for the any type).
fn show_graph(c: &mut relate::Connection, variant: &str) -> (Vec<String>, Vec<String>) {
    let result = rows(c, &format!("SHOW CURRENT GRAPH TYPE {variant} AS GRAPH"));
    let (Value::List(nodes), Value::List(rels)) = (&result[0][0], &result[0][1]) else {
        panic!("expected nodes and relationships lists");
    };
    let mut names = HashMap::new();
    let mut node_lines = Vec::new();
    for node in nodes.iter() {
        let Value::Node(entity) = node else {
            panic!("expected a node, got {node:?}")
        };
        let kind = &entity.labels[0];
        let label = match entity.properties.get("label") {
            Some(value) => graph_text(value),
            None => "*".to_string(),
        };
        names.insert(entity.id, label.clone());
        let line = if kind == "AnyLabel" {
            "AnyLabel".to_string()
        } else if kind == "NodeElementType" {
            format!(
                "NodeElementType {label} props=[{}] constraints=[{}]",
                text_list(entity.properties.get("properties")),
                text_list(entity.properties.get("constraints"))
            )
        } else {
            format!(
                "{kind} {label} constraints=[{}]",
                text_list(entity.properties.get("constraints"))
            )
        };
        node_lines.push(line);
    }
    let mut rel_lines = Vec::new();
    for rel in rels.iter() {
        let Value::Relationship(entity) = rel else {
            panic!("expected a relationship, got {rel:?}")
        };
        let (start, end) = (&names[&entity.start], &names[&entity.end]);
        let line = match entity.rel_type.as_str() {
            "IMPLIES" => format!("IMPLIES {start}->{end}"),
            kind => {
                let label = graph_text(&entity.properties["relationshipType"]);
                let constraints = text_list(entity.properties.get("constraints"));
                match entity.properties.get("properties") {
                    Some(props) => format!(
                        "{kind} {label} {start}->{end} props=[{}] constraints=[{constraints}]",
                        text_list(Some(props))
                    ),
                    None => format!("{kind} {label} {start}->{end} constraints=[{constraints}]"),
                }
            }
        };
        rel_lines.push(line);
    }
    (node_lines, rel_lines)
}

/// One variant's whole expectation: the specification string and both
/// `AS GRAPH` lists.
#[derive(Clone, Copy)]
struct Expect<'a> {
    spec: &'a str,
    nodes: &'a [&'a str],
    rels: &'a [&'a str],
}

fn assert_variant(c: &mut relate::Connection, variant: &str, expect: Expect<'_>) {
    assert_eq!(
        show_spec(c, variant),
        expect.spec,
        "{variant} specification"
    );
    let (nodes, rels) = show_graph(c, variant);
    assert_eq!(nodes, expect.nodes, "{variant} AS GRAPH nodes");
    assert_eq!(rels, expect.rels, "{variant} AS GRAPH relationships");
}

/// Plain, consolidated and flattened expectations of one example.
struct Example<'a> {
    set: &'a str,
    plain: Expect<'a>,
    consolidated: Expect<'a>,
    flattened: Expect<'a>,
}

fn check(example: Example<'_>) {
    let mut c = common::show_variants::show_variants_connection();
    run(&mut c, example.set);
    assert_variant(&mut c, "", example.plain);
    assert_variant(&mut c, "AS CONSOLIDATED", example.consolidated);
    assert_variant(&mut c, "AS FLATTENED", example.flattened);
    // Display only: the variants never change what plain SHOW returns.
    assert_variant(&mut c, "", example.plain);
}

// ---------------------------------------------------------------------
// E1 -- dependent and independent constraints on nodes
// ---------------------------------------------------------------------

/// The E1 fixture: dependent and independent constraints on nodes, shared by
/// E1 and the three-variants-in-one-query example E8.
const E1_SET: &str = "ALTER CURRENT GRAPH TYPE SET { (:A => :B {p1 :: STRING}), \
              (:C => :A&D {p2 :: FLOAT NOT NULL}), \
              CONSTRAINT d_p_int FOR (n:D) REQUIRE (n.p) IS :: INTEGER, \
              CONSTRAINT FOR (n:E) REQUIRE (n.p) IS NOT NULL }";

#[test]
fn e1_independent_node_constraints() {
    check(Example {
        set: E1_SET,
        plain: Expect {
            spec: "{  (:A => :B {p1 :: STRING}),  (:C => :A&D {p2 :: FLOAT NOT NULL}),  \
                   CONSTRAINT d_p_int FOR (n:D) REQUIRE (n.p) IS :: INTEGER,  \
                   CONSTRAINT FOR (n:E) REQUIRE (n.p) IS NOT NULL}",
            nodes: &[
                "NodeElementType A props=[p1 :: STRING] constraints=[]",
                "NodeElementType C props=[p2 :: FLOAT NOT NULL] constraints=[]",
                "NodeLabel B constraints=[]",
                "NodeLabel D constraints=[CONSTRAINT d_p_int FOR (n:D) REQUIRE n.p IS :: INTEGER]",
                "NodeLabel E constraints=[CONSTRAINT FOR (n:E) REQUIRE n.p IS NOT NULL]",
            ],
            rels: &["IMPLIES A->B", "IMPLIES C->A", "IMPLIES C->D"],
        },
        consolidated: Expect {
            spec: "{  (:A => :B {p1 :: STRING}),  (:C => :A&D {p2 :: FLOAT NOT NULL}),  \
                   (:D => {p :: INTEGER}),  (:E => {p :: ANY NOT NULL})}",
            nodes: &[
                "NodeElementType A props=[p1 :: STRING] constraints=[]",
                "NodeElementType C props=[p2 :: FLOAT NOT NULL] constraints=[]",
                "NodeElementType D props=[p :: INTEGER] constraints=[]",
                "NodeElementType E props=[p :: ANY NOT NULL] constraints=[]",
                "NodeLabel B constraints=[]",
            ],
            rels: &["IMPLIES A->B", "IMPLIES C->A", "IMPLIES C->D"],
        },
        flattened: Expect {
            spec: "{  (:A => :B {p1 :: STRING}),  \
                   (:C => :A&B&D {p1 :: STRING, p2 :: FLOAT NOT NULL, p :: INTEGER}),  \
                   CONSTRAINT d_p_int FOR (n:D) REQUIRE (n.p) IS :: INTEGER,  \
                   CONSTRAINT FOR (n:E) REQUIRE (n.p) IS NOT NULL}",
            nodes: &[
                "NodeElementType A props=[p1 :: STRING] constraints=[]",
                "NodeElementType C props=[p :: INTEGER ; p1 :: STRING ; p2 :: FLOAT NOT NULL] \
                 constraints=[]",
                "NodeLabel B constraints=[]",
                "NodeLabel D constraints=[CONSTRAINT d_p_int FOR (n:D) REQUIRE n.p IS :: INTEGER]",
                "NodeLabel E constraints=[CONSTRAINT FOR (n:E) REQUIRE n.p IS NOT NULL]",
            ],
            rels: &[
                "IMPLIES A->B",
                "IMPLIES C->A",
                "IMPLIES C->B",
                "IMPLIES C->D",
            ],
        },
    });
}

// ---------------------------------------------------------------------
// E2 -- dependent and independent constraints on relationships
// ---------------------------------------------------------------------

#[test]
fn e2_independent_relationship_constraints() {
    let plain_nodes: &[&str] = &[
        "AnyLabel",
        "AnyLabel",
        "AnyLabel",
        "NodeElementType A props=[p1 :: STRING] constraints=[]",
        "NodeLabel B constraints=[]",
        "NodeLabel C constraints=[]",
    ];
    let plain_constraints = "{  (:A => :B {p1 :: STRING}),  \
        (:C)-[:R => {p2 :: FLOAT NOT NULL}]->(:A =>),  \
        CONSTRAINT s_p_string FOR ()-[r:S]->() REQUIRE (r.p) IS :: STRING,  \
        CONSTRAINT FOR ()-[r:T]->() REQUIRE (r.p) IS NOT NULL,  \
        CONSTRAINT u_p_exists FOR ()-[r:U]->() REQUIRE (r.p) IS NOT NULL,  \
        CONSTRAINT FOR ()-[r:U]->() REQUIRE (r.p) IS :: BOOLEAN}";
    let plain_rels: &[&str] = &[
        "RELATIONSHIP_ELEMENT_TYPE R C->A props=[p2 :: FLOAT NOT NULL] constraints=[]",
        "RELATIONSHIP_TYPE S *->* constraints=[CONSTRAINT s_p_string FOR ()-[r:S]->() \
         REQUIRE r.p IS :: STRING]",
        "RELATIONSHIP_TYPE T *->* constraints=[CONSTRAINT FOR ()-[r:T]->() \
         REQUIRE r.p IS NOT NULL]",
        "RELATIONSHIP_TYPE U *->* constraints=[CONSTRAINT FOR ()-[r:U]->() \
         REQUIRE r.p IS :: BOOLEAN ; CONSTRAINT u_p_exists FOR ()-[r:U]->() \
         REQUIRE r.p IS NOT NULL]",
        "IMPLIES A->B",
    ];
    check(Example {
        set: "ALTER CURRENT GRAPH TYPE SET { (:A => :B {p1 :: STRING}), \
              (:C)-[:R => {p2 :: FLOAT NOT NULL}]->(:A), \
              CONSTRAINT s_p_string FOR ()-[r:S]->() REQUIRE (r.p) IS :: STRING, \
              CONSTRAINT FOR ()-[r:T]->() REQUIRE (r.p) IS NOT NULL, \
              CONSTRAINT FOR ()-[r:U]->() REQUIRE (r.p) IS :: BOOLEAN, \
              CONSTRAINT u_p_exists FOR ()-[r:U]->() REQUIRE (r.p) IS NOT NULL }",
        plain: Expect {
            spec: plain_constraints,
            nodes: plain_nodes,
            rels: plain_rels,
        },
        consolidated: Expect {
            spec: "{  (:A => :B {p1 :: STRING}),  \
                   (:C)-[:R => {p2 :: FLOAT NOT NULL}]->(:A =>),  \
                   ()-[:S => {p :: STRING}]->(),  ()-[:T => {p :: ANY NOT NULL}]->(),  \
                   ()-[:U => {p :: BOOLEAN NOT NULL}]->()}",
            nodes: plain_nodes,
            rels: &[
                "RELATIONSHIP_ELEMENT_TYPE R C->A props=[p2 :: FLOAT NOT NULL] constraints=[]",
                "RELATIONSHIP_ELEMENT_TYPE S *->* props=[p :: STRING] constraints=[]",
                "RELATIONSHIP_ELEMENT_TYPE T *->* props=[p :: ANY NOT NULL] constraints=[]",
                "RELATIONSHIP_ELEMENT_TYPE U *->* props=[p :: BOOLEAN NOT NULL] constraints=[]",
                "IMPLIES A->B",
            ],
        },
        // The CIP: "the AS FLATTENED variant is the same as the actual
        // graph type" -- it only touches node element types.
        flattened: Expect {
            spec: plain_constraints,
            nodes: plain_nodes,
            rels: plain_rels,
        },
    });
}

// ---------------------------------------------------------------------
// E3 -- undesignated constraints
// ---------------------------------------------------------------------

#[test]
fn e3_undesignated_constraints() {
    let plain_nodes: &[&str] = &[
        "AnyLabel",
        "NodeElementType A props=[] constraints=[CONSTRAINT a_p1_key FOR (n:A =>) \
         REQUIRE n.p1 IS KEY]",
        "NodeElementType C props=[] constraints=[]",
        "NodeLabel B constraints=[CONSTRAINT b_p2_unique FOR (n:B) REQUIRE n.p2 IS UNIQUE]",
        "NodeLabel D constraints=[CONSTRAINT d_p1_key FOR (n:D) REQUIRE n.p1 IS KEY ; \
         CONSTRAINT d_p2_p3_unique FOR (n:D) REQUIRE (n.p2, n.p3) IS UNIQUE]",
    ];
    let plain_spec = "{  (:A => :B),  (:C => :A&B),  (:B)-[:R =>]->(:A =>),  \
        CONSTRAINT a_p1_key FOR (n:A =>) REQUIRE (n.p1) IS KEY,  \
        CONSTRAINT b_p2_unique FOR (n:B) REQUIRE (n.p2) IS UNIQUE,  \
        CONSTRAINT d_p1_key FOR (n:D) REQUIRE (n.p1) IS KEY,  \
        CONSTRAINT d_p2_p3_unique FOR (n:D) REQUIRE (n.p2, n.p3) IS UNIQUE,  \
        CONSTRAINT FOR ()-[r:R =>]->() REQUIRE (r.p1, r.p2) IS KEY,  \
        CONSTRAINT FOR ()-[r:S]->() REQUIRE (r.p3) IS UNIQUE}";
    let plain_rels: &[&str] = &[
        "RELATIONSHIP_ELEMENT_TYPE R B->A props=[] constraints=[CONSTRAINT FOR ()-[r:R =>]->() \
         REQUIRE (r.p1, r.p2) IS KEY]",
        "RELATIONSHIP_TYPE S *->* constraints=[CONSTRAINT FOR ()-[r:S]->() REQUIRE r.p3 IS UNIQUE]",
        "IMPLIES A->B",
        "IMPLIES C->A",
        "IMPLIES C->B",
    ];
    check(Example {
        set: "ALTER CURRENT GRAPH TYPE SET { (:A => :B), (:C => :A&B), (:B)-[:R =>]->(:A), \
              CONSTRAINT a_p1_key FOR (n:A) REQUIRE (n.p1) IS KEY, \
              CONSTRAINT b_p2_unique FOR (n:B) REQUIRE (n.p2) IS UNIQUE, \
              CONSTRAINT d_p1_key FOR (n:D) REQUIRE (n.p1) IS KEY, \
              CONSTRAINT d_p2_p3_unique FOR (n:D) REQUIRE (n.p2, n.p3) IS UNIQUE, \
              CONSTRAINT FOR ()-[r: R]->() REQUIRE (r.p1, r.p2) IS KEY, \
              CONSTRAINT FOR ()-[r: S]->() REQUIRE (r.p3) IS UNIQUE }",
        plain: Expect {
            spec: plain_spec,
            nodes: plain_nodes,
            rels: plain_rels,
        },
        // The CIP: "the AS CONSOLIDATED variant is the same as the actual
        // graph type".
        consolidated: Expect {
            spec: plain_spec,
            nodes: plain_nodes,
            rels: plain_rels,
        },
        flattened: Expect {
            spec: "{  (:A => :B),  (:C => :A&B),  (:B)-[:R =>]->(:A =>),  \
                   CONSTRAINT a_p1_key FOR (n:A =>) REQUIRE (n.p1) IS KEY,  \
                   CONSTRAINT FOR (n:A =>) REQUIRE (n.p2) IS UNIQUE,  \
                   CONSTRAINT FOR (n:C =>) REQUIRE (n.p1) IS KEY,  \
                   CONSTRAINT FOR (n:C =>) REQUIRE (n.p2) IS UNIQUE,  \
                   CONSTRAINT b_p2_unique FOR (n:B) REQUIRE (n.p2) IS UNIQUE,  \
                   CONSTRAINT d_p1_key FOR (n:D) REQUIRE (n.p1) IS KEY,  \
                   CONSTRAINT d_p2_p3_unique FOR (n:D) REQUIRE (n.p2, n.p3) IS UNIQUE,  \
                   CONSTRAINT FOR ()-[r:R =>]->() REQUIRE (r.p1, r.p2) IS KEY,  \
                   CONSTRAINT FOR ()-[r:S]->() REQUIRE (r.p3) IS UNIQUE}",
            nodes: &[
                "AnyLabel",
                "NodeElementType A props=[] constraints=[CONSTRAINT a_p1_key FOR (n:A =>) \
                 REQUIRE n.p1 IS KEY ; CONSTRAINT FOR (n:A =>) REQUIRE n.p2 IS UNIQUE]",
                "NodeElementType C props=[] constraints=[CONSTRAINT FOR (n:C =>) \
                 REQUIRE n.p1 IS KEY ; CONSTRAINT FOR (n:C =>) REQUIRE n.p2 IS UNIQUE]",
                "NodeLabel B constraints=[CONSTRAINT b_p2_unique FOR (n:B) REQUIRE n.p2 IS UNIQUE]",
                "NodeLabel D constraints=[CONSTRAINT d_p1_key FOR (n:D) REQUIRE n.p1 IS KEY ; \
                 CONSTRAINT d_p2_p3_unique FOR (n:D) REQUIRE (n.p2, n.p3) IS UNIQUE]",
            ],
            // All three variants have the same relationships.
            rels: plain_rels,
        },
    });
}

// ---------------------------------------------------------------------
// E4 -- a mix of constraint classifications
// ---------------------------------------------------------------------

const E4_SET: &str = "ALTER CURRENT GRAPH TYPE SET { \
    (:A => :B {p1 :: INTEGER | FLOAT, p2 :: STRING NOT NULL}), \
    (:C => :A {p1 :: INTEGER}), (:D => :B), (:B)-[:R =>]->(:A), \
    (:D)-[:S => {p3 :: FLOAT}]->(), \
    CONSTRAINT a_p1_key FOR (n:A) REQUIRE n.p1 IS KEY, \
    CONSTRAINT b_p3_unique FOR (n:B) REQUIRE n.p3 IS UNIQUE, \
    CONSTRAINT b_p3_string FOR (n:B) REQUIRE n.p3 IS :: STRING, \
    CONSTRAINT FOR (n:E) REQUIRE n.p4 IS NOT NULL, \
    CONSTRAINT FOR (n:F) REQUIRE n.p5 IS KEY, \
    CONSTRAINT FOR ()-[r: R]->() REQUIRE (r.p1, r.p2) IS KEY, \
    CONSTRAINT FOR ()-[r: S]->() REQUIRE r.p3 IS UNIQUE, \
    CONSTRAINT FOR ()-[r: T]->() REQUIRE r.p4 IS :: STRING, \
    CONSTRAINT FOR ()-[r: T]->() REQUIRE r.p4 IS NOT NULL, \
    CONSTRAINT FOR ()-[r: U]->() REQUIRE r.p5 IS UNIQUE }";

const A_KEY: &str = "CONSTRAINT a_p1_key FOR (n:A =>) REQUIRE n.p1 IS KEY";
const R_KEY: &str = "CONSTRAINT FOR ()-[r:R =>]->() REQUIRE (r.p1, r.p2) IS KEY";
const S_UNIQUE: &str = "CONSTRAINT FOR ()-[r:S =>]->() REQUIRE r.p3 IS UNIQUE";
const U_UNIQUE: &str = "CONSTRAINT FOR ()-[r:U]->() REQUIRE r.p5 IS UNIQUE";

#[test]
fn e4_a_mix_of_constraint_classifications() {
    let any3: &[&str] = &["AnyLabel", "AnyLabel", "AnyLabel"];
    let join = |parts: &[&[&str]]| -> Vec<String> {
        parts
            .iter()
            .flat_map(|p| p.iter())
            .map(|s| s.to_string())
            .collect()
    };
    let mut c = common::show_variants::show_variants_connection();
    run(&mut c, E4_SET);

    // Plain.
    let plain_nodes = join(&[
        any3,
        &[
            &format!(
                "NodeElementType A props=[p1 :: INTEGER | FLOAT ; p2 :: STRING NOT NULL] \
                 constraints=[{A_KEY}]"
            ),
            "NodeElementType C props=[p1 :: INTEGER] constraints=[]",
            "NodeElementType D props=[] constraints=[]",
            "NodeLabel B constraints=[CONSTRAINT b_p3_unique FOR (n:B) REQUIRE n.p3 IS UNIQUE ; \
             CONSTRAINT b_p3_string FOR (n:B) REQUIRE n.p3 IS :: STRING]",
            "NodeLabel E constraints=[CONSTRAINT FOR (n:E) REQUIRE n.p4 IS NOT NULL]",
            "NodeLabel F constraints=[CONSTRAINT FOR (n:F) REQUIRE n.p5 IS KEY]",
        ],
    ]);
    let plain_nodes: Vec<&str> = plain_nodes.iter().map(String::as_str).collect();
    let t_rel = "RELATIONSHIP_TYPE T *->* constraints=[CONSTRAINT FOR ()-[r:T]->() \
                 REQUIRE r.p4 IS :: STRING ; CONSTRAINT FOR ()-[r:T]->() REQUIRE r.p4 IS NOT NULL]";
    let (nodes, rels) = show_graph(&mut c, "");
    assert_eq!(nodes, plain_nodes);
    assert_eq!(
        rels,
        [
            format!("RELATIONSHIP_ELEMENT_TYPE R B->A props=[] constraints=[{R_KEY}]"),
            format!(
                "RELATIONSHIP_ELEMENT_TYPE S D->* props=[p3 :: FLOAT] constraints=[{S_UNIQUE}]"
            ),
            t_rel.to_string(),
            format!("RELATIONSHIP_TYPE U *->* constraints=[{U_UNIQUE}]"),
            "IMPLIES A->B".to_string(),
            "IMPLIES C->A".to_string(),
            "IMPLIES D->B".to_string(),
        ]
    );

    // Consolidated: B and E become element types, T a relationship
    // element type; the key/unique constraints stay, and the label B they
    // sit on is now an identifying one.
    assert_eq!(
        show_spec(&mut c, "AS CONSOLIDATED"),
        "{  (:A => :B {p1 :: INTEGER | FLOAT, p2 :: STRING NOT NULL}),  \
         (:B => {p3 :: STRING}),  (:C => :A {p1 :: INTEGER}),  (:D => :B),  \
         (:E => {p4 :: ANY NOT NULL}),  (:B =>)-[:R =>]->(:A =>),  \
         (:D =>)-[:S => {p3 :: FLOAT}]->(),  ()-[:T => {p4 :: STRING NOT NULL}]->(),  \
         CONSTRAINT a_p1_key FOR (n:A =>) REQUIRE (n.p1) IS KEY,  \
         CONSTRAINT b_p3_unique FOR (n:B =>) REQUIRE (n.p3) IS UNIQUE,  \
         CONSTRAINT FOR (n:F) REQUIRE (n.p5) IS KEY,  \
         CONSTRAINT FOR ()-[r:R =>]->() REQUIRE (r.p1, r.p2) IS KEY,  \
         CONSTRAINT FOR ()-[r:S =>]->() REQUIRE (r.p3) IS UNIQUE,  \
         CONSTRAINT FOR ()-[r:U]->() REQUIRE (r.p5) IS UNIQUE}"
    );
    let (nodes, rels) = show_graph(&mut c, "AS CONSOLIDATED");
    assert_eq!(
        nodes,
        join(&[
            any3,
            &[
                &format!(
                    "NodeElementType A props=[p1 :: INTEGER | FLOAT ; p2 :: STRING NOT NULL] \
                     constraints=[{A_KEY}]"
                ),
                "NodeElementType B props=[p3 :: STRING] constraints=[CONSTRAINT b_p3_unique \
                 FOR (n:B =>) REQUIRE n.p3 IS UNIQUE]",
                "NodeElementType C props=[p1 :: INTEGER] constraints=[]",
                "NodeElementType D props=[] constraints=[]",
                "NodeElementType E props=[p4 :: ANY NOT NULL] constraints=[]",
                "NodeLabel F constraints=[CONSTRAINT FOR (n:F) REQUIRE n.p5 IS KEY]",
            ],
        ])
    );
    assert_eq!(
        rels,
        [
            format!("RELATIONSHIP_ELEMENT_TYPE R B->A props=[] constraints=[{R_KEY}]"),
            format!(
                "RELATIONSHIP_ELEMENT_TYPE S D->* props=[p3 :: FLOAT] constraints=[{S_UNIQUE}]"
            ),
            "RELATIONSHIP_ELEMENT_TYPE T *->* props=[p4 :: STRING NOT NULL] constraints=[]"
                .to_string(),
            format!("RELATIONSHIP_TYPE U *->* constraints=[{U_UNIQUE}]"),
            "IMPLIES A->B".to_string(),
            "IMPLIES C->A".to_string(),
            "IMPLIES D->B".to_string(),
        ]
    );

    // Flattened: A, C and D inline B's property and its undesignated
    // constraint, C also A's key; B's own constraints stay on B.
    assert_eq!(
        show_spec(&mut c, "AS FLATTENED"),
        "{  (:A => :B {p1 :: INTEGER | FLOAT, p2 :: STRING NOT NULL, p3 :: STRING}),  \
         (:C => :A&B {p1 :: INTEGER, p2 :: STRING NOT NULL, p3 :: STRING}),  \
         (:D => :B {p3 :: STRING}),  (:B)-[:R =>]->(:A =>),  \
         (:D =>)-[:S => {p3 :: FLOAT}]->(),  \
         CONSTRAINT a_p1_key FOR (n:A =>) REQUIRE (n.p1) IS KEY,  \
         CONSTRAINT FOR (n:A =>) REQUIRE (n.p3) IS UNIQUE,  \
         CONSTRAINT FOR (n:C =>) REQUIRE (n.p1) IS KEY,  \
         CONSTRAINT FOR (n:C =>) REQUIRE (n.p3) IS UNIQUE,  \
         CONSTRAINT FOR (n:D =>) REQUIRE (n.p3) IS UNIQUE,  \
         CONSTRAINT b_p3_unique FOR (n:B) REQUIRE (n.p3) IS UNIQUE,  \
         CONSTRAINT b_p3_string FOR (n:B) REQUIRE (n.p3) IS :: STRING,  \
         CONSTRAINT FOR (n:E) REQUIRE (n.p4) IS NOT NULL,  \
         CONSTRAINT FOR (n:F) REQUIRE (n.p5) IS KEY,  \
         CONSTRAINT FOR ()-[r:R =>]->() REQUIRE (r.p1, r.p2) IS KEY,  \
         CONSTRAINT FOR ()-[r:S =>]->() REQUIRE (r.p3) IS UNIQUE,  \
         CONSTRAINT FOR ()-[r:T]->() REQUIRE (r.p4) IS NOT NULL,  \
         CONSTRAINT FOR ()-[r:T]->() REQUIRE (r.p4) IS :: STRING,  \
         CONSTRAINT FOR ()-[r:U]->() REQUIRE (r.p5) IS UNIQUE}"
    );
    let (nodes, rels) = show_graph(&mut c, "AS FLATTENED");
    assert_eq!(
        nodes,
        join(&[
            any3,
            &[
                &format!(
                    "NodeElementType A props=[p1 :: INTEGER | FLOAT ; p2 :: STRING NOT NULL ; \
                     p3 :: STRING] constraints=[{A_KEY} ; CONSTRAINT FOR (n:A =>) \
                     REQUIRE n.p3 IS UNIQUE]"
                ),
                "NodeElementType C props=[p1 :: INTEGER ; p2 :: STRING NOT NULL ; p3 :: STRING] \
                 constraints=[CONSTRAINT FOR (n:C =>) REQUIRE n.p1 IS KEY ; \
                 CONSTRAINT FOR (n:C =>) REQUIRE n.p3 IS UNIQUE]",
                "NodeElementType D props=[p3 :: STRING] constraints=[CONSTRAINT FOR (n:D =>) \
                 REQUIRE n.p3 IS UNIQUE]",
                "NodeLabel B constraints=[CONSTRAINT b_p3_unique FOR (n:B) REQUIRE n.p3 IS UNIQUE \
                 ; CONSTRAINT b_p3_string FOR (n:B) REQUIRE n.p3 IS :: STRING]",
                "NodeLabel E constraints=[CONSTRAINT FOR (n:E) REQUIRE n.p4 IS NOT NULL]",
                "NodeLabel F constraints=[CONSTRAINT FOR (n:F) REQUIRE n.p5 IS KEY]",
            ],
        ])
    );
    assert_eq!(
        rels,
        [
            format!("RELATIONSHIP_ELEMENT_TYPE R B->A props=[] constraints=[{R_KEY}]"),
            format!(
                "RELATIONSHIP_ELEMENT_TYPE S D->* props=[p3 :: FLOAT] constraints=[{S_UNIQUE}]"
            ),
            t_rel.to_string(),
            format!("RELATIONSHIP_TYPE U *->* constraints=[{U_UNIQUE}]"),
            "IMPLIES A->B".to_string(),
            "IMPLIES C->A".to_string(),
            "IMPLIES C->B".to_string(),
            "IMPLIES D->B".to_string(),
        ]
    );
}

// ---------------------------------------------------------------------
// E5 -- property default values
// ---------------------------------------------------------------------

#[test]
fn e5_inherited_defaults_are_inlined() {
    let plain = "{  (:A => {p :: STRING DEFAULT ON COMMIT 'Hello :)'}),  \
                 (:B => :A {p :: STRING NOT NULL}),  \
                 (:C => :A {p :: STRING DEFAULT ON COMMIT 'Hi!'})}";
    let nodes: &[&str] = &[
        "NodeElementType A props=[p :: STRING DEFAULT ON COMMIT 'Hello :)'] constraints=[]",
        "NodeElementType B props=[p :: STRING NOT NULL] constraints=[]",
        "NodeElementType C props=[p :: STRING DEFAULT ON COMMIT 'Hi!'] constraints=[]",
    ];
    let rels: &[&str] = &["IMPLIES B->A", "IMPLIES C->A"];
    check(Example {
        set: "ALTER CURRENT GRAPH TYPE SET { \
              (:A => {p :: STRING DEFAULT ON COMMIT 'Hello :)'}), \
              (:B => :A {p :: STRING NOT NULL}), \
              (:C => :A {p :: STRING DEFAULT ON COMMIT 'Hi!'}) }",
        plain: Expect {
            spec: plain,
            nodes,
            rels,
        },
        consolidated: Expect {
            spec: plain,
            nodes,
            rels,
        },
        // B restates `p` without a default, so it shows the one it
        // inherits; C's own default wins.
        flattened: Expect {
            spec: "{  (:A => {p :: STRING DEFAULT ON COMMIT 'Hello :)'}),  \
                   (:B => :A {p :: STRING NOT NULL DEFAULT ON COMMIT 'Hello :)'}),  \
                   (:C => :A {p :: STRING DEFAULT ON COMMIT 'Hi!'})}",
            nodes: &[
                "NodeElementType A props=[p :: STRING DEFAULT ON COMMIT 'Hello :)'] \
                 constraints=[]",
                "NodeElementType B props=[p :: STRING NOT NULL DEFAULT ON COMMIT 'Hello :)'] \
                 constraints=[]",
                "NodeElementType C props=[p :: STRING DEFAULT ON COMMIT 'Hi!'] constraints=[]",
            ],
            rels,
        },
    });
}

// ---------------------------------------------------------------------
// E6 -- variants that cannot be SET
// ---------------------------------------------------------------------

#[test]
fn e6_variants_that_cannot_be_set() {
    let mut c = common::show_variants::show_variants_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:A => :B {p1 :: STRING}), \
         (:C => :A&D {p :: FLOAT NOT NULL}), (:E => :F), (:F => {p3 :: STRING}), \
         CONSTRAINT FOR (n:B) REQUIRE n.p2 IS KEY, \
         CONSTRAINT d_p_int FOR (n:D) REQUIRE n.p IS :: INTEGER, \
         CONSTRAINT FOR (n:D) REQUIRE n.p2 IS UNIQUE, \
         CONSTRAINT FOR (n:E) REQUIRE n.p3 IS KEY, \
         CONSTRAINT FOR (n:F) REQUIRE n.p3 IS UNIQUE }",
    );
    // D's independent `p :: INTEGER` becomes an element type next to C's
    // own `p :: FLOAT NOT NULL`: the invalid-to-SET combination the CIP
    // shows, displayed regardless.
    assert_eq!(
        show_spec(&mut c, "AS CONSOLIDATED"),
        "{  (:A => :B {p1 :: STRING}),  (:C => :A&D {p :: FLOAT NOT NULL}),  \
         (:D => {p :: INTEGER}),  (:E => :F),  (:F => {p3 :: STRING}),  \
         CONSTRAINT FOR (n:D =>) REQUIRE (n.p2) IS UNIQUE,  \
         CONSTRAINT FOR (n:E =>) REQUIRE (n.p3) IS KEY,  \
         CONSTRAINT FOR (n:F =>) REQUIRE (n.p3) IS UNIQUE,  \
         CONSTRAINT FOR (n:B) REQUIRE (n.p2) IS KEY}"
    );
    // C keeps its own `p` (a child's definition is never overridden by an
    // inherited one) and inherits B's key and D's unique constraint on
    // `p2`; E inherits F's unique constraint next to its own key on `p3`
    // -- the two conflicts the CIP calls out.
    assert_eq!(
        show_spec(&mut c, "AS FLATTENED"),
        "{  (:A => :B {p1 :: STRING}),  (:C => :A&B&D {p1 :: STRING, p :: FLOAT NOT NULL}),  \
         (:E => :F {p3 :: STRING}),  (:F => {p3 :: STRING}),  \
         CONSTRAINT FOR (n:A =>) REQUIRE (n.p2) IS KEY,  \
         CONSTRAINT FOR (n:C =>) REQUIRE (n.p2) IS KEY,  \
         CONSTRAINT FOR (n:C =>) REQUIRE (n.p2) IS UNIQUE,  \
         CONSTRAINT FOR (n:E =>) REQUIRE (n.p3) IS KEY,  \
         CONSTRAINT FOR (n:E =>) REQUIRE (n.p3) IS UNIQUE,  \
         CONSTRAINT FOR (n:F =>) REQUIRE (n.p3) IS UNIQUE,  \
         CONSTRAINT FOR (n:B) REQUIRE (n.p2) IS KEY,  \
         CONSTRAINT d_p_int FOR (n:D) REQUIRE (n.p) IS :: INTEGER,  \
         CONSTRAINT FOR (n:D) REQUIRE (n.p2) IS UNIQUE}"
    );
    let (nodes, rels) = show_graph(&mut c, "AS CONSOLIDATED");
    assert_eq!(
        nodes,
        [
            "NodeElementType A props=[p1 :: STRING] constraints=[]",
            "NodeElementType C props=[p :: FLOAT NOT NULL] constraints=[]",
            "NodeElementType D props=[p :: INTEGER] constraints=[CONSTRAINT FOR (n:D =>) \
             REQUIRE n.p2 IS UNIQUE]",
            "NodeElementType E props=[] constraints=[CONSTRAINT FOR (n:E =>) REQUIRE n.p3 IS KEY]",
            "NodeElementType F props=[p3 :: STRING] constraints=[CONSTRAINT FOR (n:F =>) \
             REQUIRE n.p3 IS UNIQUE]",
            "NodeLabel B constraints=[CONSTRAINT FOR (n:B) REQUIRE n.p2 IS KEY]",
        ]
    );
    assert_eq!(
        rels,
        [
            "IMPLIES A->B",
            "IMPLIES C->A",
            "IMPLIES C->D",
            "IMPLIES E->F"
        ]
    );
    let (nodes, _) = show_graph(&mut c, "AS FLATTENED");
    assert_eq!(
        nodes[1],
        "NodeElementType C props=[p :: FLOAT NOT NULL ; p1 :: STRING] \
         constraints=[CONSTRAINT FOR (n:C =>) REQUIRE n.p2 IS KEY ; \
         CONSTRAINT FOR (n:C =>) REQUIRE n.p2 IS UNIQUE]"
    );
}

// ---------------------------------------------------------------------
// E7 / E8 -- the migration workflow and the three variants side by side
// ---------------------------------------------------------------------

#[test]
fn e7_the_consolidated_type_can_be_set() {
    let mut c = common::show_variants::show_variants_connection();
    run(
        &mut c,
        "ALTER CURRENT GRAPH TYPE SET { (:A => :B { p1 :: STRING }), \
         CONSTRAINT FOR (n:B) REQUIRE n.p2 IS :: INTEGER, \
         CONSTRAINT FOR (n:C) REQUIRE n.p IS NOT NULL, \
         CONSTRAINT FOR ()-[r:R]->() REQUIRE r.p IS :: INTEGER }",
    );
    let consolidated = show_spec(&mut c, "AS CONSOLIDATED");
    assert_eq!(
        consolidated,
        "{  (:A => :B {p1 :: STRING}),  (:B => {p2 :: INTEGER}),  (:C => {p :: ANY NOT NULL}),  \
         ()-[:R => {p :: INTEGER}]->()}"
    );
    // The migration path: the shown specification is a graph type SET
    // accepts, and what it enforces is what the constraints enforced.
    let Value::String(shown) = scalar_one(&mut c, "SHOW CURRENT GRAPH TYPE AS CONSOLIDATED") else {
        panic!("expected a specification string");
    };
    run(&mut c, &format!("ALTER CURRENT GRAPH TYPE SET {shown}"));
    assert_eq!(show_spec(&mut c, ""), consolidated);
    let err = err_text(c.query("CREATE (:C)"));
    assert!(err.contains("requires properties"), "{err}");
    run(&mut c, "CREATE (:C {p: 1})");
    // Nothing is left to consolidate and no inheritance conflicts: the
    // two variants of the new state are the state.
    assert_eq!(show_spec(&mut c, "AS CONSOLIDATED"), consolidated);
}

#[test]
fn e8_the_three_variants_in_one_query() {
    let mut c = common::show_variants::show_variants_connection();
    run(&mut c, E1_SET);
    let row = common::row(
        &mut c,
        "SHOW CURRENT GRAPH TYPE YIELD specification AS actual \
         SHOW CURRENT GRAPH TYPE AS CONSOLIDATED YIELD specification AS consolidated \
         SHOW CURRENT GRAPH TYPE AS FLATTENED YIELD specification AS flattened \
         RETURN actual, consolidated, flattened",
    );
    let texts: Vec<String> = row.iter().map(|v| printed_spec(&graph_text(v))).collect();
    assert_eq!(texts.len(), 3);
    assert_eq!(texts[0], show_spec(&mut c, ""));
    assert_eq!(texts[1], show_spec(&mut c, "AS CONSOLIDATED"));
    assert_eq!(texts[2], show_spec(&mut c, "AS FLATTENED"));
    assert!(texts[1].contains("(:D => {p :: INTEGER}),  (:E => {p :: ANY NOT NULL})"));
}
