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
	"unsafe"
)

// decodeCell walks the root handle of cell (row, col) into a Go value
// (ADR-0502 §1). Every handle the walk mints is owned by the result, so the
// caller decodes the whole result before destroying it.
func decodeCell(res C.relate_query_result, row, col int) (any, error) {
	v := C.relate_value_at(res, C.longlong(row), C.longlong(col))
	if v == nil {
		return nil, fmt.Errorf("ABI contract violated: no handle for cell (%d,%d)", row, col)
	}
	return decodeValue(v)
}

// decodeValue decodes one value handle by its kind: one switch, recursion for
// the containers, and a defined error for a kind this client does not know.
func decodeValue(v C.relate_value) (any, error) {
	switch kind := C.relate_value_kind(v); kind {
	case C.RELATE_V_NULL:
		return nil, nil
	case C.RELATE_V_BOOL:
		var b C.uint8_t
		if err := refused(C.relate_value_bool(v, &b), "bool"); err != nil {
			return nil, err
		}
		return b != 0, nil
	case C.RELATE_V_INT:
		var n C.int64_t
		if err := refused(C.relate_value_int(v, &n), "int"); err != nil {
			return nil, err
		}
		return int64(n), nil
	case C.RELATE_V_FLOAT:
		var f C.double
		if err := refused(C.relate_value_float(v, &f), "float"); err != nil {
			return nil, err
		}
		return float64(f), nil
	case C.RELATE_V_STRING:
		var n C.size_t
		s, err := copyString(C.relate_value_string(v, &n), n, "string payload")
		if err != nil {
			return nil, err
		}
		return s, nil
	case C.RELATE_V_NODE:
		var id C.int64_t
		if err := refused(C.relate_value_id(v, &id), "id"); err != nil {
			return nil, err
		}
		labels, err := nodeLabels(v)
		if err != nil {
			return nil, err
		}
		props, err := properties(v)
		if err != nil {
			return nil, err
		}
		return Node{ElementID: C.GoString(C.relate_value_element_id(v)), ID: int64(id), Labels: labels, Properties: props}, nil
	case C.RELATE_V_RELATIONSHIP:
		var id, startID, endID C.int64_t
		if err := refused(C.relate_value_id(v, &id), "id"); err != nil {
			return nil, err
		}
		if err := refused(C.relate_value_start_id(v, &startID), "start id"); err != nil {
			return nil, err
		}
		if err := refused(C.relate_value_end_id(v, &endID), "end id"); err != nil {
			return nil, err
		}
		var n C.size_t
		typ, err := copyString(C.relate_value_string(v, &n), n, "relationship type")
		if err != nil {
			return nil, err
		}
		props, err := properties(v)
		if err != nil {
			return nil, err
		}
		return Relationship{
			ElementID:      C.GoString(C.relate_value_element_id(v)),
			StartElementID: C.GoString(C.relate_value_start_element_id(v)),
			EndElementID:   C.GoString(C.relate_value_end_element_id(v)),
			ID:             int64(id),
			StartID:        int64(startID),
			EndID:          int64(endID),
			Type:           typ,
			Properties:     props,
		}, nil
	case C.RELATE_V_PATH:
		items, err := listItems(v)
		if err != nil {
			return nil, err
		}
		return Path(items), nil
	case C.RELATE_V_LIST:
		return listItems(v)
	case C.RELATE_V_MAP:
		return properties(v)
	case C.RELATE_V_POINT:
		var srid, dim C.int32_t
		var x, y, z C.double
		if err := refused(C.relate_value_point(v, &srid, &dim, &x, &y, &z), "point"); err != nil {
			return nil, err
		}
		return Point{SRID: int32(srid), Dimension: int32(dim), X: float64(x), Y: float64(y), Z: float64(z)}, nil
	case C.RELATE_V_DATE:
		return dateParts(v)
	case C.RELATE_V_TIME:
		clock, err := clockParts(v)
		if err != nil {
			return nil, err
		}
		offset, err := offsetOf(v)
		if err != nil {
			return nil, err
		}
		return Time{Hour: clock.Hour, Minute: clock.Minute, Second: clock.Second, Nanosecond: clock.Nanosecond, OffsetSeconds: offset}, nil
	case C.RELATE_V_LOCAL_TIME:
		return clockParts(v)
	case C.RELATE_V_DATETIME:
		date, err := dateParts(v)
		if err != nil {
			return nil, err
		}
		clock, err := clockParts(v)
		if err != nil {
			return nil, err
		}
		offset, err := offsetOf(v)
		if err != nil {
			return nil, err
		}
		return DateTime{
			Year: date.Year, Month: date.Month, Day: date.Day,
			Hour: clock.Hour, Minute: clock.Minute, Second: clock.Second, Nanosecond: clock.Nanosecond,
			OffsetSeconds: offset,
			Zone:          C.GoString(C.relate_value_zone(v)),
		}, nil
	case C.RELATE_V_LOCAL_DATETIME:
		date, err := dateParts(v)
		if err != nil {
			return nil, err
		}
		clock, err := clockParts(v)
		if err != nil {
			return nil, err
		}
		return LocalDateTime{
			Year: date.Year, Month: date.Month, Day: date.Day,
			Hour: clock.Hour, Minute: clock.Minute, Second: clock.Second, Nanosecond: clock.Nanosecond,
		}, nil
	case C.RELATE_V_DURATION:
		var months, days, seconds, nanos C.int64_t
		if err := refused(C.relate_value_duration(v, &months, &days, &seconds, &nanos), "duration"); err != nil {
			return nil, err
		}
		return Duration{Months: int64(months), Days: int64(days), Seconds: int64(seconds), Nanos: int64(nanos)}, nil
	case C.RELATE_V_VECTOR:
		return vectorOf(v)
	default:
		return nil, fmt.Errorf("unsupported value kind %d", int(kind))
	}
}

