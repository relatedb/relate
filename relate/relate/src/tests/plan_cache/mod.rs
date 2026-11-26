//! ADR-0503 end-to-end: the plan cache through the public API. The three
//! families are the ADR's own split — the correctness bar
//! ([`correctness`]), the admission gates ([`admission`]) and ADR-0687's
//! footprint, epoch and grade rules ([`footprint`]) — over the fixtures here.

mod admission;
mod correctness;
mod footprint;

use crate::{Connection, Database, Value};

fn users() -> (Database, Connection) {
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    c.query("CREATE (:User {id: 1, name: 'ada'}), (:User {id: 2, name: 'bob'}), (:User {id: 3, name: 'cy'})")
        .unwrap();
    (db, c)
}

fn rows(result: &crate::QueryResult) -> Vec<Vec<Value>> {
    result.rows.clone()
}

/// The minimum PARSE-phase duration over the fixed three samples both
/// timing assertions take — the order of magnitude, not one scheduling
/// slice. `min_parse` is the no-parameter spelling.
fn min_parse_with(
    c: &mut Connection,
    statement: &str,
    params: &std::collections::BTreeMap<String, Value>,
) -> u128 {
    (0..3)
        .map(|_| {
            c.query_with_params(statement, params.clone())
                .unwrap()
                .header
                .phases
                .parse()
                .as_nanos()
        })
        .min()
        .expect("three samples")
}

fn min_parse(c: &mut Connection, statement: &str) -> u128 {
    min_parse_with(c, statement, &std::collections::BTreeMap::new())
}
