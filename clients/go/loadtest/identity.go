package loadtest

import (
	"fmt"
	"io"
	"math/rand"
	"slices"

	"github.com/relatedb/relate/clients/go"
)

const staleUntil = "2020-06-01"

// The org root user, named once (mirrors Rust's ROOT_USER_ID): user 0 has
// no manager; every planted user reports to it, and reporting_path walks to it.
const rootUserID = "user-0"

type IdentityUser struct {
	ID      string   `json:"id"`
	Name    string   `json:"name"`
	Email   string   `json:"email"`
	Status  string   `json:"status"`
	Hired   string   `json:"hired"`
	Emails  []string `json:"emails"`
	Planted *string  `json:"planted"`
}

type IdentityNamed struct {
	ID   string `json:"id"`
	Name string `json:"name"`
}

// One group row: the planted marker mirrors Rust's IdentityGroup —
// "nest" for the nest-chain groups, nil for ordinary ones.
type IdentityGroup struct {
	ID      string  `json:"id"`
	Name    string  `json:"name"`
	Planted *string `json:"planted"`
}

// The roles a random grant may hand out — stated once, consulted by the
// group-role assignment and the workload's grant pick (no re-split literal).
var grantableRoles = []string{"role-viewer", "role-editor"}

type UserRoleGrant struct {
	User  string  `json:"user"`
	Role  string  `json:"role"`
	Until *string `json:"until"`
}

type IdentityPlanted struct {
	NestedAdmin string   `json:"nested_admin"`
	NestGroups  []string `json:"nest_groups"`
	// The group the nest-leaf fan joins, named once at its source.
	FanGroup   string   `json:"fan_group"`
	NestDepth  int64    `json:"nest_depth"`
	GroupK     int64    `json:"group_k"`
	LeafUsers  []string `json:"leaf_users"`
	OrphanPriv string   `json:"orphan_priv"`
	SodUser    string   `json:"sod_user"`
	StaleGrant string   `json:"stale_grant"`
	StaleUntil string   `json:"stale_until"`
}

type IdentityGraph struct {
	Seed             int64           `json:"seed"`
	Scale            float64         `json:"scale"`
	Users            []IdentityUser  `json:"users"`
	Groups           []IdentityGroup `json:"groups"`
	Roles            []IdentityNamed `json:"roles"`
	Permissions      []IdentityNamed `json:"permissions"`
	Apps             []IdentityNamed `json:"apps"`
	Devices          []string        `json:"devices"`
	UserMemberships  [][2]string     `json:"user_memberships"`
	GroupMemberships [][2]string     `json:"group_memberships"`
	UserRoles        []UserRoleGrant `json:"user_roles"`
	GroupRoles       [][2]string     `json:"group_roles"`
	Grants           [][2]string     `json:"grants"`
	Ons              [][2]string     `json:"ons"`
	ReportsTo        [][2]string     `json:"reports_to"`
	Owns             [][2]string     `json:"owns"`
	Planted          IdentityPlanted `json:"planted"`
}

type IdentityReportConfig struct {
	URI           string  `json:"uri"`
	Scale         float64 `json:"scale"`
	GraphSeed     int64   `json:"graph_seed"`
	WorkloadSeed  int64   `json:"workload_seed"`
	NestDepth     int     `json:"nest_depth"`
	GroupK        int     `json:"group_k"`
	Ops           int     `json:"ops"`
	Warmup        int     `json:"warmup"`
	Workers       int     `json:"workers"`
	WriteFraction float64 `json:"write_fraction"`
}

type IdentitySnapshot struct {
	Nodes         int64 `json:"nodes"`
	Relationships int64 `json:"relationships"`
	Users         int64 `json:"users"`
	Groups        int64 `json:"groups"`
	Roles         int64 `json:"roles"`
	Memberships   int64 `json:"memberships"`
}

// identityWork is the identity workload state: the shared workloadState
// (mix RNG and id allocator) plus the identity-specific pools and planted
// anchors.
type identityWork struct {
	*workloadState
	ordinaryUsers  []string
	ordinaryGroups []string
	fanGroup       string
	nestedAdmin    string
	sodUser        string
	staleGrant     string
}

// normalizeIdentity moves the generator's former silent clamps to the
// Config boundary: one normalization, before any side effect, whose result
// is what the generator, the workload, and the report all see.
func normalizeIdentity(cfg *Config) error {
	if err := normalizeScale(cfg); err != nil {
		return err
	}
	cfg.NestDepth = min(max(cfg.NestDepth, 2), 8)
	cfg.GroupK = max(cfg.GroupK, 3)
	return nil
}

