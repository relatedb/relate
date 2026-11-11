package main

import (
	"fmt"
	"os"

	"github.com/relatedb/relate/clients/go"
)

func main() {
	db, err := relate.Open(":memory:")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	defer db.Close()
	conn, err := db.Connect()
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	defer conn.Close()

	// EXPLAIN reports the wrapped statement's query type without executing
	// it: the CREATE never runs, so the graph stays empty.
	res, err := conn.Query("EXPLAIN CREATE (n)")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	fmt.Println("EXPLAIN CREATE (n)  QueryType:", res.QueryType, " Namespace:", res.Namespace)
	check, err := conn.Query("MATCH (n) RETURN n")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	fmt.Println("nodes after EXPLAIN:", len(check.Rows))

	// A USE-routed statement reports its resolved namespace; the session
	// namespace is unchanged, so a following unrouted statement still
	// resolves to "neo4j".
	res, err = conn.Query("USE system SHOW DATABASES YIELD name RETURN name")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	fmt.Println("USE system SHOW ... Namespace:", res.Namespace)

	// Query notifications (ADR-0363): EXPLAIN derives them from the plan
	// tree. Two disconnected patterns plan as a Cartesian product, so the
	// result carries one Neo.ClientNotification.Statement.CartesianProduct
	// entry with its severity, category, title, description, remedy and the
	// raising operator's id.
	for _, stmt := range []string{"CREATE (:Person)", "CREATE (:City)"} {
		if _, err := conn.Query(stmt); err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
	}
	res, err = conn.Query("EXPLAIN MATCH (a:Person), (b:City) RETURN a, b")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	for _, n := range res.Notifications {
		fmt.Println("notification:", n.Code, n.Severity, n.Category)
		// Severity and Category are closed, typed vocabularies.
		if n.Severity != relate.SeverityInformation || n.Category != relate.CategoryPerformance {
			fmt.Fprintln(os.Stderr, "unexpected severity/category:", n.Severity, n.Category)
			os.Exit(1)
		}
		fmt.Println("  remedy:", n.Remedy)
		if n.OperatorID != nil {
			fmt.Println("  operator:", *n.OperatorID)
		}
	}

	// A plain run does no notification work: the slice is present and empty.
	res, err = conn.Query("RETURN 1 AS n")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	fmt.Println("RETURN 1            Namespace:", res.Namespace)
	fmt.Println("RETURN 1            notifications:", len(res.Notifications))
}
