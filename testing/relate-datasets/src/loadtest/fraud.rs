//! Fraud dataset definition for the load-test harness: catalog, plan, and
//! exec arms for the mixed payments + AML workload. Also owns
//! [`planned_cypher`], the dialect table shared with the Aura reference
//! runner.

use std::collections::BTreeMap;

use rand::rngs::StdRng;
use rand::seq::IteratorRandom;
use rand::{Rng, SeedableRng};
use relate::{Connection, Result};

use super::{
    choose_usecase, exec_statements, pick_one, Catalog, ExecOutcome, Knobs, LoadtestConfig,
    LoadtestDataset, OpKind, Param, PlannedSelection, PlannedStatement,
};
use crate::fraud::{generate, round_cents, FraudConfig, FraudGraph};
use crate::harness::scalar;
use crate::load::load_fraud;

/// Fraud dataset hook for [`super::run`].
#[derive(Debug, Clone, Copy)]
pub struct FraudLoadtest;

pub const CATALOG: &[Catalog<Workload, Planned>] = &[
    Catalog {
        id: "p2p_transfer",
        kind: OpKind::Write,
        related: false,
        description: "Ordinary customer payment: CREATE one TRANSFER between two accounts",
        weight: 28,
        plan: plan_p2p_transfer,
    },
    Catalog {
        id: "payroll_scatter",
        kind: OpKind::Write,
        related: false,
        description: "Payroll-like scatter: one source pays several distinct destinations",
        weight: 8,
        plan: plan_payroll_scatter,
    },
    Catalog {
        id: "onboard_customer",
        kind: OpKind::Write,
        related: false,
        description: "KYC onboard: new Customer, Account, and OWNED_BY",
        weight: 6,
        plan: plan_onboard_customer,
    },
    Catalog {
        id: "mule_scatter",
        kind: OpKind::Write,
        related: true,
        description: "Fraud continuation: extra scatter from planted fan-out mule",
        weight: 6,
        plan: plan_mule_scatter,
    },
    Catalog {
        id: "layering_cycle",
        kind: OpKind::Write,
        related: true,
        description: "Layering: directed cycle of TRANSFERs among 3 accounts",
        weight: 6,
        plan: plan_layering_cycle,
    },
    Catalog {
        id: "recent_transfers",
        kind: OpKind::Read,
        related: false,
        description: "Account activity: last transfers out, ORDER BY ts DESC",
        weight: 16,
        plan: plan_recent_transfers,
    },
    Catalog {
        id: "customer_portfolio",
        kind: OpKind::Read,
        related: false,
        description: "Customer view: accounts owned by a customer",
        weight: 10,
        plan: plan_customer_portfolio,
    },
    Catalog {
        id: "outgoing_volume",
        kind: OpKind::Read,
        related: false,
        description: "Statement: sum of outgoing TRANSFER amounts",
        weight: 8,
        plan: plan_outgoing_volume,
    },
    Catalog {
        id: "detect_fanout",
        kind: OpKind::Read,
        related: true,
        description: "AML: out-degree of a given account vs threshold",
        weight: 6,
        plan: plan_detect_fanout,
    },
    Catalog {
        id: "detect_cycle",
        kind: OpKind::Read,
        related: true,
        description: "AML: directed TRANSFER cycles of length 2–6",
        weight: 6,
        plan: plan_detect_cycle,
    },
    Catalog {
        id: "activity_page",
        kind: OpKind::Read,
        related: false,
        description: "Paginated activity: ORDER BY ts DESC SKIP/LIMIT",
        weight: 8,
        plan: plan_activity_page,
    },
    Catalog {
        id: "search_customer",
        kind: OpKind::Read,
        related: false,
        description: "Directory search: Customer.name CONTAINS a customer number",
        weight: 6,
        plan: plan_search_customer,
    },
    Catalog {
        id: "counterparties",
        kind: OpKind::Read,
        related: false,
        description: "Undirected TRANSFER neighbors of an account",
        weight: 8,
        plan: plan_counterparties,
    },
    Catalog {
        id: "payment_path",
        kind: OpKind::Read,
        related: false,
        description: "shortestPath along outgoing TRANSFER (directed, short)",
        weight: 6,
        plan: plan_payment_path,
    },
    Catalog {
        id: "amount_band",
        kind: OpKind::Read,
        related: false,
        description: "Statement: CASE amount band plus count",
        weight: 6,
        plan: plan_amount_band,
    },
    Catalog {
        id: "flag_account",
        kind: OpKind::Write,
        related: false,
        description: "Compliance hold: SET Account.frozen",
        weight: 5,
        plan: plan_flag_account,
    },
    Catalog {
        id: "reverse_last_out",
        kind: OpKind::Write,
        related: false,
        description: "Reversal: DELETE the latest outgoing TRANSFER",
        weight: 5,
        plan: plan_reverse_last_out,
    },
];

