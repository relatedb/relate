package relate_test

import (
	"errors"
	"math"
	"strings"
	"testing"

	"github.com/relatedb/relate/clients/go"
)

// The params tests, one topic file mirroring params.go: scalar and struct
// binding, refusals, and the typed putters (REL8-929).

// Parameters bind through the typed builder: every scalar kind, nested lists
// and maps, json-tagged structs, and a NUL-carrying string round-trip whole.
func TestParamsRoundTrip(t *testing.T) {
	conn := openConn(t)
	type pt struct {
		X    int64  `json:"x"`
		Skip string `json:"-"`
		Opt  string `json:"opt,omitempty"`
	}
	params := map[string]any{
		"i":  int64(9007199254740993),
		"f":  2.5,
		"s":  "a\x00b",
		"b":  true,
		"n":  nil,
		"l":  []any{int64(1), "two", []int{3}},
		"m":  map[string]any{"k": map[string]any{"deep": int64(7)}},
		"st": pt{X: 1, Skip: "gone"},
		"by": []byte{1, 2},
	}
	res, err := conn.QueryParams(
		"RETURN $i AS i, $f AS f, $s AS s, $b AS b, $n AS n, $l AS l, $m AS m, $st AS st, $by AS by",
		params)
	if err != nil {
		t.Fatal(err)
	}
	requireCells(t, "bound parameters", res, []any{
		int64(9007199254740993), 2.5, "a\x00b", true, nil,
		[]any{int64(1), "two", []any{int64(3)}},
		map[string]any{"k": map[string]any{"deep": int64(7)}},
		map[string]any{"x": int64(1)},
		[]any{int64(1), int64(2)},
	})
}

// A parameter the builder cannot bind is refused before the statement runs,
// with the Go value named.
func TestParamsRefusals(t *testing.T) {
	conn := openConn(t)
	requireParamRefusal(t, conn, "chan parameter", "RETURN $c AS c",
		map[string]any{"c": make(chan int)}, "unsupported parameter type")
	requireParamRefusal(t, conn, "oversized unsigned parameter", "RETURN $u AS u",
		map[string]any{"u": uint64(1) << 63}, "exceeds the INT range")
	// The struct refusal boundary is the type itself, not the filtered
	// field list: no exported fields refuses, while a struct whose every
	// field the json rules skip binds as an empty MAP — the {} encoding/json
	// produces for it.
	type onlyUnexported struct{ x int }
	requireParamRefusal(t, conn, "unexported-only struct", "RETURN $s AS s",
		map[string]any{"s": onlyUnexported{}}, "no exported fields")
	type allSkipped struct {
		A string `json:"-"`
		B int    `json:"-"`
	}
	res, err := conn.QueryParams("RETURN size(keys($s)) AS n", map[string]any{"s": allSkipped{}})
	if err != nil {
		t.Fatalf("all-skipped struct must bind an empty map: %v", err)
	}
	got, ok := res.Rows[0][0].(int64)
	if !ok || got != 0 {
		t.Fatalf("all-skipped struct binds an empty map: got %#v, want 0 keys", res.Rows[0][0])
	}
	// A name holding an interior NUL is refused — the C builder takes
	// names as NUL-terminated strings, so binding would silently truncate.
	requireParamRefusal(t, conn, "interior-NUL parameter name", "RETURN $v AS v",
		map[string]any{"a\x00b": 1}, "interior NUL")
}

