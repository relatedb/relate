---
type: adr
area: surface
claim: 011cb1e6ac6a1bd5836c792fad404d9b170fa94b
name: 0490-java-bundled-natives
status: proposed
tags:
  - clients
  - release
title: Java client packaging — bundled natives in a fat jar and per-platform classifier jars, the library skill in the jar, JNA kept over FFM
REFERENCES:
  - "[[adr/0278-npm-distribution]]"
  - "[[adr/0502-typed-c-abi-v2]]"
---

# ADR-0490: Java client packaging — bundled natives in a fat jar and per-platform classifier jars, the library skill in the jar, JNA kept over FFM

## Context

The Java client (`clients/java`, `com.cypherlite:cypherlite` 0.1.0) binds
the product C ABI through JNA 5.14. Until now it loaded `cypherlite_ffi`
only from `CYPHERLITE_LIB` or, failing that, `Native.load("cypherlite_ffi")`
— JNA's search of `jna.library.path`, `java.library.path` and the system
library path. Nobody installs `libcypherlite_ffi` system-wide, so the only
working route was a repo checkout with a hand-set environment variable: a
plain Maven dependency could not load the engine (LABS-830).

The other clients already solved distribution: the npm client ships one
optional platform package per release target ([ADR-0278](./0278-npm-distribution.md)); the Python wheel
embeds the library per platform tag. `release.yml`'s `ffi` job builds the
five targets those use — `x86_64`/`aarch64` Linux (glibc), `x86_64`/
`aarch64` macOS and `x86_64` Windows — and uploads each as
`cypherlite-ffi-<target>.tar.gz`. A release-built macOS arm64
`libcypherlite_ffi.dylib` is ~24.5 MB and deflates to ~8.4 MB inside a jar,
so an all-platform jar lands near 40 MB.

The JVM ecosystem has two established shapes for native code:

- **The fat jar** (DuckDB JDBC, sqlite-jdbc, JNA itself): one artifact
  carries every platform's library as a classpath resource; a loader
  extracts the right one to disk at first use and loads it by absolute
  path. Zero configuration, larger download.
- **Per-platform classifier jars** (netty-tcnative, grpc-netty, the
  protoc artifacts): `<artifact>-<ver>-<classifier>.jar`, selected at build
  time with `kr.motd.maven:os-maven-plugin` (Maven) or
  `com.google.osdetector` (Gradle), whose `${os.detected.classifier}`
  vocabulary is `osx-aarch_64`, `osx-x86_64`, `linux-aarch_64`,
  `linux-x86_64`, `windows-x86_64`. Smaller, but the user must pick.

The client must also ship the library skill
(`.agents/skills/cypherlite/SKILL.md`, Agent Skills format) the way the
Python wheel and the npm package do (AGENTS.md §2). library-skills.io scans
npm and PyPI only today; no JVM discovery convention exists yet.

Separately, the Foreign Function & Memory API (FFM, "Panama") became final
in Java 22 (JEP 454) and is the JDK's own replacement for JNI-based
binding libraries such as JNA. The JDK floor the client promises is Java
17: the pom compiles with `release 17` and CI's `os-smoke` lane runs the
client on Temurin 17.

## Decision

1. **Two artifact shapes from one build, one loader.**
   - The plain artifact `com.cypherlite:cypherlite:<ver>` is the fat jar:
     classes, the library skill and all five natives at
     `native/<key>/<library>`, where `<key>` is the npm platform suffix —
     `darwin-arm64`, `darwin-x64`, `linux-arm64`, `linux-x64`, `win32-x64`
     — and `<library>` is `libcypherlite_ffi.dylib`,
     `libcypherlite_ffi.so` or `cypherlite_ffi.dll`.
   - Five classifier jars `cypherlite-<ver>-<classifier>.jar` carry the
     same classes and skill plus that one platform's native at the same
     resource path. The classifiers use the os-maven-plugin / osdetector
     vocabulary (`osx-aarch_64`, `osx-x86_64`, `linux-aarch_64`,
     `linux-x86_64`, `windows-x86_64`) so `${os.detected.classifier}`
     selects one directly. A user depends on either the fat jar or one
     classifier jar, never both.
2. **Loader order, no system search.** `CYPHERLITE_LIB` (non-empty) is
   passed to `Native.load` unchanged — a development escape hatch for a
   self-built library, documented as such. Otherwise the loader detects the
   platform, looks up `native/<key>/<library>` on the classpath, extracts
   it and loads it by absolute path. A classpath without that resource
   fails with a `CypherLiteException` naming the platform key, `os.name`,
   `os.arch`, the missing resource and the classifier coordinate to add.
   The system library path, `java.library.path` and `jna.library.path` are
   never consulted for `cypherlite_ffi`.
