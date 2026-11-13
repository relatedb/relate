//! Identity dataset definition for the load-test harness: catalog, plan, and
//! exec arms for the mixed directory + IAM workload.

use std::collections::BTreeMap;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use relate::{Connection, Result};

use super::{
    choose_usecase, exec_statements, pick_one, Catalog, ExecOutcome, Knobs, LoadtestConfig,
    LoadtestDataset, OpKind, Param, PlannedSelection, PlannedStatement,
};
use crate::harness::scalar;
use crate::identity::{generate, IdentityConfig, IdentityGraph, GRANTABLE_ROLE_IDS, ROOT_USER_ID};
use crate::load::load_identity;

/// Identity dataset hook for [`super::run`].
#[derive(Debug, Clone, Copy)]
pub struct IdentityLoadtest;

const CATALOG: &[Catalog<Workload, Planned>] = &[
    Catalog {
        id: "provision_user",
        kind: OpKind::Write,
        related: false,
        description: "Provision: CREATE a User and MEMBER_OF an existing group",
        weight: 12,
        plan: plan_provision_user,
    },
    Catalog {
        id: "add_to_group",
        kind: OpKind::Write,
        related: false,
        description: "Directory: MERGE a user into a group",
        weight: 16,
        plan: plan_add_to_group,
    },
    Catalog {
        id: "grant_role",
        kind: OpKind::Write,
        related: false,
        description: "Grant: CREATE HAS_ROLE from a user to viewer/editor",
        weight: 10,
        plan: plan_grant_role,
    },
    Catalog {
        id: "attach_device",
        kind: OpKind::Write,
        related: false,
        description: "Device: CREATE Device and OWNS",
        weight: 8,
        plan: plan_attach_device,
    },
    Catalog {
        id: "nested_grant",
        kind: OpKind::Write,
        related: true,
        description: "IAM continuation: extra MEMBER_OF into the planted nest leaf",
        weight: 6,
        plan: plan_nested_grant,
    },
    Catalog {
        id: "effective_access",
        kind: OpKind::Read,
        related: false,
        description: "Effective access: direct HAS_ROLE UNION inherited MEMBER_OF* roles",
        weight: 16,
        plan: plan_effective_access,
    },
    Catalog {
        id: "org_walk",
        kind: OpKind::Read,
        related: false,
        description: "Org walk: REPORTS_TO* managers",
        weight: 10,
        plan: plan_org_walk,
    },
    Catalog {
        id: "optional_manager",
        kind: OpKind::Read,
        related: false,
        description: "OPTIONAL MATCH manager and device",
        weight: 10,
        plan: plan_optional_manager,
    },
    Catalog {
        id: "detect_nested_admin",
        kind: OpKind::Read,
        related: true,
        description: "IAM: admin via nested MEMBER_OF* from planted nested-admin",
        weight: 6,
        plan: plan_detect_nested_admin,
    },
    Catalog {
        id: "detect_sod",
        kind: OpKind::Read,
        related: true,
        description: "IAM: EXISTS requester and approver on planted sod-user",
        weight: 6,
        plan: plan_detect_sod,
    },
    Catalog {
        id: "stale_grants",
        kind: OpKind::Read,
        related: true,
        description: "IAM: HAS_ROLE.until before a cutoff on planted stale-grant",
        weight: 6,
        plan: plan_stale_grants,
    },
    Catalog {
        id: "search_directory",
        kind: OpKind::Read,
        related: false,
        description: "Directory search: User.email CONTAINS a user number",
        weight: 8,
        plan: plan_search_directory,
    },
    Catalog {
        id: "group_roster",
        kind: OpKind::Read,
        related: false,
        description: "Group members: ORDER BY name SKIP/LIMIT",
        weight: 8,
        plan: plan_group_roster,
    },
    Catalog {
        id: "access_review",
        kind: OpKind::Read,
        related: false,
        description: "Review: CASE on User.status",
        weight: 6,
        plan: plan_access_review,
    },
    Catalog {
        id: "reporting_path",
        kind: OpKind::Read,
        related: false,
        description: "shortestPath between two users over REPORTS_TO",
        weight: 6,
        plan: plan_reporting_path,
    },
    Catalog {
        id: "disable_user",
        kind: OpKind::Write,
        related: false,
        description: "Offboard: SET User.status",
        weight: 6,
        plan: plan_disable_user,
    },
    Catalog {
        id: "add_alias",
        kind: OpKind::Write,
        related: false,
        description: "Alias: SET emails list append",
        weight: 6,
        plan: plan_add_alias,
    },
    Catalog {
        id: "revoke_membership",
        kind: OpKind::Write,
        related: false,
        description: "Revoke: DELETE a MEMBER_OF edge",
        weight: 6,
        plan: plan_revoke_membership,
    },
    Catalog {
        id: "detach_device",
        kind: OpKind::Write,
        related: false,
        description: "Lost device: DELETE OWNS and Device",
        weight: 5,
        plan: plan_detach_device,
    },
    Catalog {
        id: "clear_expiry",
        kind: OpKind::Write,
        related: true,
        description: "Renew: REMOVE HAS_ROLE.until on stale-grant",
        weight: 4,
        plan: plan_clear_expiry,
    },
];