// refused turns a walker refusal into an error. Each call site has already
// switched on the kind, so a refusal is a broken ABI contract, never a value.
func refused(st C.relate_status, what string) error {
	if st != C.RELATE_OK {
		return fmt.Errorf("ABI contract violated: walker refused %s", what)
	}
	return nil
}

// copyString copies a result-owned string of n bytes. An interior NUL crosses
// whole because the length is authoritative.
func copyString(p *C.char, n C.size_t, what string) (string, error) {
	if p == nil {
		return "", fmt.Errorf("ABI contract violated: walker returned no %s", what)
	}
	if n == 0 {
		return "", nil
	}
	return string(unsafe.Slice((*byte)(unsafe.Pointer(p)), int(n))), nil
}

// listItems decodes every item of a LIST or PATH handle.
func listItems(v C.relate_value) ([]any, error) {
	n := int(C.relate_value_list_len(v))
	if n < 0 {
		return nil, fmt.Errorf("ABI contract violated: walker refused list length")
	}
	out := make([]any, 0, n)
	for i := 0; i < n; i++ {
		child := C.relate_value_list_get(v, C.longlong(i))
		if child == nil {
			return nil, fmt.Errorf("ABI contract violated: list item %d has no handle", i)
		}
		item, err := decodeValue(child)
		if err != nil {
			return nil, err
		}
		out = append(out, item)
	}
	return out, nil
}

// nodeLabels reads a NODE's labels, which the walker serves as a list of STRING.
func nodeLabels(v C.relate_value) ([]string, error) {
	items, err := listItems(v)
	if err != nil {
		return nil, err
	}
	labels := make([]string, len(items))
	for i, item := range items {
		label, ok := item.(string)
		if !ok {
			return nil, fmt.Errorf("ABI contract violated: node label %d is not a string", i)
		}
		labels[i] = label
	}
	return labels, nil
}

