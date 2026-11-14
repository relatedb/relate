"use strict";

const {
  mulberry32,
  intRange,
  unwind,
  readOp,
  writeOp,
  scalarInt,
  runMixedLoadtest,
} = require("./common");
const STALE_UNTIL = "2020-06-01";

// The org root user, named once (mirrors Rust's ROOT_USER_ID): user 0 has
// no manager; every planted user reports to it, and reporting_path walks to it.
const ROOT_USER_ID = "user-0";

// The roles a random grant may hand out — stated once, consulted by the
// group-role assignment and the workload's grant pick (no re-split literal).
const GRANTABLE_ROLES = ["role-viewer", "role-editor"];

// The fused identity catalog: one row per operation, stating its metadata
// and its planner together. The planner draws its picks from the workload
// (in the same order the former plan switch drew them) and returns an op
// from the shared readOp/writeOp constructors (the op-exec policy lives in
// common.js) — so the operation vocabulary is stated once per operation
// (mirrors the Go client's identityOpSpec and the Rust catalog's per-row
// plan fn; LABS-429, consolidated LABS-487).
const CATALOG = [
  {
    id: "provision_user", kind: "write", related: false, description: "Provision: CREATE a User and MEMBER_OF an existing group", weight: 12,
    plan(work) {
      const uid = work.alloc("lt-user");
      const gid = work.pickGroup();
      return writeOp(
        "CREATE (u:User {id: $id, name: $name, email: $email, status: 'active', hired: date(), emails: [$email]}) " +
          "WITH u MATCH (g:Group {id: $gid}) CREATE (u)-[:MEMBER_OF]->(g)",
        { id: uid, name: `Loadtest ${uid}`, email: `${uid}@example.com`, gid },
        { provisioned: uid }
      );
    },
  },
  {
    id: "add_to_group", kind: "write", related: false, description: "Directory: MERGE a user into a group", weight: 16,
    plan(work) {
      const uid = work.pickUser();
      const gid = work.pickGroup();
      return writeOp("MATCH (u:User {id: $uid}) MATCH (g:Group {id: $gid}) MERGE (u)-[:MEMBER_OF]->(g)", { uid, gid });
    },
  },
  {
    id: "grant_role", kind: "write", related: false, description: "Grant: CREATE HAS_ROLE from a user to viewer/editor", weight: 10,
    plan(work) {
      const uid = work.pickUser();
      const rid = work.pickRole();
      return writeOp("MATCH (u:User {id: $uid}) MATCH (r:Role {id: $rid}) CREATE (u)-[:HAS_ROLE]->(r)", { uid, rid });
    },
  },
  {
    id: "attach_device", kind: "write", related: false, description: "Device: CREATE Device and OWNS", weight: 8,
    plan(work) {
      const uid = work.pickUser();
      const did = work.alloc("lt-dev");
      return writeOp("MATCH (u:User {id: $uid}) CREATE (d:Device {id: $did}) CREATE (u)-[:OWNS]->(d)", { uid, did });
    },
  },
  {
    id: "nested_grant", kind: "write", related: true, description: "IAM continuation: extra MEMBER_OF into the planted nest leaf", weight: 6,
    plan(work) {
      const uid = work.pickUser();
      const gid = work.fanGroup;
      return writeOp("MATCH (u:User {id: $uid}) MATCH (g:Group {id: $gid}) CREATE (u)-[:MEMBER_OF]->(g)", { uid, gid });
    },
  },
  {
    id: "effective_access", kind: "read", related: false, description: "Effective access: direct HAS_ROLE UNION inherited MEMBER_OF* roles", weight: 16,
    plan(work) {
      const id = work.pickUser();
      return readOp(
        "MATCH (u:User {id: $id})-[:HAS_ROLE]->(r:Role)-[:GRANTS]->(perm:Permission)-[:ON]->(a:App) " +
          "RETURN DISTINCT a.name AS app, perm.name AS perm " +
          "UNION " +
          "MATCH (u:User {id: $id})-[:MEMBER_OF*1..8]->(g:Group)-[:HAS_ROLE]->(r:Role)-[:GRANTS]->(perm:Permission)-[:ON]->(a:App) " +
          "RETURN DISTINCT a.name AS app, perm.name AS perm",
        { id }
      );
    },
  },
  {
    id: "org_walk", kind: "read", related: false, description: "Org walk: REPORTS_TO* managers", weight: 10,
    plan(work) {
      const id = work.pickUser();
      return readOp("MATCH (u:User {id: $id})-[:REPORTS_TO*1..8]->(m:User) RETURN m.id AS id LIMIT 20", { id });
    },
  },
  {
    id: "optional_manager", kind: "read", related: false, description: "OPTIONAL MATCH manager and device", weight: 10,
    plan(work) {
      const id = work.pickUser();
      return readOp(
        "MATCH (u:User {id: $id}) OPTIONAL MATCH (u)-[:REPORTS_TO]->(m:User) OPTIONAL MATCH (u)-[:OWNS]->(d:Device) " +
          "RETURN u.id AS id, m.id AS manager, d.id AS device",
        { id }
      );
    },
  },
  {
    id: "detect_nested_admin", kind: "read", related: true, description: "IAM: admin via nested MEMBER_OF* from planted nested-admin", weight: 6,
    plan(work) {
      const id = work.nestedAdmin;
      return readOp(
        "MATCH (u:User {id: $id})-[:MEMBER_OF*1..8]->(g:Group)-[:HAS_ROLE]->(r:Role {name: 'admin'}) RETURN DISTINCT u.id AS id",
        { id }
      );
    },
  },
  {
    id: "detect_sod", kind: "read", related: true, description: "IAM: EXISTS requester and approver on planted sod-user", weight: 6,
    plan(work) {
      const id = work.sodUser;
      return readOp(
        "MATCH (u:User {id: $id}) WHERE EXISTS { (u)-[:HAS_ROLE]->(:Role {name: 'requester'}) } " +
          "AND EXISTS { (u)-[:HAS_ROLE]->(:Role {name: 'approver'}) } RETURN u.id AS id",
        { id }
      );
    },
  },
  {
    // stale_grants scans every stale grant (the pre-existing client dialect;
    // the Rust canonical form pins the planted stale-grant user instead —
    // preserved bit-for-bit here so the fused form replays the identical
    // statements).
    id: "stale_grants", kind: "read", related: true, description: "IAM: HAS_ROLE.until before a cutoff date", weight: 6,
    plan() {
      return readOp(
        "MATCH (u:User)-[g:HAS_ROLE]->(r:Role) WHERE g.until < date('2024-01-01') RETURN u.id AS id, g.until AS until LIMIT 20",
        {}
      );
    },
  },
  {
    // search_directory scans the whole directory for the fixed needle
    // "example.com" (the pre-existing client dialect; the Rust canonical
    // form draws a digit-suffix needle from a picked user instead —
    // preserved bit-for-bit here so the fused form replays the identical
    // statements).
    id: "search_directory", kind: "read", related: false, description: "Directory search: email CONTAINS or any() on emails list", weight: 8,
    plan() {
      return readOp(
        "MATCH (u:User) WHERE u.email CONTAINS $q OR any(e IN u.emails WHERE e CONTAINS $q) RETURN u.id AS id ORDER BY u.id LIMIT 15",
        { q: "example.com" }
      );
    },
  },
  {
    id: "group_roster", kind: "read", related: false, description: "Group members: ORDER BY name SKIP/LIMIT", weight: 8,
    plan(work) {
      const gid = work.pickGroup();
      const skip = intRange(work.rng, 2) * 5;
      return readOp(
        "MATCH (g:Group {id: $gid})<-[:MEMBER_OF]-(u:User) RETURN u.id AS id, u.name AS name ORDER BY u.name SKIP $skip LIMIT 10",
        { gid, skip }
      );
    },
  },
  {
    id: "access_review", kind: "read", related: false, description: "Review: CASE on User.status", weight: 6,
    plan(work) {
      const id = work.pickUser();
      return readOp(
        "MATCH (u:User {id: $id}) RETURN u.id AS id, CASE u.status WHEN 'active' THEN 'ok' WHEN 'inactive' THEN 'blocked' ELSE 'unknown' END AS review",
        { id }
      );
    },
  },
  {
    id: "reporting_path", kind: "read", related: false, description: "shortestPath between two users over REPORTS_TO", weight: 6,
    plan(work) {
      const from = work.pickReportingSource();
      return readOp(
        "MATCH (a:User {id: $from}), (b:User {id: $to}) RETURN length(shortestPath((a)-[:REPORTS_TO*..8]-(b))) AS hops",
        { from, to: ROOT_USER_ID }
      );
    },
  },
  {
    id: "disable_user", kind: "write", related: false, description: "Offboard: SET User.status", weight: 6,
    plan(work) {
      const uid = work.pickUser();
      return writeOp("MATCH (u:User {id: $uid}) SET u.status = CASE WHEN u.status = 'active' THEN 'inactive' ELSE 'active' END", { uid });
    },
  },
  {
    id: "add_alias", kind: "write", related: false, description: "Alias: SET emails list append", weight: 6,
    plan(work) {
      const uid = work.pickUser();
      const alias = `${work.alloc("alias")}@example.com`;
      return writeOp("MATCH (u:User {id: $uid}) SET u.emails = u.emails + [$alias]", { uid, alias });
    },
  },
  {
    id: "revoke_membership", kind: "write", related: false, description: "Revoke: DELETE a MEMBER_OF edge", weight: 6,
    plan(work) {
      const uid = work.pickUser();
      const gid = work.pickGroup();
      return writeOp("MATCH (u:User {id: $uid})-[m:MEMBER_OF]->(g:Group {id: $gid}) DELETE m", { uid, gid });
    },
  },
  {
    id: "detach_device", kind: "write", related: false, description: "Lost device: DELETE OWNS and Device", weight: 5,
    plan(work) {
      const uid = work.pickUser();
      return writeOp("MATCH (u:User {id: $uid})-[o:OWNS]->(d:Device) WITH o, d LIMIT 1 DELETE o, d", { uid });
    },
  },
  {
    id: "clear_expiry", kind: "write", related: true, description: "Renew: REMOVE HAS_ROLE.until on stale-grant", weight: 4,
    plan(work) {
      const uid = work.staleGrant;
      return writeOp("MATCH (u:User {id: $uid})-[g:HAS_ROLE]->() WHERE g.until IS NOT NULL REMOVE g.until", { uid });
    },
  },
];

