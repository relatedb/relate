package relate_test

import (
	"errors"
	"reflect"
	"strings"
	"testing"

	"github.com/relatedb/relate/clients/go"
)

// The walker decodes every scalar, container and the NUL-carrying string
// into its Go value (ADR-0502 §1).
func TestWalkerScalarsAndContainers(t *testing.T) {
	conn := openConn(t)
	res, err := conn.Query(`RETURN null AS a, true AS b, 9007199254740993 AS c, 1.5 AS d, 'a\u0000b' AS e, [1, null] AS f, {k: 'v'} AS g`)
	if err != nil {
		t.Fatal(err)
	}
	row := res.Rows[0]
	if row[0] != nil {
		t.Fatalf("null: got %#v", row[0])
	}
	if row[1] != true {
		t.Fatalf("bool: got %#v", row[1])
	}
	// An INT crosses losslessly: 2^53+1 is not representable as float64.
	if row[2] != int64(9007199254740993) {
		t.Fatalf("int: got %#v", row[2])
	}
	if row[3] != 1.5 {
		t.Fatalf("float: got %#v", row[3])
	}
	if row[4] != "a\x00b" {
		t.Fatalf("string with an interior NUL: got %q", row[4])
	}
	if !reflect.DeepEqual(row[5], []any{int64(1), nil}) {
		t.Fatalf("list: got %#v", row[5])
	}
	if !reflect.DeepEqual(row[6], map[string]any{"k": "v"}) {
		t.Fatalf("map: got %#v", row[6])
	}
}

// Nodes, relationships and paths are their own kinds, however they are
// shaped (ADR-0502 §1, §6).
func TestWalkerEntitiesAndPaths(t *testing.T) {
	conn := openConn(t)
	if _, err := conn.Query(`CREATE (:Person {name: 'Ada'})-[:KNOWS {since: 2020}]->(:Person {name: 'Bob'})`); err != nil {
		t.Fatal(err)
	}
	res, err := conn.Query(`MATCH p = (a:Person)-[r:KNOWS]->(b) RETURN a, r, b, p`)
	if err != nil {
		t.Fatal(err)
	}
	if len(res.Rows) != 1 {
		t.Fatalf("rows: %d", len(res.Rows))
	}
	a, ok := res.Rows[0][0].(relate.Node)
	if !ok {
		t.Fatalf("node: got %T", res.Rows[0][0])
	}
	if a.ElementID == "" || !reflect.DeepEqual(a.Labels, []string{"Person"}) || a.Properties["name"] != "Ada" {
		t.Fatalf("node: got %#v", a)
	}
	r, ok := res.Rows[0][1].(relate.Relationship)
	if !ok {
		t.Fatalf("relationship: got %T", res.Rows[0][1])
	}
	if r.Type != "KNOWS" || r.Properties["since"] != int64(2020) {
		t.Fatalf("relationship: got %#v", r)
	}
	if r.StartElementID != a.ElementID {
		t.Fatalf("start element id %q, want the start node's %q", r.StartElementID, a.ElementID)
	}
	p, ok := res.Rows[0][3].(relate.Path)
	if !ok || len(p) != 3 {
		t.Fatalf("path: got %#v", res.Rows[0][3])
	}
	if _, ok := p[1].(relate.Relationship); !ok {
		t.Fatalf("path element 1 is a relationship, got %T", p[1])
	}
}

// The numeric entity ids (ADR-0506) are exactly what Cypher id() returns for
// the same entities in the same row.
func TestWalkerNumericEntityIDs(t *testing.T) {
	conn := openConn(t)
	if _, err := conn.Query(`CREATE (:Person {name: 'Ada'})-[:KNOWS {since: 2020}]->(:Person {name: 'Bob'})`); err != nil {
		t.Fatal(err)
	}
	res, err := conn.Query(`MATCH (a:Person)-[r:KNOWS]->(b) ` +
		`RETURN a, r, b, id(a) AS ia, id(r) AS ir, id(startNode(r)) AS istart, id(endNode(r)) AS iend`)
	if err != nil {
		t.Fatal(err)
	}
	if len(res.Rows) != 1 {
		t.Fatalf("rows: %d", len(res.Rows))
	}
	row := res.Rows[0]
	a, ok := row[0].(relate.Node)
	if !ok {
		t.Fatalf("node: got %T", row[0])
	}
	r, ok := row[1].(relate.Relationship)
	if !ok {
		t.Fatalf("relationship: got %T", row[1])
	}
	b, ok := row[2].(relate.Node)
	if !ok {
		t.Fatalf("end node: got %T", row[2])
	}
	if a.ID != row[3] {
		t.Fatalf("node ID %d, want id(n) %v", a.ID, row[3])
	}
	if r.ID != row[4] {
		t.Fatalf("relationship ID %d, want id(r) %v", r.ID, row[4])
	}
	if r.StartID != row[5] || r.StartID != a.ID {
		t.Fatalf("relationship StartID %d, want id(startNode(r)) %v / the start node's %d", r.StartID, row[5], a.ID)
	}
	if r.EndID != row[6] || r.EndID != b.ID {
		t.Fatalf("relationship EndID %d, want id(endNode(r)) %v / the end node's %d", r.EndID, row[6], b.ID)
	}
}

