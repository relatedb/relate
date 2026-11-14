package loadtest

import (
	"github.com/relatedb/relate/clients/go"
)

// The identity operation vocabulary is stated once per operation — a
// statement constant plus a planner plus a catalog row in identityOpSpecs —
// mirroring the Rust catalog's per-row plan fn (LABS-429). A single-statement
// op is the shared readOp / writeOp value (ops.go) the planner returns
// directly; provisionUserOp is the one op with its own type, because it alone
// grows the workload's pools.
//
// identityOp is the identity instantiation of the shared op contract
// (execOp in mixed.go): exec reports per the shared op-exec policy (ops.go),
// and provision_user alone overrides grow() to record its new id
// into the ordinary-user pool.
type identityOp = execOp

type provisionUserOp struct {
	baseOp
	w                     *identityWork
	uid, name, email, gid string
}

func (op provisionUserOp) exec(conn *relate.Connection) (uint32, int, error) {
	return writeExec(conn,
		"CREATE (u:User {id: $id, name: $name, email: $email, status: 'active', hired: date(), emails: [$email]}) "+
			"WITH u MATCH (g:Group {id: $gid}) CREATE (u)-[:MEMBER_OF]->(g)",
		map[string]any{"id": op.uid, "name": op.name, "email": op.email, "gid": op.gid},
	)
}

// grow applies provision_user's pool growth after a successful exec.
func (op provisionUserOp) grow() {
	op.w.ordinaryUsers = append(op.w.ordinaryUsers, op.uid)
}

// The identity statements, one named constant per op: the planners above
// pair each with its params in a readOp / writeOp (ops.go).
const (
	addToGroupCypher      = "MATCH (u:User {id: $uid}) MATCH (g:Group {id: $gid}) MERGE (u)-[:MEMBER_OF]->(g)"
	grantRoleCypher       = "MATCH (u:User {id: $uid}) MATCH (r:Role {id: $rid}) CREATE (u)-[:HAS_ROLE]->(r)"
	attachDeviceCypher    = "MATCH (u:User {id: $uid}) CREATE (d:Device {id: $did}) CREATE (u)-[:OWNS]->(d)"
	nestedGrantCypher     = "MATCH (u:User {id: $uid}) MATCH (g:Group {id: $gid}) CREATE (u)-[:MEMBER_OF]->(g)"
	effectiveAccessCypher = "MATCH (u:User {id: $id})-[:HAS_ROLE]->(r:Role)-[:GRANTS]->(perm:Permission)-[:ON]->(a:App) " +
		"RETURN DISTINCT a.name AS app, perm.name AS perm " +
		"UNION " +
		"MATCH (u:User {id: $id})-[:MEMBER_OF*1..8]->(g:Group)-[:HAS_ROLE]->(r:Role)-[:GRANTS]->(perm:Permission)-[:ON]->(a:App) " +
		"RETURN DISTINCT a.name AS app, perm.name AS perm"
	orgWalkCypher         = "MATCH (u:User {id: $id})-[:REPORTS_TO*1..8]->(m:User) RETURN m.id AS id LIMIT 20"
	optionalManagerCypher = "MATCH (u:User {id: $id}) OPTIONAL MATCH (u)-[:REPORTS_TO]->(m:User) OPTIONAL MATCH (u)-[:OWNS]->(d:Device) " +
		"RETURN u.id AS id, m.id AS manager, d.id AS device"
	detectNestedAdminCypher = "MATCH (u:User {id: $id})-[:MEMBER_OF*1..8]->(g:Group)-[:HAS_ROLE]->(r:Role {name: 'admin'}) RETURN DISTINCT u.id AS id"
	detectSodCypher         = "MATCH (u:User {id: $id}) WHERE EXISTS { (u)-[:HAS_ROLE]->(:Role {name: 'requester'}) } " +
		"AND EXISTS { (u)-[:HAS_ROLE]->(:Role {name: 'approver'}) } RETURN u.id AS id"
	staleGrantsCypher      = "MATCH (u:User)-[g:HAS_ROLE]->(r:Role) WHERE g.until < date('2024-01-01') RETURN u.id AS id, g.until AS until LIMIT 20"
	searchDirectoryCypher  = "MATCH (u:User) WHERE u.email CONTAINS $q OR any(e IN u.emails WHERE e CONTAINS $q) RETURN u.id AS id ORDER BY u.id LIMIT 15"
	groupRosterCypher      = "MATCH (g:Group {id: $gid})<-[:MEMBER_OF]-(u:User) RETURN u.id AS id, u.name AS name ORDER BY u.name SKIP $skip LIMIT 10"
	accessReviewCypher     = "MATCH (u:User {id: $id}) RETURN u.id AS id, CASE u.status WHEN 'active' THEN 'ok' WHEN 'inactive' THEN 'blocked' ELSE 'unknown' END AS review"
	reportingPathCypher    = "MATCH (a:User {id: $from}), (b:User {id: $to}) RETURN length(shortestPath((a)-[:REPORTS_TO*..8]-(b))) AS hops"
	disableUserCypher      = "MATCH (u:User {id: $uid}) SET u.status = CASE WHEN u.status = 'active' THEN 'inactive' ELSE 'active' END"
	addAliasCypher         = "MATCH (u:User {id: $uid}) SET u.emails = u.emails + [$alias]"
	revokeMembershipCypher = "MATCH (u:User {id: $uid})-[m:MEMBER_OF]->(g:Group {id: $gid}) DELETE m"
	detachDeviceCypher     = "MATCH (u:User {id: $uid})-[o:OWNS]->(d:Device) WITH o, d LIMIT 1 DELETE o, d"
	clearExpiryCypher      = "MATCH (u:User {id: $uid})-[g:HAS_ROLE]->() WHERE g.until IS NOT NULL REMOVE g.until"
)

