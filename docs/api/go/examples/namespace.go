package main

import (
	"fmt"
	"os"

	"github.com/relatedb/relate/clients/go"
)

func main() {
	// ADR-0593: system_ontologies is visible only under the `ontology`
	// preview opt-in; set RELATE_PREVIEW before the open (it is read
	// once, at open).
	os.Setenv("RELATE_PREVIEW", "ontology")
	db, err := relate.Open(":memory:")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	defer db.Close()

	// Open a session in a named fixed namespace (ADR-0117); names match
	// case-insensitively. db.Connect() is ConnectNamespace("neo4j").
	// system_ontologies is visible under the `ontology` opt-in
	// (ADR-0593) and not queryable (ADR-0229): the one statement kind
	// that still runs there is the administration set (ADR-0116 §2) —
	// SHOW DATABASES.
	conn, err := db.ConnectNamespace("system_ontologies")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	defer conn.Close()
	res, err := conn.Query("SHOW DATABASES")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	fmt.Println("SHOW DATABASES rows:", len(res.Rows))

	// Switch the session namespace between transactions.
	if err := conn.SetNamespace("neo4j"); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	res, err = conn.Query("MATCH (n) RETURN count(n) AS n")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	fmt.Println("neo4j nodes:", res.Rows[0][0])
}