pub struct Workload {
    rng: StdRng,
    ordinary_users: Vec<String>,
    ordinary_groups: Vec<String>,
    fan_group: String,
    nested_admin: String,
    sod_user: String,
    stale_grant: String,
    next: u64,
    write_fraction: f64,
}

#[derive(Debug, Clone)]
pub enum Planned {
    Provision {
        uid: String,
        name: String,
        email: String,
        gid: String,
    },
    AddToGroup {
        uid: String,
        gid: String,
    },
    GrantRole {
        uid: String,
        rid: String,
    },
    AttachDevice {
        uid: String,
        did: String,
    },
    NestedGrant {
        uid: String,
        gid: String,
    },
    EffectiveAccess {
        id: String,
    },
    OrgWalk {
        id: String,
    },
    OptionalManager {
        id: String,
    },
    DetectNestedAdmin {
        id: String,
    },
    DetectSod {
        id: String,
    },
    StaleGrants {
        id: String,
    },
    SearchDirectory {
        q: String,
    },
    GroupRoster {
        gid: String,
        skip: i64,
    },
    AccessReview {
        id: String,
    },
    ReportingPath {
        from: String,
        to: String,
    },
    DisableUser {
        uid: String,
    },
    AddAlias {
        uid: String,
        alias: String,
    },
    RevokeMembership {
        uid: String,
        gid: String,
    },
    DetachDevice {
        uid: String,
    },
    ClearExpiry {
        uid: String,
    },
}

impl Planned {
    /// Post-op picker bookkeeping for `Provision`: the user id the workload
    /// records after a successful exec. `None` for every other variant.
    fn provision_update(&self) -> Option<String> {
        match self {
            Planned::Provision { uid, .. } => Some(uid.clone()),
            _ => None,
        }
    }
}

impl Workload {
    fn from_graph(graph: &IdentityGraph, seed: u64, write_fraction: f64) -> Self {
        let ordinary_users: Vec<String> = graph
            .users
            .iter()
            .filter(|u| u.planted.is_none())
            .map(|u| u.id.clone())
            .collect();
        let ordinary_groups: Vec<String> = graph
            .groups
            .iter()
            .filter(|g| g.planted.is_none())
            .map(|g| g.id.clone())
            .collect();
        Self {
            rng: StdRng::seed_from_u64(seed),
            ordinary_users,
            ordinary_groups,
            fan_group: graph.planted.fan_group.clone(),
            nested_admin: graph.planted.nested_admin.clone(),
            sod_user: graph.planted.sod_user.clone(),
            stale_grant: graph.planted.stale_grant.clone(),
            next: 1,
            write_fraction,
        }
    }

    /// Digit suffix of a seeded `user-N` id so CONTAINS hits that email, not every User.
    fn pick_search_q(&mut self) -> String {
        user_search_q(&self.pick_user()).unwrap_or_else(|| "0".into())
    }

    fn pick_user(&mut self) -> String {
        pick_one(&mut self.rng, &self.ordinary_users)
    }

