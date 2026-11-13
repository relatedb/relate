#!/bin/sh
# Hermetic test harness for scripts/build-xcframework.sh's modes
# (ADR-0433's build entry, ADR-0572's split modes).
#
# The build entry resolves the repository from its own location, so the
# harness copies it (plus the real C header the assembler stages) into a
# sandbox repo and stubs the toolchain and the Apple assembly tools on
# PATH; the working tree is never touched and no real compilation or
# assembly runs. The invocation logs are the witnesses: what cargo was
# asked to build, whether xcodebuild/lipo/ditto ran.
#
# Locks the mode surface: the default build-and-assemble (CI's swift
# gate and `make test-swift` ride it), --build-target (one library, no
# assembly — the release workflow's parallel legs), --assemble-only
# (assembly from staged libraries, loud on a missing one — the stamp
# job), and the mode/value/lane refusals.
#
# Output is TAP-ish: one ok/not ok line per assertion, then a summary.
# Exit status is 0 only if every assertion passed.

set -u

ROOT=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)

# The shared TAP harness (LABS-622): tap_check/tap_done, the counters,
# output format, and sandbox policy spelled once.
. "$ROOT/scripts/tests/lib/tap.sh"

# The harness's filesystem contract is the release profile's layout:
# refuse an inherited XCFRAMEWORK_CARGO_PROFILE so the staged paths
# cannot be repointed from outside.
unset XCFRAMEWORK_CARGO_PROFILE

tap_sandbox relate-xcframework-test

# The one inventory: the release legs' matrix, the staged libraries, and
# the rustup stub's installed-target list all read these names.
ALL_TARGETS="aarch64-apple-darwin x86_64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios"
# The stubbed build and assembly tools; the stub loop and reset_logs
# iterate the same list.
TOOLS="cargo lipo xcodebuild ditto"

repo="$sandbox/repo"
mkdir -p "$repo/scripts" "$repo/surfaces/relate-ffi/include" "$sandbox/bin"
cp "$ROOT/scripts/build-xcframework.sh" "$repo/scripts/"
cp "$ROOT/surfaces/relate-ffi/include/relate.h" "$repo/surfaces/relate-ffi/include/"
build="$repo/scripts/build-xcframework.sh"

# Tool stubs: every target counts as installed; the build and assembly
# tools append their argument line to a log under XCF_TEST_LOGS so the
# assertions can count and grep the invocations.
export XCF_TEST_LOGS="$sandbox/logs"
mkdir -p "$XCF_TEST_LOGS"
for tool in $TOOLS; do
	: >"$XCF_TEST_LOGS/$tool"
	cat >"$sandbox/bin/$tool" <<EOF
#!/bin/sh
printf '%s\n' "\$*" >>"\$XCF_TEST_LOGS/$tool"
EOF
	chmod +x "$sandbox/bin/$tool"
done
cat >"$sandbox/bin/rustup" <<EOF
#!/bin/sh
printf '%s\n' $ALL_TARGETS
EOF
chmod +x "$sandbox/bin/rustup"
PATH="$sandbox/bin:$PATH"
export PATH

# The assertion helpers over the stub logs: counts, fixed-string
# needles, emptiness — one shape each, the normalization spelled once.
log_count() { # $1 = tool log, $2 = expected invocations, $3 = description
	count_n=$(wc -l <"$XCF_TEST_LOGS/$1")
	[ "$((count_n))" -eq "$2" ]; tap_check "$3" $?
}
log_has() { # $1 = tool log, $2 = fixed-string needle, $3 = description
	grep -qF -- "$2" "$XCF_TEST_LOGS/$1"; tap_check "$3" $?
}
log_empty() { # $1 = tool log, $2 = description
	[ ! -s "$XCF_TEST_LOGS/$1" ]; tap_check "$2" $?
}
refuse() { # $1 = expected exit status, $2 = message needle, $3 = description; then -- and the command
	# POSIX sh has no locals: the prefixed temporaries keep the helper out
	# of the harness's own top-level out/rc namespace.
	refuse_exp="$1"; refuse_needle="$2"; refuse_desc="$3"
	shift 3
	[ "${1:-}" = "--" ] || {
		echo "refuse: missing -- separator before the command" >&2
		exit 2
	}
	shift
	refuse_out="$("$@" 2>&1)"; refuse_rc=$?
	[ "$refuse_rc" = "$refuse_exp" ]; tap_check "$refuse_desc (exit $refuse_exp)" $?
	printf '%s\n' "$refuse_out" | grep -qF -- "$refuse_needle"; tap_check "$refuse_desc names its cause" $?
}

