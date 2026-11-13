package relate_test

import (
	"strings"
	"testing"

	"github.com/relatedb/relate/clients/go"
)

// notQueryableSuffix is the pinned ADR-0229 42N17 text.
const notQueryableSuffix = "This database is not queryable. Use the db.ontology.* procedures on the user database 'neo4j' instead."

func TestQueryParams(t *testing.T) {
	conn := openConn(t)
	res, err := conn.QueryParams("RETURN $n AS n", map[string]any{"n": 7})
	if err != nil {
		t.Fatal(err)
	}
	n, ok := res.Rows[0][0].(int64)
	if !ok || n != 7 {
		t.Fatalf("got %#v", res.Rows[0][0])
	}
}

func TestReturnOne(t *testing.T) {
	conn := openConn(t)
	res, err := conn.Query("RETURN 1 AS n")
	if err != nil {
		t.Fatal(err)
	}
	if n, ok := res.Rows[0][0].(int64); !ok || n != 1 {
		t.Fatalf("got %#v", res.Rows[0][0])
	}
}

func TestExplicitTransaction(t *testing.T) {
	db, err := relate.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	w, err := db.Connect()
	if err != nil {
		t.Fatal(err)
	}
	defer w.Close()
	r, err := db.Connect()
	if err != nil {
		t.Fatal(err)
	}
	defer r.Close()
	if err := w.Begin(); err != nil {
		t.Fatal(err)
	}
	if _, err := w.Query("CREATE (n:Person {name: 'Ada'})"); err != nil {
		t.Fatal(err)
	}
	isolated, err := r.Query("MATCH (n:Person) RETURN count(n) AS n")
	if err != nil {
		t.Fatal(err)
	}
	if n, ok := isolated.Rows[0][0].(int64); !ok || n != 0 {
		t.Fatalf("uncommitted write leaked: %#v", isolated.Rows[0][0])
	}
	if _, err := w.Commit(); err != nil {
		t.Fatal(err)
	}
	after, err := r.Query("MATCH (n:Person) RETURN n.name AS name")
	if err != nil {
		t.Fatal(err)
	}
	if after.Rows[0][0] != "Ada" {
		t.Fatalf("got %#v", after.Rows[0][0])
	}
}

func TestConnectNamespace(t *testing.T) {
	// ADR-0229 under the `ontology` opt-in (ADR-0593): `system_ontologies`
	// is visible, stays selectable, but every non-administration
	// statement routed to it — reads included — fails with the 42N17
	// text; SHOW DATABASES still runs from there (ADR-0116 §2). Without
	// the opt-in the namespace is invisible and the connect fails as an
	// unknown name.
	t.Setenv("RELATE_PREVIEW", "ontology")
	db, err := relate.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	conn, err := db.ConnectNamespace("system_ontologies")
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	res, err := conn.Query("SHOW DATABASES")
	if err != nil {
		t.Fatal(err)
	}
	if len(res.Rows) != 3 {
		t.Fatalf("expected the three SHOW DATABASES rows, got %#v", res.Rows)
	}
	_, err = conn.Query("MATCH (n) RETURN count(n) AS n")
	if err == nil {
		t.Fatal("expected the not-queryable error for a data query")
	}
	if !strings.HasSuffix(err.Error(), notQueryableSuffix) {
		t.Fatalf("got %q", err.Error())
	}
}

func TestConnectNamespaceUnknown(t *testing.T) {
	db, err := relate.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	_, err = db.ConnectNamespace("nope")
	if err == nil {
		t.Fatal("expected error for unknown namespace")
	}
	if !strings.HasSuffix(err.Error(), "Graph not found: nope") {
		t.Fatalf("got %q", err.Error())
	}
}