    fn pick_group(&mut self) -> String {
        pick_one(&mut self.rng, &self.ordinary_groups)
    }

    fn pick_role(&mut self) -> String {
        pick_one(&mut self.rng, GRANTABLE_ROLE_IDS).into()
    }

    fn alloc(&mut self, prefix: &str) -> String {
        let id = format!("{prefix}-{}", self.next);
        self.next += 1;
        id
    }

    fn record_user(&mut self, uid: String) {
        self.ordinary_users.push(uid);
    }

    fn next_op(&mut self) -> PlannedSelection<Workload, Planned> {
        let uc = choose_usecase(&mut self.rng, CATALOG, self.write_fraction);
        let planned = (uc.plan)(self);
        (uc, planned)
    }
}

// One planner per catalog row. The catalog's `plan` pointer is the only
// dispatch: an id has no meaning outside its row.
fn plan_provision_user(work: &mut Workload) -> Planned {
    let uid = work.alloc("lt-user");
    Planned::Provision {
        email: format!("{uid}@example.com"),
        name: format!("Loadtest {uid}"),
        uid,
        gid: work.pick_group(),
    }
}

fn plan_add_to_group(work: &mut Workload) -> Planned {
    Planned::AddToGroup {
        uid: work.pick_user(),
        gid: work.pick_group(),
    }
}

fn plan_grant_role(work: &mut Workload) -> Planned {
    Planned::GrantRole {
        uid: work.pick_user(),
        rid: work.pick_role(),
    }
}

fn plan_attach_device(work: &mut Workload) -> Planned {
    Planned::AttachDevice {
        uid: work.pick_user(),
        did: work.alloc("lt-dev"),
    }
}

fn plan_nested_grant(work: &mut Workload) -> Planned {
    Planned::NestedGrant {
        uid: work.pick_user(),
        gid: work.fan_group.clone(),
    }
}

fn plan_effective_access(work: &mut Workload) -> Planned {
    Planned::EffectiveAccess {
        id: work.pick_user(),
    }
}

fn plan_org_walk(work: &mut Workload) -> Planned {
    Planned::OrgWalk {
        id: work.pick_user(),
    }
}

fn plan_optional_manager(work: &mut Workload) -> Planned {
    Planned::OptionalManager {
        id: work.pick_user(),
    }
}

fn plan_detect_nested_admin(work: &mut Workload) -> Planned {
    Planned::DetectNestedAdmin {
        id: work.nested_admin.clone(),
    }
}

fn plan_detect_sod(work: &mut Workload) -> Planned {
    Planned::DetectSod {
        id: work.sod_user.clone(),
    }
}

fn plan_stale_grants(work: &mut Workload) -> Planned {
    Planned::StaleGrants {
        id: work.stale_grant.clone(),
    }
}

fn plan_search_directory(work: &mut Workload) -> Planned {
    Planned::SearchDirectory {
        q: work.pick_search_q(),
    }
}

fn plan_group_roster(work: &mut Workload) -> Planned {
    Planned::GroupRoster {
        gid: work.pick_group(),
        skip: i64::from(work.rng.random_range(0u32..2) * 5),
    }
}

fn plan_access_review(work: &mut Workload) -> Planned {
    Planned::AccessReview {
        id: work.pick_user(),
    }
}

fn plan_reporting_path(work: &mut Workload) -> Planned {
    Planned::ReportingPath {
        from: work.pick_user(),
        to: ROOT_USER_ID.into(),
    }
}

fn plan_disable_user(work: &mut Workload) -> Planned {
    Planned::DisableUser {
        uid: work.pick_user(),
    }
}

fn plan_add_alias(work: &mut Workload) -> Planned {
    let uid = work.pick_user();
    let alias = work.alloc("alias");
    Planned::AddAlias {
        alias: format!("{alias}@example.com"),
        uid,
    }
}

fn plan_revoke_membership(work: &mut Workload) -> Planned {
    Planned::RevokeMembership {
        uid: work.pick_user(),
        gid: work.pick_group(),
    }
}

