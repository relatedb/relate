# The one derivation of the target inventory (LABS-1000): every
# `## <name>:` help annotation names a target. The help target greps the
# same lines; .PHONY consumes ANNOTATED below, and the inventory drift
# guard (scripts/tests/test_makefile_targets.sh, make
# test-makefile-targets) reads ANNOTATED and .PHONY back out of make's
# own database — so the name grammar is spelled once and every drift
# direction breaks a gate. (The hashes are escaped for make: an
# assignment line strips comments at a bare '#', a rule line does not.)
ANNOTATED := $(shell sed -n 's/^\#\# \([a-zA-Z0-9_-]*\):.*/\1/p' $(MAKEFILE_LIST))
.PHONY: $(ANNOTATED)

BASELINES ?= $(CURDIR)/baselines
PYTHON ?= python3
SUMMARIZE := $(PYTHON) $(CURDIR)/scripts/summarize-loadtest.py
ENVELOPE := $(PYTHON) $(CURDIR)/scripts/freeze-baseline-envelope.py
COMPARE := $(PYTHON) $(CURDIR)/scripts/compare-loadtest.py
# Runs per suite behind a freeze. One run does not pin the tail; the freeze is
# the worst-of-N envelope (ADR-0070).
BASELINE_RUNS ?= 5
# Absolute p99 allowance for the baseline container: sub-ms use cases swing to
# ~3 ms on queueing alone there, so the 250,000 ns script default (an
# unconstrained-host number) cannot hold (ADR-0070).
LOADTEST_P99_JITTER_NS ?= 5000000

.DEFAULT_GOAL := help

## help: Show this help message
help:
	@echo "Relate — Lightweight embeddable graph database"
	@echo ""
	@echo "Usage: make <target>"
	@echo ""
	@grep -E '^## ' $(MAKEFILE_LIST) | sed 's/^## /  /'
	@echo ""
	@echo "In-tree loadtests default to Docker 2 vCPU / 2 GB (ADR-0070):"
	@echo "  make memory-shape-loadtest LOGICAL_MB=32 MEMORY_SHAPE_MEMORY=2g ATTRIBUTES=1 TEXT_INDEXES=0 POINT_INDEXES=0 RANGE_INDEXES=0"

## build: Build the relate binary (release)
build:
	cargo build --release -p relate-cli

## build-wasm: Build the relate-wasm package (release-wasm profile + wasm-bindgen + wasm-opt, ADR-0084/0100)
build-wasm:
	bash scripts/build-wasm.sh --mode release

## build-xcframework: Build target/apple/RelateFFI.xcframework for the Swift package (ADR-0433)
build-xcframework:
	bash scripts/build-xcframework.sh

## build-ffi: Build the release FFI library — the artifact every language client and the C examples load
build-ffi:
	cargo build --release -p relate-ffi

MVN_ARGS ?=
## binding-java: Package the Java client after build-ffi — the fat jar plus the host-platform classifier jar (ADR-0490). MVN_ARGS passes maven flags through (e.g. MVN_ARGS="-Drelate.natives.dir=<dir>" for the staged five-platform build)
binding-java: build-ffi
	mvn -f clients/java/pom.xml package $(MVN_ARGS)

## binding-js: Pack the JavaScript client tarball after build-ffi (into target/, never the source tree)
binding-js: build-ffi
	cd clients/javascript && npm pack --pack-destination ../../target

## bindings: Package the Java and JavaScript client packages
bindings: binding-java binding-js

# The Swift targets link the locally built XCFramework, never a release binary
# (Package.swift reads this; SwiftPM keys its manifest cache on the env).
SWIFT_ENV := RELATE_LOCAL_XCFRAMEWORK=1
SWIFT_EXAMPLES := quickstart transactions result-metadata namespaces persistence

## swift-examples: Run every docs/api/swift example (each asserts its own output)
swift-examples:
	cd docs/api/swift/examples && for e in $(SWIFT_EXAMPLES); do \
		echo "== $$e"; $(SWIFT_ENV) swift run -q $$e || exit 1; done

## test-swift: The CI swift gate: XCFramework, swift test, examples, release stamp, the iOS example's tests and simulator build
test-swift: build-xcframework test-stamp-swift-package
	$(SWIFT_ENV) swift test
	$(MAKE) swift-examples
	cd examples/ios-snotel && $(SWIFT_ENV) swift test
	cd examples/ios-snotel && $(SWIFT_ENV) xcodebuild -project SNOTELExplorer.xcodeproj \
		-scheme SNOTELExplorer -destination 'generic/platform=iOS Simulator' \
		-derivedDataPath $(CURDIR)/target/apple/DerivedData CODE_SIGNING_ALLOWED=NO build

## test-go: The Go client gate: the FFI library, the client's tests, every documented example
# The debug profile, not build-ffi's release one: every clients/go cgo
# directive searches target/debug FIRST, so building release would let the
# gate link a stale debug library. This is also the profile the sonic
# `clients` stage builds (its CGO_LDFLAGS names that directory).
test-go:
	cargo build -p relate-ffi
	cd clients/go && go test -count=1 ./...
	bash scripts/check-go-examples.sh