// A cyclic value refuses at the depth bound instead of overflowing the
// stack: every cyclic shape — a map holding itself, a self-referential
// struct pointer, a slice containing itself, an interface pointing at
// itself (the unwrap path with no container on it) — reaches the same
// typed refusal (the JS client's cycle-detection counterpart).
func TestParamsCyclicRefusal(t *testing.T) {
	conn := openConn(t)
	type selfRef struct {
		Next *selfRef `json:"next"`
	}
	sr := &selfRef{}
	sr.Next = sr
	var selfIface any
	selfIface = &selfIface // an interface holding a pointer to itself
	cyclic := map[string]any{}
	cyclic["self"] = cyclic
	loop := make([]any, 1)
	loop[0] = loop
	for _, c := range []struct {
		what string
		v    any
	}{
		{"cyclic map", cyclic},
		{"self-referential struct", sr},
		{"slice containing itself", loop},
		{"interface pointing at itself", selfIface},
	} {
		t.Run(c.what, func(t *testing.T) {
			requireParamRefusal(t, conn, c.what, "RETURN $v AS v",
				map[string]any{"v": c.v}, "nests deeper than")
		})
	}
}

// A nil map is null whatever its key type — the null policy precedes the
// shape refusal, so a nil of an unbindable key kind is the same absent
// value, not a refusal that depends on guard order.
func TestParamsNilMapOfAnyKeyTypeIsNull(t *testing.T) {
	conn := openConn(t)
	res, err := conn.QueryParams("RETURN $m AS m", map[string]any{"m": map[int]int(nil)})
	if err != nil {
		t.Fatal(err)
	}
	requireCells(t, "nil map of int keys", res, []any{nil})
}

// The struct binding rules are this client's own where they part from
// encoding/json, pinned here: omitempty drops a field at reflect's zero
// value — IsZero, which drops a zero-valued struct field that encoding/json
// would keep — and an embedded struct binds as a nested MAP under its field
// name, where encoding/json flattens untagged embedded fields.
func TestStructBindingRules(t *testing.T) {
	conn := openConn(t)
	type Inner struct {
		B int64 `json:"b"`
	}
	type optStruct struct {
		Opt Inner `json:"opt,omitempty"`
	}
	res, err := conn.QueryParams("RETURN keys($s) AS ks", map[string]any{"s": optStruct{Opt: Inner{}}})
	if err != nil {
		t.Fatal(err)
	}
	if ks, ok := res.Rows[0][0].([]any); !ok || len(ks) != 0 {
		t.Fatalf("a zero struct field with omitempty is dropped: got %#v", res.Rows[0][0])
	}
	type embedded struct {
		Inner
	}
	res, err = conn.QueryParams("RETURN keys($s) AS ks, $s.Inner.b AS b", map[string]any{"s": embedded{Inner{B: 2}}})
	if err != nil {
		t.Fatal(err)
	}
	row := res.Rows[0]
	if ks, ok := row[0].([]any); !ok || len(ks) != 1 || ks[0] != "Inner" {
		t.Fatalf("an embedded struct binds nested under its name: got %#v", row[0])
	}
	if b, ok := row[1].(int64); !ok || b != 2 {
		t.Fatalf("the nested embedded field reads through: got %#v", row[1])
	}
}

