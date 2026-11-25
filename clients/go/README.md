# Relate Go client

A Go client over Relate's typed C ABI v2 (ADR-0502) via cgo.
API contract: [`docs/api/go/spec.md`](../../docs/api/go/spec.md).

## Using the module from another project

```sh
go get github.com/relatedb/relate/clients/go@vX.Y.Z
tar xzf relate-ffi-<triple>.tar.gz -C /opt/relate-ffi   # from the release assets
CGO_LDFLAGS="-L/opt/relate-ffi" go build ./...
```

The module carries its own header (`include/relate.h`, kept byte-equal to
`surfaces/relate-ffi/include/relate.h` by a test); the native library comes
from the release archive (ADR-0592). The sections below are for working in
this repository.

## Prerequisites

- Go 1.22+
- The Rust toolchain (1.99) and a C linker (clang/gcc; mingw on Windows)

## Build the native library

cgo links the library directly — every `.go` file carries:

```
#cgo LDFLAGS: -L${SRCDIR}/../../target/debug -L${SRCDIR}/../../target/release -lrelate_ffi
```

Note the order: **`target/debug` is searched first.** Build the profile
you will run against (and rebuild it after engine changes — a stale
pre-v2 dylib in `target/debug` shadows a fresh `target/release` one and
fails the link with missing `relate_value_*` symbols):

```sh
cargo build -p relate-ffi            # debug — what `go test` picks up first
cargo build -p relate-ffi --release  # release — for benchmarks
```

`make build-ffi` (repo root) builds the release library; `make test-go`
builds the profile this client resolves (debug — see the order above) and
runs the client's tests plus every documented example. `make build` builds
only the CLI.

## Run the tests

```sh
cd clients/go
go test ./...
```

No environment variables needed — cgo resolves the library at link time
from the repo's `target/` directories.

## Linking a staged / packaged native

To test or ship against a library outside the repo's `target/`:

```sh
CGO_LDFLAGS="-L<dir-holding-the-lib> -lrelate_ffi" go test ./...
```

On Windows, cgo links with mingw ld while the Rust cdylib ships an MSVC
COFF import library — generate a mingw import library first (what CI does):

```sh
gendef relate_ffi.dll
dlltool --dllname relate_ffi.dll --def relate_ffi.def --output-lib librelate_ffi.dll.a
```

## Use the client from a standalone Go project

The `#cgo LDFLAGS` directives above resolve inside this repository's
checkout. A Go project outside the tree needs the module and the library
staged. At link time the module already carries the header and
`-lrelate_ffi`, so exactly **one** variable is yours to set —
`CGO_LDFLAGS`, holding only the `-L` directory (step 3); at run time the
same directory goes on the loader path (step 4):

1. **Module** — every release pushes a `clients/go/vX.Y.Z` tag on the
   release commit (ADR-0592), so the Go proxy serves the versioned
   module:

   ```sh
   go get github.com/relatedb/relate/clients/go@vX.Y.Z
   ```

   To work against a checkout instead of a tagged version, pin it with a
   `replace`:

   ```
   require github.com/relatedb/relate/clients/go v0.0.0

   replace github.com/relatedb/relate/clients/go => <relate-checkout>/clients/go
   ```

2. **Library** — build it from that checkout
   (`cargo build -p relate-ffi --release`), or download the release
   artifact `relate-ffi-<triple>.tar.gz` (cdylib + staticlib + header)
   and extract it somewhere stable.

3. **Link** — the checkout's `target/` paths do not exist for your
   project, so point cgo at the staged library. The `-L` directory is
   all you need: the module's own directives add `-lrelate_ffi` and the
   include path for its header copy:

   ```sh
   CGO_LDFLAGS="-L<dir-holding-the-lib>" go build ./...
   ```

   To ship a self-contained binary, link the staticlib instead: put
   `librelate_ffi.a` alone in a directory (the linker prefers a `.dylib`
   when both are present) and add the system libraries the staticlib
   references — on macOS that is `-framework CoreFoundation`:

   ```sh
   CGO_LDFLAGS="-L<static-dir> -lrelate_ffi -framework CoreFoundation" go build ./...
   ```

   The built binary then carries no `librelate_ffi` dylib dependency.

4. **Run** — a dynamically linked binary needs the library findable at
   load time: `DYLD_LIBRARY_PATH` (macOS) / `LD_LIBRARY_PATH` (Linux)
   pointing at the directory holding the dylib/`.so` (what CI sets). A
   statically linked binary needs nothing.

A worked example of this setup — `go.mod` with the `replace`, the staged
library, `CGO_LDFLAGS`, and a minimal `Open`/`Connect`/`Query` program —
lives in [`docs/api/go/examples`](../../docs/api/go/examples) together
with its module file; `scripts/check-go-examples.sh` runs every one of
them. The out-of-tree half of the recipe — the module copied outside the
repository (the module-cache situation, where the directives'
`-L${SRCDIR}/../../target` paths resolve nothing), linked only through
the staged library with `CGO_LDFLAGS="-L<dir>"`, and running the
documented create_match example — is proven by
`scripts/check-go-standalone.sh`, which `make test-go` runs beside the
in-repo gate.

## Per-OS natives

The library is built per target triple (`cargo build -p relate-ffi
--release --target <triple>` → `target/<triple>/release/`): darwin
arm64/x64 `.dylib`, linux arm64/x64 `.so` (arm64 needs
`gcc-aarch64-linux-gnu` + the `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER`
override), win32-x64 `relate_ffi.dll` (MSVC). The release workflow tars
cdylib + staticlib + the `surfaces/relate-ffi/include/relate.h`
header per triple as `relate-ffi-<triple>.tar.gz`.

## Gotchas

- The client checks `relate_abi_version() == 2`; an ABI-v1 library
  fails at load.
- A `go test` link error naming `relate_value_*` symbols almost always
  means a stale `target/debug` library — rebuild it.
