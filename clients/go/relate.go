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
	"errors"
	"fmt"
	"reflect"
	"runtime"
	"strings"
	"sync"
	"time"
	"unsafe"
)

// ErrClosed is returned by every method called on a Database or Connection
// after Close. It is a client-side failure (the call never reached the
// engine), so it is a plain sentinel, not an *Error: match it with
// errors.Is.
var ErrClosed = errors.New("relate: handle is closed")

// callMode is a handle kind's concurrency contract for calls under its
// lifecycle guard: the acquire/release pair a call takes the guard's mutex
// with, chosen once per kind. It has no usable zero value: a handle kind must
// pick one explicitly, so a new kind cannot silently inherit another's
// contract (a zero callMode fails on its first call). The pair are method
// expressions, so a lifecycle stays safe to build by value.
type callMode struct {
	acquire, release func(*sync.RWMutex)
}

var (
	// callsOverlap admits calls concurrently (they hold the read side).
	callsOverlap = callMode{(*sync.RWMutex).RLock, (*sync.RWMutex).RUnlock}
	// callsSerialize admits one call at a time (they hold the write side).
	callsSerialize = callMode{(*sync.RWMutex).Lock, (*sync.RWMutex).Unlock}
)

// lifecycle is the one open/closed guard both handle types embed. A call
// holds the guard for its whole C call and Close takes the write side, so a
// Close never frees a handle under a call in flight, and a call after Close
// is refused instead of touching freed memory. A Database may be shared
// across goroutines (callsOverlap); a Connection must not be used by two
// calls at once (C contract, docs/api/c/spec.md Threading), so its calls
// are callsSerialize.
type lifecycle struct {
	mu     sync.RWMutex
	closed bool
	mode   callMode
}

// newLifecycle is the guard for a handle with the given call mode.
func newLifecycle(mode callMode) lifecycle {
	return lifecycle{mode: mode}
}

// use admits one call: it returns the release func to defer, or ErrClosed.
func (l *lifecycle) use() (func(), error) {
	l.mode.acquire(&l.mu)
	if l.closed {
		l.mode.release(&l.mu)
		return nil, ErrClosed
	}
	return func() { l.mode.release(&l.mu) }, nil
}

// guarded runs call under l's guard, the one acquire/check/release prologue
// every handle method shares: it returns ErrClosed (and the zero T) when the
// handle is closed, else call's result.
func guarded[T any](l *lifecycle, call func() (T, error)) (T, error) {
	release, err := l.use()
	if err != nil {
		var zero T
		return zero, err
	}
	defer release()
	return call()
}

// guardedErr is guarded for a call that returns only an error.
func guardedErr(l *lifecycle, call func() error) error {
	_, err := guarded(l, func() (struct{}, error) { return struct{}{}, call() })
	return err
}

// shut runs free exactly once, after in-flight calls drain; later calls are
// no-ops.
func (l *lifecycle) shut(free func()) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.closed {
		return
	}
	l.closed = true
	free()
}

// Database is an open database. Close it when done; a Database dropped
// without Close is closed by its finalizer. Every method is safe to call
// concurrently with Close and returns ErrClosed after it.
type Database struct {
	lifecycle
	ptr C.relate_database
}

// Connection is a session on a Database, closed like a Database: Close is
// idempotent, a dropped Connection is closed by its finalizer, and every
// method returns ErrClosed after Close. Calls on one Connection are
// serialized (the C contract allows one call at a time per connection), so
// sharing it across goroutines is safe but gains no parallelism: use one
// Connection per goroutine for that.
type Connection struct {
	lifecycle
	ptr C.relate_connection
}

// stageVocabulary is Stage's closed vocabulary.
var stageVocabulary = vocabulary[Stage]{"error stage", []Stage{StageCompile, StageRuntime, StageInternal}}

// Stage says where a failure arose (ADR-0502 §2).
type Stage string

