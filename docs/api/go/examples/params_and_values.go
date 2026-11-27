package main

import (
	"fmt"
	"os"

	"github.com/relatedb/relate/clients/go"
)

type person struct {
	Name string `json:"name"`
	Age  int64  `json:"age"`
	Note string `json:"note,omitempty"`
}

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

	// Parameters bind through the typed builder: an int stays an INT, a
	// struct becomes a MAP keyed by its json names, a slice becomes a LIST.
	params := map[string]any{
		"big":    int64(9007199254740993),
		"who":    person{Name: "Ada", Age: 36},
		"scores": []int64{3, 5},
	}
	res, err := conn.QueryParams(
		"RETURN $big AS big, $who AS who, $scores AS scores, date('2020-06-01') AS d",
		params)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}

	// Cells decode to Go values: int64 is lossless, a MAP is map[string]any,
	// a LIST is []any, and temporals are their components.
	big := res.Rows[0][0].(int64)
	who := res.Rows[0][1].(map[string]any)
	scores := res.Rows[0][2].([]any)
	day := res.Rows[0][3].(relate.Date)
	fmt.Println("big:", big)
	fmt.Println("who:", who["name"], who["age"])
	fmt.Println("scores:", scores)
	fmt.Printf("date: %04d-%02d-%02d\n", day.Year, day.Month, day.Day)

	// A typed value read from a result binds back as its own kind through
	// the typed putters — the walker's reverse, never a reflected map.
	echo, err := conn.QueryParams(
		"RETURN $d AS d, $d = date('2020-06-01') AS same",
		map[string]any{"d": day})
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	back := echo.Rows[0][0].(relate.Date)
	same := echo.Rows[0][1].(bool)
	fmt.Println("date round-trips:", back == day, same)
}
