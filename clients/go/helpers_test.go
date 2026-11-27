package relate_test

import (
	"errors"
	"reflect"
	"strings"
	"testing"

	"github.com/relatedb/relate/clients/go"
)

// openConn opens an in-memory database and one connection, closed at test
// end. The external test package's shared fixture lives here, in the
// lowest shared module, not inside either consumer's test file.
func openConn(t *testing.T) *relate.Connection {
	t.Helper()
	conn, err := openDB(t).Connect()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(conn.Close)
	return conn
}

// openDB opens an in-memory database, closed at test end.
func openDB(t *testing.T) *relate.Database {
	t.Helper()
	db, err := relate.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(db.Close)
	return db
}

// errOf drops a call's value so a two-result method can feed
// requireErrClosed.
func errOf[T any](_ T, err error) error { return err }

// requireErrClosed pins the closed-handle contract: the call named what
// returned an error matching relate.ErrClosed.
func requireErrClosed(t *testing.T, what string, err error) {
	t.Helper()
	if !errors.Is(err, relate.ErrClosed) {
		t.Fatalf("%s: got %v, want ErrClosed", what, err)
	}
}

// requireErrToken pins one refusal's shape: err is non-nil and mentions
// token — the one assertion every refusal check in this package builds
// on, so the diagnostic wording cannot drift between its callers.
func requireErrToken(t *testing.T, what string, err error, token string) {
	t.Helper()
	if err == nil || !strings.Contains(err.Error(), token) {
		t.Fatalf("%s: got %v, want an error mentioning %q", what, err, token)
	}
}

// requireErrorMentions pins a decoder's refusal: decoding raw fails with
// an error mentioning token. One copy of that assertion, shared by the
// error-shape and stats decode-contract tests' missing-key, mistyped,
// null-payload and closed-vocabulary checks.
func requireErrorMentions[T any](t *testing.T, decode func(string) (T, error), raw, token, what string) {
	t.Helper()
	_, err := decode(raw)
	requireErrToken(t, what, err, token)
}

// requireMissingKey is requireErrorMentions for a required key: a payload
// whose key is missing — or null, which the shape treats as absent —
// fails with an error naming the key.
func requireMissingKey[T any](t *testing.T, decode func(string) (T, error), key, raw string) {
	t.Helper()
	requireErrorMentions(t, decode, raw, `missing "`+key+`"`, "missing "+key)
}

// requireParamRefusal pins a binding refusal: the statement with these
// params fails before it runs, with an error mentioning token. The error
// comes back for the caller's own further asserts (the typed-error check,
// for instance); a mismatched refusal is fatal here.
func requireParamRefusal(t *testing.T, conn *relate.Connection, what, stmt string, params map[string]any, token string) error {
	t.Helper()
	_, err := conn.QueryParams(stmt, params)
	requireErrToken(t, what, err, token)
	return err
}

// requireCells pins one result row against want, column by column — the
// one cell-comparison assertion the params round-trip tests share, so the
// diagnostic wording and the column naming live in one place. want may be
// shorter than the row (a projection compared against part of a wider
// result); it never names a column the result does not carry.
func requireCells(t *testing.T, label string, res *relate.Result, want []any) {
	t.Helper()
	if len(res.Rows) == 0 {
		t.Fatalf("%s: no rows", label)
	}
	row := res.Rows[0]
	// The invariant stated above, checked instead of trusted — against the
	// one length that is then indexed (the column list), and naming that
	// same length, so the check, the index and the diagnostic agree.
	if len(want) > len(res.Columns) {
		t.Fatalf("%s: want names %d columns, the result carries %d", label, len(want), len(res.Columns))
	}
	for i := range want {
		if !reflect.DeepEqual(row[i], want[i]) {
			t.Fatalf("%s column %s: got %#v, want %#v", label, res.Columns[i], row[i], want[i])
		}
	}
}
