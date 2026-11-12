package relate_test

import (
	"errors"
	"sync"
	"testing"

	"github.com/relatedb/relate/clients/go"
)

func openDB(t *testing.T) *relate.Database {
	t.Helper()
	db, err := relate.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(db.Close)
	return db
}

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
	if _, err := db.Connect(); !errors.Is(err, relate.ErrClosed) {
		t.Fatalf("Connect: got %v, want ErrClosed", err)
	}
	if _, err := db.ConnectNamespace("system"); !errors.Is(err, relate.ErrClosed) {
		t.Fatalf("ConnectNamespace: got %v, want ErrClosed", err)
	}
	if err := db.Checkpoint(); !errors.Is(err, relate.ErrClosed) {
		t.Fatalf("Checkpoint: got %v, want ErrClosed", err)
	}
}

func TestConnectionMethodsAfterCloseReturnErrClosed(t *testing.T) {
	conn := openConn(t)
	conn.Close()
	if _, err := conn.Query("RETURN 1"); !errors.Is(err, relate.ErrClosed) {
		t.Fatalf("Query: got %v, want ErrClosed", err)
	}
	if _, err := conn.QueryParams("RETURN $n", map[string]any{"n": 1}); !errors.Is(err, relate.ErrClosed) {
		t.Fatalf("QueryParams: got %v, want ErrClosed", err)
	}
	if err := conn.SetNamespace("system"); !errors.Is(err, relate.ErrClosed) {
		t.Fatalf("SetNamespace: got %v, want ErrClosed", err)
	}
	if err := conn.Begin(); !errors.Is(err, relate.ErrClosed) {
		t.Fatalf("Begin: got %v, want ErrClosed", err)
	}
	if _, err := conn.Commit(); !errors.Is(err, relate.ErrClosed) {
		t.Fatalf("Commit: got %v, want ErrClosed", err)
	}
	if err := conn.Rollback(); !errors.Is(err, relate.ErrClosed) {
		t.Fatalf("Rollback: got %v, want ErrClosed", err)
	}
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
