package relate

/*
#cgo CFLAGS: -I${SRCDIR}/include
#cgo LDFLAGS: -L${SRCDIR}/../../target/debug -L${SRCDIR}/../../target/release -lrelate_ffi
#include <stdlib.h>
#include "relate.h"
*/
import "C"

import (
	"encoding/json"
	"fmt"
	"reflect"
)

// QueryType is a statement's plan-time classification (ADR-0162), the
// split per ADR-0332. It is a closed vocabulary: decodeQueryType refuses
// any value outside the four constants, the same refusal decodeError makes
// of an unknown stage.
type QueryType string

const (
	// QueryTypeRead is a read.
	QueryTypeRead QueryType = "r"
	// QueryTypeWrite is a write with no returned columns.
	QueryTypeWrite QueryType = "w"
	// QueryTypeReadWrite is a write with returned columns.
	QueryTypeReadWrite QueryType = "rw"
	// QueryTypeSchema is a schema statement.
	QueryTypeSchema QueryType = "s"
)

// queryTypeVocabulary is QueryType's closed vocabulary.
var queryTypeVocabulary = vocabulary[QueryType]{"query type",
	[]QueryType{QueryTypeRead, QueryTypeWrite, QueryTypeReadWrite, QueryTypeSchema}}

// decodeQueryType types the C ABI's query_type accessor (ADR-0162). The
// accessor's contract is the four-value vocabulary above; any other value
// is an ABI contract violation and an error.
func decodeQueryType(raw string) (QueryType, error) {
	return decodeClosed(queryTypeVocabulary, raw)
}

// Namespace is a statement's resolved session namespace (ADR-0117): one
// of the three fixed databases. Input names (ConnectNamespace,
// SetNamespace, USE) match case-insensitively; the resolved value is
// always the canonical spelling of one of these constants.
type Namespace string

const (
	// NamespaceNeo4j is the user database.
	NamespaceNeo4j Namespace = "neo4j"
	// NamespaceSystem is the administration database.
	NamespaceSystem Namespace = "system"
	// NamespaceSystemOntologies is the stored-ontology database.
	NamespaceSystemOntologies Namespace = "system_ontologies"
)

// namespaceVocabulary is Namespace's closed vocabulary.
var namespaceVocabulary = vocabulary[Namespace]{"namespace",
	[]Namespace{NamespaceNeo4j, NamespaceSystem, NamespaceSystemOntologies}}

// decodeNamespace types the C ABI's namespace accessor (ADR-0117). The
// contract is the three fixed namespaces; any other value is an ABI
// contract violation and an error, so every closed-vocabulary accessor in
// readResult fails loudly on a wire break.
func decodeNamespace(raw string) (Namespace, error) {
	return decodeClosed(namespaceVocabulary, raw)
}

type Result struct {
	Columns []string
	// Rows holds the decoded cells: nil for null, bool, int64, float64,
	// string, []any (LIST), map[string]any (MAP), and the typed values of
	// value.go (Node, Relationship, Path, Date, ... Vector).
	Rows [][]any
	// QueryType is the statement's plan-time classification (ADR-0162) as
	// a typed QueryType: QueryTypeRead, QueryTypeWrite, QueryTypeReadWrite
	// or QueryTypeSchema, the split per ADR-0332. EXPLAIN and PROFILE
	// report the wrapped statement's type.
	QueryType QueryType
	// Namespace is the statement's resolved namespace (ADR-0117) as a
	// typed Namespace: NamespaceNeo4j, NamespaceSystem or
	// NamespaceSystemOntologies. It is the USE target when the statement
	// carries a USE clause, otherwise the session namespace.
	Namespace Namespace
	// Bookmark is the auto-commit bookmark, "relate:<n>" (ADR-0310), or
	// "" when the statement committed nothing: a read, an EXPLAIN, or any
	// statement inside an explicit transaction (its commit's bookmark is
	// Connection.Commit's return value).
	Bookmark string
	// Stats is the statement's mutation counters (ADR-0216, the counting
	// per ADR-0309) as the typed Stats: the canonical 11, zeros included —
	// a read, an EXPLAIN (nothing executes) or a no-op write carries all
	// zeros.
	Stats Stats
	// Notifications is the statement's query notifications (ADR-0363) in
	// the engine-native shape. It is an empty non-nil slice on a plain run
	// and populated by EXPLAIN and PROFILE.
	Notifications []Notification
	// Plan is the statement's structured plan (LABS-418, ADR-0381), or nil
	// on a plain run. Each node's Warnings carry the operator's
	// remedy-bearing notifications (ADR-0363), joined by operator id.
	Plan *PlanDocument
}

