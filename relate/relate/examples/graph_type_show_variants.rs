//! `SHOW CURRENT GRAPH TYPE AS CONSOLIDATED | AS FLATTENED` (CIP-275,
//! ADR-0371 preview gate `graph-type-show-variants`): two display-only
//! views of the stored graph type. `AS CONSOLIDATED` shows the independent
//! constraints as the element types they would be; `AS FLATTENED` inlines
//! every inherited definition into each node element type. Neither alters
//! the graph type.
//!
//! Run with `cargo run -p relate --example graph_type_show_variants`.

use relate::{Database, Value};

/// The specification column of one `SHOW CURRENT GRAPH TYPE` statement.
fn specification(conn: &mut relate::Connection, statement: &str) -> relate::Result<String> {
    let result = conn.query(statement)?;
    let Value::String(spec) = &result.rows[0][0] else {
        panic!("expected a specification string");
    };
    Ok(spec.to_string())
}

fn main() -> relate::Result<()> {
    // The variants and the implied-label inheritance they inline are
    // preview features (ADR-0371). This in-process mutation is
    // example-only: a library must not set the process environment, the
    // process owner sets RELATE_PREVIEW before open. The variable is read
    // once, at open.
    std::env::set_var(
        "RELATE_PREVIEW",
        "graph-type-implied-labels,graph-type-show-variants",
    );
    let db = Database::open(":memory:")?;
    let mut conn = db.connect()?;

    // A graph type with an inheritance edge (C implies A and D, A implies
    // B) and two independent constraints (on D and E, which identify no
    // element type).
    conn.query(
        "ALTER CURRENT GRAPH TYPE SET { \
         (:A => :B {p1 :: STRING}), \
         (:C => :A&D {p2 :: FLOAT NOT NULL}), \
         CONSTRAINT d_p_int FOR (n:D) REQUIRE n.p IS :: INTEGER, \
         CONSTRAINT FOR (n:E) REQUIRE n.p IS NOT NULL }",
    )?;
    let plain = specification(&mut conn, "SHOW CURRENT GRAPH TYPE")?;

    // AS CONSOLIDATED: D and E become element types carrying the rules.
    let consolidated = specification(&mut conn, "SHOW CURRENT GRAPH TYPE AS CONSOLIDATED")?;
    assert!(consolidated.contains("(:`D` => {`p` :: INTEGER})"));
    assert!(consolidated.contains("(:`E` => {`p` :: ANY NOT NULL})"));
    assert!(!consolidated.contains("CONSTRAINT"));

    // AS FLATTENED: C lists every label it implies and every property it
    // inherits, D's independent type rule included.
    let flattened = specification(&mut conn, "SHOW CURRENT GRAPH TYPE AS FLATTENED")?;
    assert!(flattened.contains(":`A`&`B`&`D`"));
    assert!(flattened.contains("`p` :: INTEGER"));

    // Either variant can be viewed as a graph, and takes YIELD / WHERE.
    let result = conn.query(
        "SHOW CURRENT GRAPH TYPE AS CONSOLIDATED AS GRAPH YIELD nodes RETURN size(nodes) AS n",
    )?;
    assert_eq!(result.rows[0][0], Value::Int(5));

    // Display only: the stored graph type is what it was.
    assert_eq!(specification(&mut conn, "SHOW CURRENT GRAPH TYPE")?, plain);

    println!("{consolidated}\n{flattened}");
    Ok(())
}
