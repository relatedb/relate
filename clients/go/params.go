package relate

/*
#cgo CFLAGS: -I${SRCDIR}/include
#cgo LDFLAGS: -L${SRCDIR}/../../target/debug -L${SRCDIR}/../../target/release -lrelate_ffi
#include <stdlib.h>
#include "relate.h"
*/
import "C"

import (
	"fmt"
	"math"
	"reflect"
	"sort"
	"strings"
	"unsafe"
)

// maxBindDepth bounds the reflection descent: a cyclic value (a map
// holding itself, a self-referential struct pointer, a slice containing
// itself) or a nesting deeper than this refuses with a typed error
// instead of overflowing the stack — the bound encoding/json's cycle
// detection stands for here, as a cap that also bounds runaway depth.
const maxBindDepth = 100

// bindParams writes a Go parameter map into a params builder root through the
// typed putters (ADR-0502 §4). No JSON is produced: each Go value maps to one
// putter, and a container maps to a child builder the root owns.
//
// The mapping: nil (and a nil pointer, interface, slice or map) → null; bool;
// every signed and unsigned integer → INT (an unsigned value above MaxInt64
// is refused); every float → FLOAT; string → STRING; a slice or array (a
// []byte included, which the engine reads as a list of INT) → LIST; a map
// with string keys → MAP; this client's own scalar value types (value.go)
// → their engine kinds through the typed putters, so a walker value
// round-trips back as a parameter; an entity (Node, Relationship, Path)
// refuses — pass its properties explicitly; any other struct → MAP keyed
// by its exported fields' json names (`json:"name"`, `json:"-"` skipped,
// `omitempty` drops zero values). Any other Go type is refused with a
// typed error.
//
// The root is bound as one entry list, through the same bindEntries loop
// every container uses.
func bindParams(root C.relate_params, params map[string]any) error {
	entries, err := mapEntries(reflect.ValueOf(params))
	if err != nil {
		return err
	}
	return bindEntries(root, entries, "parameter", maxBindDepth)
}

// bindReflect binds one value under name — one dispatch, stated once: the
// absent-value predicate, the indirection unwrap, this client's own value
// types through their typed putters, and the reflection kinds as the
// fall-through. A nil name appends to a LIST builder; the engine's
// container rule decides, not this function. depth is the remaining
// recursion budget; see maxBindDepth.
//
// Precondition: rv is a valid, interface-able Value — every call site
// hands one over (a map entry, a slice element, an exported struct field,
// the Elem of a non-nil pointer), so an invalid Value is a caller bug
// this function does not absorb as null.
func bindReflect(p C.relate_params, name *C.char, rv reflect.Value, depth int) error {
	if depth <= 0 {
		return fmt.Errorf("value nests deeper than %d levels", maxBindDepth)
	}
	// The absent-value policy, one predicate consulted once: a nil
	// interface, pointer, slice or map is the same absent value — null —
	// whatever its element or key type (a nil Path included).
	if isAbsent(rv) {
		return putNull(p, name)
	}
	// Indirections unwrap before any type or kind question, so a *Date
	// reaches the typed switch as a Date. Every descent into bindReflect
	// costs one level, this one included: an interface pointing at itself
	// (var i any; i = &i) unwraps forever otherwise, with no container on
	// the path to stop it.
	if indirect(rv.Kind()) {
		return bindReflect(p, name, rv.Elem(), depth-1)
	}
	// The client's own value types (value.go) bind through their typed
	// putters (ADR-0502 §4) — the same per-class table the JS client binds
	// by — never as the reflected MAP the reflection arms would build from
	// their exported fields. The scalar kinds bind; entities carry no
	// putter (the walker's reverse has no node/relationship/path row) and
	// refuse rather than bind a reflected MAP no query expects — their
	// properties bind explicitly. Everything else falls through to the
	// reflection kinds below.
	switch v := rv.Interface().(type) {
	case Date:
		return putDate(p, name, v)
	case LocalTime:
		return putLocalTime(p, name, v)
	case Time:
		return putTime(p, name, v)
	case LocalDateTime:
		return putLocalDateTime(p, name, v)
	case DateTime:
		return putDateTime(p, name, v)
	case Duration:
		return putDuration(p, name, v)
	case Point:
		return putPoint(p, name, v)
	case Vector:
		return putVector(p, name, v)
	case Node, Relationship, Path:
		return fmt.Errorf("entity type %s does not bind as a parameter; pass its Properties (a Path: its elements) explicitly", rv.Type())
	default:
		switch rv.Kind() {
		case reflect.Bool:
			return putBool(p, name, rv.Bool())
		case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
			return putInt(p, name, rv.Int())
		case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
			u := rv.Uint()
			if u > math.MaxInt64 {
				return fmt.Errorf("unsigned value %d exceeds the INT range", u)
			}
			return putInt(p, name, int64(u))
		case reflect.Float32, reflect.Float64:
			return putFloat(p, name, rv.Float())
		case reflect.String:
			return putString(p, name, rv.String())
		case reflect.Slice, reflect.Array:
			return bindList(p, name, rv, depth)
		case reflect.Map:
			return bindMap(p, name, rv, depth)
		case reflect.Struct:
			return bindStruct(p, name, rv, depth)
		default:
			return fmt.Errorf("unsupported parameter type %s", rv.Type())
		}
	}
}