const (
	// StageCompile is a parse or plan failure.
	StageCompile Stage = "compile"
	// StageRuntime is an execution failure.
	StageRuntime Stage = "runtime"
	// StageInternal is a failure the engine never saw: a NULL or poisoned
	// handle, a caught panic, an undecodable argument, or a params refusal.
	StageInternal Stage = "internal"
)

// Error is a failed call's error (ADR-0502 §2): the engine's classification
// of the failure, carried with the call that failed. Every failure from this
// package that the engine or the boundary reported is an *Error; match it
// with errors.As and branch on the typed fields, never on Message text.
//
//	var e *relate.Error
//	if errors.As(err, &e) && e.Retryable { /* re-run the same work */ }
type Error struct {
	// Code is the Neo4j status code (Neo.<Classification>.<Category>.<Title>).
	Code string `json:"code"`
	// GQLStatus is the five-character GQLSTATUS.
	GQLStatus string `json:"gql_status"`
	// Stage is compile, runtime or internal.
	Stage Stage `json:"stage"`
	// Retryable is true exactly when re-running the same work may succeed
	// (a write-write conflict, a retryable memory cap).
	Retryable bool `json:"retryable"`
	// Message is the engine's message, or the boundary's for an internal failure.
	Message string `json:"message"`
}

func (e *Error) Error() string { return e.Message }

// errorKeys is the required-key list of the error shape, derived from
// Error's own field tags at init (like statsKeys): every field is required,
// so presence and the struct can never drift. A field without a bare json
// name is a programming error, caught here before any error decodes.
var errorKeys = errorWireKeys()

func errorWireKeys() []string {
	typ := reflect.TypeOf(Error{})
	keys := make([]string, 0, typ.NumField())
	for i := 0; i < typ.NumField(); i++ {
		name, opts := jsonTagName(typ.Field(i))
		if name == "" || name == "-" || opts != "" {
			panic(fmt.Sprintf("Error field %s carries %q; every field is a bare json name on the wire",
				typ.Field(i).Name, typ.Field(i).Tag.Get("json")))
		}
		keys = append(keys, name)
	}
	return keys
}

// decodeError parses the ADR-0502 §2 shape. Every field is required: a
// missing or mistyped one is an ABI contract violation naming the field,
// never a zero value in its place (a missing "retryable" would silently
// decode as false and a retry loop would decline to retry). An unknown
// stage is a violation too.
func decodeError(raw string) (*Error, error) {
	if err := missingKey("error shape", []byte(raw), errorKeys...); err != nil {
		return nil, err
	}
	// Every key is present, so zero values cannot stand in for a missing one;
	// the stage is read as text and typed through its closed vocabulary.
	var shape struct {
		Error
		Stage string `json:"stage"`
	}
	if err := json.Unmarshal([]byte(raw), &shape); err != nil {
		return nil, fmt.Errorf("ABI contract violated: decoding error shape: %w", err)
	}
	stage, err := decodeClosed(stageVocabulary, shape.Stage)
	if err != nil {
		return nil, err
	}
	e := shape.Error
	e.Stage = stage
	return &e, nil
}

// takeError copies and frees a caller-owned error_json out-param. The out-param
// is always passed non-NULL, so a NULL one after a failed status is a broken
// ABI contract.
func takeError(errJSON *C.char) error {
	if errJSON == nil {
		return errors.New("ABI contract violated: call failed without an error detail")
	}
	defer C.relate_free_string(errJSON)
	e, err := decodeError(C.GoString(errJSON))
	if err != nil {
		return err
	}
	return e
}

// statusErr is the error of a fallible status call: nil on RELATE_OK,
// otherwise the error shape the call wrote to its out-param.
func statusErr(st C.relate_status, errJSON *C.char) error {
	if st == C.RELATE_OK {
		return nil
	}
	return takeError(errJSON)
}

