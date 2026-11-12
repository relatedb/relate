package relate_test

import (
	"errors"
	"sync"
	"testing"

	"github.com/relatedb/relate/clients/go"
)

func TestCloseIsIdempotent(t *testing.T) {
	db := openDB(t)
	conn, err := db.Connect()
	if err != nil {
		t.Fatal(err)
	}
	conn.Close()
	conn.Close()
	db.Close()
	db.Close()
}

func TestDatabaseMethodsAfterCloseReturnErrClosed(t *testing.T) {
	db := openDB(t)
	db.Close()
	requireErrClosed(t, "Connect", errOf(db.Connect()))
	requireErrClosed(t, "ConnectNamespace", errOf(db.ConnectNamespace("system")))
	requireErrClosed(t, "Checkpoint", db.Checkpoint())
}

func TestConnectionMethodsAfterCloseReturnErrClosed(t *testing.T) {
	conn := openConn(t)
	conn.Close()
	requireErrClosed(t, "Query", errOf(conn.Query("RETURN 1")))
	requireErrClosed(t, "QueryParams", errOf(conn.QueryParams("RETURN $n", map[string]any{"n": 1})))
	requireErrClosed(t, "SetNamespace", conn.SetNamespace("system"))
	requireErrClosed(t, "Begin", conn.Begin())
	requireErrClosed(t, "Commit", errOf(conn.Commit()))
	requireErrClosed(t, "Rollback", conn.Rollback())
}

// A Close racing in-flight calls on one Connection neither crashes nor frees
// a handle under a call: calls serialize, and every call either succeeds or
// returns ErrClosed. Run with -race.
func TestCloseRacesInFlightCalls(t *testing.T) {
	db := openDB(t)
	conn, err := db.Connect()
	if err != nil {
		t.Fatal(err)
	}
	var wg, started sync.WaitGroup
	for i := 0; i < 8; i++ {
		wg.Add(1)
		started.Add(1)
		go func() {
			defer wg.Done()
			started.Done()
			for j := 0; j < 50; j++ {
				if _, err := conn.Query("RETURN 1"); err != nil && !errors.Is(err, relate.ErrClosed) {
					t.Errorf("Query: %v", err)
					return
				}
			}
		}()
	}
	started.Wait() // every worker is running before Close races them
	conn.Close()
	db.Close()
	wg.Wait()
}
