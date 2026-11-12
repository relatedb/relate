package relate

import (
	"reflect"
	"testing"
)

// errorKeys is derived from Error's tags, so the required-key list and the
// struct cannot drift.
func TestErrorKeysDerivedFromTags(t *testing.T) {
	want := []string{"code", "gql_status", "stage", "retryable", "message"}
	if !reflect.DeepEqual(errorKeys, want) {
		t.Fatalf("errorKeys = %v, want %v", errorKeys, want)
	}
}