// cString mints one NUL-terminated C string from a Go string — the one
// home of the interior-NUL policy. Every port that takes a Go string
// NUL-terminated (statement text, parameter and entry names, map-key
// lookups, the uri and namespace names) would silently truncate an
// interior NUL, so what names the string and the NUL is refused here.
// Value bytes that cross with an explicit length (putString) do not come
// through here: their NULs are data.
func cString(what, s string) (*C.char, error) {
	if strings.IndexByte(s, 0) >= 0 {
		return nil, fmt.Errorf("%s holds an interior NUL and cannot cross the C boundary", what)
	}
	return C.CString(s), nil
}

// withCString mints one NUL-terminated C string through cString and frees
// it when use returns — the one ownership shape every NUL-terminated
// argument crossing this boundary shares (guarded/guardedErr's
// value-returning pair), so a call site cannot state the free itself or
// forget it. The refusal cString itself makes (an interior NUL) crosses
// as the error, untouched.
func withCString[T any](what, s string, use func(*C.char) (T, error)) (T, error) {
	c, err := cString(what, s)
	if err != nil {
		var zero T
		return zero, err
	}
	defer C.free(unsafe.Pointer(c))
	return use(c)
}

// withCStringErr is withCString for a use that returns only an error.
func withCStringErr(what, s string, use func(*C.char) error) error {
	_, err := withCString(what, s, func(c *C.char) (struct{}, error) {
		return struct{}{}, use(c)
	})
	return err
}

func Open(uri string) (*Database, error) {
	return open(uri, false)
}

// OpenReadOnly loads an existing database once and refuses writes and
// checkpoints (ADR-0136). A missing store is an error — only the writable
// Open creates one.
func OpenReadOnly(uri string) (*Database, error) {
	return open(uri, true)
}

func open(uri string, readOnly bool) (*Database, error) {
	var db C.relate_database
	var errJSON *C.char
	var st C.relate_status
	if err := withCStringErr("database uri", uri, func(cUri *C.char) error {
		if readOnly {
			st = C.relate_database_open_read_only(cUri, &db, &errJSON)
		} else {
			st = C.relate_database_open(cUri, &db, &errJSON)
		}
		return statusErr(st, errJSON)
	}); err != nil {
		return nil, err
	}
	d := &Database{lifecycle: newLifecycle(callsOverlap), ptr: db}
	runtime.SetFinalizer(d, (*Database).Close)
	return d, nil
}

// Close frees the database. It is idempotent: a second Close is a no-op.
func (d *Database) Close() {
	d.shut(func() { C.relate_database_close(d.ptr) })
}

// Connect opens a connection in the default "neo4j" session namespace
// (ADR-0117): it calls the canonical relate_connect_namespace with a
// NULL name (ADR-0220).
func (d *Database) Connect() (*Connection, error) {
	return d.connect(nil)
}

// ConnectNamespace opens a connection in the named session namespace
// (ADR-0117): "neo4j", "system" or "system_ontologies", matched
// case-insensitively. Connect is ConnectNamespace("neo4j").
func (d *Database) ConnectNamespace(name string) (*Connection, error) {
	return withCString("namespace name", name, d.connect)
}

func (d *Database) connect(cName *C.char) (*Connection, error) {
	return guarded(&d.lifecycle, func() (*Connection, error) {
		var conn C.relate_connection
		var errJSON *C.char
		st := C.relate_connect_namespace(d.ptr, cName, &conn, &errJSON)
		if err := statusErr(st, errJSON); err != nil {
			return nil, err
		}
		c := &Connection{lifecycle: newLifecycle(callsSerialize), ptr: conn}
		runtime.SetFinalizer(c, (*Connection).Close)
		return c, nil
	})
}

func (d *Database) Checkpoint() error {
	return guardedErr(&d.lifecycle, func() error {
		var errJSON *C.char
		st := C.relate_checkpoint(d.ptr, &errJSON)
		return statusErr(st, errJSON)
	})
}

// Close frees the connection (an open transaction is rolled back). It is
// idempotent: a second Close is a no-op.
func (c *Connection) Close() {
	c.shut(func() { C.relate_disconnect(c.ptr) })
}