## test: Run all Rust tests (excluding the Python crate)
test:
	cargo test --jobs 1 \
		-p relate-model -p relate-parser -p relate-error \
		-p relate-formatter -p relate-arrow \
		-p relate-splitter -p relate-cli -p relate-ffi \
		-p relate-core -p relate-catalog -p relate-front \
		-p relate-planner -p relate-executor -p relate \
		-p relate-ir -p relate-storage
	cargo test -p relate-datasets --test memory_shapes

## test-all: Run all tests (Rust + Python)
test-all: test test-python

## lint: Run Rust linters (fmt + clippy; exclude Python as CI)
lint:
	cargo fmt --all --check && cargo clippy --workspace --all-targets --exclude relate-python -- -D warnings

## run: Run the CLI with an in-memory database
run:
	cargo run -p relate-cli -- :memory:

## install-python: Set up the Python package dev environment (maturin)
install-python:
	cd surfaces/relate-python && uv venv .venv && . .venv/bin/activate && uv pip install maturin pytest pyarrow && maturin develop

## test-python: Run Python package tests
test-python:
	cd surfaces/relate-python && . .venv/bin/activate && python -m pytest python/tests/ -v

## run-demo: Run the Python demo script
run-demo:
	cd surfaces/relate-python && . .venv/bin/activate && python $(CURDIR)/examples/python/python_demo.py

# Directories `make clean` deletes. Each one must stay untracked.
# The Sonic cargo caches are Docker volumes named from the `type: cache`
# sources in .sonic/ci.yml (`gosonic-cache-<source>`).
CLEAN_PATHS := \
	target \
	surfaces/relate-python/.venv \
	clients/javascript/node_modules \
	clients/java/target \
	docs/api/bolt/examples/websocket/node_modules \
	surfaces/relate-wasm/pkg
SONIC_CACHE_VOLUMES := \
	gosonic-cache-cargo-home \
	gosonic-cache-cargo-target \
	gosonic-cache-cargo-registry \
	gosonic-cache-cargo-git

## clean: Remove build artifacts and Sonic cargo cache volumes
clean:
	@tracked=$$(git -c safe.directory='$(CURDIR)' ls-files -- $(CLEAN_PATHS)); \
	if [ -n "$$tracked" ]; then \
		printf '%s\n' "make clean: refusing to remove tracked files:" "$$tracked" >&2; \
		exit 1; \
	fi
	rm -rf $(CLEAN_PATHS)
	@if command -v docker >/dev/null 2>&1; then \
		for volume in $(SONIC_CACHE_VOLUMES); do \
			if docker volume inspect "$$volume" >/dev/null 2>&1; then \
				docker volume rm "$$volume"; \
			fi; \
		done; \
	fi

## baseline: Measure source mains; write summaries under baselines/
baseline: baseline-relate baseline-loadtests
	@echo "baseline: wrote $(BASELINES)"

## baseline-relate: Run Relate main surface tests (test, python, doc-examples)
baseline-relate:
	$(MAKE) test
	$(MAKE) install-python
	$(MAKE) test-python
	$(MAKE) build
	RELATE_BIN=$(CURDIR)/target/release/relate bash $(CURDIR)/scripts/check-doc-examples.sh