pub struct Workload {
    rng: StdRng,
    ordinary: Vec<String>,
    customers: Vec<String>,
    fanout_src: String,
    next: u64,
    fanout_k: usize,
    write_fraction: f64,
}

/// One `TRANSFER` to create: relationship id, endpoints, amount, and ts.
#[derive(Debug, Clone)]
pub struct TransferOp {
    pub from: String,
    pub to: String,
    pub id: String,
    pub amount: f64,
    pub ts: i64,
}

#[derive(Debug, Clone)]
pub enum Planned {
    Transfers {
        ops: Vec<TransferOp>,
    },
    Onboard {
        cid: String,
        aid: String,
        name: String,
    },
    Recent {
        id: String,
    },
    Portfolio {
        id: String,
    },
    Volume {
        id: String,
    },
    Fanout {
        id: String,
        k: usize,
    },
    Cycle,
    ActivityPage {
        id: String,
        skip: i64,
    },
    Search {
        q: String,
    },
    Counterparties {
        id: String,
    },
    PaymentPath {
        from: String,
        to: String,
    },
    AmountBand {
        id: String,
    },
    FlagAccount {
        id: String,
    },
    ReverseLastOut {
        id: String,
    },
}

impl Planned {
    /// Post-op picker bookkeeping for `Onboard`: the `(account, customer)`
    /// ids the workload records after a successful exec. `None` for every
    /// other variant.
    pub fn onboard_update(&self) -> Option<(String, String)> {
        match self {
            Planned::Onboard { aid, cid, .. } => Some((aid.clone(), cid.clone())),
            _ => None,
        }
    }
}

impl Workload {
    /// Build the workload over `graph`'s ordinary pool: exactly `generate`'s
    /// `n_accounts` (at least 20), which `record_onboard` only grows.
    pub fn from_graph(graph: &FraudGraph, seed: u64, write_fraction: f64) -> Self {
        let ordinary: Vec<String> = graph
            .accounts
            .iter()
            .filter(|a| a.planted.is_none())
            .map(|a| a.id.clone())
            .collect();
        Self {
            rng: StdRng::seed_from_u64(seed),
            ordinary,
            customers: graph.customers.iter().map(|c| c.id.clone()).collect(),
            fanout_src: graph.planted.fanout_source.clone(),
            next: 1,
            fanout_k: graph.planted.fanout_k,
            write_fraction,
        }
    }

    fn pick_account(&mut self) -> String {
        pick_one(&mut self.rng, &self.ordinary)
    }

    /// Two distinct ordinary accounts, sampled without replacement. The
    /// ordinary pool holds `generate`'s `n_accounts` (at least 20) and only
    /// grows, so a distinct pair always exists.
    fn pick_two_accounts(&mut self) -> (String, String) {
        let [from, to] = <[String; 2]>::try_from(self.pick_distinct(2))
            .expect("workload picker pool holds at least two ordinary accounts by construction");
        (from, to)
    }

    fn pick_customer(&mut self) -> String {
        pick_one(&mut self.rng, &self.customers)
    }

    /// Digit suffix of a seeded `cust-N` id so CONTAINS hits that name, not every Customer.
    fn pick_search_q(&mut self) -> String {
        for _ in 0..16 {
            if let Some(n) = customer_search_q(&self.pick_customer()) {
                return n;
            }
        }
        self.customers
            .iter()
            .find_map(|id| customer_search_q(id))
            .unwrap_or_else(|| "0".into())
    }