function hiredIso(i) {
  const day = 1 + (i % 28);
  const month = 1 + (i % 12);
  const year = 2015 + (i % 10);
  return `${String(year).padStart(4, "0")}-${String(month).padStart(2, "0")}-${String(day).padStart(2, "0")}`;
}

function ordinaryUser(id, i) {
  const email = `${id}@example.com`;
  const emails = [email];
  if (i % 5 === 0) emails.push(`${id}.alias@example.com`);
  return {
    id, name: `User ${i}`, email, status: i % 17 === 0 ? "inactive" : "active",
    hired: hiredIso(i), emails, planted: null,
  };
}

function plantedUser(id, name, planted) {
  const email = `${id}@example.com`;
  return { id, name, email, status: "active", hired: "2019-03-15", emails: [email], planted };
}

function generateIdentity({ graphSeed, scale, nestDepth, groupK }) {
  const rng = mulberry32(graphSeed >>> 0);
  const nUsers = Math.max(20, Math.round(10000 * scale));
  const nGroups = Math.max(8, Math.floor(nUsers / 20));
  nestDepth = Math.max(2, Math.min(8, nestDepth));
  groupK = Math.max(3, groupK);

  const apps = [
    { id: "app-payroll", name: "Payroll" },
    { id: "app-crm", name: "CRM" },
    { id: "app-wiki", name: "Wiki" },
    { id: "app-admin", name: "Admin Console" },
  ];
  const roles = [
    { id: "role-viewer", name: "viewer" },
    { id: "role-editor", name: "editor" },
    { id: "role-admin", name: "admin" },
    { id: "role-requester", name: "requester" },
    { id: "role-approver", name: "approver" },
  ];
  const permissions = [
    { id: "perm-wiki-read", name: "wiki.read" },
    { id: "perm-crm-write", name: "crm.write" },
    { id: "perm-admin-all", name: "admin.all" },
    { id: "perm-payroll-read", name: "payroll.read" },
  ];
  const grants = [
    ["role-viewer", "perm-wiki-read"],
    ["role-editor", "perm-crm-write"],
    ["role-admin", "perm-admin-all"],
    ["role-requester", "perm-payroll-read"],
    ["role-approver", "perm-payroll-read"],
  ];
  const ons = [
    ["perm-wiki-read", "app-wiki"],
    ["perm-crm-write", "app-crm"],
    ["perm-admin-all", "app-admin"],
    ["perm-payroll-read", "app-payroll"],
  ];

  const groups = [];
  for (let i = 0; i < nGroups; i++) groups.push({ id: `grp-${i}`, name: `Group ${i}`, planted: null });
  const groupMemberships = [];
  const nRoots = Math.max(2, Math.floor(nGroups / 5));
  for (let i = nRoots; i < nGroups; i++) groupMemberships.push([`grp-${i}`, `grp-${i % nRoots}`]);
  const groupRoles = [];
  for (let i = 0; i < nRoots; i++) groupRoles.push([`grp-${i}`, GRANTABLE_ROLES[i % GRANTABLE_ROLES.length]]);

  const nestGroups = [];
  for (let k = 0; k < nestDepth; k++) {
    const id = `nest-${k}`;
    groups.push({ id, name: `Nested ${k}`, planted: "nest" });
    nestGroups.push(id);
  }
  for (let k = 0; k < nestDepth - 1; k++) groupMemberships.push([nestGroups[k], nestGroups[k + 1]]);
  groupRoles.push([nestGroups[nestDepth - 1], "role-admin"]);

  const users = [];
  const userMemberships = [];
  const userRoles = [];
  const reportsTo = [];
  const devices = [];
  const owns = [];

  for (let i = 0; i < nUsers; i++) {
    const id = `user-${i}`;
    users.push(ordinaryUser(id, i));
    userMemberships.push([id, `grp-${i % nGroups}`]);
    if (rng() < 0.35) {
      // n_groups >= 8 (clamped earlier), so the skip never wraps onto the primary.
      const skip = 1 + intRange(rng, nGroups - 1);
      const other = (i % nGroups + skip) % nGroups;
      userMemberships.push([id, `grp-${other}`]);
    }
    if (rng() < 0.25) userRoles.push({ user: id, role: "role-viewer" });
    if (i > 0 && i % 9 !== 0) {
      const manager = `user-${Math.floor(i / 8)}`;
      reportsTo.push([id, manager]);
    }
    if (rng() < 0.20) {
      const did = `dev-${i}`;
      devices.push(did);
      owns.push([id, did]);
    }
  }

  // The group the nest-leaf fan joins, named once at its source (mirrors
  // IdentityPlanted.fan_group on the Rust side).
  const fanGroup = nestGroups[0];
  // Each planted user's id, named once (mirrors the Go generator's
  // single-source naming): referenced at the node's creation and in the
  // planted contract below.
  const nestedAdmin = "nested-admin";
  const orphanPriv = "orphan-priv";
  const sodUser = "sod-user";
  const staleGrant = "stale-grant";
  const leafUsers = [];
  users.push(plantedUser(nestedAdmin, "Nested Admin", "nested-admin"));
  userMemberships.push([nestedAdmin, fanGroup]);
  leafUsers.push(nestedAdmin);
  for (let k = 0; k < groupK - 1; k++) {
    const id = `leaf-${k}`;
    users.push(plantedUser(id, `Leaf ${k}`, "nest-leaf"));
    userMemberships.push([id, fanGroup]);
    reportsTo.push([id, ROOT_USER_ID]);
    leafUsers.push(id);
  }

  users.push(plantedUser(orphanPriv, "Orphan Priv", "orphan-priv"));
  userRoles.push({ user: orphanPriv, role: "role-admin" });
  userMemberships.push([orphanPriv, `grp-${nGroups - 1}`]);

  users.push(plantedUser(sodUser, "SoD User", "sod"));
  userRoles.push({ user: sodUser, role: "role-requester" }, { user: sodUser, role: "role-approver" });
  userMemberships.push([sodUser, "grp-0"]);
  reportsTo.push([sodUser, ROOT_USER_ID]);

  users.push(plantedUser(staleGrant, "Stale Grant", "stale"));
  userRoles.push({ user: staleGrant, role: "role-editor", until: STALE_UNTIL });
  userMemberships.push([staleGrant, "grp-1"]);
  reportsTo.push([staleGrant, ROOT_USER_ID]);

  return {
    seed: graphSeed,
    scale,
    users,
    groups,
    roles,
    permissions,
    apps,
    devices,
    userMemberships,
    groupMemberships,
    userRoles,
    groupRoles,
    grants,
    ons,
    reportsTo,
    owns,
    planted: {
      nested_admin: nestedAdmin,
      nest_groups: nestGroups,
      fan_group: fanGroup,
      nest_depth: nestDepth,
      group_k: groupK,
      leaf_users: leafUsers,
      orphan_priv: orphanPriv,
      sod_user: sodUser,
      stale_grant: staleGrant,
      stale_until: STALE_UNTIL,
    },
  };
}