3. **One canonical platform table.** `com.cypherlite.NativePlatform` maps
   `os.name` / `os.arch` onto the five rows (key, library file name,
   classifier). Any other combination — Windows on ARM, Linux on RISC-V,
   FreeBSD, 32-bit x86 — throws with both inputs in the message; nothing
   nearest-matches.
4. **Extraction is atomic and per version.** The library is extracted to
   `<cache>/<version>/<key>-<sha256[:16]>/<library>`. The cache root is
   `-Dcypherlite.native.cache` when set, else the per-user cache directory
   plus `cypherlite/native` (`~/Library/Caches` on macOS,
   `$XDG_CACHE_HOME` or `~/.cache` on Linux, `%LOCALAPPDATA%` on Windows);
   a missing home or `LOCALAPPDATA` fails naming the property. The content
   digest in the directory name keeps a `-SNAPSHOT` rebuild from reusing a
   stale library under an unchanged version. An existing target is reused;
   a missing one is written to a temp file in the same directory, forced to
   disk and moved into place with `ATOMIC_MOVE`, so concurrent JVMs never
   load a partial file; losing the rename race to a complete copy is
   success. A per-user directory, not the shared `java.io.tmpdir`, so no
   other local user can pre-plant a library at the predictable path.
5. **Build wiring.** `mvn package` builds the fat jar plus one classifier
   jar per staged platform. `-Dcypherlite.natives.dir=<dir>` (release/CI)
   stages `<dir>/<key>/<library>` and the build fails unless all five are
   present; without it (local development) the build bundles the build
   host's library from `-Dcypherlite.ffi.dir` (default
   `../../target/release`) and fails when that file is missing.
   `PackagedJarIT` (failsafe, `mvn verify`) checks every jar's listing and
   loads the fat jar and the host classifier jar in child JVMs with
   `CYPHERLITE_LIB`, `java.library.path` and the loader-path environment
   removed — including two JVMs racing one fresh cache.
6. **The library skill ships in every jar** at
   `.agents/skills/cypherlite/SKILL.md` (the Python/JS naming schema), a
   Java-flavored Agent Skills document, kept in sync with the Java API
   under AGENTS.md §2's library-skill rule. It is shipped although no JVM
   scanner exists yet: it is a few KB and forward-compatible.
7. **JNA stays the binding.** FFM is evaluated below and deferred, not
   rejected.

Publication to Maven Central is out of scope: no workflow publishes the
Java client today. Wiring `release.yml` to stage the `ffi` job's five
artifacts into `-Dcypherlite.natives.dir` and deploy the six jars is a
follow-up, as is lockstep versioning of `clients/java/pom.xml` in
`scripts/release-bump.sh`.

## Alternatives considered