    /// Up to `n` distinct ordinary accounts matching `pred`, sampled without
    /// replacement. Non-empty whenever at least one ordinary account matches
    /// `pred`.
    fn pick_distinct_where(&mut self, n: usize, pred: impl Fn(&str) -> bool) -> Vec<String> {
        self.ordinary
            .iter()
            .filter(|a| pred(a))
            .cloned()
            .choose_multiple(&mut self.rng, n)
    }

    fn pick_distinct(&mut self, n: usize) -> Vec<String> {
        self.pick_distinct_where(n, |_| true)
    }

    /// Up to `n` distinct ordinary accounts, none equal to `exclude`. Always
    /// non-empty when the graph has an ordinary account besides `exclude`.
    fn pick_distinct_excluding(&mut self, n: usize, exclude: &str) -> Vec<String> {
        self.pick_distinct_where(n, |a| a != exclude)
    }

    fn alloc(&mut self, prefix: &str) -> String {
        let id = format!("{prefix}-{}", self.next);
        self.next += 1;
        id
    }

    pub fn record_onboard(&mut self, aid: String, cid: String) {
        self.ordinary.push(aid);
        self.customers.push(cid);
    }

    pub fn next_op(&mut self) -> PlannedSelection<Workload, Planned> {
        let uc = choose_usecase(&mut self.rng, CATALOG, self.write_fraction);
        let planned = (uc.plan)(self);
        (uc, planned)
    }
}

// One planner per catalog row. The catalog's `plan` pointer is the only
// dispatch: an id has no meaning outside its row.
fn plan_p2p_transfer(work: &mut Workload) -> Planned {
    let (from, to) = work.pick_two_accounts();
    let id = work.alloc("lt-tx");
    Planned::Transfers {
        ops: vec![TransferOp {
            from,
            to,
            id,
            amount: amount(work),
            ts: ts(work),
        }],
    }
}

/// One transfer per destination of a single-source scatter, each with its own
/// allocated id and timestamp.
fn scatter(
    work: &mut Workload,
    from: String,
    dests: Vec<String>,
    tag: &str,
    amount: f64,
) -> Planned {
    let ops = dests
        .into_iter()
        .map(|to| {
            let id = work.alloc(tag);
            TransferOp {
                from: from.clone(),
                to,
                id,
                amount,
                ts: ts(work),
            }
        })
        .collect();
    Planned::Transfers { ops }
}

fn plan_payroll_scatter(work: &mut Workload) -> Planned {
    let from = work.pick_account();
    let dests = work.pick_distinct_excluding(4, &from);
    scatter(work, from, dests, "lt-pay", 2500.0)
}

fn plan_onboard_customer(work: &mut Workload) -> Planned {
    let cid = work.alloc("lt-cust");
    let aid = work.alloc("lt-acc");
    Planned::Onboard {
        name: format!("Loadtest {cid}"),
        cid,
        aid,
    }
}

fn plan_mule_scatter(work: &mut Workload) -> Planned {
    let dests = work.pick_distinct(3);
    let from = work.fanout_src.clone();
    scatter(work, from, dests, "lt-mule", 99.0)
}

fn plan_layering_cycle(work: &mut Workload) -> Planned {
    let nodes = <[String; 3]>::try_from(work.pick_distinct(3))
        .expect("workload picker pool holds at least three ordinary accounts by construction");
    let mut ops = Vec::new();
    for i in 0..nodes.len() {
        let from = nodes[i].clone();
        let to = nodes[(i + 1) % nodes.len()].clone();
        let id = work.alloc("lt-cyc");
        ops.push(TransferOp {
            from,
            to,
            id,
            amount: 1000.0 + i as f64,
            ts: ts(work),
        });
    }
    Planned::Transfers { ops }
}

fn plan_recent_transfers(work: &mut Workload) -> Planned {
    Planned::Recent {
        id: work.pick_account(),
    }
}

fn plan_customer_portfolio(work: &mut Workload) -> Planned {
    Planned::Portfolio {
        id: work.pick_customer(),
    }
}

fn plan_outgoing_volume(work: &mut Workload) -> Planned {
    Planned::Volume {
        id: work.pick_account(),
    }
}

fn plan_detect_fanout(work: &mut Workload) -> Planned {
    Planned::Fanout {
        id: work.fanout_src.clone(),
        k: work.fanout_k,
    }
}

fn plan_detect_cycle(_work: &mut Workload) -> Planned {
    Planned::Cycle
}

