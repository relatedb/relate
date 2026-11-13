//! Load dataset graphs into a Relate connection via Cypher.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use relate::{Connection, Result, Temporal, Value};

use crate::fraud::FraudGraph;
use crate::identity::IdentityGraph;

const BATCH: usize = 200;

pub fn load_fraud(conn: &mut Connection, graph: &FraudGraph) -> Result<()> {
    let customers: Vec<Value> = graph
        .customers
        .iter()
        .map(|c| {
            map([
                ("id", Value::String(c.id.as_str().into())),
                ("name", Value::String(c.name.as_str().into())),
            ])
        })
        .collect();
    unwind_create(
        conn,
        &customers,
        "UNWIND $rows AS c CREATE (:Customer {id: c.id, name: c.name})",
    )?;

    let accounts: Vec<Value> = graph
        .accounts
        .iter()
        .map(|a| {
            let planted = a
                .planted
                .as_ref()
                .map(|s| Value::String(s.as_str().into()))
                .unwrap_or(Value::Null);
            map([
                ("id", Value::String(a.id.as_str().into())),
                ("planted", planted),
            ])
        })
        .collect();
    unwind_create(
        conn,
        &accounts,
        "UNWIND $rows AS a CREATE (:Account {id: a.id, planted: a.planted})",
    )?;

    // Equality seeks need a RANGE index and CONTAINS needs TEXT. Created
    // after the nodes exist so the following MATCH loads and the workload
    // both read the built indexes.
    for cypher in [
        "CREATE RANGE INDEX FOR (a:Account) ON (a.id)",
        "CREATE RANGE INDEX FOR (c:Customer) ON (c.id)",
        "CREATE RANGE INDEX FOR (a:Account) ON (a.planted)",
        "CREATE TEXT INDEX FOR (c:Customer) ON (c.name)",
    ] {
        conn.query(cypher)?;
    }

    let owned: Vec<Value> = graph
        .ownership
        .iter()
        .map(|(a, c)| {
            map([
                ("a", Value::String(a.as_str().into())),
                ("c", Value::String(c.as_str().into())),
            ])
        })
        .collect();
    unwind_create(
        conn,
        &owned,
        "UNWIND $rows AS r MATCH (a:Account {id: r.a}) MATCH (c:Customer {id: r.c}) CREATE (a)-[:OWNED_BY]->(c)",
    )?;

    let txs: Vec<Value> = graph
        .transfers
        .iter()
        .map(|t| {
            map([
                ("id", Value::String(t.id.as_str().into())),
                ("from", Value::String(t.from.as_str().into())),
                ("to", Value::String(t.to.as_str().into())),
                ("amount", Value::Float(t.amount)),
                ("ts", Value::Int(t.ts)),
            ])
        })
        .collect();
    unwind_create(
        conn,
        &txs,
        "UNWIND $rows AS t MATCH (a:Account {id: t.from}) MATCH (b:Account {id: t.to}) CREATE (a)-[:TRANSFER {id: t.id, amount: t.amount, ts: t.ts}]->(b)",
    )?;
    Ok(())
}

/// Load order: the five entity sets first, then the edges, which `MATCH`
/// their endpoints and so need every entity present.
pub fn load_identity(conn: &mut Connection, graph: &IdentityGraph) -> Result<()> {
    load_users(conn, graph)?;
    load_named(
        conn,
        graph.groups.iter().map(|g| (&g.id, &g.name)),
        "UNWIND $rows AS g CREATE (:Group {id: g.id, name: g.name})",
    )?;
    load_named(
        conn,
        graph.roles.iter().map(|r| (&r.id, &r.name)),
        "UNWIND $rows AS r CREATE (:Role {id: r.id, name: r.name})",
    )?;
    load_named(
        conn,
        graph.permissions.iter().map(|p| (&p.id, &p.name)),
        "UNWIND $rows AS p CREATE (:Permission {id: p.id, name: p.name})",
    )?;
    load_named(
        conn,
        graph.apps.iter().map(|a| (&a.id, &a.name)),
        "UNWIND $rows AS a CREATE (:App {id: a.id, name: a.name})",
    )?;
    load_devices(conn, graph)?;

    load_edges(
        conn,
        &graph.user_memberships,
        ["u", "g"],
        "UNWIND $rows AS r MATCH (u:User {id: r.u}) MATCH (g:Group {id: r.g}) CREATE (u)-[:MEMBER_OF]->(g)",
    )?;
    load_edges(
        conn,
        &graph.group_memberships,
        ["c", "p"],
        "UNWIND $rows AS r MATCH (c:Group {id: r.c}) MATCH (p:Group {id: r.p}) CREATE (c)-[:MEMBER_OF]->(p)",
    )?;
    load_user_roles(conn, graph)?;
    load_edges(
        conn,
        &graph.group_roles,
        ["g", "r"],
        "UNWIND $rows AS r MATCH (g:Group {id: r.g}) MATCH (role:Role {id: r.r}) CREATE (g)-[:HAS_ROLE]->(role)",
    )?;
    load_edges(
        conn,
        &graph.grants,
        ["r", "p"],
        "UNWIND $rows AS r MATCH (role:Role {id: r.r}) MATCH (p:Permission {id: r.p}) CREATE (role)-[:GRANTS]->(p)",
    )?;
    load_edges(
        conn,
        &graph.ons,
        ["p", "a"],
        "UNWIND $rows AS r MATCH (p:Permission {id: r.p}) MATCH (a:App {id: r.a}) CREATE (p)-[:ON]->(a)",
    )?;
    load_edges(
        conn,
        &graph.reports_to,
        ["u", "m"],
        "UNWIND $rows AS r MATCH (u:User {id: r.u}) MATCH (m:User {id: r.m}) CREATE (u)-[:REPORTS_TO]->(m)",
    )?;
    load_edges(
        conn,
        &graph.owns,
        ["u", "d"],
        "UNWIND $rows AS r MATCH (u:User {id: r.u}) MATCH (d:Device {id: r.d}) CREATE (u)-[:OWNS]->(d)",
    )
}