// indirect reports whether k is a kind this binder unwraps — an interface
// or a pointer — the one home of the indirection vocabulary, beside
// isAbsent's nil-able table.
func indirect(k reflect.Kind) bool {
	return k == reflect.Interface || k == reflect.Pointer
}

// isAbsent reports whether rv is the absent value: a nil interface,
// pointer, slice or map. The one home of the null policy — the dispatch
// consults it once, and no arm restates it.
func isAbsent(rv reflect.Value) bool {
	switch rv.Kind() {
	case reflect.Interface, reflect.Pointer, reflect.Slice, reflect.Map:
		return rv.IsNil()
	}
	return false
}

// bindMap binds a map as a MAP under name; the entries bind under "key" —
// a non-string key kind refuses where its name would be minted, in
// mapEntries. (A nil map is null, whatever its key type: bindReflect's
// isAbsent answers that before this arm is reached.)
func bindMap(p C.relate_params, name *C.char, rv reflect.Value, depth int) error {
	entries, err := mapEntries(rv)
	if err != nil {
		return err
	}
	return bindContainer(p, name, "key", entries, depth)
}

// bindStruct binds a struct as a MAP of its exported fields under their
// json names. The refusal boundary is the type itself, not the filtered
// field list: a struct whose every field the binding rules skip
// (json:"-", or omitempty at a zero value) binds as an empty MAP.
func bindStruct(p C.relate_params, name *C.char, rv reflect.Value, depth int) error {
	if !hasExportedField(rv.Type()) {
		return fmt.Errorf("struct type %s has no exported fields to bind", rv.Type())
	}
	return bindContainer(p, name, "field", jsonFields(rv), depth)
}

// bindContainer mints an empty MAP under name and fills it with entries —
// the one container-then-entries prologue bindMap and bindStruct share.
func bindContainer(p C.relate_params, name *C.char, noun string, entries []entry, depth int) error {
	child, err := putMap(p, name)
	if err != nil {
		return err
	}
	return bindEntries(child, entries, noun, depth)
}

func bindList(p C.relate_params, name *C.char, rv reflect.Value, depth int) error {
	child, err := putList(p, name)
	if err != nil {
		return err
	}
	for i := 0; i < rv.Len(); i++ {
		if err := bindReflect(child, nil, rv.Index(i), depth-1); err != nil {
			return fmt.Errorf("item %d: %w", i, err)
		}
	}
	return nil
}

