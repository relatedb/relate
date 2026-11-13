//! Ledger loaded by the processing-memory cases.
//!
//! Eight organisation levels, each holding accounts, with transfers that
//! step down one level. That is deep enough for a variable-length walk and
//! wide enough in attributes for a sort to retain more than a bounded read.
//! One row or scratch entity is charged at 1024 bytes, so the stretch
//! queries are sized against that charge, not against payload width.

use relate_test_support::bolt::client::Session;

pub const ORG_DEPTH: i64 = 8;
pub const ACCOUNTS_PER_ORG: i64 = 12;
/// Rows a bounded read may retain. The full account sort and the deep walk
/// both produce more than this.
pub const BOUNDED_ROWS: i64 = 8;
/// Retained rows one query is allowed in the size-stretch cases.
pub const QUERY_ROWS: i64 = 64;
/// Entities one overlapping write is allowed. Two of those exceed the
/// shared processing budget when it is set to this same count.
pub const WRITE_BATCH: i64 = 40;

const REGIONS: [&str; 2] = ["north", "south"];
const TIERS: [&str; 4] = ["retail", "commercial", "treasury", "private"];
const CURRENCIES: [&str; 3] = ["USD", "EUR", "GBP"];

#[derive(Debug, Clone, Copy)]
pub struct Ledger {
    pub depth: i64,
    pub accounts_per_org: i64,
}

impl Ledger {
    /// Scale 1 is the default ledger. Width grows with the scale. Depth
    /// stays at 8 until scale 5, then gains one level every four steps.
    pub fn from_scale(scale: u64) -> Self {
        let scale = scale.max(1);
        Self {
            depth: ORG_DEPTH + ((scale - 1) / 4) as i64,
            accounts_per_org: ACCOUNTS_PER_ORG * scale as i64,
        }
    }

    pub fn account_count(self) -> i64 {
        self.depth * self.accounts_per_org
    }
}

