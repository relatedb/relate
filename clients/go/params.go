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

// bindParams writes a Go parameter map into a params builder root through the
// typed putters (ADR-0502 §4). No JSON is produced: each Go value maps to one
// putter, and a container maps to a child builder the root owns.
//
// The mapping: nil (and a nil pointer, interface, slice or map) → null; bool;
// every signed and unsigned integer → INT (an unsigned value above MaxInt64
// is refused); every float → FLOAT; string → STRING; a slice or array (a
// []byte included, which the engine reads as a list of INT) → LIST; a map
// with string keys → MAP; a struct → MAP keyed by its exported fields' json
// names (`json:"name"`, `json:"-"` skipped, `omitempty` drops zero values).
// Any other Go type is refused with a typed error.
//
// The root is bound as one entry list, through the same bindEntries loop
// every container uses.
func bindParams(root C.relate_params, params map[string]any) error {
	return bindEntries(root, mapEntries(reflect.ValueOf(params)), "parameter")
}

// bindReflect binds one value under name. A nil name appends to a LIST
// builder; the engine's container rule decides, not this function.
func bindReflect(p C.relate_params, name *C.char, rv reflect.Value) error {
	if !rv.IsValid() {
		return putNull(p, name)
	}
	switch rv.Kind() {
	case reflect.Interface, reflect.Pointer:
		if rv.IsNil() {
			return putNull(p, name)
		}
		return bindReflect(p, name, rv.Elem())
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
	case reflect.Slice:
		if rv.IsNil() {
			return putNull(p, name)
		}
		return bindList(p, name, rv)
	case reflect.Array:
		return bindList(p, name, rv)
	case reflect.Map:
		if rv.Type().Key().Kind() != reflect.String {
			return fmt.Errorf("map key type %s is not a string", rv.Type().Key())
		}
		if rv.IsNil() {
			return putNull(p, name)
		}
		child, err := putContainer(p, name, false)
		if err != nil {
			return err
		}
		return bindEntries(child, mapEntries(rv), "key")
	case reflect.Struct:
		// The refusal boundary is the type itself, not the filtered field
		// list: a struct whose every field the binding rules skip
		// (json:"-", or omitempty at a zero value) binds as an empty MAP.
		if !hasExportedField(rv.Type()) {
			return fmt.Errorf("struct type %s has no exported fields to bind", rv.Type())
		}
		child, err := putContainer(p, name, false)
		if err != nil {
			return err
		}
		return bindEntries(child, jsonFields(rv), "field")
	default:
		return fmt.Errorf("unsupported parameter type %s", rv.Type())
	}
}

func bindList(p C.relate_params, name *C.char, rv reflect.Value) error {
	child, err := putContainer(p, name, true)
	if err != nil {
		return err
	}
	for i := 0; i < rv.Len(); i++ {
		if err := bindReflect(child, nil, rv.Index(i)); err != nil {
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
func bindEntries(p C.relate_params, entries []entry, noun string) error {
	for _, e := range entries {
		name, err := cString(fmt.Sprintf("%s %q", noun, e.name), e.name)
		if err != nil {
			return err
		}
		err = bindReflect(p, name, e.value)
		C.free(unsafe.Pointer(name))
		if err != nil {
			return fmt.Errorf("%s %q: %w", noun, e.name, err)
		}
	}
	return nil
}

// mapEntries returns a map's string-keyed entries sorted by key, so binding
// is deterministic. The call sites carry the precondition — bindReflect
// refuses a non-string key kind and bindParams is statically
// map[string]any. A non-map here panics in MapKeys; a non-string key would
// not panic but bind String's "<T Value>" placeholder names, which is why
// the key-kind guard lives at the call site, not here.
func mapEntries(rv reflect.Value) []entry {
	entries := make([]entry, 0, rv.Len())
	for _, k := range rv.MapKeys() {
		entries = append(entries, entry{name: k.String(), value: rv.MapIndex(k)})
	}
	sort.Slice(entries, func(i, j int) bool { return entries[i].name < entries[j].name })
	return entries
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

// putContainer inserts an empty LIST or MAP and returns its child builder,
// which the root owns until it is freed.
func putContainer(p C.relate_params, name *C.char, list bool) (C.relate_params, error) {
	var child C.relate_params
	var errJSON *C.char
	var st C.relate_status
	if list {
		st = C.relate_params_put_list(p, name, &child, &errJSON)
	} else {
		st = C.relate_params_put_map(p, name, &child, &errJSON)
	}
	if err := statusErr(st, errJSON); err != nil {
		return nil, err
	}
	return child, nil
}