// bindEntries binds one container's entries under their names: the
// parameter root, a MAP's keys and a struct's fields are all the same
// (name, value) pairs bound through the same loop, differing only in the
// noun their wrapped errors name. Names mint through cString, the C
// boundary's one interior-NUL refusal, so map keys and json field names
// are covered alike.
func bindEntries(p C.relate_params, entries []entry, noun string, depth int) error {
	for _, e := range entries {
		// One label names the entry everywhere: the cString refusal
		// self-describes with it, and the value's own error is wrapped
		// with it inside the callback — so no message states it twice.
		label := fmt.Sprintf("%s %q", noun, e.name)
		if err := withCStringErr(label, e.name, func(name *C.char) error {
			if err := bindReflect(p, name, e.value, depth-1); err != nil {
				return fmt.Errorf("%s: %w", label, err)
			}
			return nil
		}); err != nil {
			return err
		}
	}
	return nil
}

// mapEntries returns a map's entries sorted by key, so binding is
// deterministic. The string-key invariant is enforced here, where the
// names are minted: a non-string key kind refuses — a k.String() over it
// would otherwise bind the "<T Value>" placeholder as the key's name.
// A non-map panics in MapKeys, a caller's type error (bindMap has already
// switched on Kind Map; bindParams is statically map[string]any).
func mapEntries(rv reflect.Value) ([]entry, error) {
	if rv.Type().Key().Kind() != reflect.String {
		return nil, fmt.Errorf("map key type %s is not a string", rv.Type().Key())
	}
	entries := make([]entry, 0, rv.Len())
	for _, k := range rv.MapKeys() {
		entries = append(entries, entry{name: k.String(), value: rv.MapIndex(k)})
	}
	sort.Slice(entries, func(i, j int) bool { return entries[i].name < entries[j].name })
	return entries, nil
}

// entry is one named entry of any bound container: a parameter of the
// root map, a key of a MAP value, or a struct field under its json name.
type entry struct {
	name  string
	value reflect.Value
}

// hasExportedField reports whether a struct type has any exported field,
// before the json rules skip any of them — the struct refusal boundary
// consults the type, never the filtered field list.
func hasExportedField(t reflect.Type) bool {
	for i := 0; i < t.NumField(); i++ {
		if t.Field(i).IsExported() {
			return true
		}
	}
	return false
}

// jsonFields lists a struct's exported fields under their json names, in
// declaration order, applying this client's binding rules. They follow
// encoding/json for `json:"name"` and `json:"-"`, and depart from it in two
// pinned ways (TestStructBindingRules): `omitempty` drops a field at
// reflect's zero value — IsZero, broader than encoding/json, which never
// drops a struct-valued field and drops arrays only when empty — and an
// embedded struct field binds as a nested MAP under its own name, where
// encoding/json flattens untagged embedded fields into the parent.
func jsonFields(rv reflect.Value) []entry {
	t := rv.Type()
	var out []entry
	for i := 0; i < t.NumField(); i++ {
		sf := t.Field(i)
		if !sf.IsExported() {
			continue
		}
		// The skip stays on the raw tag, exactly where encoding/json puts
		// it: `json:"-,"` names a field "-" and is not a skip, so this
		// check must not ride on the parsed name.
		if sf.Tag.Get("json") == "-" {
			continue
		}
		name, opts := jsonTagName(sf)
		if name == "" {
			name = sf.Name
		}
		fv := rv.Field(i)
		if strings.Contains(opts, "omitempty") && fv.IsZero() {
			continue
		}
		out = append(out, entry{name: name, value: fv})
	}
	return out
}

// jsonTagName splits a struct field's json tag into its wire name and its
// comma-separated options — the one place the tag is split. Callers apply
// their own policy to the result: jsonFields falls back to the field name
// when the tag names none, and statsWireKeys refuses a field the tag does
// not name. jsonFields' own `-` skip stays on the raw tag beside this
// call, where encoding/json puts it: `json:"-,"` names a field "-" and is
// not a skip, so that check must not ride on the parsed name.
func jsonTagName(sf reflect.StructField) (name, opts string) {
	name, opts, _ = strings.Cut(sf.Tag.Get("json"), ",")
	return name, opts
}