fn load_users(conn: &mut Connection, graph: &IdentityGraph) -> Result<()> {
    let users: Vec<Value> = graph
        .users
        .iter()
        .map(|u| {
            let planted = u
                .planted
                .as_ref()
                .map(|s| Value::String(s.as_str().into()))
                .unwrap_or(Value::Null);
            let emails = Value::List(
                u.emails
                    .iter()
                    .map(|e| Value::String(e.as_str().into()))
                    .collect::<Vec<_>>()
                    .into(),
            );
            map([
                ("id", Value::String(u.id.as_str().into())),
                ("name", Value::String(u.name.as_str().into())),
                ("email", Value::String(u.email.as_str().into())),
                ("status", Value::String(u.status.as_str().into())),
                ("hired", date_value(&u.hired)),
                ("emails", emails),
                ("planted", planted),
            ])
        })
        .collect();
    unwind_create(
        conn,
        &users,
        "UNWIND $rows AS u CREATE (:User {id: u.id, name: u.name, email: u.email, status: u.status, hired: u.hired, emails: u.emails, planted: u.planted})",
    )
}

/// One entity set of `(id, name)` rows.
fn load_named<'a>(
    conn: &mut Connection,
    rows: impl Iterator<Item = (&'a String, &'a String)>,
    cypher: &str,
) -> Result<()> {
    let rows: Vec<Value> = rows
        .map(|(id, name)| {
            map([
                ("id", Value::String(id.as_str().into())),
                ("name", Value::String(name.as_str().into())),
            ])
        })
        .collect();
    unwind_create(conn, &rows, cypher)
}

fn load_devices(conn: &mut Connection, graph: &IdentityGraph) -> Result<()> {
    let devices: Vec<Value> = graph
        .devices
        .iter()
        .map(|d| map([("id", Value::String(d.id.as_str().into()))]))
        .collect();
    unwind_create(
        conn,
        &devices,
        "UNWIND $rows AS d CREATE (:Device {id: d.id})",
    )
}

/// One edge set of `(from, to)` id pairs, bound to the two row keys `cols`
/// the statement reads.
fn load_edges(
    conn: &mut Connection,
    pairs: &[(String, String)],
    cols: [&str; 2],
    cypher: &str,
) -> Result<()> {
    let rows: Vec<Value> = pairs
        .iter()
        .map(|(from, to)| {
            map([
                (cols[0], Value::String(from.as_str().into())),
                (cols[1], Value::String(to.as_str().into())),
            ])
        })
        .collect();
    unwind_create(conn, &rows, cypher)
}

/// `HAS_ROLE` from users carries the optional `until` date.
fn load_user_roles(conn: &mut Connection, graph: &IdentityGraph) -> Result<()> {
    let ur: Vec<Value> = graph
        .user_roles
        .iter()
        .map(|g| {
            let until = g
                .until
                .as_ref()
                .map(|s| date_value(s))
                .unwrap_or(Value::Null);
            map([
                ("u", Value::String(g.user.as_str().into())),
                ("r", Value::String(g.role.as_str().into())),
                ("until", until),
            ])
        })
        .collect();
    unwind_create(
        conn,
        &ur,
        "UNWIND $rows AS r MATCH (u:User {id: r.u}) MATCH (role:Role {id: r.r}) CREATE (u)-[:HAS_ROLE {until: r.until}]->(role)",
    )
}

fn date_value(iso: &str) -> Value {
    let d = NaiveDate::parse_from_str(iso, "%Y-%m-%d")
        .unwrap_or_else(|_| NaiveDate::from_ymd_opt(2020, 1, 1).expect("fallback date"));
    Value::Temporal(Temporal::Date(d))
}

fn unwind_create(conn: &mut Connection, rows: &[Value], cypher: &str) -> Result<()> {
    for chunk in rows.chunks(BATCH) {
        let mut params = BTreeMap::new();
        params.insert("rows".into(), Value::List(chunk.to_vec().into()));
        conn.query_with_params(cypher, params)?;
    }
    Ok(())
}

fn map<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    let mut m = BTreeMap::new();
    for (k, v) in pairs {
        m.insert(k.to_string(), v);
    }
    Value::Map(m.into())
}