// Temporals and the other typed kinds decode to their components.
func TestWalkerTypedKinds(t *testing.T) {
	conn := openConn(t)
	res, err := conn.Query(`RETURN date('2020-06-01') AS d, localtime('12:30:00.5') AS lt, ` +
		`datetime('2020-06-01T10:00:00+02:00') AS dt, duration('P1Y2M') AS dur, point({x: 1.0, y: 2.0}) AS pt`)
	if err != nil {
		t.Fatal(err)
	}
	row := res.Rows[0]
	if row[0] != (relate.Date{Year: 2020, Month: 6, Day: 1}) {
		t.Fatalf("date: got %#v", row[0])
	}
	if row[1] != (relate.LocalTime{Hour: 12, Minute: 30, Second: 0, Nanosecond: 500000000}) {
		t.Fatalf("local time: got %#v", row[1])
	}
	dt, ok := row[2].(relate.DateTime)
	if !ok || dt.Year != 2020 || dt.Hour != 10 || dt.OffsetSeconds != 7200 {
		t.Fatalf("datetime: got %#v", row[2])
	}
	if row[3] != (relate.Duration{Months: 14}) {
		t.Fatalf("duration: got %#v", row[3])
	}
	pt, ok := row[4].(relate.Point)
	if !ok || pt.SRID != 7203 || pt.Dimension != 2 || pt.X != 1 || pt.Y != 2 {
		t.Fatalf("point: got %#v", row[4])
	}
}

// A VECTOR decodes to its canonical dtype and its coordinates, split by family.
func TestWalkerVector(t *testing.T) {
	conn := openConn(t)
	res, err := conn.Query("RETURN vector([1, 2, 3], 3, INTEGER) AS iv, vector([0.5, 1.5], 2, FLOAT32) AS fv")
	if err != nil {
		t.Fatal(err)
	}
	iv, ok := res.Rows[0][0].(relate.Vector)
	if !ok || iv.DType != "INTEGER" || !reflect.DeepEqual(iv.Ints, []int64{1, 2, 3}) || iv.Floats != nil {
		t.Fatalf("integer vector: got %#v", res.Rows[0][0])
	}
	fv, ok := res.Rows[0][1].(relate.Vector)
	if !ok || fv.DType != "FLOAT32" || !reflect.DeepEqual(fv.Floats, []float64{0.5, 1.5}) || fv.Ints != nil {
		t.Fatalf("float vector: got %#v", res.Rows[0][1])
	}
}

// A statement failure rides the result and maps to a typed *Error carrying
// the engine's classification (ADR-0502 §2).
func TestStatementErrorIsTyped(t *testing.T) {
	conn := openConn(t)
	_, err := conn.Query("RETURN 1/0 AS x")
	var e *relate.Error
	if !errors.As(err, &e) {
		t.Fatalf("got %T %v, want *relate.Error", err, err)
	}
	if e.Code != "Neo.ClientError.Statement.ArithmeticError" || e.GQLStatus != "22G03" {
		t.Fatalf("classification: got %#v", e)
	}
	if e.Stage != relate.StageRuntime || e.Retryable {
		t.Fatalf("stage/retryable: got %#v", e)
	}
	if e.Message != err.Error() || !strings.Contains(e.Message, "/ by zero") {
		t.Fatalf("message: got %q", e.Message)
	}

	_, err = conn.Query("RETURN (")
	if !errors.As(err, &e) || e.Stage != relate.StageCompile {
		t.Fatalf("syntax error: got %#v", err)
	}
}

// A connection-level failure reports through the call's own out-param and
// maps to the same typed error.
func TestCallErrorIsTyped(t *testing.T) {
	db, err := relate.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	_, err = db.ConnectNamespace("nope")
	var e *relate.Error
	if !errors.As(err, &e) {
		t.Fatalf("got %T %v, want *relate.Error", err, err)
	}
	if !strings.HasSuffix(e.Message, "Graph not found: nope") {
		t.Fatalf("message: got %q", e.Message)
	}
}