// One planner per op, mirroring Rust's plan_* functions: each draws its
// picks from the workload (under the caller's mutex) and builds its op.
// The planner is the only place an op's picks are made; exec never draws.
func planProvisionUser(w *identityWork) identityOp {
	uid := w.alloc("lt-user")
	return provisionUserOp{w: w, uid: uid, name: "Loadtest " + uid, email: uid + "@example.com", gid: w.pickGroup()}
}

func planAddToGroup(w *identityWork) identityOp {
	return writeOp(addToGroupCypher, map[string]any{"uid": w.pickUser(), "gid": w.pickGroup()})
}

func planGrantRole(w *identityWork) identityOp {
	return writeOp(grantRoleCypher, map[string]any{"uid": w.pickUser(), "rid": w.pickRole()})
}

func planAttachDevice(w *identityWork) identityOp {
	return writeOp(attachDeviceCypher, map[string]any{"uid": w.pickUser(), "did": w.alloc("lt-dev")})
}

func planNestedGrant(w *identityWork) identityOp {
	return writeOp(nestedGrantCypher, map[string]any{"uid": w.pickUser(), "gid": w.fanGroup})
}

func planEffectiveAccess(w *identityWork) identityOp {
	return readOp(effectiveAccessCypher, map[string]any{"id": w.pickUser()})
}

func planOrgWalk(w *identityWork) identityOp {
	return readOp(orgWalkCypher, map[string]any{"id": w.pickUser()})
}

func planOptionalManager(w *identityWork) identityOp {
	return readOp(optionalManagerCypher, map[string]any{"id": w.pickUser()})
}

func planDetectNestedAdmin(w *identityWork) identityOp {
	return readOp(detectNestedAdminCypher, map[string]any{"id": w.nestedAdmin})
}

func planDetectSod(w *identityWork) identityOp {
	return readOp(detectSodCypher, map[string]any{"id": w.sodUser})
}

// stale_grants scans every stale grant (the pre-existing client dialect; the
// Rust canonical form pins the planted stale-grant user instead — preserved
// bit-for-bit here so the fused form replays the identical statements).
func planStaleGrants(*identityWork) identityOp {
	return readOp(staleGrantsCypher, nil)
}

// search_directory scans the whole directory for the fixed needle
// "example.com". The needle is a fixed literal, not a workload draw: the client's
// pre-existing dialect (see planSearchDirectory) — documented rather than
// unified with Rust's picked-needle form, which would change the executed
// statements.
func planSearchDirectory(*identityWork) identityOp {
	return readOp(searchDirectoryCypher, map[string]any{"q": "example.com"})
}

func planGroupRoster(w *identityWork) identityOp {
	return readOp(groupRosterCypher, map[string]any{"gid": w.pickGroup(), "skip": int64(w.rng.Intn(2) * 5)})
}

func planAccessReview(w *identityWork) identityOp {
	return readOp(accessReviewCypher, map[string]any{"id": w.pickUser()})
}

func planReportingPath(w *identityWork) identityOp {
	return readOp(reportingPathCypher, map[string]any{"from": w.pickUser(), "to": rootUserID})
}

func planDisableUser(w *identityWork) identityOp {
	return writeOp(disableUserCypher, map[string]any{"uid": w.pickUser()})
}

func planAddAlias(w *identityWork) identityOp {
	uid := w.pickUser()
	alias := w.alloc("alias")
	return writeOp(addAliasCypher, map[string]any{"uid": uid, "alias": alias + "@example.com"})
}