- **FFM (Panama) instead of JNA.** Evaluated in full:
  - *JDK floor.* FFM is final only from Java 22 (preview in 19–21, with
    API changes between previews; incubator before that). Java 17 is the
    client's promised floor and an LTS still in wide production use;
    Java 21 — the newest LTS before 25 — has FFM only as a preview needing
    `--enable-preview` and a matching class-file version. An FFM-only
    client would exclude every 17 and 21 user. This is the blocker.
  - *Binding generation.* `jextract` can generate the bindings from
    `surfaces/cypherlite-ffi/include/cypherlite.h` (374 lines, ~40
    functions, all opaque handles, integers and C strings — no structs by
    value or callbacks). It is a separate download, not part of the JDK,
    and its output targets the JDK it was built for, so the build would
    either pin a jextract per JDK line or commit the generated sources and
    regenerate on every ABI change. For an ABI this small, hand-written
    `MethodHandle` downcalls (~150 lines) are the simpler path; either way
    it is a second binding to keep in lockstep with the header.
  - *Native-access warnings.* FFM's restricted methods warn on Java 22+
    unless `--enable-native-access=ALL-UNNAMED` (or the module name) is
    passed. This is no longer a differentiator: JEP 472 (Java 24) applies
    the same warning to JNI library loading, so JNA on Java 25 prints
    `WARNING: java.lang.System::load has been called by com.sun.jna.Native`
    (observed in this change's test runs), and both are slated to be denied
    by default in a future release. The flag is the user's to set either
    way; the docs name it.
  - *Footprint.* FFM needs no dependency; JNA adds the ~1.9 MB `jna` jar
    carrying its own `jnidispatch` for every platform, extracted to its own
    temp directory. Against a ~40 MB fat jar (or an ~8 MB classifier jar)
    the saving is real but secondary.
  - *Performance.* JNA's interface-proxy calls marshal arguments through
    reflection and copy every returned C string; FFM downcalls are
    JIT-inlinable method handles and can read result-owned strings in
    place. The client's call pattern is one call per statement, then,
    per cell, `cypherlite_value_at` plus the walker calls that read it: the
    kind, then one accessor per component (a list or map adds a call per
 element or key). Under the v2 ABI ([ADR-0502](./0502-typed-c-abi-v2.md)) a cell is many small calls,
    not one JSON string, so the per-call cost no longer amortises over a
    JSON decode. The argument shifts: JNA's reflective marshalling is paid
    per walker call, and FFM's in-place reads matter more for wide results.
    This ADR does not settle FFM versus JNA on that basis. Not measured in
    this change; any switch must measure a large-result read through the
    walker on both bindings first.
  - *Multi-release jar.* A `META-INF/versions/22/` FFM implementation of a
    package-private binding interface, with the JNA implementation in the
    base layer, would give 22+ users FFM and keep 17–21 on JNA from one
    artifact. Costs: two bindings over one ABI, CI lanes on both a ≤21 and
    a ≥22 JDK, JNA still a mandatory Maven dependency (an MRJAR cannot
    vary dependencies), and IDE/build tooling that handles MRJARs
    unevenly.

  **Deferred, not rejected forever.** Rejected for now because an
  FFM-only client excludes the Java 17 floor and an MRJAR doubles the
  binding for no user-visible capability. Revisit when the supported floor
  reaches Java 22 (then FFM-only, dropping JNA), or earlier if JNA's JNI
  loading starts to require more than the same `--enable-native-access`
  flag FFM needs, or a measured large-result read shows JNA call overhead
  dominating.
- **Load from the system library path** (`Native.load("cypherlite_ffi")`,
  the previous default and fallback). Rejected: no distribution installs
  `libcypherlite_ffi` system-wide, so the path only ever worked with
  hand-set environment; as a silent fallback it would also load whatever
  stale or foreign library happens to be found ahead of the bundled one.
- **Fat jar only.** Rejected: every consumer — including size-sensitive
  ones (containers, serverless, Android-adjacent tooling) — would ship ~40
  MB of libraries for four platforms it never runs on.
- **Classifier jars only.** Rejected: the plain dependency would carry no
  native and fail at first use, so every user would have to learn
  os-maven-plugin or osdetector before running one query. The zero-config
  path must be the default coordinate.
- **Separate native-only classifier jars beside a thin main jar** (the
  netty-tcnative split). Rejected: the thin main artifact alone would
  fail, which is the classifiers-only UX again for anyone who skips the
  second dependency.
- **Classifiers named by the platform key** (`darwin-arm64`, …).
  Rejected: `${os.detected.classifier}` would then need a per-build
  mapping table in every consumer's pom; the os-maven-plugin vocabulary is
  what JVM native artifacts already use. The resource directory keeps the
  npm key, shared with the release matrix and the JS client.
- **Extract to `java.io.tmpdir` per run and delete on exit**
  (sqlite-jdbc). Rejected: repeated ~25 MB writes on every start, deletion
  that never happens on Windows while the DLL is loaded, and a shared
  directory where another local user can pre-plant the library.

## Consequences

- A plain Maven or Gradle dependency on `com.cypherlite:cypherlite` loads
  and runs a query on the five platforms with no environment variable or
  library path; a classifier dependency does the same at ~8 MB.
- Unsupported platforms (Windows on ARM, other OSes and architectures)
  fail at first use with a message naming the platform — the same
  platform set as npm. The Linux natives are glibc builds: on musl
  (Alpine) the `linux-*` library is found but JNA's load fails with its
  own `UnsatisfiedLinkError`. Widening the set is a `release.yml` matrix
  change plus one `NativePlatform` row, one pom profile and one classifier
  execution.
- The pom carries the platform rows a second time (host profiles and
  classifier executions — Maven cannot read Java); `PackagedJarIT` derives
  its expectations from `NativePlatform`, so drift fails the build.
- First use per version writes one library into the user cache; later
  JVMs only re-hash the resource (one pass over ~25 MB). Old versions'
  directories are never pruned automatically — deleting the cache is safe.
- `CYPHERLITE_LIB` keeps working for development and CI; it is no longer
  the only way to load the library and the docs label it an escape hatch.
- Java 24+ prints JEP 472's JNI warning until the user passes
  `--enable-native-access=ALL-UNNAMED`; this is documented, not suppressed.
- The library skill rides in every jar; a JVM-side skill scanner does not
  exist yet, so discovery is manual until one does.
- Maven Central publication and release-version lockstep for the pom are
  follow-ups; until then the jars are built from source.
