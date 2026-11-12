package main

import (
	"errors"
	"fmt"
	"os"

	"github.com/relatedb/relate/clients/go"
)

// Close is idempotent, and a call after Close returns ErrClosed instead of
// touching freed memory.
func main() {
	db, err := relate.Open(":memory:")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	conn, err := db.Connect()
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	conn.Close()
	conn.Close() // a second Close is a no-op
	db.Close()
	db.Close()

	if _, err := conn.Query("RETURN 1"); !errors.Is(err, relate.ErrClosed) {
		fmt.Fprintln(os.Stderr, "want ErrClosed, got", err)
		os.Exit(1)
	}
	fmt.Println("closed handles return ErrClosed")
}