function loadIdentity(conn, g) {
  unwind(
    conn,
    g.users.map((u) => ({
      id: u.id, name: u.name, email: u.email, status: u.status, hired: u.hired, emails: u.emails, planted: u.planted,
    })),
    "UNWIND $rows AS u CREATE (:User {id: u.id, name: u.name, email: u.email, status: u.status, hired: date(u.hired), emails: u.emails, planted: u.planted})"
  );
  unwind(conn, g.groups, "UNWIND $rows AS g CREATE (:Group {id: g.id, name: g.name})");
  unwind(conn, g.roles, "UNWIND $rows AS r CREATE (:Role {id: r.id, name: r.name})");
  unwind(conn, g.permissions, "UNWIND $rows AS p CREATE (:Permission {id: p.id, name: p.name})");
  unwind(conn, g.apps, "UNWIND $rows AS a CREATE (:App {id: a.id, name: a.name})");
  unwind(conn, g.devices.map((id) => ({ id })), "UNWIND $rows AS d CREATE (:Device {id: d.id})");
  unwind(
    conn,
    g.userMemberships.map(([u, g]) => ({ u, g })),
    "UNWIND $rows AS r MATCH (u:User {id: r.u}) MATCH (g:Group {id: r.g}) CREATE (u)-[:MEMBER_OF]->(g)"
  );
  unwind(
    conn,
    g.groupMemberships.map(([c, p]) => ({ c, p })),
    "UNWIND $rows AS r MATCH (c:Group {id: r.c}) MATCH (p:Group {id: r.p}) CREATE (c)-[:MEMBER_OF]->(p)"
  );
  const noUntil = g.userRoles.filter((x) => !x.until).map((x) => ({ u: x.user, r: x.role }));
  const withUntil = g.userRoles.filter((x) => x.until).map((x) => ({ u: x.user, r: x.role, until: x.until }));
  unwind(conn, noUntil, "UNWIND $rows AS r MATCH (u:User {id: r.u}) MATCH (role:Role {id: r.r}) CREATE (u)-[:HAS_ROLE]->(role)");
  unwind(
    conn,
    withUntil,
    "UNWIND $rows AS r MATCH (u:User {id: r.u}) MATCH (role:Role {id: r.r}) CREATE (u)-[:HAS_ROLE {until: date(r.until)}]->(role)"
  );
  unwind(
    conn,
    g.groupRoles.map(([g, r]) => ({ g, r })),
    "UNWIND $rows AS r MATCH (g:Group {id: r.g}) MATCH (role:Role {id: r.r}) CREATE (g)-[:HAS_ROLE]->(role)"
  );
  unwind(
    conn,
    g.grants.map(([r, p]) => ({ r, p })),
    "UNWIND $rows AS r MATCH (role:Role {id: r.r}) MATCH (p:Permission {id: r.p}) CREATE (role)-[:GRANTS]->(p)"
  );
  unwind(
    conn,
    g.ons.map(([p, a]) => ({ p, a })),
    "UNWIND $rows AS r MATCH (p:Permission {id: r.p}) MATCH (a:App {id: r.a}) CREATE (p)-[:ON]->(a)"
  );
  unwind(
    conn,
    g.reportsTo.map(([u, m]) => ({ u, m })),
    "UNWIND $rows AS r MATCH (u:User {id: r.u}) MATCH (m:User {id: r.m}) CREATE (u)-[:REPORTS_TO]->(m)"
  );
  unwind(
    conn,
    g.owns.map(([u, d]) => ({ u, d })),
    "UNWIND $rows AS r MATCH (u:User {id: r.u}) MATCH (d:Device {id: r.d}) CREATE (u)-[:OWNS]->(d)"
  );
}