// identityCLIFlags is the identity workload's own flag table, consumed only
// by IdentityCLI (the descriptor both the main and the tests drive).
func identityCLIFlags() []CLIFlag {
	return []CLIFlag{
		intFlag("nest-depth", "Planted MEMBER_OF hops from leaf group to admin (nest-0 …)", func(c *Config) *int { return &c.NestDepth }),
		intFlag("group-k", "Users in the nested-admin leaf (nested-admin plus leaf-*)", func(c *Config) *int { return &c.GroupK }),
	}
}

// generateIdentity builds the identity graph from normalized Config values:
// scale is at least minScale, nestDepth in [2, 8], groupK at least 3
// (normalizeIdentity).
func generateIdentity(seed int64, scale float64, nestDepth, groupK int) *IdentityGraph {
	rng := rand.New(rand.NewSource(seed))
	nUsers := graphSize(scale)
	nGroups := nUsers / 20

	apps := []IdentityNamed{{"app-payroll", "Payroll"}, {"app-crm", "CRM"}, {"app-wiki", "Wiki"}, {"app-admin", "Admin Console"}}
	roles := []IdentityNamed{{"role-viewer", "viewer"}, {"role-editor", "editor"}, {"role-admin", "admin"}, {"role-requester", "requester"}, {"role-approver", "approver"}}
	permissions := []IdentityNamed{{"perm-wiki-read", "wiki.read"}, {"perm-crm-write", "crm.write"}, {"perm-admin-all", "admin.all"}, {"perm-payroll-read", "payroll.read"}}
	grants := [][2]string{{"role-viewer", "perm-wiki-read"}, {"role-editor", "perm-crm-write"}, {"role-admin", "perm-admin-all"}, {"role-requester", "perm-payroll-read"}, {"role-approver", "perm-payroll-read"}}
	ons := [][2]string{{"perm-wiki-read", "app-wiki"}, {"perm-crm-write", "app-crm"}, {"perm-admin-all", "app-admin"}, {"perm-payroll-read", "app-payroll"}}

	var groups []IdentityGroup
	for i := 0; i < nGroups; i++ {
		groups = append(groups, IdentityGroup{fmt.Sprintf("grp-%d", i), fmt.Sprintf("Group %d", i), nil})
	}
	var groupMemberships [][2]string
	nRoots := max(nGroups/5, 2)
	for i := nRoots; i < nGroups; i++ {
		groupMemberships = append(groupMemberships, [2]string{fmt.Sprintf("grp-%d", i), fmt.Sprintf("grp-%d", i%nRoots)})
	}
	var groupRoles [][2]string
	for i := 0; i < nRoots; i++ {
		roleID := grantableRoles[i%len(grantableRoles)]
		groupRoles = append(groupRoles, [2]string{fmt.Sprintf("grp-%d", i), roleID})
	}

	var nestGroups []string
	planted := "nest"
	for k := 0; k < nestDepth; k++ {
		id := fmt.Sprintf("nest-%d", k)
		groups = append(groups, IdentityGroup{id, fmt.Sprintf("Nested %d", k), &planted})
		nestGroups = append(nestGroups, id)
	}
	for k := 0; k < nestDepth-1; k++ {
		groupMemberships = append(groupMemberships, [2]string{nestGroups[k], nestGroups[k+1]})
	}
	groupRoles = append(groupRoles, [2]string{nestGroups[nestDepth-1], "role-admin"})

	var users []IdentityUser
	var userMemberships [][2]string
	var userRoles []UserRoleGrant
	var reportsTo [][2]string
	var devices []string
	var owns [][2]string

	for i := 0; i < nUsers; i++ {
		id := fmt.Sprintf("user-%d", i)
		users = append(users, ordinaryUser(id, i))
		userMemberships = append(userMemberships, [2]string{id, fmt.Sprintf("grp-%d", i%nGroups)})
		if rng.Float64() < 0.35 {
			// n_groups >= 8 (minScale's user floor), so the skip never wraps
			// onto the primary.
			skip := 1 + rng.Intn(nGroups-1)
			other := (i%nGroups + skip) % nGroups
			userMemberships = append(userMemberships, [2]string{id, fmt.Sprintf("grp-%d", other)})
		}
		if rng.Float64() < 0.25 {
			userRoles = append(userRoles, UserRoleGrant{User: id, Role: "role-viewer"})
		}
		if i > 0 && i%9 != 0 {
			manager := fmt.Sprintf("user-%d", i/8)
			reportsTo = append(reportsTo, [2]string{id, manager})
		}
		if rng.Float64() < 0.20 {
			did := fmt.Sprintf("dev-%d", i)
			devices = append(devices, did)
			owns = append(owns, [2]string{id, did})
		}
	}

	var leafUsers []string
	// The group the nest-leaf fan joins, named once at its source.
	fanGroup := nestGroups[0]
	nestedAdmin := "nested-admin"
	users = append(users, plantedUser(nestedAdmin, "Nested Admin", "nested-admin"))
	userMemberships = append(userMemberships, [2]string{nestedAdmin, fanGroup})
	leafUsers = append(leafUsers, nestedAdmin)
	for k := 0; k < groupK-1; k++ {
		id := fmt.Sprintf("leaf-%d", k)
		users = append(users, plantedUser(id, fmt.Sprintf("Leaf %d", k), "nest-leaf"))
		userMemberships = append(userMemberships, [2]string{id, fanGroup})
		reportsTo = append(reportsTo, [2]string{id, rootUserID})
		leafUsers = append(leafUsers, id)
	}

	orphanPriv := "orphan-priv"
	users = append(users, plantedUser(orphanPriv, "Orphan Priv", "orphan-priv"))
	userRoles = append(userRoles, UserRoleGrant{User: orphanPriv, Role: "role-admin"})
	userMemberships = append(userMemberships, [2]string{orphanPriv, fmt.Sprintf("grp-%d", nGroups-1)})

	sodUser := "sod-user"
	users = append(users, plantedUser(sodUser, "SoD User", "sod"))
	userRoles = append(userRoles, UserRoleGrant{User: sodUser, Role: "role-requester"}, UserRoleGrant{User: sodUser, Role: "role-approver"})
	userMemberships = append(userMemberships, [2]string{sodUser, "grp-0"})
	reportsTo = append(reportsTo, [2]string{sodUser, rootUserID})

	staleGrant := "stale-grant"
	until := staleUntil
	users = append(users, plantedUser(staleGrant, "Stale Grant", "stale"))
	userRoles = append(userRoles, UserRoleGrant{User: staleGrant, Role: "role-editor", Until: &until})
	userMemberships = append(userMemberships, [2]string{staleGrant, "grp-1"})
	reportsTo = append(reportsTo, [2]string{staleGrant, rootUserID})

	return &IdentityGraph{
		Seed: seed, Scale: scale, Users: users, Groups: groups, Roles: roles, Permissions: permissions, Apps: apps,
		Devices: devices, UserMemberships: userMemberships, GroupMemberships: groupMemberships, UserRoles: userRoles,
		GroupRoles: groupRoles, Grants: grants, Ons: ons, ReportsTo: reportsTo, Owns: owns,
		Planted: IdentityPlanted{
			NestedAdmin: nestedAdmin, NestGroups: nestGroups, FanGroup: fanGroup,
			NestDepth: int64(nestDepth), GroupK: int64(groupK),
			LeafUsers: leafUsers, OrphanPriv: orphanPriv, SodUser: sodUser, StaleGrant: staleGrant, StaleUntil: staleUntil,
		},
	}
}

