# Relate Java client (spike)

A Java client over Relate's typed C ABI v2 (ADR-0502) via JNA:
`Database` / connection / typed values / typed params, the bundled-native
jar packaging of ADR-0490. API contract: [`docs/api/java/spec.md`](../../docs/api/java/spec.md).

## Prerequisites

- JDK 17+ and Maven
- The Rust toolchain (1.88) to build the native library

## Build the native library

The client loads `librelate_ffi` (macOS `.dylib`, Linux `.so`,
Windows `relate_ffi.dll`). `make build` at the repo root builds only
the CLI — build the FFI crate itself:

```sh
cargo build -p relate-ffi --release   # → target/release/librelate_ffi.dylib
```

The C header is `surfaces/relate-ffi/include/relate.h`.

One step from the repo root does build + package:
`make binding-java` (the fat jar plus the host-platform classifier jar;
`make binding-java MVN_ARGS="-Drelate.natives.dir=<dir>"` for the
staged five-platform build — ADR-0490).

## Run the tests

Two ways, same effect:

```sh
# 1. The dev escape hatch (absolute or cwd-relative both work):
export RELATE_LIB=../../target/release/librelate_ffi.dylib
export JAVA_TOOL_OPTIONS=--enable-native-access=ALL-UNNAMED   # JDK 24+
mvn test

# 2. The CI way — bundle this host's native into the test jars:
mvn -f clients/java/pom.xml -Drelate.ffi.dir="$PWD/target/release" test
```

`PackagedJarIT` runs the built jars in child JVMs with `RELATE_LIB`
removed, so it exercises the bundled-native extraction path (ADR-0490).

## Packaging (ADR-0490)

`mvn package` produces:

- `relate-<ver>.jar` — the all-platform jar: classes, the library
  skill, every staged native under `native/<key>/`
- `relate-<ver>-<classifier>.jar` — one per platform, that platform's
  native only

Natives come from one of two sources:

- **Local (default, `host-native` profile):** the build host's own library
  from `${relate.ffi.dir}` (default `../../target/release`). One
  classifier jar.
- **Release/CI (`staged-natives` profile):** all five prebuilt natives from
  a staged directory — `mvn package -Drelate.natives.dir=<dir>` laid
  out as:

  ```
  <dir>/darwin-arm64/librelate_ffi.dylib
  <dir>/darwin-x64/librelate_ffi.dylib
  <dir>/linux-arm64/librelate_ffi.so
  <dir>/linux-x64/librelate_ffi.so
  <dir>/win32-x64/relate_ffi.dll
  ```

### Release artifacts

The release workflow packages all of this automatically: its `java` job
stages the five per-triple FFI tarballs into the `staged-natives` layout,
runs `mvn package` with the client's test suite against the packaged
native, and attaches the fat jar and the five classifier jars to the
GitHub release — so the jars on a release always carry the release
version (the pom is a `release-bump` version surface like the npm
manifests and `pyproject.toml`).

Publication to Maven Central is prepared for but dormant — see
["Publishing to Maven Central"](#publishing-to-maven-central) below.

## Publishing to Maven Central

Everything needed ships in the repo, off by default:

- the pom's `central-publish` profile (activated only by
  `-Pcentral-publish`): sources jar, javadoc jar, GPG signature, and the
  central-publishing-maven-plugin upload with `autoPublish=false` — the
  first deployments are validated and released by hand from the Central
  Portal;
- the `maven-central` job in `publish-registries.yml`: it downloads the
  release's five FFI archives, verifies their build attestations, stages
  the natives and runs `mvn deploy -Pcentral-publish`. It runs only on a
  manual dispatch of that workflow with `maven` on, so Maven can be
  published alone without touching npm, PyPI or crates.io.

The coordinates are `ai.relatedb:relate` (the fat jar) plus one
classifier jar per platform. Turning it on is an owner action — Central
account, namespace verification, signing key and the four secrets — and
the step-by-step is "Maven Central" in
[`docs/PUBLISHING.md`](../../docs/PUBLISHING.md).

### Cross-building the five natives

Per-target builds (what the release workflow does):

```sh
rustup target add <triple>
cargo build -p relate-ffi --release --target <triple>
```

| Platform key | Triple | Library |
| --- | --- | --- |
| darwin-arm64 | `aarch64-apple-darwin` | `librelate_ffi.dylib` |
| darwin-x64 | `x86_64-apple-darwin` | `librelate_ffi.dylib` |
| linux-arm64 | `aarch64-unknown-linux-gnu` (needs `gcc-aarch64-linux-gnu`, `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc`) | `librelate_ffi.so` |
| linux-x64 | `x86_64-unknown-linux-gnu` | `librelate_ffi.so` |
| win32-x64 | `x86_64-pc-windows-msvc` | `relate_ffi.dll` |

Artifacts land in `target/<triple>/release/`. The release workflow tars
each (cdylib + staticlib + header) as `relate-ffi-<triple>.tar.gz`.

## Gotchas

- A **stale** `target/{debug,release}` dylib predating ABI v2 fails with
  `UnsatisfiedLinkError: relate_abi_version` — rebuild the profile you
  point at.
- The client requires ABI version 2 exactly (`relate_abi_version()`);
  there is no v1 fallback.