fn plan_activity_page(work: &mut Workload) -> Planned {
    Planned::ActivityPage {
        id: work.pick_account(),
        skip: i64::from(work.rng.random_range(0u32..3) * 5),
    }
}

fn plan_search_customer(work: &mut Workload) -> Planned {
    Planned::Search {
        q: work.pick_search_q(),
    }
}

fn plan_counterparties(work: &mut Workload) -> Planned {
    Planned::Counterparties {
        id: work.pick_account(),
    }
}

fn plan_payment_path(work: &mut Workload) -> Planned {
    let (from, to) = work.pick_two_accounts();
    Planned::PaymentPath { from, to }
}

fn plan_amount_band(work: &mut Workload) -> Planned {
    Planned::AmountBand {
        id: work.pick_account(),
    }
}

fn plan_flag_account(work: &mut Workload) -> Planned {
    Planned::FlagAccount {
        id: work.pick_account(),
    }
}

fn plan_reverse_last_out(work: &mut Workload) -> Planned {
    Planned::ReverseLastOut {
        id: work.pick_account(),
    }
}

/// One statement list per planned op. This is the single source of the fraud
/// catalog's Cypher: the in-crate Relate exec and the Aura reference
/// runner execute what this table returns.
pub fn planned_cypher(planned: &Planned) -> Vec<PlannedStatement> {
    match planned {
        Planned::Transfers { ops } => ops
            .iter()
            .map(|op| {
                PlannedStatement::write(
                    "MATCH (a:Account {id: $from}) MATCH (b:Account {id: $to}) CREATE (a)-[:TRANSFER {id: $id, amount: $amount, ts: $ts}]->(b)",
                )
                .param("from", op.from.as_str().into())
                .param("to", op.to.as_str().into())
                .param("id", op.id.as_str().into())
                .param("amount", Param::Float(op.amount))
                .param("ts", Param::Int(op.ts))
            })
            .collect(),
        Planned::Onboard { cid, aid, name } => vec![PlannedStatement::write(
            "CREATE (c:Customer {id: $cid, name: $name}) CREATE (a:Account {id: $aid}) CREATE (a)-[:OWNED_BY]->(c)",
        )
        .param("cid", cid.as_str().into())
        .param("name", name.as_str().into())
        .param("aid", aid.as_str().into())],
        Planned::Recent { id } => vec![PlannedStatement::read(
            "MATCH (a:Account {id: $id})-[t:TRANSFER]->(b) RETURN t.id AS id, t.amount AS amount, b.id AS to ORDER BY t.ts DESC LIMIT 20",
        )
        .param("id", id.as_str().into())],
        Planned::Portfolio { id } => vec![PlannedStatement::read(
            "MATCH (c:Customer {id: $id})<-[:OWNED_BY]-(a:Account) RETURN a.id AS id",
        )
        .param("id", id.as_str().into())],
        Planned::Volume { id } => vec![PlannedStatement::read(
            "MATCH (a:Account {id: $id})-[t:TRANSFER]->() RETURN sum(t.amount) AS volume, count(t) AS n",
        )
        .param("id", id.as_str().into())],
        Planned::Fanout { id, k } => vec![PlannedStatement::read(
            "MATCH (a:Account {id: $id})-[:TRANSFER]->(b) WITH a, count(DISTINCT b) AS n WHERE n >= $k RETURN a.id AS id, n",
        )
        .param("id", id.as_str().into())
        .param("k", Param::Int(*k as i64))],
        Planned::Cycle => vec![PlannedStatement::read(
            "MATCH (a:Account {planted: 'cycle'})-[:TRANSFER*2..6]->(a) RETURN DISTINCT a.id AS id LIMIT 20",
        )],
        Planned::ActivityPage { id, skip } => vec![PlannedStatement::read(
            "MATCH (a:Account {id: $id})-[t:TRANSFER]->(b) \
             RETURN t.id AS id, t.amount AS amount, b.id AS to \
             ORDER BY t.ts DESC SKIP $skip LIMIT 10",
        )
        .param("skip", Param::Int(*skip))
        .param("id", id.as_str().into())],
        Planned::Search { q } => vec![PlannedStatement::read(
            "MATCH (c:Customer) WHERE c.name CONTAINS $q RETURN c.id AS id, c.name AS name ORDER BY c.name LIMIT 10",
        )
        .param("q", q.as_str().into())],
        Planned::Counterparties { id } => vec![PlannedStatement::read(
            "MATCH (a:Account {id: $id})-[:TRANSFER]-(b:Account) RETURN DISTINCT b.id AS id LIMIT 20",
        )
        .param("id", id.as_str().into())],
        Planned::PaymentPath { from, to } => vec![PlannedStatement::read(
            "MATCH (a:Account {id: $from}), (b:Account {id: $to}) \
             RETURN length(shortestPath((a)-[:TRANSFER*1..3]->(b))) AS hops",
        )
        .param("from", from.as_str().into())
        .param("to", to.as_str().into())],
        Planned::AmountBand { id } => vec![PlannedStatement::read(
            "MATCH (a:Account {id: $id})-[t:TRANSFER]->() \
             RETURN CASE WHEN t.amount >= 1000.0 THEN 'large' ELSE 'small' END AS band, count(*) AS n",
        )
        .param("id", id.as_str().into())],
        Planned::FlagAccount { id } => vec![PlannedStatement::write(
            "MATCH (a:Account {id: $id}) SET a.frozen = true",
        )
        .param("id", id.as_str().into())],
        Planned::ReverseLastOut { id } => vec![PlannedStatement::write(
            "MATCH (a:Account {id: $id})-[t:TRANSFER]->() WITH t ORDER BY t.ts DESC LIMIT 1 DELETE t",
        )
        .param("id", id.as_str().into())],
    }
}

