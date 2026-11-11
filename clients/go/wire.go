package relate

import (
	"encoding/json"
	"fmt"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf16"
	"unicode/utf8"
)

// The bounds on untrusted wire text echoed into an error message: a value
// (a closed-vocabulary string) and a payload (a whole JSON document). The
// Java and Swift clients use the same two bounds.
const (
	wireValueMax   = 64
	wirePayloadMax = 256
)

// quotedWire is the display form of an offending wire value: the sanitized
// value (control and other non-printable characters escaped, the value's
// own quote and backslash escaped, bounded to wireValueMax) wrapped in
// double quotes. It is the one place a refusal quotes an untrusted value,
// so a hostile native library cannot inject log lines through an error
// message (CWE-117).
func quotedWire(s string) string {
	return `"` + escapeWire(s, wireValueMax, true) + `"`
}

// safeWire is the display form of an echoed payload: only non-printable
// characters are escaped (a document's structural quotes and backslashes
// stay legible) and the result is bounded to wirePayloadMax.
func safeWire(s string) string {
	return escapeWire(s, wirePayloadMax, false)
}

// escapeWire escapes every non-printable code point, and every byte that is
// not valid UTF-8, as \uXXXX (a supplementary code point as its UTF-16
// surrogate pair; an invalid byte as \u00XX); when inQuotes it escapes the
// value's own quote and backslash too. The escaped body is bounded to max
// characters, and a ...(truncated) marker is appended when the input is cut.
func escapeWire(s string, max int, inQuotes bool) string {
	var out strings.Builder
	i := 0
	for i < len(s) {
		r, size := utf8.DecodeRuneInString(s[i:])
		invalid := r == utf8.RuneError && size <= 1 // a byte that is not UTF-8
		piece := string(r)
		switch {
		case invalid:
			piece = hex4(rune(s[i]))
		case !unicode.IsPrint(r) || (inQuotes && (r == '\\' || r == '"')):
			piece = escapedRune(r)
		}
		if out.Len()+len(piece) > max {
			break
		}
		out.WriteString(piece)
		i += size
	}
	if i < len(s) {
		out.WriteString("...(truncated)")
	}
	return out.String()
}

// escapedRune renders one rune as its \uXXXX escape(s). A rune is printable
// exactly when unicode.IsPrint says so — control characters, the line and
// paragraph separators, and format, unassigned and non-ASCII space code
// points are not (the strictest reading of the Java client's policy).
func escapedRune(r rune) string {
	if r >= 0x10000 {
		r1, r2 := utf16.EncodeRune(r)
		return hex4(r1) + hex4(r2)
	}
	return hex4(r)
}

func hex4(r rune) string {
	h := strconv.FormatInt(int64(r), 16)
	return `\u` + strings.Repeat("0", 4-len(h)) + h
}

// vocabulary is a closed wire vocabulary: the noun a refusal names and the
// constants a value may take. Each vocabulary is declared once, beside its
// type, so no call site spells the noun as a free string.
type vocabulary[E ~string] struct {
	what  string
	known []E
}

// decodeClosed is the one closed-vocabulary check: the constant of v whose
// value is raw, or an ABI contract violation naming v's noun and the
// sanitized offending value. Query type, namespace, error stage, severity
// and category all refuse through it.
func decodeClosed[E ~string](v vocabulary[E], raw string) (E, error) {
	for _, k := range v.known {
		if string(k) == raw {
			return k, nil
		}
	}
	return "", fmt.Errorf("ABI contract violated: %s %s", v.what, quotedWire(raw))
}

// missingKey is the one required-key check for a JSON object: the first of
// keys that is absent or null in data (null is absent: a zero value must
// never stand in for it), as an ABI contract violation naming subject and
// the key. A payload that is not an object is a violation too.
func missingKey(subject string, data []byte, keys ...string) error {
	var present map[string]*json.RawMessage
	if err := json.Unmarshal(data, &present); err != nil {
		return fmt.Errorf("ABI contract violated: decoding %s: %w", subject, err)
	}
	if present == nil {
		return fmt.Errorf("ABI contract violated: %s is JSON null, expected an object: %s", subject, safeWire(string(data)))
	}
	for _, k := range keys {
		if value, ok := present[k]; !ok || value == nil {
			return fmt.Errorf("ABI contract violated: %s is missing %q", subject, k)
		}
	}
	return nil
}