// loadUserRoleGrants is the one grant-load statement: every grant row carries
// user/role/until; an absent until binds as null, date(null) is null,
// and CREATE omits a null property — the contract
// TestUserRoleGrantNullUntilOmitted pins at the engine boundary.
const loadUserRoleGrants = "UNWIND $rows AS r MATCH (u:User {id: r.user}) MATCH (role:Role {id: r.role}) CREATE (u)-[:HAS_ROLE {until: date(r.until)}]->(role)"

func loadIdentity(conn *relate.Connection, g *IdentityGraph) error {
	// Every row set below is passed to unwind as the workload's row shape
	// the parameter builder binds: the node and grant sets as their typed,
	// json-tagged row structs (each field bound under its json name), the
	// relation sets through edgeRows' {from, to} rows, and devices as a
	// bare []string the statement reads as d itself. The nil-able pointers
	// that reach a statement — users' planted and grants' until — bind as
	// null (the engine omits the null property on CREATE); a group's
	// planted marker is generator data only and never reaches a statement.
	// The steps are one ordered list, first error wins.
	return runSteps(
		unwindStep(conn, g.Users, "UNWIND $rows AS u CREATE (:User {id: u.id, name: u.name, email: u.email, status: u.status, hired: date(u.hired), emails: u.emails, planted: u.planted})"),
		unwindStep(conn, g.Groups, "UNWIND $rows AS g CREATE (:Group {id: g.id, name: g.name})"),
		unwindStep(conn, g.Roles, "UNWIND $rows AS r CREATE (:Role {id: r.id, name: r.name})"),
		unwindStep(conn, g.Permissions, "UNWIND $rows AS p CREATE (:Permission {id: p.id, name: p.name})"),
		unwindStep(conn, g.Apps, "UNWIND $rows AS a CREATE (:App {id: a.id, name: a.name})"),
		unwindStep(conn, g.Devices, "UNWIND $rows AS d CREATE (:Device {id: d})"),
		unwindStep(conn, edgeRows(g.UserMemberships), "UNWIND $rows AS r MATCH (u:User {id: r.from}) MATCH (g:Group {id: r.to}) CREATE (u)-[:MEMBER_OF]->(g)"),
		unwindStep(conn, edgeRows(g.GroupMemberships), "UNWIND $rows AS r MATCH (c:Group {id: r.from}) MATCH (p:Group {id: r.to}) CREATE (c)-[:MEMBER_OF]->(p)"),
		unwindStep(conn, g.UserRoles, loadUserRoleGrants),
		unwindStep(conn, edgeRows(g.GroupRoles), "UNWIND $rows AS r MATCH (g:Group {id: r.from}) MATCH (role:Role {id: r.to}) CREATE (g)-[:HAS_ROLE]->(role)"),
		unwindStep(conn, edgeRows(g.Grants), "UNWIND $rows AS r MATCH (role:Role {id: r.from}) MATCH (p:Permission {id: r.to}) CREATE (role)-[:GRANTS]->(p)"),
		unwindStep(conn, edgeRows(g.Ons), "UNWIND $rows AS r MATCH (p:Permission {id: r.from}) MATCH (a:App {id: r.to}) CREATE (p)-[:ON]->(a)"),
		unwindStep(conn, edgeRows(g.ReportsTo), "UNWIND $rows AS r MATCH (u:User {id: r.from}) MATCH (m:User {id: r.to}) CREATE (u)-[:REPORTS_TO]->(m)"),
		unwindStep(conn, edgeRows(g.Owns), "UNWIND $rows AS r MATCH (u:User {id: r.from}) MATCH (d:Device {id: r.to}) CREATE (u)-[:OWNS]->(d)"),
	)
}