fn plan_detach_device(work: &mut Workload) -> Planned {
    Planned::DetachDevice {
        uid: work.pick_user(),
    }
}

fn plan_clear_expiry(work: &mut Workload) -> Planned {
    Planned::ClearExpiry {
        uid: work.stale_grant.clone(),
    }
}

/// One statement list per planned op: the single source of the identity
/// catalog's Cypher. Identity runs in-crate only, so unlike fraud's shared
/// dialect table this one feeds just the exec below.
fn planned_cypher(planned: &Planned) -> Vec<PlannedStatement> {
    match planned {
        Planned::Provision {
            uid,
            name,
            email,
            gid,
        } => vec![PlannedStatement::write(
            "CREATE (u:User {id: $id, name: $name, email: $email, status: 'active', hired: date(), emails: [$email]}) \
             WITH u MATCH (g:Group {id: $gid}) CREATE (u)-[:MEMBER_OF]->(g)",
        )
        .param("id", uid.as_str().into())
        .param("name", name.as_str().into())
        .param("email", email.as_str().into())
        .param("gid", gid.as_str().into())],
        Planned::AddToGroup { uid, gid } => vec![PlannedStatement::write(
            "MATCH (u:User {id: $uid}) MATCH (g:Group {id: $gid}) MERGE (u)-[:MEMBER_OF]->(g)",
        )
        .param("uid", uid.as_str().into())
        .param("gid", gid.as_str().into())],
        Planned::GrantRole { uid, rid } => vec![PlannedStatement::write(
            "MATCH (u:User {id: $uid}) MATCH (r:Role {id: $rid}) CREATE (u)-[:HAS_ROLE]->(r)",
        )
        .param("uid", uid.as_str().into())
        .param("rid", rid.as_str().into())],
        Planned::AttachDevice { uid, did } => vec![PlannedStatement::write(
            "MATCH (u:User {id: $uid}) CREATE (d:Device {id: $did}) CREATE (u)-[:OWNS]->(d)",
        )
        .param("uid", uid.as_str().into())
        .param("did", did.as_str().into())],
        Planned::NestedGrant { uid, gid } => vec![PlannedStatement::write(
            "MATCH (u:User {id: $uid}) MATCH (g:Group {id: $gid}) CREATE (u)-[:MEMBER_OF]->(g)",
        )
        .param("uid", uid.as_str().into())
        .param("gid", gid.as_str().into())],
        Planned::EffectiveAccess { id } => vec![PlannedStatement::read(
            "MATCH (u:User {id: $id})-[:HAS_ROLE]->(r:Role)-[:GRANTS]->(perm:Permission)-[:ON]->(a:App) \
             RETURN DISTINCT a.name AS app, perm.name AS perm \
             UNION \
             MATCH (u:User {id: $id})-[:MEMBER_OF*1..8]->(g:Group)-[:HAS_ROLE]->(r:Role)-[:GRANTS]->(perm:Permission)-[:ON]->(a:App) \
             RETURN DISTINCT a.name AS app, perm.name AS perm",
        )
        .param("id", id.as_str().into())],
        Planned::OrgWalk { id } => vec![PlannedStatement::read(
            "MATCH (u:User {id: $id})-[:REPORTS_TO*1..8]->(m:User) RETURN m.id AS id LIMIT 20",
        )
        .param("id", id.as_str().into())],
        Planned::OptionalManager { id } => vec![PlannedStatement::read(
            "MATCH (u:User {id: $id}) OPTIONAL MATCH (u)-[:REPORTS_TO]->(m:User) OPTIONAL MATCH (u)-[:OWNS]->(d:Device) \
             RETURN u.id AS id, m.id AS manager, d.id AS device",
        )
        .param("id", id.as_str().into())],
        Planned::DetectNestedAdmin { id } => vec![PlannedStatement::read(
            "MATCH (u:User {id: $id})-[:MEMBER_OF*1..8]->(g:Group)-[:HAS_ROLE]->(r:Role {name: 'admin'}) \
             RETURN DISTINCT u.id AS id",
        )
        .param("id", id.as_str().into())],
        Planned::DetectSod { id } => vec![PlannedStatement::read(
            "MATCH (u:User {id: $id}) \
             WHERE EXISTS { (u)-[:HAS_ROLE]->(:Role {name: 'requester'}) } \
               AND EXISTS { (u)-[:HAS_ROLE]->(:Role {name: 'approver'}) } \
             RETURN u.id AS id",
        )
        .param("id", id.as_str().into())],
        Planned::StaleGrants { id } => vec![PlannedStatement::read(
            "MATCH (u:User {id: $id})-[g:HAS_ROLE]->(r:Role) \
             WHERE g.until < date('2024-01-01') \
             RETURN u.id AS id, g.until AS until LIMIT 20",
        )
        .param("id", id.as_str().into())],
        Planned::SearchDirectory { q } => vec![PlannedStatement::read(
            "MATCH (u:User) WHERE u.email CONTAINS $q \
             RETURN u.id AS id ORDER BY u.id LIMIT 15",
        )
        .param("q", q.as_str().into())],
        Planned::GroupRoster { gid, skip } => vec![PlannedStatement::read(
            "MATCH (g:Group {id: $gid})<-[:MEMBER_OF]-(u:User) \
             RETURN u.id AS id, u.name AS name ORDER BY u.name SKIP $skip LIMIT 10",
        )
        .param("gid", gid.as_str().into())
        .param("skip", Param::Int(*skip))],
        Planned::AccessReview { id } => vec![PlannedStatement::read(
            "MATCH (u:User {id: $id}) \
             RETURN u.id AS id, \
               CASE u.status WHEN 'active' THEN 'ok' WHEN 'inactive' THEN 'blocked' ELSE 'unknown' END AS review",
        )
        .param("id", id.as_str().into())],
        Planned::ReportingPath { from, to } => vec![PlannedStatement::read(
            "MATCH (a:User {id: $from}), (b:User {id: $to}) \
             RETURN length(shortestPath((a)-[:REPORTS_TO*..8]-(b))) AS hops",
        )
        .param("from", from.as_str().into())
        .param("to", to.as_str().into())],
        Planned::DisableUser { uid } => vec![PlannedStatement::write(
            "MATCH (u:User {id: $uid}) SET u.status = CASE WHEN u.status = 'active' THEN 'inactive' ELSE 'active' END",
        )
        .param("uid", uid.as_str().into())],
        Planned::AddAlias { uid, alias } => vec![PlannedStatement::write(
            "MATCH (u:User {id: $uid}) SET u.emails = u.emails + [$alias]",
        )
        .param("uid", uid.as_str().into())
        .param("alias", alias.as_str().into())],
        Planned::RevokeMembership { uid, gid } => vec![PlannedStatement::write(
            "MATCH (u:User {id: $uid})-[m:MEMBER_OF]->(g:Group {id: $gid}) DELETE m",
        )
        .param("uid", uid.as_str().into())
        .param("gid", gid.as_str().into())],
        Planned::DetachDevice { uid } => vec![PlannedStatement::write(
            "MATCH (u:User {id: $uid})-[o:OWNS]->(d:Device) WITH o, d LIMIT 1 DELETE o, d",
        )
        .param("uid", uid.as_str().into())],
        Planned::ClearExpiry { uid } => vec![PlannedStatement::write(
            "MATCH (u:User {id: $uid})-[g:HAS_ROLE]->() WHERE g.until IS NOT NULL REMOVE g.until",
        )
        .param("uid", uid.as_str().into())],
    }
}

