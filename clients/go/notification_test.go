package relate_test

import (
	"strings"
	"testing"

	"github.com/relatedb/relate/clients/go"
)

const notificationTail = `"title":"t","description":"d","remedy":"r"`

func notificationEntry(severity, category string) string {
	return `[{"code":"c","gql_status":"03N90","severity":` + severity +
		`,"category":` + category + `,` + notificationTail + `}]`
}

// Severity and Category are closed vocabularies (ADR-0363): every listed
// value decodes typed, and anything else is an ABI contract violation
// naming the key and the offending value.
func TestNotificationVocabulariesAreClosed(t *testing.T) {
	for sev, wantSev := range map[string]relate.Severity{
		"INFORMATION": relate.SeverityInformation,
		"WARNING":     relate.SeverityWarning,
	} {
		for cat, wantCat := range map[string]relate.Category{
			"PERFORMANCE": relate.CategoryPerformance, "UNRECOGNIZED": relate.CategoryUnrecognized,
			"DEPRECATION": relate.CategoryDeprecation, "SCHEMA": relate.CategorySchema,
			"HINT": relate.CategoryHint,
		} {
			got, err := relate.DecodeNotificationsForTest(notificationEntry(`"`+sev+`"`, `"`+cat+`"`))
			if err != nil || len(got) != 1 || got[0].Severity != wantSev || got[0].Category != wantCat {
				t.Fatalf("decoding %s/%s: got (%#v, %v)", sev, cat, got, err)
			}
		}
	}
	for _, bad := range []string{"", "warning", "ERROR", "INFO"} {
		_, err := relate.DecodeNotificationsForTest(notificationEntry(`"`+bad+`"`, `"HINT"`))
		if err == nil || !strings.Contains(err.Error(), `unknown severity "`+bad+`"`) {
			t.Fatalf("severity %q: got %v, want an unknown-severity refusal", bad, err)
		}
		_, err = relate.DecodeNotificationsForTest(notificationEntry(`"WARNING"`, `"`+bad+`"`))
		if err == nil || !strings.Contains(err.Error(), `unknown category "`+bad+`"`) {
			t.Fatalf("category %q: got %v, want an unknown-category refusal", bad, err)
		}
	}
	// A missing or null key is refused too, never an empty string.
	for _, raw := range []string{
		`[{"code":"c","gql_status":"g","category":"HINT",` + notificationTail + `}]`,
		notificationEntry(`null`, `"HINT"`),
		notificationEntry(`7`, `"HINT"`),
		`[null]`,
	} {
		if got, err := relate.DecodeNotificationsForTest(raw); err == nil {
			t.Fatalf("decoding %s: got %#v, want an error", raw, got)
		}
	}
	requireMissingKey(t, relate.DecodeNotificationsForTest, "category",
		`[{"code":"c","gql_status":"g","severity":"WARNING",`+notificationTail+`}]`)
}

// A plan node's Warnings decode through the same strict Notification type.
func TestPlanWarningsRefuseUnknownSeverity(t *testing.T) {
	plan := `{"schema":"relate.plan/v1","profiled":false,"statement":null,"root":{"operator":"o",` +
		`"details":"","identifiers":[],"children":[],"warnings":` +
		notificationEntry(`"LOUD"`, `"HINT"`) + `}}`
	requireErrorMentions(t, relate.DecodePlanForTest, plan, `unknown severity "LOUD"`, "plan warning")
}

// Untrusted wire values echoed into an error message are sanitized
// (CWE-117): control and non-printable characters are escaped, quotes and
// backslashes inside a quoted value are escaped, and values and payloads are
// bounded (64 and 256).
func TestRefusalsSanitizeEchoedWireValues(t *testing.T) {
	hostile := "INFO\r\nFAKE LOG \"x\"\\ ‮\x00\xff"
	_, err := relate.DecodeNotificationsForTest(
		notificationEntry(`"`+jsonEscape(hostile)+`"`, `"HINT"`))
	if err == nil {
		t.Fatal("want an error")
	}
	assertSanitized(t, err.Error())
	if !strings.Contains(err.Error(), `\u000d\u000a`) || !strings.Contains(err.Error(), `\u2028`) ||
		!strings.Contains(err.Error(), `\u202e`) || !strings.Contains(err.Error(), `\u0000`) {
		t.Fatalf("control characters not escaped: %q", err)
	}

	long := strings.Repeat("a", 2000)
	_, err = relate.DecodeNotificationsForTest(notificationEntry(`"`+long+`"`, `"HINT"`))
	if err == nil || strings.Contains(err.Error(), strings.Repeat("a", 300)) ||
		!strings.Contains(err.Error(), "...(truncated)") {
		t.Fatalf("value not bounded: %v", err)
	}
	if len(err.Error()) > 64+256+200 {
		t.Fatalf("message not bounded: %d bytes", len(err.Error()))
	}

	for name, decode := range map[string]func(string) error{
		"query type": func(raw string) error { _, err := relate.DecodeQueryTypeForTest(raw); return err },
		"namespace":  func(raw string) error { _, err := relate.DecodeNamespaceForTest(raw); return err },
		"stage": func(raw string) error {
			_, err := relate.DecodeErrorForTest(`{"code":"c","gql_status":"g","stage":"` +
				jsonEscape(raw) + `","retryable":false,"message":"m"}`)
			return err
		},
	} {
		err := decode(hostile)
		if err == nil {
			t.Fatalf("%s: want an error", name)
		}
		assertSanitized(t, err.Error())
	}
	// An invalid UTF-8 byte (the C accessors hand back raw bytes) escapes.
	if _, err := relate.DecodeQueryTypeForTest("a\xff"); err == nil || !strings.Contains(err.Error(), `a\u00ff`) {
		t.Fatalf("invalid UTF-8 byte not escaped: %v", err)
	}
	// A non-array payload echo is bounded and escaped as well.
	_, err = relate.DecodeNotificationsForTest("null\n" + strings.Repeat("z", 600))
	if err == nil || len(err.Error()) > 256+200 {
		t.Fatalf("non-array echo not bounded: %v", err)
	}
}

func assertSanitized(t *testing.T, msg string) {
	t.Helper()
	for _, r := range msg {
		if r < 0x20 || r == 0x7f || r == 0x2028 || r == 0x202e {
			t.Fatalf("raw control character %U in %q", r, msg)
		}
	}
}

// jsonEscape escapes s as the body of a JSON string. Invalid UTF-8 bytes are
// emitted raw.
func jsonEscape(s string) string {
	var b strings.Builder
	for _, r := range s {
		switch {
		case r == '"' || r == '\\':
			b.WriteByte('\\')
			b.WriteRune(r)
		case r < 0x20:
			b.WriteString(`\u00`)
			b.WriteString(string("0123456789abcdef"[r>>4]))
			b.WriteString(string("0123456789abcdef"[r&0xf]))
		default:
			b.WriteRune(r)
		}
	}
	return b.String()
}
