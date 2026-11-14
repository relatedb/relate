package loadtest

import (
	"github.com/relatedb/relate/clients/go"
)

// The op-exec reporting policy both workloads consult, stated once
// (LABS-487): every op's exec reports its statement count and its row
// count, and returns an op-level failure as an error (runOp records it as
// data). The statement count is the op's TRUE count, never floored (a 0
// is the op's own data, surfacing a broken planner), and a failed op
// reports 0 statements — the count is work that executed, not the plan
// (runOp enforces the failure half; the JS twin's catch records the
// same 0). Pool growth is the grow() half of the op contract (mixed.go),
// no-op by default via the embedded baseOp and overridden
// only by the one op per workload that grows the pools, and applied by
// runOp after a successful exec. The fraudOp and identityOp interfaces
// state their exec contracts by reference to this policy; their ops are
// delegates to the helpers below.

// rowLen is readExec's row-count half: the read query's row count,
// error-tolerant (a failed query reports 0 rows). QueryParams returns
// (nil, err) or a non-nil result, so err alone is the failure boundary.
func rowLen(r *relate.Result, err error) int {
	if err != nil {
		return 0
	}
	return len(r.Rows)
}

// readExec reports a read op: one planned statement, the query's row count
// (0 on error), and the query error.
func readExec(conn *relate.Connection, cypher string, params map[string]any) (uint32, int, error) {
	r, err := conn.QueryParams(cypher, params)
	return 1, rowLen(r, err), err
}

// writeExec reports a single-statement write op: one statement, no rows,
// and the write error.
func writeExec(conn *relate.Connection, cypher string, params map[string]any) (uint32, int, error) {
	_, err := writeQuery(conn, cypher, params)
	return 1, 0, err
}

// stmtOp is the shared single-statement op: its cypher and params, executed
// through run (readExec or writeExec). Planners return it directly via
// readOp / writeOp, so an op whose exec adds nothing needs no type of its own.
type stmtOp struct {
	baseOp
	run    func(*relate.Connection, string, map[string]any) (uint32, int, error)
	cypher string
	params map[string]any
}

func (op stmtOp) exec(conn *relate.Connection) (uint32, int, error) {
	return op.run(conn, op.cypher, op.params)
}

// readOp builds a single-statement read op.
func readOp(cypher string, params map[string]any) stmtOp {
	return stmtOp{run: readExec, cypher: cypher, params: params}
}

// writeOp builds a single-statement write op.
func writeOp(cypher string, params map[string]any) stmtOp {
	return stmtOp{run: writeExec, cypher: cypher, params: params}
}