## baseline-loadtests: Freeze fraud, identity, checkpoint, memory-shape, query-latency (Docker env; ADR-0070, ADR-0245)
baseline-loadtests:
	@if [ "$(BASELINE_ENV)" != docker ]; then \
		echo "baseline-loadtests: BASELINE_ENV must be docker (ADR-0070); host runs are not freezeable"; \
		exit 1; \
	fi
	mkdir -p $(BASELINES)
	rm -f $(CURDIR)/target/*-envelope-run*.json
	@set -e; i=1; while [ $$i -le $(BASELINE_RUNS) ]; do \
		echo "baseline-loadtests: round $$i/$(BASELINE_RUNS)"; \
		$(MAKE) fraud-loadtest OUT_FRAUD=target/fraud-loadtest-envelope-run$$i.json; \
		$(MAKE) identity-loadtest OUT_IDENTITY=target/identity-loadtest-envelope-run$$i.json; \
		$(MAKE) fraud-checkpoint OUT_FRAUD_CP=target/fraud-checkpoint-envelope-run$$i.json; \
		$(MAKE) memory-shape-loadtest OUT_MEMORY_SHAPE=target/memory-shape-loadtest-envelope-run$$i.json; \
		$(MAKE) query-latency OUT_QUERY_LATENCY=target/query-latency-envelope-run$$i.json; \
		i=$$((i + 1)); \
	done
	$(ENVELOPE) --out $(BASELINES)/fraud-loadtest.json $(CURDIR)/target/fraud-loadtest-envelope-run*.json
	$(ENVELOPE) --out $(BASELINES)/identity-loadtest.json $(CURDIR)/target/identity-loadtest-envelope-run*.json
	$(ENVELOPE) --out $(BASELINES)/fraud-checkpoint.json $(CURDIR)/target/fraud-checkpoint-envelope-run*.json
	$(ENVELOPE) --out $(BASELINES)/memory-shape-loadtest.json $(CURDIR)/target/memory-shape-loadtest-envelope-run*.json
	$(PYTHON) $(CURDIR)/scripts/freeze-query-latency-envelope.py --out $(QL_BASELINE) $(CURDIR)/target/query-latency-envelope-run*.json
	cp $(CURDIR)/target/fraud-loadtest-envelope-run$(BASELINE_RUNS).json $(CURDIR)/target/fraud-loadtest.json
	cp $(CURDIR)/target/identity-loadtest-envelope-run$(BASELINE_RUNS).json $(CURDIR)/target/identity-loadtest.json
	cp $(CURDIR)/target/fraud-checkpoint-envelope-run$(BASELINE_RUNS).json $(CURDIR)/target/fraud-checkpoint.json
	cp $(CURDIR)/target/memory-shape-loadtest-envelope-run$(BASELINE_RUNS).json $(CURDIR)/target/memory-shape-loadtest.json

## baseline-memory-controls: Freeze the memory-controls envelope (Docker only, full profile)
baseline-memory-controls:
	@if [ "$(BASELINE_ENV)" != docker ]; then \
		echo "baseline-memory-controls: BASELINE_ENV must be docker (ADR-0070); host runs are not freezeable"; \
		exit 1; \
	fi
	mkdir -p $(BASELINES)
	rm -f $(CURDIR)/target/memory-controls-envelope-run*.json
	@set -e; i=1; while [ $$i -le $(BASELINE_RUNS) ]; do \
		echo "baseline-memory-controls: round $$i/$(BASELINE_RUNS)"; \
		$(MAKE) memory-controls-test MEMORY_CONTROLS_PROFILE=full OUT_MEMORY_CONTROLS=target/memory-controls-envelope-run$$i.json; \
		i=$$((i + 1)); \
	done
	$(PYTHON) $(CURDIR)/scripts/freeze-memory-controls.py --out $(BASELINES)/memory-controls.json $(CURDIR)/target/memory-controls-envelope-run*.json

## compare-memory-controls: Judge target/memory-controls.json against the envelope
compare-memory-controls:
	$(PYTHON) $(CURDIR)/scripts/compare-memory-controls.py --baseline $(BASELINES)/memory-controls.json --report $(OUT_MEMORY_CONTROLS)

## memory-controls-gate: Run the suite, then compare
memory-controls-gate: memory-controls-test compare-memory-controls

## baseline-memory-shape: Freeze the memory-shape envelope (Docker only)
baseline-memory-shape:
	@if [ "$(BASELINE_ENV)" != docker ]; then \
		echo "baseline-memory-shape: BASELINE_ENV must be docker (ADR-0070); host runs are not freezeable"; \
		exit 1; \
	fi
	mkdir -p $(BASELINES)
	rm -f $(CURDIR)/target/memory-shape-envelope-run*.json
	@set -e; i=1; while [ $$i -le $(BASELINE_RUNS) ]; do \
		echo "baseline-memory-shape: round $$i/$(BASELINE_RUNS)"; \
		$(MAKE) memory-shape-loadtest OUT_MEMORY_SHAPE=target/memory-shape-envelope-run$$i.json; \
		i=$$((i + 1)); \
	done
	$(PYTHON) $(CURDIR)/scripts/freeze-memory-shape.py --out $(BASELINES)/memory-shape.json $(CURDIR)/target/memory-shape-envelope-run*.json

## compare-memory-shape: Judge target/memory-shape-loadtest.json against the envelope
compare-memory-shape:
	$(PYTHON) $(CURDIR)/scripts/compare-memory-shape.py --baseline $(BASELINES)/memory-shape.json --report $(OUT_MEMORY_SHAPE)

## memory-shape-gate: Run the suite, then compare
memory-shape-gate: memory-shape-loadtest compare-memory-shape

## compare-loadtests: Compare current in-tree reports to baselines/fraud-loadtest.json
compare-loadtests:
	@test -f $(BASELINES)/fraud-loadtest.json
	BASELINES=$(BASELINES) LOADTEST_P99_JITTER_NS=$(LOADTEST_P99_JITTER_NS) bash $(CURDIR)/scripts/compare-baselines.sh

## render-loadtest-comparison: Render the combined baseline-vs-report loadtest
##   comparison markdown (target/loadtest-comparison.md) for every pair —
##   fraud, identity — without
##   gating; missing baselines/reports render as notes. Sets the gate's
##   LOADTEST_P99_JITTER_NS so the tables match its bands. Appends to
##   $GITHUB_STEP_SUMMARY when set, else prints to stdout.
render-loadtest-comparison:
	BASELINES=$(BASELINES) LOADTEST_P99_JITTER_NS=$(LOADTEST_P99_JITTER_NS) bash $(CURDIR)/scripts/compare-baselines.sh --render-only

## gate: Required in-tree checks (surface + loadtest compare)
gate:
	$(MAKE) lint
	$(MAKE) test
	$(MAKE) test-compare-loadtest
	@if [ -f $(BASELINES)/fraud-loadtest.json ]; then $(MAKE) compare-loadtests; else echo "gate: loadtest baselines missing; run make baseline-loadtests"; exit 1; fi
	@if [ -f $(CURDIR)/target/query-latency/report-graph-benchmark-ldbc-sf$(SF).json ]; then $(MAKE) compare-query-latency; else echo "gate: query-latency report missing (make query-latency); the envelope skip is compare-query-latency's own"; fi

BASELINE_ENV ?= docker
BASELINE_CPUS ?= 2
BASELINE_MEMORY ?= 2g
BASELINE_BUILD_IMAGE ?= rust:1.88-bookworm
BASELINE_RUN_IMAGE ?= debian:bookworm-slim
BASELINE_ENV_SH := $(CURDIR)/scripts/baseline-env.sh

SCALE ?= 0.10
OPS ?= 20000
WARMUP ?= 8
# Sized so the two-CPU baseline container is loaded but not saturated: at
# ADR-0048's 50/8 the queueing tail alone swings p99 by 2-4x (ADR-0070).
WORKERS ?= 8
WORKERS_IDENTITY ?= 4
WRITE_FRACTION ?= 0.44
GRAPH_SEED ?= 42
WORKLOAD_SEED ?= 7
LOGICAL_GRAPH_SIZE_LIMIT ?= 20mib
LOGICAL_GRAPH_SIZE_LIMIT_IDENTITY ?= 40mib
ifeq ($(BASELINE_ENV),docker)
URI_FRAUD ?= /tmp/fraud-lt
URI_FRAUD_CP ?= /tmp/fraud-checkpoint
else
URI_FRAUD ?= ./target/fraud-lt
URI_FRAUD_CP ?= ./target/fraud-checkpoint
endif
OUT_FRAUD ?= target/fraud-loadtest.json
OUT_FRAUD_CP ?= target/fraud-checkpoint.json
FANOUT_K ?= 12
CYCLE_LEN ?= 4
URI_IDENTITY ?= :memory:
OUT_IDENTITY ?= target/identity-loadtest.json
OUT_MEMORY_SHAPE ?= target/memory-shape-loadtest.json
# One directory per invocation. Each shape is stored at $(URI_MEMORY_SHAPE)/<shape>.
MEMORY_SHAPE_RUN ?= $(shell date +%Y%m%dT%H%M%S)
URI_MEMORY_SHAPE ?= /tmp/memory-shape-loadtest-$(MEMORY_SHAPE_RUN)
NEST_DEPTH ?= 4
GROUP_K ?= 12
ifeq ($(BASELINE_ENV),docker)
URI_BULK ?= /tmp/bulk-loadtest-data
else
URI_BULK ?= ./target/bulk-loadtest-data
endif
OUT_BULK ?= target/bulk-loadtest.json
CARGO_PROFILE ?= --release
ARGS ?=

FRAUD_LOADTEST_ARGS = --uri $(URI_FRAUD) --reset \
	--scale $(SCALE) --ops $(OPS) --warmup $(WARMUP) --workers $(WORKERS) \
	--write-fraction $(WRITE_FRACTION) \
	--logical-graph-size-limit $(LOGICAL_GRAPH_SIZE_LIMIT) \
	--graph-seed $(GRAPH_SEED) --workload-seed $(WORKLOAD_SEED) \
	--fanout-k $(FANOUT_K) --cycle-len $(CYCLE_LEN) \
	--out $(OUT_FRAUD) $(ARGS)

IDENTITY_LOADTEST_ARGS = --uri $(URI_IDENTITY) --reset \
	--scale $(SCALE) --ops $(OPS) --warmup $(WARMUP) --workers $(WORKERS_IDENTITY) \
	--write-fraction $(WRITE_FRACTION) \
	--logical-graph-size-limit $(LOGICAL_GRAPH_SIZE_LIMIT_IDENTITY) \
	--graph-seed $(GRAPH_SEED) --workload-seed $(WORKLOAD_SEED) \
	--nest-depth $(NEST_DEPTH) --group-k $(GROUP_K) \
	--out $(OUT_IDENTITY) $(ARGS)

# Bulk create/merge/update/delete per batch size. No --workers: the harness is
# single-threaded. --dir is a filesystem URI; the Docker run env mounts
# /tmp as tmpfs, so the default URI lives there. No memory limit: the bulk
# working set grows with batch size, and a fixed cap (the fraud/identity one)
# fails the 50k/100k sizes with capacity-exceeded.
BULK_LOADTEST_ARGS = --dir $(URI_BULK) \
	--out $(OUT_BULK) $(ARGS)

FRAUD_CHECKPOINT_ARGS = --uri $(URI_FRAUD_CP) --reset \
	--scale $(SCALE) \
	--graph-seed $(GRAPH_SEED) \
	--fanout-k $(FANOUT_K) --cycle-len $(CYCLE_LEN) \
	--out $(OUT_FRAUD_CP) $(ARGS)

MEMORY_SHAPE_ARGS = --uri $(URI_MEMORY_SHAPE) --logical-mb $(LOGICAL_MB) --attributes $(ATTRIBUTES) \
	--text-indexes $(TEXT_INDEXES) --point-indexes $(POINT_INDEXES) --range-indexes $(RANGE_INDEXES) \
	$(ARGS)
# Logical size is per filled shape (1024×1024 bytes). At 32 MB, unindexed
# dense-ring RSS is about 1.3 GB, so the container default matches the other
# suites. A larger LOGICAL_MB needs a higher MEMORY_SHAPE_MEMORY.
LOGICAL_MB ?= 32
MEMORY_SHAPE_MEMORY ?= 2g
# Integer properties on every node. The first is `id`.
ATTRIBUTES ?= 1
# Indexed attributes per node, on every filled shape. Text and point are extra
# properties. Range indexes the first ATTRIBUTES integer properties.
TEXT_INDEXES ?= 0
POINT_INDEXES ?= 0
RANGE_INDEXES ?= 0

export BASELINE_CPUS BASELINE_MEMORY BASELINE_BUILD_IMAGE BASELINE_RUN_IMAGE CARGO_PROFILE

## fraud-loadtest: In-tree fraud mixed-payments loadtest (ADR-0070 Docker env)
fraud-loadtest:
ifeq ($(BASELINE_ENV),host)
	cargo run -p relate-datasets $(CARGO_PROFILE) --bin fraud-loadtest -- $(FRAUD_LOADTEST_ARGS)
else ifeq ($(BASELINE_ENV),docker)
	$(BASELINE_ENV_SH) build fraud-loadtest
	$(BASELINE_ENV_SH) run fraud-loadtest $(FRAUD_LOADTEST_ARGS)
else
	$(error BASELINE_ENV must be docker or host (got '$(BASELINE_ENV)'))
endif

## identity-loadtest: In-tree identity loadtest (ADR-0070 Docker env)
identity-loadtest:
ifeq ($(BASELINE_ENV),host)
	cargo run -p relate-datasets $(CARGO_PROFILE) --bin identity-loadtest -- $(IDENTITY_LOADTEST_ARGS)
else ifeq ($(BASELINE_ENV),docker)
	$(BASELINE_ENV_SH) build identity-loadtest
	$(BASELINE_ENV_SH) run identity-loadtest $(IDENTITY_LOADTEST_ARGS)
else
	$(error BASELINE_ENV must be docker or host (got '$(BASELINE_ENV)'))
endif

## fraud-checkpoint: Load fraud graph and checkpoint (ADR-0070 Docker env)
fraud-checkpoint:
ifeq ($(BASELINE_ENV),host)
	cargo run -p relate-datasets $(CARGO_PROFILE) --bin fraud-checkpoint -- $(FRAUD_CHECKPOINT_ARGS)
else ifeq ($(BASELINE_ENV),docker)
	$(BASELINE_ENV_SH) build fraud-checkpoint
	$(BASELINE_ENV_SH) run fraud-checkpoint $(FRAUD_CHECKPOINT_ARGS)
else
	$(error BASELINE_ENV must be docker or host (got '$(BASELINE_ENV)'))
endif

## memory-shape-loadtest: serve-process RSS vs logical bytes per graph shape (ADR-0070)
##   The bolt cases spawn `relate serve --bolt PORT --logical-graph-size-limit …` (the
##   standalone relate-bolt server is retired, ADR-0368; serve carries the
##   storage flag since ADR-0390, LABS-424).
memory-shape-loadtest:
ifeq ($(BASELINE_ENV),host)
	cargo build $(CARGO_PROFILE) -p relate-cli --bin relate
	cargo build $(CARGO_PROFILE) -p relate-datasets --bin memory-shape-loadtest
	cargo run -p relate-datasets $(CARGO_PROFILE) --bin memory-shape-loadtest -- --out $(OUT_MEMORY_SHAPE) $(MEMORY_SHAPE_ARGS)
else ifeq ($(BASELINE_ENV),docker)
	@echo "memory-shape  logical $(LOGICAL_MB) MB  attributes $(ATTRIBUTES)  text $(TEXT_INDEXES)  point $(POINT_INDEXES)  range $(RANGE_INDEXES)  container $(MEMORY_SHAPE_MEMORY)  uri $(URI_MEMORY_SHAPE)"
	BASELINE_MEMORY=$(MEMORY_SHAPE_MEMORY) HOST_OUT=$(OUT_MEMORY_SHAPE) MEMORY_SHAPE_URI=$(URI_MEMORY_SHAPE) bash $(CURDIR)/scripts/memory-shape-docker.sh $(MEMORY_SHAPE_ARGS)
else
	$(error BASELINE_ENV must be docker or host (got '$(BASELINE_ENV)'))
endif

OUT_MEMORY_CONTROLS ?= target/memory-controls.json
MEMORY_CONTROLS_RUN ?= $(shell date +%Y%m%dT%H%M%S)
URI_MEMORY_CONTROLS ?= /tmp/memory-controls-$(MEMORY_CONTROLS_RUN)
GRAPH_SCALE ?= 10
READ_WRITE_RATIO ?= 80:20
MAX_CONNECTIONS ?= 0
CONCURRENT_LOGICAL_WORKSPACE_LIMIT ?= 55829120
WARMUP_OPS ?= 8
INDEX_MODE ?= both
# WORKERS, OPS, and WORKLOAD_SEED are the fraud and identity knobs, set
# earlier in this file. A later ?= cannot replace them, and a later = would
# change those lanes. These apply only while this target's recipe runs, and
# a value on the make command line still wins.
memory-controls-test: WORKERS = 16
memory-controls-test: OPS = 40
memory-controls-test: WORKLOAD_SEED = 1
READ_QUERY ?= mix
CALIBRATION_REPEATS ?= 3
CALIBRATION_HOLD_MS ?= 2500
CALIBRATION_FAMILY ?=
# pr is one repeat of every family (25 observations). full is the three-repeat
# matrix (75 observations) used by the cadence and the baseline freeze.
ifeq ($(MEMORY_CONTROLS_PROFILE),pr)
CALIBRATION_REPEATS = 1
endif
ifeq ($(MEMORY_CONTROLS_PROFILE),full)
CALIBRATION_REPEATS = 3
endif
# CASE selects one case. PROBE and SKIP_PROBES remain as the previous names.
CASE ?= $(PROBE)
SKIP_CONTROLS ?= $(SKIP_PROBES)
ifeq ($(CASE),connections)
CASE := connection-cap
endif
ifeq ($(CASE),peak-token-rss)
CASE := workspace-memory-calibration
endif
MEMORY_CONTROLS_ARGS = \
	--graph-scale $(GRAPH_SCALE) \
	--workers $(WORKERS) \
	--read-write-ratio $(READ_WRITE_RATIO) \
	--max-connections $(MAX_CONNECTIONS) \
	--concurrent-logical-workspace-limit $(CONCURRENT_LOGICAL_WORKSPACE_LIMIT) \
	--ops $(OPS) \
	--warmup-ops $(WARMUP_OPS) \
	--workload-seed $(WORKLOAD_SEED) \
	--index-mode $(INDEX_MODE) \
	--read-query $(READ_QUERY) \
	--calibration-repeats $(CALIBRATION_REPEATS) \
	--calibration-hold-ms $(CALIBRATION_HOLD_MS)
ifeq ($(SKIP_CONTROLS),1)
MEMORY_CONTROLS_ARGS += --skip-controls
endif
ifneq ($(CASE),)
MEMORY_CONTROLS_ARGS += --case $(CASE)
endif
ifneq ($(CALIBRATION_FAMILY),)
MEMORY_CONTROLS_ARGS += --calibration-family $(CALIBRATION_FAMILY)
endif

## memory-controls-test: memory control verifications and workspace memory calibration (ADR-0070)
##   Every case runs `relate serve`. The standalone relate-bolt binary
##   is retired (LABS-381, ADR-0368).
memory-controls-test:
ifeq ($(BASELINE_ENV),host)
	cargo build $(CARGO_PROFILE) -p relate-cli --bin relate
	cargo build $(CARGO_PROFILE) -p relate-datasets --bin memory-controls
	cargo run -p relate-datasets $(CARGO_PROFILE) --bin memory-controls -- --out $(OUT_MEMORY_CONTROLS) --uri $(URI_MEMORY_CONTROLS) $(MEMORY_CONTROLS_ARGS)
else ifeq ($(BASELINE_ENV),docker)
	@echo "memory-controls  container $(MEMORY_SHAPE_MEMORY)  scale $(GRAPH_SCALE)  workers $(WORKERS)  ratio $(READ_WRITE_RATIO)  index $(INDEX_MODE)  connections $(MAX_CONNECTIONS)  workspace $(CONCURRENT_LOGICAL_WORKSPACE_LIMIT)  uri $(URI_MEMORY_CONTROLS)  profile $(MEMORY_CONTROLS_PROFILE)"
	BASELINE_MEMORY=$(MEMORY_SHAPE_MEMORY) HOST_OUT=$(OUT_MEMORY_CONTROLS) MEMORY_CONTROLS_URI=$(URI_MEMORY_CONTROLS) bash $(CURDIR)/scripts/memory-controls-docker.sh $(MEMORY_CONTROLS_ARGS)
else
	$(error BASELINE_ENV must be docker or host (got '$(BASELINE_ENV)'))
endif

## processing-memory-loadtest: previous name of memory-controls-test
processing-memory-loadtest:
	@echo "processing-memory-loadtest was renamed to memory-controls-test"
	$(MAKE) memory-controls-test

# query-latency-profile knobs (ADR-0244): the query id is required.
SF ?= 0.1
SECONDS ?= 10
QUERY_LATENCY_PROFILE_ARGS ?=

# query-latency gate knobs (ADR-0245): the timed suite runs in the same
# Docker recipe as the loadtests but with its own memory cap (SF0.1 peaks
# above 2.7 GB during the load); the report path feeds both the freeze
# (make baseline-loadtests) and the compare (make gate).
QUERY_LATENCY_MEMORY ?= 6g
# The report-path knob (the one-knob model of the four loadtest
# siblings): the target owns the invocation, callers override the path.
# QUERY_LATENCY_ARGS carries only extras (--qids, --min-time, ...).
OUT_QUERY_LATENCY ?= target/query-latency/report-graph-benchmark-ldbc-sf$(SF).json
QUERY_LATENCY_ARGS ?=
# The suite invocation, composed once and consulted by both arms; the
# frozen envelope path, spelled once.
QUERY_LATENCY_RUN_ARGS = run --sf $(SF) --out $(OUT_QUERY_LATENCY) $(QUERY_LATENCY_ARGS)
QL_BASELINE ?= $(BASELINES)/query-latency-sf$(SF).json

## query-latency: The timed 30-query suite in the ADR-0245 baseline env
##   (Docker 2 vCPU / QUERY_LATENCY_MEMORY [6g]); BASELINE_ENV=host is a
##   debug escape whose reports are never frozen or gate-compared
query-latency:
ifeq ($(BASELINE_ENV),host)
	cargo run $(CARGO_PROFILE) -p relate-query-latency -- $(QUERY_LATENCY_RUN_ARGS)
else ifeq ($(BASELINE_ENV),docker)
	BASELINE_MEMORY=$(QUERY_LATENCY_MEMORY) PACKAGE=relate-query-latency \
		bash $(BASELINE_ENV_SH) build query-latency
	BASELINE_MEMORY=$(QUERY_LATENCY_MEMORY) PACKAGE=relate-query-latency \
		bash $(BASELINE_ENV_SH) run query-latency $(QUERY_LATENCY_RUN_ARGS)
else
	$(error BASELINE_ENV must be docker or host (got '$(BASELINE_ENV)'))
endif

## compare-query-latency: Compare the timed report to the frozen envelope
##   (ADR-0245 bands). The one skip rule lives here: until the first CI
##   freeze lands the envelope does not exist, and the compare reports the
##   skip and exits 0 (the memory-shape precedent, ADR-0289) — a missing
##   envelope must not redden every PR.
compare-query-latency:
	@if [ -f $(QL_BASELINE) ]; then $(PYTHON) $(CURDIR)/scripts/compare-query-latency.py --baseline $(QL_BASELINE) --report $(CURDIR)/target/query-latency/report-graph-benchmark-ldbc-sf$(SF).json; else echo "compare-query-latency: $(QL_BASELINE) missing (ADR-0245: skipped until the first CI freeze)"; fi

## query-latency-profile: Sample one graph-benchmark-ldbc query (QID=19 [SF=0.1] [SECONDS=10]) -> target/query-latency/profile/ (ADR-0244)
query-latency-profile:
	@test -n "$(QID)" || { echo "query-latency-profile: set QID (1-30), e.g. make query-latency-profile QID=19"; exit 2; }
	cargo run --profile profiling -p relate-query-latency -- profile --sf $(SF) --qid $(QID) --seconds $(SECONDS) $(QUERY_LATENCY_PROFILE_ARGS)

## bulk-loadtest: Bulk create/merge/update/delete per batch size (ADR-0070 Docker env)
bulk-loadtest:
ifeq ($(BASELINE_ENV),host)
	cargo run -p relate-datasets $(CARGO_PROFILE) --bin bulk-loadtest -- $(BULK_LOADTEST_ARGS)
else ifeq ($(BASELINE_ENV),docker)
	$(BASELINE_ENV_SH) build bulk-loadtest
	$(BASELINE_ENV_SH) run bulk-loadtest $(BULK_LOADTEST_ARGS)
else
	$(error BASELINE_ENV must be docker or host (got '$(BASELINE_ENV)'))
endif

## test-compare-loadtest: Unit-test the loadtest baseline comparator, the
##   comparison-document aggregator, the self-updating PR comment, the
##   worst-of-N freeze envelope, and the ADR-0245 query-latency gate
##   (freeze + compare)
test-compare-loadtest:
	$(PYTHON) $(CURDIR)/scripts/tests/test_compare_loadtest.py
	$(PYTHON) $(CURDIR)/scripts/tests/test_compare_baselines.py
	$(PYTHON) $(CURDIR)/scripts/tests/test_update_pr_comment.py
	$(PYTHON) $(CURDIR)/scripts/tests/test_compare_verdict.py
	$(PYTHON) $(CURDIR)/scripts/tests/test_freeze_baseline_envelope.py
	$(PYTHON) $(CURDIR)/scripts/tests/test_query_latency_gate.py
	$(PYTHON) $(CURDIR)/scripts/tests/test_compare_memory_controls.py
	$(PYTHON) $(CURDIR)/scripts/tests/test_compare_memory_shape.py

## test-adr-scripts: Unit-test the ADR number mint and the PR conflict
##   checker (scripts/adr_mint.py, scripts/check_adr_numbers.py)
test-adr-scripts:
	$(PYTHON) $(CURDIR)/scripts/tests/test_adr_mint.py
	$(PYTHON) $(CURDIR)/scripts/tests/test_check_adr_numbers.py

## test-bolt-compat-scripts: Unit-test the Bolt compat probe's framing,
##   masking, selectors, transcript rendering, manifest decode and findings
##   catalog (scripts/bolt_compat_probe.py and its modules)
test-bolt-compat-scripts:
	$(PYTHON) $(CURDIR)/scripts/tests/test_bolt_compat_probe.py

## test-release-scripts: Unit-test the release sweep's version-token rule
##   (scripts/version_tokens.py) and the CHANGELOG rotation
##   (scripts/changelog_rotate.py) that scripts/release-bump.sh runs,
##   and the bump script end to end (first-release and steady-state paths)
test-release-scripts:
	$(PYTHON) $(CURDIR)/scripts/tests/test_version_tokens.py
	$(PYTHON) $(CURDIR)/scripts/tests/test_pom_py.py
	$(PYTHON) $(CURDIR)/scripts/tests/test_changelog_rotate.py
	sh $(CURDIR)/scripts/tests/test_release_bump.sh
	sh $(CURDIR)/scripts/tests/test_toml_sh.sh

## test-front-bench-scripts: Unit-test the front-bench statement corpus
##   builder (scripts/front_bench_corpus.py): the Gherkin walk's container
##   sink, the doc-string dedent (textwrap.dedent parity), the semicolon
##   splitter, the keyword filter, and the one dedupe pass
test-front-bench-scripts:
	$(PYTHON) $(CURDIR)/scripts/tests/test_front_bench_corpus.py

## test-install-sh: Offline stub-curl harness for distribution/install.sh
test-install-sh:
	sh $(CURDIR)/scripts/tests/test_install_sh.sh

## test-stage-setup-scripts: Tests for the shared CI stage-setup scripts
##   (wait-for-port.sh, the wasm-toolchain and Docker-baseline wrappers)
##   and the .sonic/ci.yml drift-guard keeping each setup spelled once
test-stage-setup-scripts:
	sh $(CURDIR)/scripts/tests/test_stage_setup_scripts.sh

## test-baseline-scripts: The shared baseline container ceremony's rules
##   (scripts/baseline-container.sh: the --clear guard's authored refusals,
##   asserted without a Docker daemon)
test-baseline-scripts:
	sh $(CURDIR)/scripts/tests/test_baseline_container.sh

## test-makefile-targets: The Makefile's own inventory drift guards
##   (scripts/tests/test_makefile_targets.sh): every task target carries
##   a `## <name>:` annotation (the one inventory `help` greps, ANNOTATED
##   derives, and `.PHONY` consumes), no annotation names a missing
##   target, and make's own `.PHONY` is exactly that inventory
test-makefile-targets:
	sh $(CURDIR)/scripts/tests/test_makefile_targets.sh

## test-stamp-swift-package: The Swift package release-stamp rules
##   (scripts/tests/test_stamp_swift_package.sh, hermetic sandbox copy;
##   skips with one TAP line where `swift` is unavailable)
test-stamp-swift-package:
	sh $(CURDIR)/scripts/tests/test_stamp_swift_package.sh

## test-scripts: Every scripts/tests suite through one owner — the CI
##   lanes invoke this aggregate, not each suite, so a new script guard
##   extends the prerequisite list below instead of a lane's command
##   flow. The prerequisite list is the member inventory; membership is
##   enforced by test_makefile_targets.sh (every target whose recipe
##   runs a scripts/tests suite hangs off this list)
test-scripts: test-compare-loadtest test-install-sh test-stage-setup-scripts \
	test-baseline-scripts test-bolt-compat-scripts test-adr-scripts \
	test-release-scripts test-front-bench-scripts test-stamp-swift-package \
	test-makefile-targets

VERSION ?= $(shell sed -n 's/^version = "\([^"]*\)".*/\1/p' Cargo.toml | head -1)