// The identity workload's spec for the shared mixed runner
// (runMixedLoadtest in mixed.go): the pieces the orchestration varies per
// workload, mirroring the spec object JS's runIdentity passes to its
// runMixedLoadtest. Everything else — validation, the load and snapshot
// sequences, the pick → plan → exec loop, the report envelope — lives once
// in the runner.
var identitySpec = mixedSpec[*IdentityGraph, IdentitySnapshot, IdentityReportConfig, IdentityPlanted, *identityWork, identityOpSpec]{
	name: "identity-mixed-iam",
	generate: func(cfg Config) *IdentityGraph {
		return generateIdentity(cfg.GraphSeed, cfg.Scale, cfg.NestDepth, cfg.GroupK)
	},
	load:      loadIdentity,
	snapshot:  identitySnapshot,
	newWork:   newIdentityWork,
	normalize: normalizeIdentity,
	planted:   func(g *IdentityGraph) IdentityPlanted { return g.Planted },
	reportConfig: func(cfg Config) IdentityReportConfig {
		return IdentityReportConfig{
			URI: cfg.URI, Scale: cfg.Scale, GraphSeed: cfg.GraphSeed, WorkloadSeed: cfg.WorkloadSeed,
			NestDepth: cfg.NestDepth, GroupK: cfg.GroupK, Ops: cfg.Ops, Warmup: cfg.Warmup, Workers: cfg.Workers,
			WriteFraction: cfg.WriteFraction,
		}
	},
	catalog: identityOpSpecs,
}

// IdentityCLI states the identity binary's command line once — the twin
// of FraudCLI: the cmd main and the CLI tests both build their CLI
// through it.
func IdentityCLI[R any](
	run func(Config) (R, error),
	print func(R, string, io.Writer),
) CLI[R] {
	return CLI[R]{
		Name:          "identity-loadtest",
		Summary:       "Mixed read/write load test on the synthetic IAM identity graph",
		Defaults:      DefaultIdentity(),
		Vocabulary:    CLIVocabulary{Graph: "identity", ScaleUnit: "users", PickNoun: "user"},
		WorkloadFlags: identityCLIFlags(),
		Run:           run,
		Print:         print,
	}
}

func RunIdentity(cfg Config) (*Report[IdentitySnapshot, IdentityReportConfig, IdentityPlanted], error) {
	return runMixedLoadtest(cfg, identitySpec)
}