func TestSetNamespaceCaseInsensitive(t *testing.T) {
	// ADR-0593: the system_ontologies leg needs the `ontology` opt-in —
	// without it the switch fails as an unknown name.
	t.Setenv("RELATE_PREVIEW", "ontology")
	conn := openConn(t)
	if err := conn.SetNamespace("SYSTEM_ONTOLOGIES"); err != nil {
		t.Fatal(err)
	}
	// The case-insensitive switch landed on system_ontologies, which is
	// not queryable (ADR-0229): a read fails with the 42N17 text.
	_, err := conn.Query("MATCH (n) RETURN count(n) AS n")
	if err == nil {
		t.Fatal("expected the not-queryable error for a read")
	}
	if !strings.HasSuffix(err.Error(), notQueryableSuffix) {
		t.Fatalf("got %q", err.Error())
	}
	if err := conn.SetNamespace("Neo4j"); err != nil {
		t.Fatal(err)
	}
	if _, err := conn.Query("RETURN 1 AS n"); err != nil {
		t.Fatal(err)
	}
}

func TestExplainCreateReportsWriteWithoutExecuting(t *testing.T) {
	conn := openConn(t)
	res, err := conn.Query("EXPLAIN CREATE (n)")
	if err != nil {
		t.Fatal(err)
	}
	if res.QueryType != relate.QueryTypeWrite {
		t.Fatalf("EXPLAIN CREATE (n) QueryType: got %q, want %q", res.QueryType, relate.QueryTypeWrite)
	}
	check, err := conn.Query("MATCH (n) RETURN n")
	if err != nil {
		t.Fatal(err)
	}
	if len(check.Rows) != 0 {
		t.Fatalf("EXPLAIN must not execute: got %d rows", len(check.Rows))
	}
}

func TestResultNamespaceResolvedVsSession(t *testing.T) {
	conn := openConn(t)
	routed, err := conn.Query("USE system SHOW DATABASES YIELD name RETURN name")
	if err != nil {
		t.Fatal(err)
	}
	if routed.Namespace != "system" {
		t.Fatalf("USE-routed statement Namespace: got %q, want %q", routed.Namespace, "system")
	}
	plain, err := conn.Query("RETURN 1 AS n")
	if err != nil {
		t.Fatal(err)
	}
	if plain.Namespace != "neo4j" {
		t.Fatalf("unrouted statement Namespace: got %q, want %q", plain.Namespace, "neo4j")
	}
}

func TestSetNamespaceInTransactionRefused(t *testing.T) {
	conn := openConn(t)
	if err := conn.Begin(); err != nil {
		t.Fatal(err)
	}
	defer conn.Rollback()
	err := conn.SetNamespace("system")
	if err == nil {
		t.Fatal("expected error for namespace change in transaction")
	}
	if !strings.HasSuffix(err.Error(), "Cannot change the namespace while a transaction is open.") {
		t.Fatalf("got %q", err.Error())
	}
}