// The error shape (ADR-0502 §2) is total: every field is required, so a
// payload missing one — "retryable" above all, whose zero value would make
// a retry loop decline to retry — is an ABI contract violation naming the
// field, never a zero-value *Error. A mistyped field and an unknown stage
// are violations too.
func TestErrorShapeIsTotal(t *testing.T) {
	full := `{"code":"Neo.TransientError.Transaction.Outdated","gql_status":"40N00",` +
		`"stage":"runtime","retryable":true,"message":"write-write conflict"}`
	e, err := relate.DecodeErrorForTest(full)
	if err != nil {
		t.Fatal(err)
	}
	if e.Code != "Neo.TransientError.Transaction.Outdated" || e.GQLStatus != "40N00" ||
		e.Stage != relate.StageRuntime || !e.Retryable || e.Message != "write-write conflict" {
		t.Fatalf("full shape: got %#v", e)
	}
	// "retryable": false present decodes as false; only absence is a violation.
	presentFalse := strings.Replace(full, `"retryable":true`, `"retryable":false`, 1)
	if e, err := relate.DecodeErrorForTest(presentFalse); err != nil || e.Retryable {
		t.Fatalf("retryable false present: got (%#v, %v)", e, err)
	}

	// One field dropped at a time: each drop is an error naming the field.
	requireMissingKey(t, relate.DecodeErrorForTest, "code",
		strings.Replace(full, `"code":"Neo.TransientError.Transaction.Outdated",`, "", 1))
	requireMissingKey(t, relate.DecodeErrorForTest, "gql_status",
		strings.Replace(full, `"gql_status":"40N00",`, "", 1))
	requireMissingKey(t, relate.DecodeErrorForTest, "stage",
		strings.Replace(full, `"stage":"runtime",`, "", 1))
	requireMissingKey(t, relate.DecodeErrorForTest, "retryable",
		strings.Replace(full, `"retryable":true,`, "", 1))
	requireMissingKey(t, relate.DecodeErrorForTest, "message",
		strings.Replace(full, `,"message":"write-write conflict"`, "", 1))

	// A null field is absent as far as the shape is concerned.
	requireMissingKey(t, relate.DecodeErrorForTest, "retryable",
		strings.Replace(full, `"retryable":true`, `"retryable":null`, 1))

	// A mistyped field is a decoding violation naming the field.
	requireErrorMentions(t, relate.DecodeErrorForTest,
		strings.Replace(full, `"retryable":true`, `"retryable":"yes"`, 1),
		"retryable", "mistyped retryable")

	// The unknown-stage rejection stays.
	requireErrorMentions(t, relate.DecodeErrorForTest,
		strings.Replace(full, `"stage":"runtime"`, `"stage":"nope"`, 1),
		"error stage", "unknown stage")
}

// The query-type vocabulary is closed (ADR-0162): the decoder accepts the
// four constants' values and refuses everything else, the same refusal
// decodeError makes of an unknown stage.
func TestQueryTypeVocabularyIsClosed(t *testing.T) {
	for raw, want := range map[string]relate.QueryType{
		"r":  relate.QueryTypeRead,
		"w":  relate.QueryTypeWrite,
		"rw": relate.QueryTypeReadWrite,
		"s":  relate.QueryTypeSchema,
	} {
		qt, err := relate.DecodeQueryTypeForTest(raw)
		if err != nil || qt != want {
			t.Fatalf("decoding query type %q: got (%q, %v), want %q", raw, qt, err, want)
		}
	}
	for _, raw := range []string{"", "x", "rw!"} {
		if qt, err := relate.DecodeQueryTypeForTest(raw); err == nil {
			t.Fatalf("decoding query type %q: got %q, want an error", raw, qt)
		}
	}
}

// The namespace vocabulary is closed (ADR-0117): the decoder accepts the
// three fixed names, typed, and refuses everything else.
func TestNamespaceVocabularyIsClosed(t *testing.T) {
	for raw, want := range map[string]relate.Namespace{
		"neo4j":             relate.NamespaceNeo4j,
		"system":            relate.NamespaceSystem,
		"system_ontologies": relate.NamespaceSystemOntologies,
	} {
		ns, err := relate.DecodeNamespaceForTest(raw)
		if err != nil || ns != want {
			t.Fatalf("decoding namespace %q: got (%q, %v), want %q", raw, ns, err, want)
		}
	}
	for _, raw := range []string{"", "Neo4j", "main"} {
		if ns, err := relate.DecodeNamespaceForTest(raw); err == nil {
			t.Fatalf("decoding namespace %q: got %q, want an error", raw, ns)
		}
	}
}