impl LoadtestDataset for FraudLoadtest {
    type Graph = FraudGraph;
    type Work = Workload;
    type Planned = Planned;
    type Update = Option<(String, String)>;

    fn kind(self) -> &'static str {
        "fraud"
    }

    fn name(self) -> &'static str {
        "fraud-mixed-payments"
    }

    fn unit(self) -> &'static str {
        "accounts"
    }

    fn catalog(self) -> &'static [Catalog<Workload, Planned>] {
        CATALOG
    }

    fn generate(self, cfg: &LoadtestConfig) -> Result<FraudGraph> {
        let Knobs::Fraud {
            fanout_k,
            cycle_len,
        } = cfg.knobs
        else {
            return Err(relate::Error::semantic(
                "fraud-loadtest requires fraud knobs (fanout_k, cycle_len)",
            ));
        };
        Ok(generate(&FraudConfig {
            seed: cfg.graph_seed,
            scale: cfg.scale,
            fanout_k,
            cycle_len,
        }))
    }

    fn load(self, conn: &mut Connection, graph: &FraudGraph) -> Result<()> {
        load_fraud(conn, graph)
    }

    fn snapshot_scalars(self, conn: &mut Connection) -> Result<BTreeMap<String, i64>> {
        Ok(BTreeMap::from([
            (
                "accounts".into(),
                scalar(conn, "MATCH (a:Account) RETURN count(a) AS n")?,
            ),
            (
                "customers".into(),
                scalar(conn, "MATCH (c:Customer) RETURN count(c) AS n")?,
            ),
            (
                "transfers".into(),
                scalar(conn, "MATCH ()-[t:TRANSFER]->() RETURN count(t) AS n")?,
            ),
        ]))
    }

    fn workload(self, graph: &FraudGraph, seed: u64, write_fraction: f64) -> Workload {
        Workload::from_graph(graph, seed, write_fraction)
    }

    fn next_op(self, work: &mut Workload) -> PlannedSelection<Workload, Planned> {
        work.next_op()
    }

    fn exec(
        self,
        conn: &mut Connection,
        planned: Planned,
    ) -> Result<ExecOutcome<Option<(String, String)>>> {
        let update = planned.onboard_update();
        let (statements, rows) = exec_statements(conn, &planned_cypher(&planned))?;
        Ok(ExecOutcome {
            statements,
            rows,
            update,
        })
    }

    fn record(self, work: &mut Workload, update: Option<(String, String)>) {
        if let Some((aid, cid)) = update {
            work.record_onboard(aid, cid);
        }
    }

    fn planted_value(self, graph: &FraudGraph) -> serde_json::Value {
        serde_json::to_value(&graph.planted).expect(
            "planted patterns serialize infallibly: plain string, integer, and string-list fields",
        )
    }
}