impl LoadtestDataset for IdentityLoadtest {
    type Graph = IdentityGraph;
    type Work = Workload;
    type Planned = Planned;
    type Update = Option<String>;

    fn kind(self) -> &'static str {
        "identity"
    }

    fn name(self) -> &'static str {
        "identity-mixed-iam"
    }

    fn unit(self) -> &'static str {
        "users"
    }

    fn catalog(self) -> &'static [Catalog<Workload, Planned>] {
        CATALOG
    }

    fn generate(self, cfg: &LoadtestConfig) -> Result<IdentityGraph> {
        let Knobs::Identity {
            nest_depth,
            group_k,
        } = cfg.knobs
        else {
            return Err(relate::Error::semantic(
                "identity-loadtest requires identity knobs (nest_depth, group_k)",
            ));
        };
        Ok(generate(&IdentityConfig {
            seed: cfg.graph_seed,
            scale: cfg.scale,
            nest_depth,
            group_k,
        }))
    }

    fn load(self, conn: &mut Connection, graph: &IdentityGraph) -> Result<()> {
        load_identity(conn, graph)
    }

    fn snapshot_scalars(self, conn: &mut Connection) -> Result<BTreeMap<String, i64>> {
        Ok(BTreeMap::from([
            (
                "users".into(),
                scalar(conn, "MATCH (u:User) RETURN count(u) AS n")?,
            ),
            (
                "groups".into(),
                scalar(conn, "MATCH (g:Group) RETURN count(g) AS n")?,
            ),
            (
                "roles".into(),
                scalar(conn, "MATCH (r:Role) RETURN count(r) AS n")?,
            ),
            (
                "memberships".into(),
                scalar(conn, "MATCH ()-[m:MEMBER_OF]->() RETURN count(m) AS n")?,
            ),
        ]))
    }

    fn workload(self, graph: &IdentityGraph, seed: u64, write_fraction: f64) -> Workload {
        Workload::from_graph(graph, seed, write_fraction)
    }

    fn next_op(self, work: &mut Workload) -> PlannedSelection<Workload, Planned> {
        work.next_op()
    }

    fn exec(self, conn: &mut Connection, planned: Planned) -> Result<ExecOutcome<Option<String>>> {
        let update = planned.provision_update();
        let (statements, rows) = exec_statements(conn, &planned_cypher(&planned))?;
        Ok(ExecOutcome {
            statements,
            rows,
            update,
        })
    }

    fn record(self, work: &mut Workload, update: Option<String>) {
        if let Some(uid) = update {
            work.record_user(uid);
        }
    }

    fn planted_value(self, graph: &IdentityGraph) -> serde_json::Value {
        serde_json::to_value(&graph.planted).expect(
            "planted patterns serialize infallibly: plain string, integer, and string-list fields",
        )
    }
}