// The notifications decoder's contract (ADR-0363): the payload is an
// always-present JSON array — null and non-array payloads are ABI
// violations instead of a nil slice, and "[]" stays the non-nil empty
// list.
func TestNotificationsDecodeContract(t *testing.T) {
	for _, raw := range []string{"null", "{}", "", "not json"} {
		if got, err := relate.DecodeNotificationsForTest(raw); err == nil {
			t.Fatalf("decoding notifications %q: got %#v, want an error", raw, got)
		}
	}
	empty, err := relate.DecodeNotificationsForTest("[]")
	if err != nil {
		t.Fatalf("decoding an empty notifications array: %v", err)
	}
	if empty == nil || len(empty) != 0 {
		t.Fatalf("empty notifications array: got %#v, want a non-nil empty slice", empty)
	}
}

// The stats decoder's contract (ADR-0216): the payload is the canonical 11
// counters, zeros included — every key is required, so a payload missing
// one is an ABI contract violation naming it, never a zero value in its
// place (a dropped counter would read as "nothing happened"). Null and
// non-object payloads are violations too, and a zero value present decodes
// as zero — only absence is a violation.
func TestStatsDecodeContract(t *testing.T) {
	full := `{"nodes_created":1,"nodes_deleted":0,"relationships_created":0,` +
		`"relationships_deleted":0,"properties_set":2,"labels_added":1,"labels_removed":0,` +
		`"indexes_added":0,"indexes_removed":0,"constraints_added":0,"constraints_removed":0}`
	// The expected value, spelled once: the literal pins all 11 counters,
	// the eight the payload leaves at zero included, and the unknown-key
	// case below decodes the same eleven.
	want := relate.Stats{NodesCreated: 1, PropertiesSet: 2, LabelsAdded: 1}
	if s, err := relate.DecodeStatsForTest(full); err != nil || s != want {
		t.Fatalf("full shape: got (%#v, %v), want %#v", s, err, want)
	}

	// One counter made absent at a time — the rename leaves an unknown "-"
	// key riding along, which the decoder ignores the way decodeError
	// ignores unknown error-shape keys: each absence is an error naming the
	// key, never a zero value in its place.
	for _, key := range []string{
		"nodes_created", "nodes_deleted", "relationships_created", "relationships_deleted",
		"properties_set", "labels_added", "labels_removed", "indexes_added", "indexes_removed",
		"constraints_added", "constraints_removed",
	} {
		requireMissingKey(t, relate.DecodeStatsForTest, key,
			strings.Replace(full, `"`+key+`":`, `"-":`, 1))
	}

	// A null counter is absent as far as the shape is concerned.
	requireMissingKey(t, relate.DecodeStatsForTest, "labels_added",
		strings.Replace(full, `"labels_added":1`, `"labels_added":null`, 1))

	// Non-object payloads are violations: arrays and scalars fail the
	// payload unmarshal, and a JSON null payload is its own shape fault
	// naming the object the contract expects — the same diagnosis decodePlan
	// makes of a null plan, not a misleading "first counter missing".
	for _, raw := range []string{"[]", `"w"`, "", "not json"} {
		if s, err := relate.DecodeStatsForTest(raw); err == nil {
			t.Fatalf("decoding stats %q: got %#v, want an error", raw, s)
		}
	}
	requireErrorMentions(t, relate.DecodeStatsForTest, "null",
		"stats payload is JSON null, expected an object", "null stats payload")

	// A key outside the canonical set is ignored whatever shape it carries
	// — the tolerance the engine's own stats decoder documents.
	extra := strings.Replace(full, `"nodes_created":1`, `"nodes_created":1,"extra":{"n":1}`, 1)
	if s, err := relate.DecodeStatsForTest(extra); err != nil || s != want {
		t.Fatalf("unknown key: got (%#v, %v), want %#v", s, err, want)
	}

	// A mistyped canonical counter fails the value decode, which names the
	// field it could not type.
	requireErrorMentions(t, relate.DecodeStatsForTest,
		strings.Replace(full, `"nodes_created":1`, `"nodes_created":"one"`, 1),
		"nodes_created", "mistyped nodes_created")
}