// PlanDocument is a statement's structured plan (LABS-418, ADR-0381): the
// relate.plan/v1 document.
type PlanDocument struct {
	Schema    string   `json:"schema"`
	Profiled  bool     `json:"profiled"`
	Statement *string  `json:"statement"`
	Root      PlanNode `json:"root"`
}

// PlanNode is one operator of a plan document.
type PlanNode struct {
	Operator      string         `json:"operator"`
	Details       string         `json:"details"`
	Identifiers   []string       `json:"identifiers"`
	EstimatedRows *int64         `json:"estimated_rows"`
	Actual        *PlanActual    `json:"actual"`
	Children      []PlanNode     `json:"children"`
	Warnings      []Notification `json:"warnings"`
}

// PlanActual is one operator's PROFILE metrics; nil on an EXPLAIN tree.
type PlanActual struct {
	Rows          int64      `json:"rows"`
	TimeMs        float64    `json:"time_ms"`
	DbHits        int64      `json:"db_hits"`
	MemoryBytes   int64      `json:"memory_bytes"`
	Hops          int64      `json:"hops"`
	NeighborWalks int64      `json:"neighbor_walks"`
	NodeReads     int64      `json:"node_reads"`
	Start         *PlanStart `json:"start"`
}

// PlanStart is the walk's chosen start element (LABS-369, ADR-0375).
type PlanStart struct {
	Element  int64  `json:"element"`
	Backward bool   `json:"backward"`
	Reason   string `json:"reason"`
}

// statementError decodes a statement failure from a result's error channel.
// The shape is result-owned: it is copied here and must not be freed.
func statementError(res C.relate_query_result) error {
	e, err := decodeError(C.GoString(C.relate_result_error_json(res)))
	if err != nil {
		return err
	}
	return e
}

// Stats is a statement's mutation counters (ADR-0216): the canonical 11,
// zeros included — every counter is always present, so a read or a no-op
// write reports all zeros, never a missing field. The counting follows
// Neo4j 4.4 (ADR-0309): labels per node, one PropertiesSet per property
// write, schema DDL in the index and constraint counters.
type Stats struct {
	NodesCreated         int64 `json:"nodes_created"`
	NodesDeleted         int64 `json:"nodes_deleted"`
	RelationshipsCreated int64 `json:"relationships_created"`
	RelationshipsDeleted int64 `json:"relationships_deleted"`
	PropertiesSet        int64 `json:"properties_set"`
	LabelsAdded          int64 `json:"labels_added"`
	LabelsRemoved        int64 `json:"labels_removed"`
	IndexesAdded         int64 `json:"indexes_added"`
	IndexesRemoved       int64 `json:"indexes_removed"`
	ConstraintsAdded     int64 `json:"constraints_added"`
	ConstraintsRemoved   int64 `json:"constraints_removed"`
}

// statsKeys is the canonical counter vocabulary (ADR-0216), derived from
// Stats's own field tags at init: the tags are the single home of the wire
// names, so a new counter is one field on Stats and nothing else — the
// presence check can never drift from the value decode. Every field must
// be an int64 counter carrying a json name; a field that breaks that
// invariant is a programming error, caught here at init before any result
// decodes, never a decode-time fallback.
var statsKeys = statsWireKeys()

func statsWireKeys() []string {
	typ := reflect.TypeOf(Stats{})
	keys := make([]string, 0, typ.NumField())
	for i := 0; i < typ.NumField(); i++ {
		field := typ.Field(i)
		if field.Type.Kind() != reflect.Int64 {
			panic(fmt.Sprintf("Stats field %s is a %s; every counter is an int64", field.Name, field.Type))
		}
		// jsonTagName (params.go) is the one place the tag is split; this
		// vocabulary's policy is the refusal: a counter the tag does not
		// name cannot be required on the wire, and no tag option is
		// meaningful on a decode-only counter — `,string` would make the
		// value decode expect a quoted string and reject every real
		// payload while this init guard stayed green.
		name, opts := jsonTagName(field)
		if name == "" || name == "-" || opts != "" {
			panic(fmt.Sprintf("Stats field %s carries %q; every counter is a bare json name on the wire",
				field.Name, field.Tag.Get("json")))
		}
		keys = append(keys, name)
	}
	return keys
}