## docker: Build the relate-bolt server image (Dockerfile, ADR-0128;
##   unified `relate serve` runtime since ADR-0249)
##   DOCKER_BUILD_FLAGS="..." passes extra flags to docker build (CI adds
##   the gha cache backend there; leave empty for a plain local build).
##   DOCKER_PREBUILT_DIR=<dir> skips the in-container chef build and copies
##   prebuilt static binaries (relate, entrypoint) from
##   <dir> instead — e.g. staged by `scripts/prebuilt-bin.sh --release --bolt`
##   or extracted from a previously built image (ADR-0251 §5,
##   docs/testing/build-caches.md). The default in-container build stays the
##   from-scratch guarantee; the prebuilt path is the opt-in fast lane.
DOCKER_BUILD_FLAGS ?=
docker:
	mkdir -p target/docker-prebuilt
ifdef DOCKER_PREBUILT_DIR
	cp $(DOCKER_PREBUILT_DIR)/relate $(DOCKER_PREBUILT_DIR)/entrypoint target/docker-prebuilt/
endif
	token="$${GH_TOKEN:-$${GITHUB_TOKEN:-}}"; \
	secret=""; \
	gha=""; \
	if [ -n "$$token" ] && [ "$$token" != "GITHUB_TOKEN" ] && [ "$$token" != "GH_TOKEN" ]; then \
		export GITHUB_TOKEN="$$token"; \
		secret="--secret id=GITHUB_TOKEN,env=GITHUB_TOKEN"; \
	fi; \
	if [ "$${GITHUB_ACTIONS:-}" = "true" ]; then \
		gha="--build-arg GITHUB_ACTIONS=true"; \
	fi; \
	docker build $$secret $$gha $(DOCKER_BUILD_FLAGS) $(if $(DOCKER_PREBUILT_DIR),--build-arg BINARY_SOURCE=prebuilt,) -t relate-bolt:$(VERSION) -t relate-bolt:latest .

## prebuilt: Stage the relate binary for the current commit into
##   target/.prebuilt/<sha>[-dirty]/<profile>/ (ADR-0251 §5,
##   scripts/prebuilt-bin.sh); reuse it via
##   `export RELATE_BIN="$(scripts/prebuilt-bin.sh)"`. PREBUILT_PROFILE=
##   release stages the release binary.
PREBUILT_PROFILE ?=
prebuilt:
	@bash $(CURDIR)/scripts/prebuilt-bin.sh $(if $(PREBUILT_PROFILE),--$(PREBUILT_PROFILE),)

## docker-run: Run the server image (Bolt on 7687, HTTP on 7474, MCP on
##   7475; data volume in target/docker-data)
docker-run:
	mkdir -p target/docker-data
	docker run --rm -p 7687:7687 -p 7474:7474 -p 7475:7475 -v $(CURDIR)/target/docker-data:/data relate-bolt:$(VERSION)