func putNull(p C.relate_params, name *C.char) error {
	var errJSON *C.char
	st := C.relate_params_put_null(p, name, &errJSON)
	return statusErr(st, errJSON)
}

func putBool(p C.relate_params, name *C.char, v bool) error {
	var b C.uint8_t
	if v {
		b = 1
	}
	var errJSON *C.char
	st := C.relate_params_put_bool(p, name, b, &errJSON)
	return statusErr(st, errJSON)
}

func putInt(p C.relate_params, name *C.char, v int64) error {
	var errJSON *C.char
	st := C.relate_params_put_int(p, name, C.int64_t(v), &errJSON)
	return statusErr(st, errJSON)
}

func putFloat(p C.relate_params, name *C.char, v float64) error {
	var errJSON *C.char
	st := C.relate_params_put_float(p, name, C.double(v), &errJSON)
	return statusErr(st, errJSON)
}

// putString passes the bytes with their length, so an interior NUL crosses
// whole; the putter copies them, so the Go string need not outlive the call.
func putString(p C.relate_params, name *C.char, v string) error {
	cv := C.CString(v)
	defer C.free(unsafe.Pointer(cv))
	var errJSON *C.char
	st := C.relate_params_put_string(p, name, cv, C.size_t(len(v)), &errJSON)
	return statusErr(st, errJSON)
}

// The typed putters (ADR-0502 §4): one wrapper per value type in value.go,
// the walker's readers in reverse. Each passes the components whole; the
// engine — never this file — validates them (a bad date, a DST-gap wall
// clock, an srid/dimension pairing, a coordinate outside the dtype's width
// refuse through the putter's own status).

func putDate(p C.relate_params, name *C.char, v Date) error {
	var errJSON *C.char
	st := C.relate_params_put_date(p, name, C.int64_t(v.Year), C.int64_t(v.Month), C.int64_t(v.Day), &errJSON)
	return statusErr(st, errJSON)
}

func putLocalTime(p C.relate_params, name *C.char, v LocalTime) error {
	var errJSON *C.char
	st := C.relate_params_put_local_time(p, name,
		C.int64_t(v.Hour), C.int64_t(v.Minute), C.int64_t(v.Second), C.int64_t(v.Nanosecond), &errJSON)
	return statusErr(st, errJSON)
}

// putTime binds a TIME offset-only: the kind keeps no zone id (the walker
// reads none back), so the zone argument is always NULL.
func putTime(p C.relate_params, name *C.char, v Time) error {
	var errJSON *C.char
	st := C.relate_params_put_time(p, name,
		C.int64_t(v.Hour), C.int64_t(v.Minute), C.int64_t(v.Second), C.int64_t(v.Nanosecond),
		C.int64_t(v.OffsetSeconds), nil, &errJSON)
	return statusErr(st, errJSON)
}

func putLocalDateTime(p C.relate_params, name *C.char, v LocalDateTime) error {
	var errJSON *C.char
	st := C.relate_params_put_local_datetime(p, name,
		C.int64_t(v.Year), C.int64_t(v.Month), C.int64_t(v.Day),
		C.int64_t(v.Hour), C.int64_t(v.Minute), C.int64_t(v.Second), C.int64_t(v.Nanosecond), &errJSON)
	return statusErr(st, errJSON)
}

// putDateTime binds a DATETIME offset-only when Zone is "" — the walker's
// offset-only shape, a NULL zone — and by zone otherwise: the engine keeps
// the zone and resolves the offset at that wall clock, ignoring
// OffsetSeconds.
func putDateTime(p C.relate_params, name *C.char, v DateTime) error {
	if v.Zone == "" {
		return putDateTimeAt(p, name, v, nil)
	}
	return withCStringErr(fmt.Sprintf("datetime zone %q", v.Zone), v.Zone, func(zone *C.char) error {
		return putDateTimeAt(p, name, v, zone)
	})
}

