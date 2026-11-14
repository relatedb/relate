//! The tiny Rust MCP fixture (ADR-0442): the compiled child the CLI e2e
//! installs as a command package. It links the wire kit's
//! [`McpChild`] and performs the whole five-tool surface in order:
//! `initialize` and `notifications/initialized` (in `start`),
//! `update_schema`, `write_cypher` with rows, `read_cypher`, `batch_cypher`,
//! and `report_output`.
//!
//! It takes no argv and no env: the envelope (one JSON line on stdin,
//! written by the host before the handshake) is its whole input, and the
//! `count` flag it declares drives its rows — the end-to-end proof that
//! flag values reach an MCP child and that every tool round-trips. A
//! failed exchange exits non-zero with stderr, never a silent success.
//! A test fixture, never a product artifact (the `extensions`
//! required-feature keeps the minimal build from compiling it).

use serde_json::{json, Value};

use relate_wire::mcp::{self, McpChild};

/// The child's name in `clientInfo` — the mock manifest the e2e installs
/// declares its own mount name; the client info is informational.
const NAME: &str = "mcp-greet";
/// The version the child reports to the host.
const VERSION: &str = "0.1.0";

/// The fixture's request envelope: the manifest's `count` flag.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    count: i64,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let (mut child, facts, envelope) = McpChild::start::<Envelope>(
        std::io::BufReader::new(std::io::stdin()),
        std::io::stdout(),
        NAME,
        VERSION,
    )?;
    let count = envelope.count;
    // The handshake publishes the store's budget as one consistent triple;
    // the writing results below count it down.
    let opened = facts.capacity;
    if opened.store_used_bytes + opened.store_remaining_bytes != opened.store_limit_bytes {
        return Err(format!(
            "the initialize capacity is inconsistent: {opened:?}"
        ));
    }

    child.call(
        mcp::UPDATE_SCHEMA,
        json!({
            "statement": "CREATE CONSTRAINT greet_id_unique FOR (n:Greet) REQUIRE n.id IS UNIQUE",
        }),
    )?;

    let rows: Vec<Value> = (1..=count)
        .map(|id| json!({"id": id, "name": format!("greet-{id}")}))
        .collect();
    let written = child.call(
        mcp::WRITE_CYPHER,
        json!({
            "statement": "UNWIND $rows AS row CREATE (n:Greet {id: row.id, name: row.name})",
            "parameters": {"rows": rows},
        }),
    )?;
    let created = written["counts"]["nodes_created"]
        .as_i64()
        .ok_or_else(|| format!("the write result carries no node count: {written}"))?;
    if created != count {
        return Err(format!(
            "the host wrote {created} nodes, the fixture sent {count}"
        ));
    }
    let written_remaining = written["storeRemainingBytes"]
        .as_u64()
        .ok_or_else(|| format!("the write result carries no remaining bytes: {written}"))?;

    // The read's content is asserted against the rows the fixture wrote —
    // the columns the query named and every row's id and name, in order —
    // so a green run proves the write landed as sent, not merely that some
    // count matched.
    let read = child.call(
        mcp::READ_CYPHER,
        json!({"statement": "MATCH (n:Greet) RETURN n.id AS id, n.name AS name ORDER BY n.id"}),
    )?;
    let columns = read["columns"]
        .as_array()
        .ok_or_else(|| format!("the read result carries no columns: {read}"))?;
    if columns != &[json!("id"), json!("name")] {
        return Err(format!(
            "the read returned columns {columns:?}, the query named id and name"
        ));
    }
    let stored = read["rows"]
        .as_array()
        .ok_or_else(|| format!("the read result carries no rows: {read}"))?;
    if stored.len() != rows.len() {
        return Err(format!(
            "the host read {} rows, the fixture wrote {}",
            stored.len(),
            rows.len()
        ));
    }
    for (stored, sent) in stored.iter().zip(&rows) {
        if stored != sent {
            return Err(format!("the host read {stored}, the fixture wrote {sent}"));
        }
    }

    // The multi-statement tool through the real binary: two statements in
    // the call's one transaction, their counters returned in order.
    let batched = child.call(
        mcp::BATCH_CYPHER,
        json!({"statements": [
            {"statement": "CREATE (n:Batch {slot: 1})"},
            {"statement": "CREATE (n:Batch {slot: 2})"},
        ]}),
    )?;
    let results = batched["results"]
        .as_array()
        .ok_or_else(|| format!("the batch result carries no results: {batched}"))?;
    let created: Vec<i64> = results
        .iter()
        .map(|counter| {
            counter["nodes_created"]
                .as_i64()
                .ok_or_else(|| format!("a batch counter is no integer nodes_created: {counter}"))
        })
        .collect::<Result<_, _>>()?;
    if created != [1, 1] {
        return Err(format!(
            "the host reported {created:?} batch counters, the fixture sent two creates"
        ));
    }
    // The remaining budget only ever counts down: it starts at the
    // handshake's reading and the batch's result never exceeds the write's.
    // Under `whole-import` the store holds the writes only at commit, so
    // both results report the handshake's number there — the fixture
    // therefore asserts monotonicity, which holds under both policies.
    let batched_remaining = batched["storeRemainingBytes"]
        .as_u64()
        .ok_or_else(|| format!("the batch result carries no remaining bytes: {batched}"))?;
    if batched_remaining > written_remaining || written_remaining > opened.store_remaining_bytes {
        return Err(format!(
            "the store's remaining bytes grew across the fixture's writes: \
             {} -> {written_remaining} -> {batched_remaining}",
            opened.store_remaining_bytes
        ));
    }

    child.report_output(
        &format!("imported {count} nodes"),
        &["MATCH (n:Greet) RETURN n.name AS name ORDER BY name".to_string()],
    )?;
    Ok(())
}