/// `user-42` → `42`. Planted / onboard ids are not numeric names.
fn user_search_q(id: &str) -> Option<String> {
    let rest = id.strip_prefix("user-")?;
    if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
        Some(rest.to_string())
    } else {
        None
    }
}

/// Identity-graph planted-pattern CLI knobs (flattened into `identity-loadtest`).
#[derive(clap::Args, Debug, Clone, Copy)]
pub struct IdentityKnobsArgs {
    /// Planted MEMBER_OF hops from leaf group to admin (`nest-0` …)
    #[arg(long, default_value_t = 4)]
    pub nest_depth: usize,
    /// Users in the nested-admin leaf group (`nested-admin` plus `leaf-*`)
    #[arg(long, default_value_t = 12)]
    pub group_k: usize,
}

impl From<IdentityKnobsArgs> for Knobs {
    fn from(k: IdentityKnobsArgs) -> Knobs {
        Knobs::Identity {
            nest_depth: k.nest_depth,
            group_k: k.group_k,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_search_q_is_numeric_suffix() {
        assert_eq!(user_search_q("user-42").as_deref(), Some("42"));
        assert_eq!(user_search_q(ROOT_USER_ID).as_deref(), Some("0"));
        assert!(user_search_q("nested-admin").is_none());
        assert!(user_search_q("stale-grant").is_none());
        assert!(user_search_q("lt-user-3").is_none());
    }

    #[test]
    fn every_catalog_id_plans() {
        let graph = generate(&IdentityConfig {
            seed: 42,
            scale: 0.05,
            nest_depth: 4,
            group_k: 12,
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
    fn grantable_roles_are_generated_roles() {
        let graph = generate(&IdentityConfig::ci());
        let role_ids: Vec<&str> = graph.roles.iter().map(|r| r.id.as_str()).collect();
        for rid in GRANTABLE_ROLE_IDS {
            assert!(
                role_ids.contains(rid),
                "{rid} is grantable but not a generated role"
            );
        }
    }
}
