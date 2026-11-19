//! The wire envelope carries the structured plan document (LABS-418,
//! ADR-0381): a `relate.plan/v1` object on EXPLAIN/PROFILE, null on a
//! plain run — the key the WASM surface and any C-ABI envelope reader sees.

use relate::{plan_wire_json, query_result_to_wire, Database};
use relate_test_support::deep_plan::{create_chain, long_create_chain, CLAUSES};

#[test]
fn the_wire_envelope_carries_the_plan_document() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    conn.execute("CREATE (:Person)").unwrap();

    let explained = conn.execute("EXPLAIN MATCH (p:Person) RETURN p").unwrap();
    let envelope = query_result_to_wire(&explained).unwrap();
    let plan = &envelope["plan"];
    assert_eq!(plan["schema"], "relate.plan/v1");
    assert_eq!(plan["profiled"], false);
    assert_eq!(plan["root"]["operator"], "ProduceResults");
    assert!(
        plan["root"]["actual"].is_null(),
        "EXPLAIN carries no metrics"
    );

    let profiled = conn.execute("PROFILE MATCH (p:Person) RETURN p").unwrap();
    assert_eq!(
        query_result_to_wire(&profiled).unwrap()["plan"]["profiled"],
        true
    );

    // A plain run carries null, never an absent key or an empty document.
    let plain = conn.execute("RETURN 1 AS n").unwrap();
    let envelope = query_result_to_wire(&plain).unwrap();
    assert!(
        envelope.as_object().unwrap().contains_key("plan"),
        "the key is always present"
    );
    assert!(envelope["plan"].is_null(), "a plain run carries no plan");
}

/// ADR-0628: the long CREATE chain's EXPLAIN plans, and its plan document
/// (`plan_wire_json`, the one projection the Python, FFI and MCP surfaces
/// read) walks without panicking — past the cap the deep nodes crossed as
/// `Null` and the invariant-bound wire accessor panicked. The document's
/// operator chain stays deep: every level crossed intact. The chain itself
/// is the shared `deep_plan` fixture (the formatter's presentation test
/// pins the same statement).
#[test]
fn a_long_create_chain_explain_walks_its_plan_document() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();

    let explained = conn
        .execute(&format!("EXPLAIN {}", long_create_chain()))
        .unwrap();
    let plan = plan_wire_json(&explained)
        .expect("the repro chain sits far under the plan-wire budget")
        .expect("EXPLAIN stamps a plan document");
    assert_eq!(plan["schema"], "relate.plan/v1");
    assert_eq!(plan["profiled"], false);
    assert_eq!(plan["root"]["operator"], "ProduceResults");

    // Descend the single-child chain to its leaf — the same walk every
    // consumer runs; a node that crossed as Null would have panicked inside
    // the projection above, and a lost level would stop the chain early.
    let mut node = &plan["root"];
    let mut operators = 1usize;
    while let Some(child) = node["children"].as_array().and_then(|c| c.first()) {
        node = child;
        operators += 1;
    }
    assert_eq!(
        node["operator"], "NodeByLabelScan",
        "the chain's leaf is the MATCH's label scan: {node:?}"
    );
    assert_eq!(
        operators, CLAUSES,
        "one operator per clause — the MATCH's scan, every CREATE, the RETURN's \
         projection: the full chain crossed intact ({operators})"
    );
}

/// The executing form of the same chain (no EXPLAIN) keeps working — the
/// fix touches the plan crossing only, never the executing path. An empty
/// graph feeds the MATCH no rows, so the CREATEs never fire and the
/// statement returns nothing.
#[test]
fn a_long_create_chain_executes_on_an_empty_graph() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();

    let result = conn.execute(&long_create_chain()).unwrap();
    assert!(
        result.rows.is_empty(),
        "an empty graph feeds the MATCH no rows: the chain returns nothing"
    );
    assert_eq!(
        result.stats.nodes_created, 0,
        "the CREATEs never fire without a matched row"
    );
}

/// ADR-0628's stack-safe budget (the security round's closure): a chain
/// past it — a few hundred KB of cheap chained clauses, legal text far
/// under ADR-0257's 4 MiB — plans and executes, and the plan document
/// projection refuses from the TYPED tree, before any build, with the
/// typed capacity error whose text states the scope: the statement itself
/// executed; only its plan document is refused.
#[test]
fn a_past_budget_chain_executes_and_refuses_only_its_plan_document() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    // A plan node costs its map plus the children list around the next —
    // ~2 container levels per clause, ~4 per chain node — so a node count
    // past a quarter of the budget (plus margin) crosses it.
    let nodes = relate::MAX_PLAN_WIRE_DEPTH as usize / 4 + 50;
    let statement = format!("PROFILE {}", create_chain(nodes));

    // PROFILE executes the statement (0 rows on the empty graph) and
    // stamps the plan: the engine itself never refuses it.
    let executed = conn.execute(&statement).unwrap();
    assert!(
        executed.plan_root().is_some(),
        "PROFILE stamps the plan: the engine handles the statement"
    );
    assert!(
        executed.rows.is_empty(),
        "the chain executes: the empty graph feeds the MATCH no rows"
    );

    // The refusal fires from the TYPED path, before any build: the probe
    // reads the tree's depth past the budget, and the budget check — the
    // first thing every plan-crossing boundary runs — refuses without
    // `to_value` ever building the engine `Value` tree.
    let root = executed.plan_root().expect("PROFILE stamps the plan");
    assert!(
        root.plan_wire_depth() > relate::MAX_PLAN_WIRE_DEPTH,
        "the probe reads the typed tree's depth past the budget: {}",
        root.plan_wire_depth()
    );
    match root.ensure_plan_wire_budget() {
        Err(relate::Error::Capacity {
            kind: relate::CapacityKind::PlanWire,
            limit,
            ..
        }) => {
            assert_eq!(
                limit,
                u64::from(relate::MAX_PLAN_WIRE_DEPTH),
                "the typed refusal names the budget"
            );
        }
        other => panic!("the typed budget check refuses a past-budget plan: {other:?}"),
    }

    // With the probe refusing before any build and the typed tree's own
    // drop an iterative drain (the security round's closure), the refusal
    // path is stack-flat on any thread — no big-stack runner needed: the
    // crossed value and document builds never run, and the result's deep
    // typed tree drops where it stands.
    let refusal = plan_wire_json(&executed)
        .expect_err("the past-budget plan document is refused, never truncated");
    match refusal {
        relate::Error::Capacity {
            kind: relate::CapacityKind::PlanWire,
            limit,
            ..
        } => {
            assert_eq!(
                limit,
                u64::from(relate::MAX_PLAN_WIRE_DEPTH),
                "the refusal names the budget"
            );
            let text = refusal.to_string();
            assert!(
                text.contains("only its plan document is refused"),
                "the text states the refusal's scope: {text}"
            );
        }
        other => panic!("the refusal is the typed plan-wire capacity class, got {other:?}"),
    }
}