// properties reads a MAP's entries, or a NODE's and RELATIONSHIP's properties,
// in the engine's order.
func properties(v C.relate_value) (map[string]any, error) {
	n := int(C.relate_value_map_len(v))
	if n < 0 {
		return nil, fmt.Errorf("ABI contract violated: walker refused map length")
	}
	out := make(map[string]any, n)
	for i := 0; i < n; i++ {
		var klen C.size_t
		key, err := copyString(C.relate_value_map_key(v, C.longlong(i), &klen), klen, "map key")
		if err != nil {
			return nil, err
		}
		// map_get takes a NUL-terminated key, so a key with an interior NUL
		// cannot be looked up through the walker — cString refuses it
		// rather than let a truncated name read a different key's value.
		ckey, err := cString(fmt.Sprintf("map key %q", key), key)
		if err != nil {
			return nil, err
		}
		child := C.relate_value_map_get(v, ckey)
		C.free(unsafe.Pointer(ckey))
		if child == nil {
			return nil, fmt.Errorf("ABI contract violated: walker lost map key %q", key)
		}
		val, err := decodeValue(child)
		if err != nil {
			return nil, err
		}
		out[key] = val
	}
	return out, nil
}

func dateParts(v C.relate_value) (Date, error) {
	var y, m, d C.int64_t
	if err := refused(C.relate_value_year(v, &y), "year"); err != nil {
		return Date{}, err
	}
	if err := refused(C.relate_value_month(v, &m), "month"); err != nil {
		return Date{}, err
	}
	if err := refused(C.relate_value_day(v, &d), "day"); err != nil {
		return Date{}, err
	}
	return Date{Year: int64(y), Month: int64(m), Day: int64(d)}, nil
}

func clockParts(v C.relate_value) (LocalTime, error) {
	var h, mi, s, ns C.int64_t
	if err := refused(C.relate_value_hour(v, &h), "hour"); err != nil {
		return LocalTime{}, err
	}
	if err := refused(C.relate_value_minute(v, &mi), "minute"); err != nil {
		return LocalTime{}, err
	}
	if err := refused(C.relate_value_second(v, &s), "second"); err != nil {
		return LocalTime{}, err
	}
	if err := refused(C.relate_value_nanosecond(v, &ns), "nanosecond"); err != nil {
		return LocalTime{}, err
	}
	return LocalTime{Hour: int64(h), Minute: int64(mi), Second: int64(s), Nanosecond: int64(ns)}, nil
}

func offsetOf(v C.relate_value) (int64, error) {
	var off C.int64_t
	if err := refused(C.relate_value_offset_seconds(v, &off), "offset_seconds"); err != nil {
		return 0, err
	}
	return int64(off), nil
}

// vectorOf reads a VECTOR. The dtype names are the engine's closed set of
// eight canonical spellings; they are matched exactly, never by prefix.
func vectorOf(v C.relate_value) (Vector, error) {
	dt := C.relate_value_vector_dtype(v)
	if dt == nil {
		return Vector{}, fmt.Errorf("ABI contract violated: walker returned no vector dtype")
	}
	dtype := C.GoString(dt)
	dim := int(C.relate_value_vector_dim(v))
	if dim < 0 {
		return Vector{}, fmt.Errorf("ABI contract violated: walker refused vector dimension")
	}
	switch dtype {
	case "INTEGER8", "INTEGER16", "INTEGER32", "INTEGER":
		ints := make([]int64, dim)
		for i := range ints {
			var x C.int64_t
			if err := refused(C.relate_value_vector_int(v, C.longlong(i), &x), "vector coordinate"); err != nil {
				return Vector{}, err
			}
			ints[i] = int64(x)
		}
		return Vector{DType: dtype, Ints: ints}, nil
	case "FLOAT16", "BFLOAT16", "FLOAT32", "FLOAT":
		floats := make([]float64, dim)
		for i := range floats {
			var x C.double
			if err := refused(C.relate_value_vector_float(v, C.longlong(i), &x), "vector coordinate"); err != nil {
				return Vector{}, err
			}
			floats[i] = float64(x)
		}
		return Vector{DType: dtype, Floats: floats}, nil
	default:
		return Vector{}, fmt.Errorf("unsupported vector dtype %q", dtype)
	}
}