func PrintIdentity(r *Report[IdentitySnapshot, IdentityReportConfig, IdentityPlanted], out string, w io.Writer) {
	printReport(w, r, r.Config.Ops, r.Config.Workers, out,
		fmt.Sprintf("graph nodes %d → %d, users %d → %d, memberships %d → %d\n",
			r.Dataset.Start.Nodes, r.Dataset.Finish.Nodes, r.Dataset.Start.Users, r.Dataset.Finish.Users,
			r.Dataset.Start.Memberships, r.Dataset.Finish.Memberships))
}

func identitySnapshot(conn *relate.Connection) (IdentitySnapshot, error) {
	var s IdentitySnapshot
	var err error
	if s.Nodes, err = scalarInt(conn, "MATCH (n) RETURN count(n) AS n"); err != nil {
		return s, err
	}
	if s.Relationships, err = scalarInt(conn, "MATCH ()-[r]->() RETURN count(r) AS n"); err != nil {
		return s, err
	}
	if s.Users, err = scalarInt(conn, "MATCH (u:User) RETURN count(u) AS n"); err != nil {
		return s, err
	}
	if s.Groups, err = scalarInt(conn, "MATCH (g:Group) RETURN count(g) AS n"); err != nil {
		return s, err
	}
	if s.Roles, err = scalarInt(conn, "MATCH (r:Role) RETURN count(r) AS n"); err != nil {
		return s, err
	}
	s.Memberships, err = scalarInt(conn, "MATCH ()-[m:MEMBER_OF]->() RETURN count(m) AS n")
	return s, err
}

func ordinaryUser(id string, i int) IdentityUser {
	email := id + "@example.com"
	emails := []string{email}
	if i%5 == 0 {
		emails = append(emails, id+".alias@example.com")
	}
	status := "active"
	if i%17 == 0 {
		status = "inactive"
	}
	return IdentityUser{ID: id, Name: fmt.Sprintf("User %d", i), Email: email, Status: status, Hired: hiredISO(i), Emails: emails}
}

func plantedUser(id, name, planted string) IdentityUser {
	email := id + "@example.com"
	p := planted
	return IdentityUser{ID: id, Name: name, Email: email, Status: "active", Hired: "2019-03-15", Emails: []string{email}, Planted: &p}
}

func hiredISO(i int) string {
	return fmt.Sprintf("%04d-%02d-%02d", 2015+(i%10), 1+(i%12), 1+(i%28))
}

func (w *identityWork) pickUser() string {
	return w.ordinaryUsers[w.rng.Intn(len(w.ordinaryUsers))]
}

// pickReportingSource is the reporting path's source user: any ordinary
// user but the org root — the destination is always the root, and a source
// that equals it plans a same-node shortestPath, which raises 51N23
// (LABS-1664). Resampling keeps the pick uniform over the pool minus the
// root; newIdentityWork panics if the pool holds no one else.
func (w *identityWork) pickReportingSource() string {
	for {
		uid := w.pickUser()
		if uid != rootUserID {
			return uid
		}
	}
}

func (w *identityWork) pickGroup() string {
	return w.ordinaryGroups[w.rng.Intn(len(w.ordinaryGroups))]
}

func (w *identityWork) pickRole() string {
	return grantableRoles[w.rng.Intn(len(grantableRoles))]
}

// The workload construction (mirrors makeWork on the JS side and
// newFraudWork here): the ordinary pools read the typed planted markers,
// the grant roles the shared grantableRoles vocabulary; the shared
// workloadState is seeded once from cfg.WorkloadSeed (the write fraction
// stays with the runner).
func newIdentityWork(graph *IdentityGraph, cfg Config) *identityWork {
	work := &identityWork{
		workloadState: newWorkloadState(cfg),
		fanGroup:      graph.Planted.FanGroup,
		nestedAdmin:   graph.Planted.NestedAdmin,
		sodUser:       graph.Planted.SodUser,
		staleGrant:    graph.Planted.StaleGrant,
	}
	for _, u := range graph.Users {
		if u.Planted == nil {
			work.ordinaryUsers = append(work.ordinaryUsers, u.ID)
		}
	}
	for _, g := range graph.Groups {
		if g.Planted == nil {
			work.ordinaryGroups = append(work.ordinaryGroups, g.ID)
		}
	}
	// LABS-1664: reporting_path draws its source from this pool minus the
	// org root, and the root is itself ordinary — refuse a graph whose
	// ordinary pool is only the root, or the source pick would spin.
	someoneBesidesRoot := slices.ContainsFunc(work.ordinaryUsers, func(uid string) bool { return uid != rootUserID })
	if !someoneBesidesRoot {
		panic("no ordinary user besides the org root to plan reporting_path from")
	}
	return work
}