// The client's own value types bind through the typed putters (REL8-929):
// a walker value passed back as a parameter round-trips — every typed
// kind, the offset-only and named-zone datetimes, the 2D and 3D points,
// and both vector families. The echoed cells must DeepEqual the cells they
// came from: binding is the walker's exact reverse.
func TestParamsTypedRoundTrip(t *testing.T) {
	conn := openConn(t)
	res, err := conn.Query(`RETURN date('2026-11-04') AS d, localtime('12:34:56.000000007') AS lt, ` +
		`time('09:30:00.000000006+01:00') AS t, localdatetime('2026-01-02T03:04:05.000000007') AS ldt, ` +
		`datetime('2026-11-04T09:30:00+01:00') AS dt, datetime('2026-11-04T09:30:00+01:00[Europe/Berlin]') AS zdt, ` +
		`duration('P1Y2M3DT4H5M6.000000007S') AS dur, point({x: 1.0, y: 2.0}) AS p2, ` +
		`point({x: 1.0, y: 2.0, z: 3.0}) AS p3, vector([1, 2, 3], 3, INTEGER) AS iv, ` +
		`vector([0.25, 1.5], 2, FLOAT32) AS fv`)
	if err != nil {
		t.Fatal(err)
	}
	orig := res.Rows[0]
	params := make(map[string]any, len(res.Columns))
	echoed := make([]string, len(res.Columns))
	for i, col := range res.Columns {
		params[col] = orig[i]
		echoed[i] = "$" + col + " AS " + col
	}
	// The echo statement derives from the same column list that built the
	// params, so adding a kind means touching one place, not two aligned
	// literals.
	echo, err := conn.QueryParams("RETURN "+strings.Join(echoed, ", ")+
		`, $zdt = datetime('2026-11-04T09:30:00+01:00[Europe/Berlin]') AS zdtOk`, params)
	if err != nil {
		t.Fatal(err)
	}
	requireCells(t, "typed round-trip", echo, orig)
	// The equality cell is found by column name, not position, so the
	// derived list and this assertion cannot disagree.
	okCol := -1
	for i, col := range echo.Columns {
		if col == "zdtOk" {
			okCol = i
			break
		}
	}
	if okCol < 0 {
		t.Fatalf("echo result carries no zdtOk column: %v", echo.Columns)
	}
	same, isBool := echo.Rows[0][okCol].(bool)
	if !isBool {
		t.Fatalf("named-zone datetime equality cell: got %T, want bool", echo.Rows[0][okCol])
	}
	if !same {
		t.Fatal("named-zone datetime must compare equal in Cypher")
	}
}

// A user-constructed DateTime with a zone binds by the zone: the engine
// resolves the offset at that wall clock (June in Stockholm is CEST,
// +02:00) and keeps the zone — the OffsetSeconds the struct carried is
// ignored, exactly as the putter contract says.
func TestParamsZoneDateTimeResolvesOffset(t *testing.T) {
	conn := openConn(t)
	zoned := relate.DateTime{Year: 2020, Month: 6, Day: 1, Hour: 10, Zone: "Europe/Stockholm"}
	res, err := conn.QueryParams(
		`RETURN $z AS z, $z = datetime('2020-06-01T10:00:00[Europe/Stockholm]') AS ok`,
		map[string]any{"z": zoned})
	if err != nil {
		t.Fatal(err)
	}
	row := res.Rows[0]
	z, isDT := row[0].(relate.DateTime)
	if !isDT || z.Zone != "Europe/Stockholm" || z.OffsetSeconds != 7200 || z.Hour != 10 {
		t.Fatalf("zone datetime: got %#v", row[0])
	}
	if ok, isBool := row[1].(bool); !isBool {
		t.Fatalf("zone datetime equality cell: got %T, want bool", row[1])
	} else if !ok {
		t.Fatal("zone datetime must compare equal in Cypher")
	}
}

// Typed values nest: the same putters run at any depth, inside typed
// slices, behind pointers, and under map keys and struct fields.
func TestParamsTypedNested(t *testing.T) {
	conn := openConn(t)
	d := relate.Date{Year: 2026, Month: 11, Day: 4}
	tm := relate.Time{Hour: 9, Minute: 30, Nanosecond: 6, OffsetSeconds: 3600}
	dur := relate.Duration{Months: 14}
	type holder struct {
		D relate.Date `json:"d"`
	}
	params := map[string]any{
		"l":   []any{[]relate.Date{d}, tm, []any{dur}},
		"m":   map[string]any{"t": tm, "inner": map[string]any{"d": d}},
		"ptr": &d,
		"st":  holder{D: d},
	}
	res, err := conn.QueryParams(`RETURN $l AS l, $m AS m, $ptr AS ptr, $st AS st`, params)
	if err != nil {
		t.Fatal(err)
	}
	requireCells(t, "nested typed", res, []any{
		[]any{[]any{d}, tm, []any{dur}},
		map[string]any{"t": tm, "inner": map[string]any{"d": d}},
		d,
		map[string]any{"d": d},
	})
}