/// `max_entities` is the most scratch entities one load statement may
/// create. `u64::MAX` loads each phase in one statement. An account and
/// its `:HOLDS` edge are two entities; a transfer is one.
pub fn load(
    session: &mut Session,
    ledger: Ledger,
    indexes: bool,
    max_entities: u64,
) -> Result<(), String> {
    for (start, end) in chunks(ledger.depth, 1, max_entities) {
        let rows = (start..end)
            .map(|org| {
                let region = REGIONS[(org as usize) % REGIONS.len()];
                let tier = TIERS[(org as usize) % TIERS.len()];
                format!(
                    "{{id: {org}, name: 'Org {org}', region: '{region}', tier: '{tier}', code: 'ORG-{org}', opened: {}}}",
                    20200101 + org
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        session.run(&format!(
            "UNWIND [{rows}] AS row CREATE (:Org {{id: row.id, name: row.name, region: row.region, tier: row.tier, code: row.code, opened: row.opened}})"
        ))?;
    }
    if indexes {
        session.run("CREATE RANGE INDEX FOR (o:Org) ON (o.id)")?;
    }
    for (start, end) in chunks(ledger.depth - 1, 1, max_entities) {
        let rows = (start..end)
            .map(|parent| format!("{{parent: {parent}, child: {}}}", parent + 1))
            .collect::<Vec<_>>()
            .join(", ");
        session.run(&format!(
            "UNWIND [{rows}] AS row MATCH (parent:Org {{id: row.parent}}) MATCH (child:Org {{id: row.child}}) CREATE (parent)-[:PARENT_OF]->(child)"
        ))?;
    }
    let accounts = ledger.account_count();
    for (start, end) in chunks(accounts, 2, max_entities) {
        let rows = (start..end)
            .map(|id| {
                let org = id / ledger.accounts_per_org;
                let region = REGIONS[(id as usize) % REGIONS.len()];
                let currency = CURRENCIES[(id as usize) % CURRENCIES.len()];
                format!(
                    "{{org: {org}, id: {id}, name: 'Account {id}', currency: '{currency}', balance: {}, status: 'open', opened: {}, region: '{region}', risk: {}, note: 'ledger account {id}'}}",
                    1_000 + id * 17,
                    20210101 + (id % 365),
                    id % 100
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        session.run(&format!(
            "UNWIND [{rows}] AS row MATCH (o:Org {{id: row.org}}) CREATE (o)-[:HOLDS]->(:Account {{id: row.id, name: row.name, currency: row.currency, balance: row.balance, status: row.status, opened: row.opened, region: row.region, risk: row.risk, note: row.note}})"
        ))?;
    }
    if indexes {
        session.run("CREATE RANGE INDEX FOR (a:Account) ON (a.id)")?;
    }
    let transfers = (ledger.depth - 1) * ledger.accounts_per_org * 2;
    for (start, end) in chunks(transfers, 1, max_entities) {
        let rows = (start..end)
            .map(|index| {
                let per_level = ledger.accounts_per_org * 2;
                let org = index / per_level;
                let within = index % per_level;
                let slot = within / 2;
                let hop = within % 2;
                let from = org * ledger.accounts_per_org + slot;
                let to_slot = (slot + hop) % ledger.accounts_per_org;
                let to = (org + 1) * ledger.accounts_per_org + to_slot;
                format!(
                    "{{from: {from}, to: {to}, amount: {}, ts: {}}}",
                    250 + from * 3 + hop,
                    1_700_000_000 + from
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        session.run(&format!(
            "UNWIND [{rows}] AS row MATCH (a:Account {{id: row.from}}) MATCH (b:Account {{id: row.to}}) CREATE (a)-[:TRANSFER {{amount: row.amount, ts: row.ts, kind: 'wire'}}]->(b)"
        ))?;
    }
    if indexes {
        for cypher in [
            "CREATE RANGE INDEX FOR (a:Account) ON (a.balance)",
            "CREATE TEXT INDEX FOR (a:Account) ON (a.name)",
        ] {
            session.run(cypher)?;
        }
    }
    Ok(())
}

/// One account by id. With the range index this is a seek; without it the
/// same predicate scans the label and still returns one row.
pub fn point_read() -> &'static str {
    "MATCH (a:Account) WHERE a.id = 7 RETURN a.id, a.name, a.currency, a.balance, a.status, a.region, a.risk, a.note"
}

/// A short page. Stays under the query budget. The indexed case serves the
/// range from the account id index.
pub fn page_read() -> String {
    format!(
        "MATCH (a:Account) WHERE a.id >= 0 AND a.id < {BOUNDED_ROWS} RETURN a.id, a.name, a.balance, a.region ORDER BY a.id"
    )
}

/// Mark one account and record a review. The create commits one node.
pub fn record_review() -> &'static str {
    "MATCH (a:Account) WHERE a.id = 7 SET a.status = 'review' CREATE (:Review {account: 7, kind: 'manual', note: 'threshold', region: a.region})"
}

/// Retains every account. This is the size boundary.
pub fn size_sort() -> &'static str {
    "MATCH (a:Account) RETURN a.id, a.name, a.balance, a.region, a.risk, a.note ORDER BY a.balance"
}

/// Walks the organisation spine and several transfer hops. This is the
/// cost boundary: the walk retains a path row for every match.
pub fn deep_paths() -> &'static str {
    "MATCH (o:Org {id: 0})-[:PARENT_OF*1..7]->(leaf:Org)-[:HOLDS]->(a:Account)-[:TRANSFER*1..4]->(b:Account) RETURN a.id, b.id"
}

/// Statement ranges of `items` things that cost `entities_each` scratch
/// entities, never asking one statement for more than `max_entities`.
fn chunks(items: i64, entities_each: i64, max_entities: u64) -> Vec<(i64, i64)> {
    let step = if max_entities == u64::MAX {
        items
    } else {
        ((max_entities as i64) / entities_each).max(1)
    };
    let mut out = Vec::new();
    let mut start = 0;
    while start < items {
        let end = (start + step).min(items);
        out.push((start, end));
        start = end;
    }
    out
}

/// A write that fits one query budget and fills the shared processing budget.
pub fn bulk_reviews() -> String {
    review_batch(WRITE_BATCH, "bulk")
}

/// One more review than the query budget, so the statement is refused.
pub fn over_budget_reviews() -> String {
    review_batch(QUERY_ROWS + 1, "bulk")
}

fn review_batch(count: i64, kind: &str) -> String {
    let last = count - 1;
    format!(
        "UNWIND range(0, {last}) AS i CREATE (:Review {{account: i, kind: '{kind}', note: 'batch review', region: 'north'}})"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ledger_is_deeper_and_wider_than_the_bounded_read() {
        let ledger = Ledger::from_scale(1);
        assert!(ledger.depth >= 8);
        assert!(ledger.account_count() > QUERY_ROWS);
        assert!(ledger.account_count() > BOUNDED_ROWS);
        const {
            assert!(WRITE_BATCH <= QUERY_ROWS);
        };
    }

    #[test]
    fn scale_grows_width_faster_than_depth() {
        let wide = Ledger::from_scale(5);
        assert_eq!(wide.accounts_per_org, ACCOUNTS_PER_ORG * 5);
        assert_eq!(wide.depth, ORG_DEPTH + 1);
    }

    #[test]
    fn stretch_queries_visit_the_whole_ledger() {
        assert!(size_sort().contains("ORDER BY"));
        assert!(deep_paths().contains("*1..7"));
        assert!(deep_paths().contains("*1..4"));
        assert!(point_read().contains("a.id = 7"));
        assert!(record_review().contains("CREATE (:Review"));
    }

    #[test]
    fn load_chunks_stay_inside_a_tight_entity_budget() {
        let accounts = chunks(Ledger::from_scale(1).account_count(), 2, 8);
        assert!(accounts.iter().all(|(start, end)| (end - start) * 2 <= 8));
        assert_eq!(
            accounts.iter().map(|(start, end)| end - start).sum::<i64>(),
            Ledger::from_scale(1).account_count()
        );
    }
}