// putDateTimeAt is putDateTime's one C call, over the zone argument its
// caller minted (NULL = offset-only).
func putDateTimeAt(p C.relate_params, name *C.char, v DateTime, zone *C.char) error {
	var errJSON *C.char
	st := C.relate_params_put_datetime(p, name,
		C.int64_t(v.Year), C.int64_t(v.Month), C.int64_t(v.Day),
		C.int64_t(v.Hour), C.int64_t(v.Minute), C.int64_t(v.Second), C.int64_t(v.Nanosecond),
		C.int64_t(v.OffsetSeconds), zone, &errJSON)
	return statusErr(st, errJSON)
}

func putDuration(p C.relate_params, name *C.char, v Duration) error {
	var errJSON *C.char
	st := C.relate_params_put_duration(p, name,
		C.int64_t(v.Months), C.int64_t(v.Days), C.int64_t(v.Seconds), C.int64_t(v.Nanos), &errJSON)
	return statusErr(st, errJSON)
}

// putPoint binds a POINT: z crosses as a pointer only when Dimension is 3,
// NULL for the 2D kinds — the pairing the engine validates against the
// srid. A Dimension outside {2, 3} is refused here: the walker only ever
// produces those two, so anything else is a malformed value that must not
// silently coerce to 2D.
func putPoint(p C.relate_params, name *C.char, v Point) error {
	var z *C.double
	switch v.Dimension {
	case 2: // z stays NULL — the 2D kinds
	case 3:
		z = (*C.double)(unsafe.Pointer(&v.Z))
	default:
		return fmt.Errorf("point dimension %d is neither 2 nor 3", v.Dimension)
	}
	var errJSON *C.char
	st := C.relate_params_put_point(p, name, C.int32_t(v.SRID),
		C.double(v.X), C.double(v.Y), z, &errJSON)
	return statusErr(st, errJSON)
}

// putVector binds a VECTOR through the coordinate family its slices name:
// Ints for the INTEGER* dtypes, Floats for the FLOAT* ones — exactly the
// split the walker produced them by. A Vector carrying neither or both is
// malformed and refused here, before the C boundary: the dtype alone cannot
// pick a putter for it.
func putVector(p C.relate_params, name *C.char, v Vector) error {
	return withCStringErr(fmt.Sprintf("vector dtype %q", v.DType), v.DType, func(dtype *C.char) error {
		var errJSON *C.char
		var st C.relate_status
		switch {
		case v.Ints != nil && v.Floats == nil:
			st = C.relate_params_put_vector_int(p, name, dtype,
				(*C.int64_t)(coordsPtr(v.Ints)), C.size_t(len(v.Ints)), &errJSON)
		case v.Floats != nil && v.Ints == nil:
			st = C.relate_params_put_vector_float(p, name, dtype,
				(*C.double)(coordsPtr(v.Floats)), C.size_t(len(v.Floats)), &errJSON)
		default:
			return fmt.Errorf("vector dtype %s must carry exactly one coordinate slice, Ints or Floats", v.DType)
		}
		return statusErr(st, errJSON)
	})
}

// coordsPtr is the pointer to a coordinate slice's first element, NULL
// when the slice is empty — the one home of the empty-vector rule, shared
// by both putter arms so they differ only in the C putter they call.
func coordsPtr[T int64 | float64](xs []T) unsafe.Pointer {
	if len(xs) == 0 {
		return nil
	}
	return unsafe.Pointer(&xs[0])
}

// putMap inserts an empty MAP under name and returns its child builder,
// which the root owns until it is freed.
func putMap(p C.relate_params, name *C.char) (C.relate_params, error) {
	var child C.relate_params
	var errJSON *C.char
	st := C.relate_params_put_map(p, name, &child, &errJSON)
	if err := statusErr(st, errJSON); err != nil {
		return nil, err
	}
	return child, nil
}

// putList inserts an empty LIST under name and returns its child builder,
// which the root owns until it is freed.
func putList(p C.relate_params, name *C.char) (C.relate_params, error) {
	var child C.relate_params
	var errJSON *C.char
	st := C.relate_params_put_list(p, name, &child, &errJSON)
	if err := statusErr(st, errJSON); err != nil {
		return nil, err
	}
	return child, nil
}
