//! The boundary's plan-text rendering (ADR-0359): the engine emits the
//! plan tree only; every surface renders the text through this crate's
//! one layout — the text entry (`plan_text_of`) over the engine's own
//! wire form (`PlanNode::to_value` crossed once inside it), the `Query:`
//! echo header included. The CLI's table, Bolt's `string-representation`
//! and the HTTP plan text all carry these same bytes.
//!
//! Run with `cargo run -p relate-formatter --example plan_text`.

use relate_formatter::{plan_text_of, Formatter, Mode, PlanPresentation, StatementMeta};

fn main() -> relate::Result<()> {
    let db = relate::Database::open(":memory:")?;
    let mut conn = db.connect()?;
    conn.query("CREATE (a:Person {name: 'Alice'})-[:KNOWS]->(b:Person {name: 'Bob'})")?;

    // EXPLAIN stamps the structured tree on the result — no data columns,
    // no data rows (ADR-0359): the text below is this crate's rendering of
    // it, never an engine product.
    let result = conn.query(
        "EXPLAIN MATCH (p:Person)-[k:KNOWS]->(f:Person) WHERE f.name = 'Bob' RETURN p.name AS who",
    )?;
    assert!(result.header.columns.is_empty());
    assert!(result.rows.is_empty());
    let tree = result.plan_root().expect("EXPLAIN stamps the plan");

    // The boundary's text-only entry (ADR-0628): the engine's own wire
    // form crossed once, then the formatter's one layout over that
    // crossing — the same render the plan-wire composition runs for the
    // Neo4j-facing `string-representation`.
    let lines = plan_text_of(tree)?;
    assert_eq!(
        lines[0],
        "Query: MATCH (p:Person)-[k:KNOWS]->(f:Person) WHERE f.name = 'Bob' RETURN p.name AS who"
    );
    assert_eq!(lines[1], "Plan:");
    assert!(lines
        .iter()
        .any(|l| l.contains("Expand") && l.contains("(p:Person)-[k:KNOWS]->(f:Person)")));

    println!("{}", lines.join("\n"));

    // The composed table-mode presentation (ADR-0363 decision 8) reaches a
    // surface through the formatter's two-writer entry point: `format_with`
    // sends the data table to the first writer and the plan table plus its
    // one `Notifications:` block to the diagnostic writer, so the block
    // follows the table it annotates. `PlanView` is the one presentation
    // bundle; its prebuilt header means the renderer never re-derives it
    // from the echo.
    let result = conn.query("EXPLAIN MATCH (p:Persn) RETURN p")?;
    // The one-call composition (ADR-0359): the owned presentation converts
    // the typed tree's forms and stamps its EXPLAIN/PROFILE class once —
    // a surface never hand-assembles the view's fields.
    let presentation = result
        .plan_root()
        .map(|tree| PlanPresentation::of(tree, result.notifications()))
        .transpose()?
        .expect("EXPLAIN stamps the plan");
    let view = presentation.as_view();
    let formatter = Formatter::new(Mode::Table);
    let mut data = Vec::new();
    let mut diagnostics = Vec::new();
    formatter.format_with(
        &mut data,
        &mut diagnostics,
        &result.header.columns,
        &[],
        StatementMeta {
            plan: Some(view),
            ..Default::default()
        },
    )?;
    let presentation = String::from_utf8(diagnostics).expect("the formatter writes UTF-8");
    assert!(presentation.lines().any(|line| line == "Notifications:"));
    assert!(presentation
        .lines()
        .any(|line| line.contains("The label `Persn` does not exist in this database.")));

    println!("\n{presentation}");
    Ok(())
}