// SetNamespace changes the session namespace (ADR-0117), matched
// case-insensitively. It fails while an explicit transaction is open.
func (c *Connection) SetNamespace(name string) error {
	return guardedErr(&c.lifecycle, func() error {
		return withCStringErr("namespace name", name, func(cName *C.char) error {
			var errJSON *C.char
			st := C.relate_conn_set_namespace(c.ptr, cName, &errJSON)
			return statusErr(st, errJSON)
		})
	})
}

// SetStatementTimeout bounds each following statement on the connection to
// d (ADR-0692). A statement still running past it is cancelled: Query
// returns an error whose code is
// Neo.ClientError.Transaction.TransactionTimedOutClientConfiguration and the
// connection stays usable. A zero d clears the bound (the default: none);
// a negative d is an error. The embedded library applies no default; the
// network surfaces default to 30 s.
func (c *Connection) SetStatementTimeout(d time.Duration) error {
	if d < 0 {
		return errors.New("relate: statement timeout must not be negative")
	}
	return guardedErr(&c.lifecycle, func() error {
		var errJSON *C.char
		st := C.relate_conn_set_statement_timeout(c.ptr, C.uint64_t((d+time.Millisecond-1)/time.Millisecond), &errJSON)
		return statusErr(st, errJSON)
	})
}

func (c *Connection) Begin() error {
	return guardedErr(&c.lifecycle, func() error {
		var errJSON *C.char
		st := C.relate_conn_begin(c.ptr, &errJSON)
		return statusErr(st, errJSON)
	})
}

// Commit commits the explicit transaction and returns its bookmark
// (ADR-0502 §7), "relate:<n>". It returns "" with a nil error when the
// transaction committed nothing (no write ran, or every write was a no-op):
// no bookmark is fabricated for a commit that crossed no WAL boundary.
func (c *Connection) Commit() (string, error) {
	return guarded(&c.lifecycle, func() (string, error) {
		var bm *C.char
		var errJSON *C.char
		st := C.relate_conn_commit(c.ptr, &bm, &errJSON)
		if err := statusErr(st, errJSON); err != nil {
			return "", err
		}
		// OK with no bookmark: the transaction committed nothing (ADR-0510).
		if bm == nil {
			return "", nil
		}
		defer C.relate_free_string(bm)
		return C.GoString(bm), nil
	})
}

func (c *Connection) Rollback() error {
	return guardedErr(&c.lifecycle, func() error {
		var errJSON *C.char
		st := C.relate_conn_rollback(c.ptr, &errJSON)
		return statusErr(st, errJSON)
	})
}

func (c *Connection) Query(cypher string) (*Result, error) {
	return c.QueryParams(cypher, nil)
}

// QueryParams runs a statement with named parameters: the map is bound
// through the typed params builder (see bindParams for the Go-to-value
// mapping; a nil map runs the statement with no parameters), the bound
// statement is sent over the connection, and a successful result is decoded
// by readResult.
//
// A statement failure (syntax, type, constraint, write-write conflict, …)
// returns a *Error from the result's error channel. A connection-level
// failure returns a *Error from the call's own out-param.
func (c *Connection) QueryParams(cypher string, params map[string]any) (*Result, error) {
	return guarded(&c.lifecycle, func() (*Result, error) {
		return withCString("statement", cypher, func(q *C.char) (*Result, error) {
			var root C.relate_params
			if params != nil {
				root = C.relate_params_new()
				defer C.relate_params_free(root)
				if err := bindParams(root, params); err != nil {
					return nil, err
				}
			}
			var res C.relate_query_result
			var errJSON *C.char
			st := C.relate_query_params(c.ptr, q, root, &res, &errJSON)
			if err := statusErr(st, errJSON); err != nil {
				return nil, err
			}
			defer C.relate_destroy_result(res)
			return readResult(res)
		})
	})
}