func TestOpenReadOnly(t *testing.T) {
	db, err := relate.OpenReadOnly(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	conn, err := db.Connect()
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	if _, err := conn.Query("RETURN 1 AS n"); err != nil {
		t.Fatal(err)
	}
	_, err = conn.Query("CREATE ()")
	if err == nil || !strings.Contains(err.Error(), "read-only") {
		t.Fatalf("got %v", err)
	}

	writable, err := relate.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer writable.Close()
	wconn, err := writable.Connect()
	if err != nil {
		t.Fatal(err)
	}
	defer wconn.Close()
	if _, err := wconn.Query("CREATE ()"); err != nil {
		t.Fatal(err)
	}
}

func TestNotifications(t *testing.T) {
	conn := openConn(t)

	// One :Person and one :City anchor the two disconnected patterns below.
	for _, stmt := range []string{"CREATE (:Person)", "CREATE (:City)"} {
		if _, err := conn.Query(stmt); err != nil {
			t.Fatal(err)
		}
	}

	// EXPLAIN derives the plan-time list (ADR-0363): two patterns that
	// share no node variable plan as a Cartesian product.
	res, err := conn.Query("EXPLAIN MATCH (a:Person), (b:City) RETURN a, b")
	if err != nil {
		t.Fatal(err)
	}
	if len(res.Notifications) != 1 {
		t.Fatalf("expected one CartesianProduct notification, got %#v", res.Notifications)
	}
	n := res.Notifications[0]
	if n.Code != "Neo.ClientNotification.Statement.CartesianProduct" {
		t.Fatalf("Code: got %q", n.Code)
	}
	if n.GQLStatus != "03N90" {
		t.Fatalf("GQLStatus: got %q", n.GQLStatus)
	}
	if n.Severity != "INFORMATION" {
		t.Fatalf("Severity: got %q", n.Severity)
	}
	if n.Category != "PERFORMANCE" {
		t.Fatalf("Category: got %q", n.Category)
	}
	if n.Title == "" || n.Description == "" || n.Remedy == "" {
		t.Fatalf("expected title, description and remedy, got %#v", n)
	}
	if n.OperatorID == nil {
		t.Fatal("expected an operator_id on the operator-scoped CartesianProduct entry")
	}

	// A plain run does no notification work: the list is present and empty.
	plain, err := conn.Query("RETURN 1 AS n")
	if err != nil {
		t.Fatal(err)
	}
	if plain.Notifications == nil {
		t.Fatal("Notifications must be a non-nil empty slice on a plain run")
	}
	if len(plain.Notifications) != 0 {
		t.Fatalf("expected no notifications, got %#v", plain.Notifications)
	}
}

// The mutation counters (ADR-0216, the counting per ADR-0309) ride every
// result: a write reports what it mutated, a read and an EXPLAIN (nothing
// executes) report all zeros — the canonical 11 are always present. Every
// case compares the whole Stats, so the counters it leaves at zero are
// pinned too.
func TestStats(t *testing.T) {
	conn := openConn(t)

	check := func(stmt string, want relate.Stats) {
		t.Helper()
		res, err := conn.Query(stmt)
		if err != nil {
			t.Fatal(err)
		}
		if got := res.Stats; got != want {
			t.Fatalf("%s stats: got %#v, want %#v", stmt, got, want)
		}
	}

	check("CREATE (p:Person {name: 'Ada'})-[:KNOWS]->(q:Person {name: 'Grace'})",
		relate.Stats{NodesCreated: 2, RelationshipsCreated: 1, PropertiesSet: 2, LabelsAdded: 2})

	check("MATCH (p:Person {name: 'Grace'}) SET p.name = 'Grace Hopper'",
		relate.Stats{PropertiesSet: 1})

	// The property removal counts as a property write; DETACH DELETE counts
	// the node delete and the relationship delete. No label is removed.
	check("MATCH (p:Person {name: 'Grace Hopper'}) REMOVE p.name DETACH DELETE p",
		relate.Stats{NodesDeleted: 1, RelationshipsDeleted: 1, PropertiesSet: 1})

	// A label removal counts per node (ADR-0309).
	check("MATCH (p:Person {name: 'Ada'}) REMOVE p:Person", relate.Stats{LabelsRemoved: 1})

	check("CREATE CONSTRAINT person_name FOR (p:Person) REQUIRE p.name IS UNIQUE",
		relate.Stats{ConstraintsAdded: 1})

	// A read reports all zeros, every counter present.
	check("MATCH (p:Person) RETURN p.name AS name", relate.Stats{})

	// EXPLAIN does not execute: the wrapped write's counters stay zero.
	check("EXPLAIN CREATE (n)", relate.Stats{})
}

// The structured plan document (LABS-418, ADR-0381): a typed
// relate.plan/v1 document on EXPLAIN/PROFILE, nil on a plain run, with
// each node's warnings carrying the engine's remedy-bearing notifications.
func TestPlanDocument(t *testing.T) {
	conn := openConn(t)

	for _, stmt := range []string{"CREATE (:Person)", "CREATE (:City)"} {
		if _, err := conn.Query(stmt); err != nil {
			t.Fatal(err)
		}
	}

	explained, err := conn.Query("EXPLAIN MATCH (p:Person) RETURN p")
	if err != nil {
		t.Fatal(err)
	}
	if explained.Plan == nil {
		t.Fatal("EXPLAIN carries a plan document")
	}
	if explained.Plan.Schema != "relate.plan/v1" {
		t.Fatalf("schema: got %q", explained.Plan.Schema)
	}
	if explained.Plan.Profiled {
		t.Fatal("EXPLAIN carries no runtime metrics")
	}
	if explained.Plan.Root.Operator != "ProduceResults" {
		t.Fatalf("root operator: got %q", explained.Plan.Root.Operator)
	}
	if explained.Plan.Root.Actual != nil {
		t.Fatalf("EXPLAIN carries no metrics: got %#v", explained.Plan.Root.Actual)
	}
	if !strings.HasPrefix(*explained.Plan.Statement, "MATCH (p:Person)") {
		t.Fatalf("statement: got %q", *explained.Plan.Statement)
	}

	profiled, err := conn.Query("PROFILE MATCH (p:Person) RETURN p")
	if err != nil {
		t.Fatal(err)
	}
	if !profiled.Plan.Profiled {
		t.Fatal("PROFILE carries runtime metrics")
	}
	scan := findNode(profiled.Plan.Root, "NodeByLabelScan")
	if scan == nil || scan.Actual == nil {
		t.Fatalf("expected a scanned operator with metrics, got %#v", profiled.Plan.Root)
	}
	if scan.Actual.NodeReads == 0 {
		t.Fatal("the scan reads the node store")
	}
	if scan.Actual.Start == nil || scan.Actual.Start.Reason == "" {
		t.Fatalf("the walk names its start: %#v", scan.Actual.Start)
	}

	// The cartesian MATCH's warning carries its remedy, joined per operator.
	warned, err := conn.Query("EXPLAIN MATCH (a:Person), (b:City) RETURN a, b")
	if err != nil {
		t.Fatal(err)
	}
	warnings := planWarnings(warned.Plan.Root)
	if len(warnings) != 1 {
		t.Fatalf("expected one warning, got %#v", warnings)
	}
	if warnings[0].Code != "Neo.ClientNotification.Statement.CartesianProduct" {
		t.Fatalf("code: got %q", warnings[0].Code)
	}
	if warnings[0].Remedy == "" {
		t.Fatal("the warning carries its remedy")
	}

	// A plain run carries nil, never an empty document.
	plain, err := conn.Query("RETURN 1 AS n")
	if err != nil {
		t.Fatal(err)
	}
	if plain.Plan != nil {
		t.Fatalf("a plain run carries no plan: got %#v", plain.Plan)
	}
}

// findNode returns the first operator with the given name, pre-order.
func findNode(node relate.PlanNode, operator string) *relate.PlanNode {
	if node.Operator == operator {
		return &node
	}
	for _, child := range node.Children {
		if found := findNode(child, operator); found != nil {
			return found
		}
	}
	return nil
}

// planWarnings collects every node's warnings from a plan document, pre-order.
func planWarnings(node relate.PlanNode) []relate.Notification {
	out := append([]relate.Notification{}, node.Warnings...)
	for _, child := range node.Children {
		out = append(out, planWarnings(child)...)
	}
	return out
}

// TestCreateOmitsNullProperty pins the engine contract the loadtest loaders
// rely on: a null map value (nil, including a nil pointer) creates no
// property on CREATE. keys() distinguishes omission from a stored null.
func TestCreateOmitsNullProperty(t *testing.T) {
	conn := openConn(t)
	rows := []map[string]any{
		{"id": "dated", "until": "2020-06-01"},
		{"id": "open", "until": nil},
	}
	if _, err := conn.QueryParams("UNWIND $rows AS r CREATE (:Grant {id: r.id, until: date(r.until)})", map[string]any{"rows": rows}); err != nil {
		t.Fatal(err)
	}
	res, err := conn.Query("MATCH (g:Grant) RETURN g.id AS id, size(keys(g)) AS keys ORDER BY g.id")
	if err != nil {
		t.Fatal(err)
	}
	if len(res.Rows) != 2 {
		t.Fatalf("rows: %+v", res.Rows)
	}
	if got := res.Rows[0][1]; got != int64(2) {
		t.Fatalf("dated grant should carry id+until, got %v", got)
	}
	if got := res.Rows[1][1]; got != int64(1) {
		t.Fatalf("open grant should carry only id (null property omitted), got %v", got)
	}
}