/// `cust-42` → `42`. Planted / onboard ids are not numeric names.
fn customer_search_q(id: &str) -> Option<String> {
    let rest = id.strip_prefix("cust-")?;
    if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
        Some(rest.to_string())
    } else {
        None
    }
}

fn amount(work: &mut Workload) -> f64 {
    round_cents(work.rng.random_range(5.0f64..2_500.0))
}

fn ts(work: &mut Workload) -> i64 {
    1_730_000_000 + work.next as i64
}

/// Fraud-graph planted-pattern CLI knobs (flattened into `fraud-loadtest`).
#[derive(clap::Args, Debug, Clone, Copy)]
pub struct FraudKnobsArgs {
    /// Planted mule out-degree (`fanout-src` destinations)
    #[arg(long, default_value_t = 12)]
    pub fanout_k: usize,
    /// Planted directed cycle length (`cycle-0` …)
    #[arg(long, default_value_t = 4)]
    pub cycle_len: usize,
}

impl From<FraudKnobsArgs> for Knobs {
    fn from(k: FraudKnobsArgs) -> Knobs {
        Knobs::Fraud {
            fanout_k: k.fanout_k,
            cycle_len: k.cycle_len,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn customer_search_q_is_seeded_number() {
        assert_eq!(customer_search_q("cust-42").as_deref(), Some("42"));
        assert_eq!(customer_search_q("cust-0").as_deref(), Some("0"));
        assert!(customer_search_q("cust-planted-fanout").is_none());
        assert!(customer_search_q("lt-cust-3").is_none());
    }

    #[test]
    fn every_catalog_id_plans() {
        let graph = generate(&FraudConfig {
            seed: 42,
            scale: 0.05,
            fanout_k: 12,
            cycle_len: 4,
        });
        let mut work = Workload::from_graph(&graph, 7, 0.5);
        for c in CATALOG {
            let planned = (c.plan)(&mut work);
            assert!(
                !planned_cypher(&planned).is_empty(),
                "{} planned zero statements",
                c.id
            );
        }
    }

    #[test]
    fn payroll_scatter_never_plans_empty() {
        let graph = generate(&FraudConfig {
            seed: 42,
            scale: 0.05,
            fanout_k: 12,
            cycle_len: 4,
        });
        for seed in 0..64 {
            let mut work = Workload::from_graph(&graph, seed, 0.5);
            let planned = plan_payroll_scatter(&mut work);
            assert!(!planned_cypher(&planned).is_empty(), "seed {seed}");
        }
    }

    #[test]
    fn pick_two_accounts_is_distinct() {
        let graph = generate(&FraudConfig {
            seed: 42,
            scale: 0.05,
            fanout_k: 12,
            cycle_len: 4,
        });
        let mut work = Workload::from_graph(&graph, 7, 0.5);
        for _ in 0..64 {
            let (a, b) = work.pick_two_accounts();
            assert_ne!(a, b);
            assert!(work.ordinary.contains(&a));
            assert!(work.ordinary.contains(&b));
        }
    }

    #[test]
    fn amount_is_quoted_in_whole_cents() {
        let graph = generate(&FraudConfig {
            seed: 42,
            scale: 0.05,
            fanout_k: 12,
            cycle_len: 4,
        });
        let mut work = Workload::from_graph(&graph, 7, 0.5);
        for _ in 0..64 {
            let a = amount(&mut work);
            assert_eq!(a, round_cents(a));
        }
    }

    #[test]
    fn onboard_update_only_for_onboard() {
        let onboard = Planned::Onboard {
            cid: "lt-cust-1".into(),
            aid: "lt-acc-2".into(),
            name: "Loadtest lt-cust-1".into(),
        };
        assert_eq!(
            onboard.onboard_update(),
            Some(("lt-acc-2".into(), "lt-cust-1".into()))
        );
        assert_eq!(Planned::Cycle.onboard_update(), None);
        assert_eq!(
            Planned::Transfers {
                ops: vec![TransferOp {
                    from: "from".into(),
                    to: "to".into(),
                    id: "tx-1".into(),
                    amount: 1.0,
                    ts: 0,
                }],
            }
            .onboard_update(),
            None
        );
    }
}
