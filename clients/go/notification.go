package relate

import (
	"encoding/json"
	"fmt"
)

// Severity is a notification's severity (ADR-0363), a closed vocabulary:
// decoding refuses any value outside the two constants, the same refusal
// decodeError makes of an unknown stage.
type Severity string

const (
	// SeverityInformation is an informational notification.
	SeverityInformation Severity = "INFORMATION"
	// SeverityWarning is a warning notification.
	SeverityWarning Severity = "WARNING"
)

// Category is a notification's category (ADR-0363), a closed vocabulary:
// decoding refuses any value outside the five constants.
type Category string

const (
	// CategoryPerformance is a performance diagnostic.
	CategoryPerformance Category = "PERFORMANCE"
	// CategoryUnrecognized is an unrecognized name or feature.
	CategoryUnrecognized Category = "UNRECOGNIZED"
	// CategoryDeprecation is a deprecation diagnostic.
	CategoryDeprecation Category = "DEPRECATION"
	// CategorySchema is a schema diagnostic.
	CategorySchema Category = "SCHEMA"
	// CategoryHint is a hint diagnostic.
	CategoryHint Category = "HINT"
)

// Notification is one query notification (ADR-0363) in the engine-native
// shape. Operator-scoped kinds carry the raising operator's plan id as
// OperatorID; statement-scoped kinds leave it nil. Severity and Category
// are closed vocabularies: UnmarshalJSON refuses a missing, null or
// unknown value as an ABI contract violation naming the key and the
// (sanitized) offending value, in the statement's list and in a plan
// node's Warnings alike.
type Notification struct {
	Code        string   `json:"code"`
	GQLStatus   string   `json:"gql_status"`
	Severity    Severity `json:"severity"`
	Category    Category `json:"category"`
	Title       string   `json:"title"`
	Description string   `json:"description"`
	Remedy      string   `json:"remedy"`
	OperatorID  *int64   `json:"operator_id,omitempty"`
}

// UnmarshalJSON decodes one notification entry strictly. Pointer fields
// make a missing key detectable (json.Unmarshal would leave a plain string
// at its zero value), and the two closed keys are typed through the one
// vocabulary check below.
func (n *Notification) UnmarshalJSON(data []byte) error {
	type plain Notification // the method-free shape, so no recursion
	var shape struct {
		plain
		Severity *string `json:"severity"`
		Category *string `json:"category"`
	}
	if err := json.Unmarshal(data, &shape); err != nil {
		return err
	}
	severity, err := notificationKey("severity", shape.Severity, data, SeverityInformation, SeverityWarning)
	if err != nil {
		return err
	}
	category, err := notificationKey("category", shape.Category, data,
		CategoryPerformance, CategoryUnrecognized, CategoryDeprecation, CategorySchema, CategoryHint)
	if err != nil {
		return err
	}
	*n = Notification(shape.plain)
	n.Severity, n.Category = severity, category
	return nil
}

// notificationKey is one closed notification key's decode: the constant of
// known for *raw, or an ABI contract violation naming the key — and the
// sanitized offending value and entry payload — when the key is missing,
// null or outside the vocabulary.
func notificationKey[E ~string](key string, raw *string, payload []byte, known ...E) (E, error) {
	if raw == nil {
		return "", fmt.Errorf("ABI contract violated: notification entry is missing %q: %s",
			key, safeWire(string(payload)))
	}
	v, err := decodeClosed("notification entry carries an unknown "+key, *raw, known...)
	if err != nil {
		return "", fmt.Errorf("%w: %s", err, safeWire(string(payload)))
	}
	return v, nil
}

// decodeNotifications decodes the C ABI's engine-native notification array
// (ADR-0363). The accessor's contract is an always-present JSON array, so a
// null or non-array payload is an ABI contract violation and an error — never
// a silently nil slice, which json.Unmarshal would otherwise produce for
// "null".
func decodeNotifications(raw string) ([]Notification, error) {
	var notifications []Notification
	if err := json.Unmarshal([]byte(raw), &notifications); err != nil {
		return nil, fmt.Errorf("ABI contract violated: decoding notifications: %w", err)
	}
	if notifications == nil {
		return nil, fmt.Errorf("ABI contract violated: decoding notifications: expected a JSON array, got %s", safeWire(raw))
	}
	return notifications, nil
}