// decodeStats types the C ABI's mutation-counter object (ADR-0216). The
// accessor's contract is the canonical 11 keys, zeros included, so a
// missing key — or one carrying JSON null — is an ABI contract violation
// naming it, never a zero value in its place, which would read a dropped
// counter as "nothing happened", the same refusal decodeError makes of a
// missing shape field. Keys outside the canonical set are ignored,
// whatever shape they carry — the tolerance the engine's own stats
// decoder documents, so a reader must not fail on an entry it does not
// know. Presence is missingKey's (the one required-key check; a JSON null
// payload or counter is refused there); the value decode into Stats owns
// the counter types and reports a mistyped counter by its field name.
func decodeStats(raw string) (Stats, error) {
	if err := missingKey("stats payload", []byte(raw), statsKeys...); err != nil {
		return Stats{}, err
	}
	var stats Stats
	if err := json.Unmarshal([]byte(raw), &stats); err != nil {
		return Stats{}, fmt.Errorf("ABI contract violated: decoding stats counters: %w", err)
	}
	return stats, nil
}

// planSchema is the plan document's pinned schema identifier (LABS-418,
// ADR-0381).
const planSchema = "relate.plan/v1"

// decodePlan decodes the C ABI's structured plan document (LABS-418,
// ADR-0381). The accessor returns NULL on a plain run — a legitimately
// absent plan, not an error — so an empty payload yields a nil
// *PlanDocument; a malformed, null, or object-shaped-but-not-plan payload
// is an ABI contract violation and an error, never a zero-value document
// posing as a plan. The pointer target makes JSON null detectable, the
// same trick decodeError documents for missing keys; the schema identifier
// and root operator carry the rest of the shape.
func decodePlan(raw string) (*PlanDocument, error) {
	if raw == "" {
		return nil, nil
	}
	var plan *PlanDocument
	if err := json.Unmarshal([]byte(raw), &plan); err != nil {
		return nil, fmt.Errorf("ABI contract violated: decoding plan: %w", err)
	}
	if plan == nil {
		return nil, fmt.Errorf("ABI contract violated: plan payload is JSON null, expected an object: %s", safeWire(raw))
	}
	if plan.Schema != planSchema {
		return nil, fmt.Errorf("ABI contract violated: plan schema %s is not %q", quotedWire(plan.Schema), planSchema)
	}
	if plan.Root.Operator == "" {
		return nil, fmt.Errorf("ABI contract violated: plan document carries no root operator")
	}
	return plan, nil
}

// readResult decodes one query result's whole envelope — the error channel,
// the metadata (query type, namespace, bookmark, stats), the notifications,
// the plan, the columns and the rows — into a Result. Every payload is
// result-owned: GoString copies it into Go memory before this returns, so
// the caller is free to destroy the C result afterwards.
func readResult(res C.relate_query_result) (*Result, error) {
	if C.relate_result_is_error(res) != 0 {
		return nil, statementError(res)
	}
	cols := int(C.relate_column_count(res))
	rowsN := int(C.relate_row_count(res))
	queryType, err := decodeQueryType(C.GoString(C.relate_result_query_type(res)))
	if err != nil {
		return nil, err
	}
	namespace, err := decodeNamespace(C.GoString(C.relate_result_namespace(res)))
	if err != nil {
		return nil, err
	}
	out := &Result{Columns: make([]string, cols), Rows: make([][]any, rowsN)}
	out.QueryType = queryType
	out.Namespace = namespace
	out.Bookmark = C.GoString(C.relate_result_bookmark(res))
	// The stats JSON is result-owned too (NULL only on an error result,
	// which the is-error check above already routed away); GoString copies
	// it into Go memory before decoding.
	stats, err := decodeStats(C.GoString(C.relate_result_stats(res)))
	if err != nil {
		return nil, err
	}
	out.Stats = stats
	// The notifications JSON is result-owned (valid until the caller's
	// destroy); GoString copies it into Go memory before decoding.
	notifications, err := decodeNotifications(C.GoString(C.relate_result_notifications(res)))
	if err != nil {
		return nil, err
	}
	out.Notifications = notifications
	// The plan document (LABS-418, ADR-0381) is result-owned too: NULL on a
	// plain run (GoString yields ""), a JSON object otherwise.
	plan, err := decodePlan(C.GoString(C.relate_result_plan_json(res)))
	if err != nil {
		return nil, err
	}
	out.Plan = plan
	for i := 0; i < cols; i++ {
		out.Columns[i] = C.GoString(C.relate_column_name(res, C.longlong(i)))
	}
	for r := 0; r < rowsN; r++ {
		row := make([]any, cols)
		for col := 0; col < cols; col++ {
			cell, err := decodeCell(res, r, col)
			if err != nil {
				return nil, fmt.Errorf("decoding cell (%d,%d): %w", r, col, err)
			}
			row[col] = cell
		}
		out.Rows[r] = row
	}
	return out, nil
}
