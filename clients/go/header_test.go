package relate

import (
	"bytes"
	"errors"
	"io/fs"
	"os"
	"testing"
)

// The module ships its own copy of the C header (ADR-0592) because a
// module zip holds only this directory; the copy must stay byte-equal to
// the canonical one so the cgo declarations never drift from the ABI.
func TestVendoredHeaderMatchesFFI(t *testing.T) {
	canonical, err := os.ReadFile("../../surfaces/relate-ffi/include/relate.h")
	if errors.Is(err, fs.ErrNotExist) {
		t.Skip("canonical header not in this tree (module consumed outside the repo)")
	}
	if err != nil {
		t.Fatalf("reading the canonical header: %v", err)
	}
	vendored, err := os.ReadFile("include/relate.h")
	if err != nil {
		t.Fatalf("vendored header missing: %v", err)
	}
	if !bytes.Equal(canonical, vendored) {
		t.Fatal("clients/go/include/relate.h drifted from surfaces/relate-ffi/include/relate.h; " +
			"run: cp surfaces/relate-ffi/include/relate.h clients/go/include/relate.h")
	}
}