function snapshot(conn) {
  return {
    nodes: scalarInt(conn, "MATCH (n) RETURN count(n) AS n"),
    relationships: scalarInt(conn, "MATCH ()-[r]->() RETURN count(r) AS n"),
    users: scalarInt(conn, "MATCH (u:User) RETURN count(u) AS n"),
    groups: scalarInt(conn, "MATCH (g:Group) RETURN count(g) AS n"),
    roles: scalarInt(conn, "MATCH (r:Role) RETURN count(r) AS n"),
    memberships: scalarInt(conn, "MATCH ()-[m:MEMBER_OF]->() RETURN count(m) AS n"),
  };
}

function makeWork(graph, seed) {
  const work = {
    rng: mulberry32(seed >>> 0),
    ordinaryUsers: graph.users.filter((u) => !u.planted).map((u) => u.id),
    ordinaryGroups: graph.groups.filter((g) => !g.planted).map((g) => g.id),
    fanGroup: graph.planted.fan_group,
    nestedAdmin: graph.planted.nested_admin,
    sodUser: graph.planted.sod_user,
    staleGrant: graph.planted.stale_grant,
    next: 1,
    pickUser() {
      return this.ordinaryUsers[intRange(this.rng, this.ordinaryUsers.length)];
    },
    // The reporting path's source: any ordinary user but the org root —
    // the destination is always the root, and a source that equals it
    // plans a same-node shortestPath, which raises 51N23 (LABS-1664).
    // Resampling keeps the pick uniform over the pool minus the root;
    // the construction check below guarantees someone else to draw.
    pickReportingSource() {
      let uid = this.pickUser();
      while (uid === ROOT_USER_ID) {
        uid = this.pickUser();
      }
      return uid;
    },
    pickGroup() {
      return this.ordinaryGroups[intRange(this.rng, this.ordinaryGroups.length)];
    },
    pickRole() {
      return GRANTABLE_ROLES[intRange(this.rng, GRANTABLE_ROLES.length)];
    },
    alloc(prefix) {
      const id = `${prefix}-${this.next}`;
      this.next++;
      return id;
    },
  };
  // LABS-1664: reporting_path draws its source from this pool minus the
  // org root, and the root is itself ordinary — refuse a graph whose
  // ordinary pool is only the root, or the source pick would spin.
  const someoneBesidesRoot = work.ordinaryUsers.some((uid) => uid !== ROOT_USER_ID);
  if (!someoneBesidesRoot) {
    throw new Error("no ordinary user besides the org root to plan reporting_path from");
  }
  return work;
}

function runIdentity(cfg) {
  return runMixedLoadtest(cfg, {
    name: "identity-mixed-iam",
    configExtras: (c) => ({ nest_depth: c.nestDepth, group_k: c.groupK }),
    generate: generateIdentity,
    load: loadIdentity,
    snapshot,
    makeWork,
    catalog: CATALOG,
    // provision_user is the only op whose exec result carries a provisioned
    // id (the JS twin of the Go identityOp exec contract's ""-for-others);
    // every other op's result has no provisioned key.
    applyResult: (work, out) => {
      if (out.provisioned) work.ordinaryUsers.push(out.provisioned);
    },
  });
}

const DEFAULTS = {
  uri: ":memory:",
  scale: 0.05,
  graphSeed: 42,
  workloadSeed: 7,
  nestDepth: 4,
  groupK: 12,
  ops: 80,
  warmup: 8,
  out: "target/identity-loadtest.json",
  walDir: "",
  reset: false,
  workers: 1,
  writeFraction: 0.5,
  logicalGraphSizeLimit: "",
};

module.exports = { CATALOG, generateIdentity, loadIdentity, runIdentity, makeWork, DEFAULTS };