// Non-finite floats bind (the JSON era could not send them) and decode
// back as themselves.
func TestParamsNonFiniteFloats(t *testing.T) {
	conn := openConn(t)
	for _, v := range []float64{math.NaN(), math.Inf(1), math.Inf(-1)} {
		res, err := conn.QueryParams("RETURN $v AS v", map[string]any{"v": v})
		if err != nil {
			t.Fatalf("%v: %v", v, err)
		}
		got, ok := res.Rows[0][0].(float64)
		if !ok {
			t.Fatalf("%v: got %#v, want a float64", v, res.Rows[0][0])
		}
		if math.IsNaN(v) && !math.IsNaN(got) {
			t.Fatalf("NaN: got %#v", got)
		}
		if !math.IsNaN(v) && got != v {
			t.Fatalf("%v: got %#v", v, got)
		}
	}
}

// The typed putters' refusals stay typed, never a silent map: a Vector
// carrying neither or both coordinate slices is refused in the client —
// the dtype alone cannot pick a putter for it — and what the engine
// validates (an srid that is no point() coordinate system) comes back as
// a typed *Error.
func TestParamsTypedRefusals(t *testing.T) {
	conn := openConn(t)
	for _, v := range []relate.Vector{
		{DType: "FLOAT"},
		{DType: "INTEGER", Ints: []int64{1}, Floats: []float64{1}},
	} {
		requireParamRefusal(t, conn, "malformed vector", "RETURN $v AS v",
			map[string]any{"v": v}, "exactly one coordinate slice")
	}
	// What the engine validates comes back as a typed *Error, never a
	// silent map.
	err := requireParamRefusal(t, conn, "bad-srid point", "RETURN $p AS p",
		map[string]any{"p": relate.Point{SRID: 1234, Dimension: 2, X: 1, Y: 2}},
		"is not a point() coordinate system")
	var e *relate.Error
	if !errors.As(err, &e) {
		t.Fatalf("bad-srid point: got %T %v, want *relate.Error", err, err)
	}
	// A Dimension outside {2, 3} — the walker produces only those — is
	// refused in the client, never silently coerced to 2D.
	requireParamRefusal(t, conn, "dimension-4 point", "RETURN $p AS p",
		map[string]any{"p": relate.Point{SRID: 7203, Dimension: 4, X: 1, Y: 2, Z: 3}},
		"neither 2 nor 3")
}

// Entities refuse as parameters — no putter carries them, and a reflected
// struct MAP of ElementID/ID/Labels/Properties is a shape no query expects.
// The properties map (or a path's elements) bind explicitly instead.
func TestParamsEntityRefusal(t *testing.T) {
	conn := openConn(t)
	if _, err := conn.Query("CREATE (:P {k: 'v'})"); err != nil {
		t.Fatal(err)
	}
	found, err := conn.Query("MATCH (n:P) RETURN n")
	if err != nil {
		t.Fatal(err)
	}
	node, ok := found.Rows[0][0].(relate.Node)
	if !ok {
		t.Fatalf("walker node: got %T", found.Rows[0][0])
	}
	requireParamRefusal(t, conn, "node parameter", "RETURN $n AS n",
		map[string]any{"n": node}, "does not bind as a parameter")
	rel := relate.Relationship{Type: "KNOWS", Properties: map[string]any{"since": int64(2020)}}
	requireParamRefusal(t, conn, "relationship parameter", "RETURN $r AS r",
		map[string]any{"r": rel}, "does not bind as a parameter")
	requireParamRefusal(t, conn, "path parameter", "RETURN $p AS p",
		map[string]any{"p": relate.Path{node, rel, node}}, "does not bind as a parameter")
	// The properties map itself binds — the documented alternative.
	echo, err := conn.QueryParams("RETURN $props AS props", map[string]any{"props": node.Properties})
	if err != nil {
		t.Fatalf("properties map must bind: %v", err)
	}
	if props, ok := echo.Rows[0][0].(map[string]any); !ok || props["k"] != "v" {
		t.Fatalf("properties map: got %#v", echo.Rows[0][0])
	}
}