reset_logs() {
	for tool in $TOOLS; do
		: >"$XCF_TEST_LOGS/$tool"
	done
}

# Stage one stub library per triple at the path lib_for computes.
stage_libs() {
	for t in $ALL_TARGETS; do
		mkdir -p "$repo/target/$t/release"
		printf 'stub-lib\n' >"$repo/target/$t/release/librelate_ffi.a"
	done
}

# 1. The default mode is unchanged: five builds, then assembly + zip.
reset_logs
stage_libs
tap_run_success "default builds and assembles" bash "$build" --zip
log_count cargo 5 "default builds all five targets"
log_has xcodebuild "-create-xcframework" "default assembles the xcframework"
log_has ditto "RelateFFI.xcframework.zip" "default zips via ditto"
log_count lipo 2 "default lipos both multi-arch slices"

# 2. --slices macos stays the two-darwin-target build (the swift test
# lane's shape).
reset_logs
tap_run_success "--slices macos builds the darwin pair" bash "$build" --slices macos
log_count cargo 2 "--slices macos builds two targets"

# 3. --build-target: one library, printed, no assembly.
reset_logs
out="$(bash "$build" --build-target aarch64-apple-ios-sim 2>&1)"; rc=$?
tap_check "--build-target succeeds" "$rc"
[ "$out" = "$repo/target/aarch64-apple-ios-sim/release/librelate_ffi.a" ]; tap_check "prints the library path" $?
log_count cargo 1 "builds exactly one target"
log_has cargo "--target aarch64-apple-ios-sim" "passes the triple to cargo"
log_empty xcodebuild "build-target assembles nothing"

# 4. Refusals, each through the one helper: one capture, the exit
# status and the message asserted from the same invocation.
refuse 2 "unknown --build-target: armv7-none-eabi" \
	"rejects an unknown --build-target" \
	-- bash "$build" --build-target armv7-none-eabi
refuse 2 "--build-target needs a triple as its value" \
	"rejects an empty --build-target value" \
	-- bash "$build" --build-target ""
refuse 2 "--build-target needs a triple as its value" \
	"rejects an empty --build-target= form" \
	-- bash "$build" --build-target=
refuse 2 "two modes; pick one" \
	"rejects --build-target with --assemble-only" \
	-- bash "$build" --build-target aarch64-apple-darwin --assemble-only
refuse 2 "there is no XCFramework to zip" \
	"rejects --build-target with --zip" \
	-- bash "$build" --build-target aarch64-apple-darwin --zip
refuse 2 "--slices is meaningless beside it" \
	"rejects --build-target with --slices" \
	-- bash "$build" --build-target aarch64-apple-darwin --slices macos

# 5. --assemble-only without staged libraries: a loud stop, no build.
reset_logs
rm -rf "$repo/target"
refuse 1 "assemble-only: missing or empty" \
	"assemble-only refuses a missing library" \
	-- bash "$build" --assemble-only --zip
log_empty cargo "the refusal builds nothing"

# 6. --assemble-only with the five libraries staged: assembly only.
reset_logs
stage_libs
tap_run_success "assemble-only assembles from staged libraries" bash "$build" --assemble-only --zip
log_empty cargo "assemble-only runs no cargo build"
log_has xcodebuild "-create-xcframework" "assemble-only assembles the xcframework"
log_has ditto "RelateFFI.xcframework.zip" "assemble-only zips via ditto"
log_count lipo 2 "assemble-only lipos both multi-arch slices"

# 7. --assemble-only still validates --slices (shape refusals come
# before the presence check).
refuse 2 "unknown --slices value: bogus" \
	"assemble-only still validates --slices" \
	-- bash "$build" --assemble-only --slices bogus

# 8. Help documents the modes.
out="$(bash "$build" --help 2>&1)"; rc=$?
tap_check "help exits 0" "$rc"
printf '%s\n' "$out" | grep -qF -- "--build-target"; tap_check "help documents --build-target" $?
printf '%s\n' "$out" | grep -qF -- "--assemble-only"; tap_check "help documents --assemble-only" $?

tap_done