func planRevokeMembership(w *identityWork) identityOp {
	return writeOp(revokeMembershipCypher, map[string]any{"uid": w.pickUser(), "gid": w.pickGroup()})
}

func planDetachDevice(w *identityWork) identityOp {
	return writeOp(detachDeviceCypher, map[string]any{"uid": w.pickUser()})
}

func planClearExpiry(w *identityWork) identityOp {
	return writeOp(clearExpiryCypher, map[string]any{"uid": w.staleGrant})
}

// identityOpSpec: one catalog row per operation, stating its metadata and
// its planner together. This is the identity vocabulary's only table — the
// pick helpers, summarize, and the shared runner consume it directly
// through their catalogRow / opSpecRow views — replacing the former pair of
// a bare metadata table plus a string-keyed plan switch and a string-keyed
// exec switch (LABS-429).
type identityOpSpec struct {
	catalogEntry
	plan func(*identityWork) identityOp
}

// planOp adapts the plan field to the shared catalog-row contract (opSpecRow
// in mixed.go): the shared runner dispatches through it.
func (s identityOpSpec) planOp(w *identityWork) identityOp { return s.plan(w) }

var identityOpSpecs = []identityOpSpec{
	{catalogEntry{ID: "provision_user", Kind: KindWrite, Description: "Provision: CREATE a User and MEMBER_OF an existing group", Weight: 12}, planProvisionUser},
	{catalogEntry{ID: "add_to_group", Kind: KindWrite, Description: "Directory: MERGE a user into a group", Weight: 16}, planAddToGroup},
	{catalogEntry{ID: "grant_role", Kind: KindWrite, Description: "Grant: CREATE HAS_ROLE from a user to viewer/editor", Weight: 10}, planGrantRole},
	{catalogEntry{ID: "attach_device", Kind: KindWrite, Description: "Device: CREATE Device and OWNS", Weight: 8}, planAttachDevice},
	{catalogEntry{ID: "nested_grant", Kind: KindWrite, Related: true, Description: "IAM continuation: extra MEMBER_OF into the planted nest leaf", Weight: 6}, planNestedGrant},
	{catalogEntry{ID: "effective_access", Kind: KindRead, Description: "Effective access: direct HAS_ROLE UNION inherited MEMBER_OF* roles", Weight: 16}, planEffectiveAccess},
	{catalogEntry{ID: "org_walk", Kind: KindRead, Description: "Org walk: REPORTS_TO* managers", Weight: 10}, planOrgWalk},
	{catalogEntry{ID: "optional_manager", Kind: KindRead, Description: "OPTIONAL MATCH manager and device", Weight: 10}, planOptionalManager},
	{catalogEntry{ID: "detect_nested_admin", Kind: KindRead, Related: true, Description: "IAM: admin via nested MEMBER_OF* from planted nested-admin", Weight: 6}, planDetectNestedAdmin},
	{catalogEntry{ID: "detect_sod", Kind: KindRead, Related: true, Description: "IAM: EXISTS requester and approver on planted sod-user", Weight: 6}, planDetectSod},
	{catalogEntry{ID: "stale_grants", Kind: KindRead, Related: true, Description: "IAM: HAS_ROLE.until before a cutoff date", Weight: 6}, planStaleGrants},
	{catalogEntry{ID: "search_directory", Kind: KindRead, Description: "Directory search: email CONTAINS or any() on emails list", Weight: 8}, planSearchDirectory},
	{catalogEntry{ID: "group_roster", Kind: KindRead, Description: "Group members: ORDER BY name SKIP/LIMIT", Weight: 8}, planGroupRoster},
	{catalogEntry{ID: "access_review", Kind: KindRead, Description: "Review: CASE on User.status", Weight: 6}, planAccessReview},
	{catalogEntry{ID: "reporting_path", Kind: KindRead, Description: "shortestPath between two users over REPORTS_TO", Weight: 6}, planReportingPath},
	{catalogEntry{ID: "disable_user", Kind: KindWrite, Description: "Offboard: SET User.status", Weight: 6}, planDisableUser},
	{catalogEntry{ID: "add_alias", Kind: KindWrite, Description: "Alias: SET emails list append", Weight: 6}, planAddAlias},
	{catalogEntry{ID: "revoke_membership", Kind: KindWrite, Description: "Revoke: DELETE a MEMBER_OF edge", Weight: 6}, planRevokeMembership},
	{catalogEntry{ID: "detach_device", Kind: KindWrite, Description: "Lost device: DELETE OWNS and Device", Weight: 5}, planDetachDevice},
	{catalogEntry{ID: "clear_expiry", Kind: KindWrite, Related: true, Description: "Renew: REMOVE HAS_ROLE.until on stale-grant", Weight: 4}, planClearExpiry},
}