// The plan decoder's contract (LABS-418, ADR-0381): an empty payload is
// the accessor's NULL (a plain run) and yields no document; null,
// non-object, and object-shaped-but-not-plan payloads are ABI violations —
// never a zero-value document posing as a plan.
func TestPlanDecodeContract(t *testing.T) {
	if plan, err := relate.DecodePlanForTest(""); err != nil || plan != nil {
		t.Fatalf("decoding an absent plan: got (%#v, %v), want (nil, nil)", plan, err)
	}
	plan, err := relate.DecodePlanForTest(`{"schema":"relate.plan/v1","root":{"operator":"X"}}`)
	if err != nil {
		t.Fatalf("decoding a minimal plan document: %v", err)
	}
	if plan.Root.Operator != "X" {
		t.Fatalf("minimal plan document root: got %q", plan.Root.Operator)
	}
	for _, raw := range []string{
		"null", "not json", "[]", "{}",
		`{"schema":"relate.plan/v1"}`,
		`{"schema":"wrong","root":{"operator":"X"}}`,
	} {
		if got, err := relate.DecodePlanForTest(raw); err == nil {
			t.Fatalf("decoding plan %q: got %#v, want an error", raw, got)
		}
	}
}

// Commit returns the transaction's bookmark; an auto-commit write carries its
// bookmark on the result; a commit that wrote nothing has no bookmark.
func TestBookmarks(t *testing.T) {
	conn := openConn(t)

	auto, err := conn.Query("CREATE (:Note {n: 0})")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(auto.Bookmark, "relate:") {
		t.Fatalf("auto-commit bookmark: got %q", auto.Bookmark)
	}
	read, err := conn.Query("RETURN 1 AS n")
	if err != nil {
		t.Fatal(err)
	}
	if read.Bookmark != "" {
		t.Fatalf("a read carries no bookmark: got %q", read.Bookmark)
	}

	if err := conn.Begin(); err != nil {
		t.Fatal(err)
	}
	if _, err := conn.Query("CREATE (:Note {n: 1})"); err != nil {
		t.Fatal(err)
	}
	bm, err := conn.Commit()
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(bm, "relate:") || bm == auto.Bookmark {
		t.Fatalf("commit bookmark %q must be a new commit after %q", bm, auto.Bookmark)
	}

	if err := conn.Begin(); err != nil {
		t.Fatal(err)
	}
	empty, err := conn.Commit()
	if err != nil {
		t.Fatal(err)
	}
	if empty != "" {
		t.Fatalf("a commit that wrote nothing has no bookmark: got %q", empty)
	}
}

// A failed commit (ADR-0510) routes through the shared status/error helper:
// RELATE_ERROR carries the write-write conflict's classification through
// error_json — the retryable transient code a retry loop branches on — and
// no bookmark is fabricated for the uncommitted transaction.
func TestCommitFailureCarriesConflictClassification(t *testing.T) {
	db, err := relate.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	winner, err := db.Connect()
	if err != nil {
		t.Fatal(err)
	}
	defer winner.Close()
	loser, err := db.Connect()
	if err != nil {
		t.Fatal(err)
	}
	defer loser.Close()
	if _, err := winner.Query("CREATE CONSTRAINT p_id FOR (n:P) REQUIRE n.id IS UNIQUE"); err != nil {
		t.Fatal(err)
	}
	for _, c := range []*relate.Connection{winner, loser} {
		if err := c.Begin(); err != nil {
			t.Fatal(err)
		}
		if _, err := c.Query("CREATE (:P {id: 77})"); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := winner.Commit(); err != nil {
		t.Fatal(err)
	}
	bm, err := loser.Commit()
	if err == nil {
		t.Fatal("the losing transaction's commit must fail")
	}
	if bm != "" {
		t.Fatalf("a failed commit returns no bookmark: got %q", bm)
	}
	var e *relate.Error
	if !errors.As(err, &e) {
		t.Fatalf("got %T %v, want *relate.Error", err, err)
	}
	if e.Code != "Neo.TransientError.Transaction.Outdated" {
		t.Fatalf("code: got %q", e.Code)
	}
	if !e.Retryable {
		t.Fatalf("a write-write conflict is retryable: %#v", e)
	}
	if e.Stage != relate.StageRuntime {
		t.Fatalf("stage: got %q", e.Stage)
	}
}

// The statement's own precondition: text that crosses into C
// NUL-terminated is refused before the engine sees a truncated prefix.
func TestStatementRefusals(t *testing.T) {
	conn := openConn(t)
	if _, err := conn.QueryParams("RETURN 1 AS\x00 n", nil); err == nil ||
		!strings.Contains(err.Error(), "interior NUL") {
		t.Fatalf("interior-NUL statement: got %v", err)
	}
}
