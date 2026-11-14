package loadtest

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/relatedb/relate/clients/go"
)

// runSmoke drives the shared smoke scaffolding — one temp report path, the
// fixed small mix, the run, the mix assertions, and the report-file check —
// and lets each workload supply only its own expectations.
func runSmoke[R reportView](t *testing.T, cfg Config, run func(Config) (R, error), check func(*testing.T, R)) {
	t.Helper()
	cfg.Ops, cfg.Warmup, cfg.Workers = 32, 4, 2
	cfg.Out = filepath.Join(t.TempDir(), "report.json")
	r, err := run(cfg)
	if err != nil {
		t.Fatal(err)
	}
	assertMix(t, r, cfg.Ops, cfg.Warmup)
	check(t, r)
	if _, err := os.Stat(cfg.Out); err != nil {
		t.Fatal(err)
	}
}

func TestFraudLoadtestSmoke(t *testing.T) {
	runSmoke(t, DefaultFraud(), RunFraud, func(t *testing.T, r *Report[FraudSnapshot, FraudReportConfig, FraudPlanted]) {
		if r.Name != "fraud-mixed-payments" || r.SchemaVersion != reportSchemaVersion {
			t.Fatalf("report meta: %+v", r)
		}
		if r.Planted.FanoutSource != "fanout-src" {
			t.Fatalf("planted: %+v", r.Planted)
		}
		if r.Dataset.Start.Accounts < 500 || r.Dataset.Start.Transfers == 0 {
			t.Fatalf("dataset: %+v", r.Dataset.Start)
		}
	})
}

func TestIdentityLoadtestSmoke(t *testing.T) {
	runSmoke(t, DefaultIdentity(), RunIdentity, func(t *testing.T, r *Report[IdentitySnapshot, IdentityReportConfig, IdentityPlanted]) {
		if r.Name != "identity-mixed-iam" || r.SchemaVersion != reportSchemaVersion {
			t.Fatalf("report meta: %+v", r)
		}
		if r.Planted.NestedAdmin != "nested-admin" {
			t.Fatalf("planted: %+v", r.Planted)
		}
		if r.Dataset.Start.Users < 500 || r.Dataset.Start.Memberships == 0 {
			t.Fatalf("dataset: %+v", r.Dataset.Start)
		}
	})
}

// The two report types are distinct generic instantiations; the shared
// checks read them through this view (no `any` assertions).
type reportView interface {
	opRecords() []OpRecord
	throughput() Throughput
	loadDurationMs() int64
	workerDurationMs() int64
}

func (r *Report[Snap, Cfg, Plant]) opRecords() []OpRecord   { return r.Ops }
func (r *Report[Snap, Cfg, Plant]) throughput() Throughput  { return r.Throughput }
func (r *Report[Snap, Cfg, Plant]) loadDurationMs() int64   { return r.LoadDurationMs }
func (r *Report[Snap, Cfg, Plant]) workerDurationMs() int64 { return r.WorkerDurationMs }

// mixedWorkloads is the workload pair every both-workloads check iterates:
// the runner plus its defaults, carried as data.
var mixedWorkloads = []struct {
	name     string
	run      func(Config) (reportView, error)
	defaults func() Config
}{
	{"fraud", func(c Config) (reportView, error) { return RunFraud(c) }, DefaultFraud},
	{"identity", func(c Config) (reportView, error) { return RunIdentity(c) }, DefaultIdentity},
}

// The pure-kind fractions are a cross product: workload pair x fraction —
// write-fraction 0 runs reads only, 1 writes only, on both workloads.
func TestWriteFractionPureKinds(t *testing.T) {
	fractions := []struct {
		writeFraction float64
		wantKind      OpKind
	}{
		{0, KindRead},
		{1, KindWrite},
	}
	for _, wl := range mixedWorkloads {
		for _, fr := range fractions {
			t.Run(fmt.Sprintf("%s write-fraction %v", wl.name, fr.writeFraction), func(t *testing.T) {
				cfg := wl.defaults()
				cfg.Ops = 12
				cfg.Warmup = 0
				cfg.WriteFraction = fr.writeFraction
				cfg.Out = ""
				r, err := wl.run(cfg)
				if err != nil {
					t.Fatal(err)
				}
				for _, op := range r.opRecords() {
					if op.Kind != fr.wantKind {
						t.Fatalf("write-fraction %v expected %s, got %+v", fr.writeFraction, fr.wantKind, op)
					}
				}
				// Every kind other than the wanted one must have zero ops.
				tp := r.throughput()
				for _, other := range []struct {
					kind  OpKind
					count int
				}{{KindRead, tp.ReadOps}, {KindWrite, tp.WriteOps}} {
					if other.kind != fr.wantKind && other.count != 0 {
						t.Fatalf("write-fraction %v: %s ops %d", fr.writeFraction, other.kind, other.count)
					}
				}
			})
		}
	}
}

// The Config boundary records the values the generator actually ran with:
// out-of-range knobs are normalized there, not silently inside the
// generator (which now sees only valid input).
func TestFraudConfigNormalizedInReport(t *testing.T) {
	cfg := DefaultFraud()
	cfg.Scale, cfg.FanoutK, cfg.CycleLen = 0.001, 1, 99
	cfg.Ops, cfg.Warmup, cfg.Out = 0, 0, ""
	r, err := RunFraud(cfg)
	if err != nil {
		t.Fatal(err)
	}
	if r.Config.Scale != minScale || r.Config.FanoutK != 3 || r.Config.CycleLen != 6 {
		t.Fatalf("normalized config: %+v", r.Config)
	}
	if r.Planted.FanoutK != 3 || len(r.Planted.CycleAccounts) != 6 {
		t.Fatalf("normalized planted: %+v", r.Planted)
	}
}

func TestIdentityConfigNormalizedInReport(t *testing.T) {
	cfg := DefaultIdentity()
	cfg.Scale, cfg.NestDepth, cfg.GroupK = 0.001, 1, 1
	cfg.Ops, cfg.Warmup, cfg.Out = 0, 0, ""
	r, err := RunIdentity(cfg)
	if err != nil {
		t.Fatal(err)
	}
	if r.Config.Scale != minScale || r.Config.NestDepth != 2 || r.Config.GroupK != 3 {
		t.Fatalf("normalized config: %+v", r.Config)
	}
	if r.Planted.NestDepth != 2 || r.Planted.GroupK != 3 || len(r.Planted.NestGroups) != 2 {
		t.Fatalf("normalized planted: %+v", r.Planted)
	}
}

// A non-finite scale has no normalized value, so it is rejected at the
// boundary rather than reaching the generator.
func TestNonFiniteScaleRejected(t *testing.T) {
	for _, tc := range []struct {
		name  string
		scale float64
	}{
		{"NaN", math.NaN()},
		{"+Inf", math.Inf(1)},
		{"-Inf", math.Inf(-1)},
	} {
		t.Run(tc.name, func(t *testing.T) {
			rejectOnBothWorkloads(t, "scale "+tc.name, func(c *Config) { c.Scale = tc.scale })
		})
	}
}

// The retry predicate reads the typed Retryable flag, never the message: an
// error that only repeats the conflict text must be final.
func TestConflictRetryUsesTypedFlag(t *testing.T) {
	conflict := &relate.Error{Code: "Neo.TransientError.Transaction.Outdated", GQLStatus: "40N00", Stage: relate.StageRuntime, Retryable: true, Message: "write-write conflict"}
	if !isRetryableConflict(conflict) {
		t.Fatalf("a retryable *relate.Error must be retryable")
	}
	if !isRetryableConflict(fmt.Errorf("wrapped: %w", conflict)) {
		t.Fatalf("a wrapped retryable *relate.Error must be retryable")
	}
	nonRetryable := &relate.Error{Code: "Neo.ClientError.Statement.ArithmeticError", Stage: relate.StageRuntime, Message: "write-write conflict"}
	for _, err := range []error{nonRetryable, errors.New("write-write conflict"), errors.New("write-write conflict at commit")} {
		if isRetryableConflict(err) {
			t.Fatalf("%v must not be retryable", err)
		}
	}
}

// The engine contract the collapsed grant load relies on: an absent
// UserRoleGrant.Until binds as null, date(null) is null, and CREATE
// omits a null property. Pin it at the engine boundary so the one-statement
// load cannot silently start storing a null `until`.
func TestUserRoleGrantNullUntilOmitted(t *testing.T) {
	db, err := relate.Open(":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	conn, err := db.Connect()
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()

	if err := unwind(conn,
		[]IdentityNamed{{ID: "user-a", Name: "A"}, {ID: "user-b", Name: "B"}},
		"UNWIND $rows AS u CREATE (:User {id: u.id, name: u.name})"); err != nil {
		t.Fatal(err)
	}
	if err := unwind(conn,
		[]IdentityNamed{{ID: "role-viewer", Name: "viewer"}, {ID: "role-editor", Name: "editor"}},
		"UNWIND $rows AS r CREATE (:Role {id: r.id, name: r.name})"); err != nil {
		t.Fatal(err)
	}
	until := staleUntil
	grants := []UserRoleGrant{
		{User: "user-a", Role: "role-viewer"},
		{User: "user-b", Role: "role-editor", Until: &until},
	}
	if err := unwind(conn, grants, loadUserRoleGrants); err != nil {
		t.Fatal(err)
	}

	keys := func(user string) []any {
		r, err := conn.QueryParams("MATCH (u:User {id: $u})-[h:HAS_ROLE]->() RETURN keys(h) AS k", map[string]any{"u": user})
		if err != nil {
			t.Fatal(err)
		}
		if len(r.Rows) != 1 {
			t.Fatalf("%s: expected one HAS_ROLE, got %d", user, len(r.Rows))
		}
		k, ok := r.Rows[0][0].([]any)
		if !ok {
			t.Fatalf("%s: keys(h) is %T, want a list", user, r.Rows[0][0])
		}
		return k
	}
	if k := keys("user-a"); len(k) != 0 {
		t.Fatalf("a null until must not be created, keys=%v", k)
	}
	if k := keys("user-b"); len(k) != 1 || k[0] != "until" {
		t.Fatalf("a dated until must be the one property, keys=%v", k)
	}
}

func TestCustomerSearchQ(t *testing.T) {
	if n, ok := customerSearchQ("cust-42"); !ok || n != "42" {
		t.Fatalf("cust-42: %q %v", n, ok)
	}
	if n, ok := customerSearchQ("cust-0"); !ok || n != "0" {
		t.Fatalf("cust-0: %q %v", n, ok)
	}
	if _, ok := customerSearchQ("cust-planted-fanout"); ok {
		t.Fatal("planted should not be a search needle")
	}
	if _, ok := customerSearchQ("lt-cust-3"); ok {
		t.Fatal("onboard id should not be a search needle")
	}
}

// The input is deliberately unsorted: the ordering contract (min before
// max, the median in the middle) can actually fail.
func TestPercentilesOrder(t *testing.T) {
	l := percentiles([]uint64{40, 10, 50, 20, 30})
	if l == nil || l.Min != 10 || l.Max != 50 || l.P50 != 30 {
		t.Fatalf("got %+v", l)
	}
}

// rejectOnBothWorkloads is the canonical both-workloads rejection check the
// validation tests share: a config mutation each runner must reject before
// any side effect.
func rejectOnBothWorkloads(t *testing.T, what string, mutate func(*Config)) {
	t.Helper()
	for _, wl := range mixedWorkloads {
		t.Run(wl.name, func(t *testing.T) {
			cfg := wl.defaults()
			mutate(&cfg)
			if _, err := wl.run(cfg); err == nil {
				t.Fatalf("%s: %s: expected rejection, got nil", wl.name, what)
			}
		})
	}
}

// The former behavior silently coerced --workers 0 to 1; it is now rejected
// before any side effect (the runner validates before the database opens).
func TestWorkersRejectedBelowOne(t *testing.T) {
	for _, tc := range []struct {
		name    string
		workers int
	}{
		{"zero", 0},
		{"negative", -2},
	} {
		t.Run(tc.name, func(t *testing.T) {
			rejectOnBothWorkloads(t, fmt.Sprintf("--workers %d", tc.workers), func(c *Config) { c.Workers = tc.workers })
		})
	}
}

// A negative --ops or --warmup would wrap the uint64 op-count cast and spin
// the worker loop essentially forever; it is now rejected pre-flight, like
// the workers count. Zero stays legal on both (warmup 0 is a normal config).
func TestCountsRejectedNegative(t *testing.T) {
	for _, tc := range []struct {
		name        string
		ops, warmup int
	}{
		{"negative ops", -10, 8},
		{"negative warmup", 12, -1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			rejectOnBothWorkloads(t, fmt.Sprintf("ops=%d warmup=%d", tc.ops, tc.warmup), func(c *Config) { c.Ops, c.Warmup = tc.ops, tc.warmup })
		})
	}
}

// The legal boundary is a property of the validator itself, not of a full
// pipeline run: ops 4 warmup 0 must pass.
func TestValidateCountsLegalBoundary(t *testing.T) {
	if err := validateCounts(4, 0); err != nil {
		t.Fatalf("ops 4 warmup 0 is legal: %v", err)
	}
}

func assertMix(t *testing.T, r reportView, ops, warmup int) {
	t.Helper()
	var measured, warm, reads, writes, related int
	for _, op := range r.opRecords() {
		if op.Phase == PhaseWarmup {
			warm++
			continue
		}
		if op.Phase != PhaseMeasured {
			t.Fatalf("op has a rogue phase: %+v", op)
		}
		measured++
		if !op.OK {
			t.Fatalf("op failed: %+v", op)
		}
		if op.LatencyNs == 0 {
			t.Fatalf("zero latency: %+v", op)
		}
		// The kind vocabulary mirrors the phase treatment above: a rogue
		// kind value fails loudly instead of being silently classified.
		if op.Kind == KindRead {
			reads++
		} else if op.Kind == KindWrite {
			writes++
		} else {
			t.Fatalf("op has a rogue kind: %+v", op)
		}
		if op.Related {
			related++
		}
	}
	if measured != ops || warm != warmup {
		t.Fatalf("counts measured=%d warmup=%d", measured, warm)
	}
	if reads == 0 || writes == 0 {
		t.Fatalf("expected mixed kinds reads=%d writes=%d", reads, writes)
	}
	if related == 0 {
		t.Fatalf("expected some planted-pattern ops")
	}
	tp := r.throughput()
	if tp.ReadOps != reads || tp.WriteOps != writes {
		t.Fatalf("throughput counts %+v vs %d/%d", tp, reads, writes)
	}
	if tp.OpsPerSec <= 0 || r.loadDurationMs() <= 0 || r.workerDurationMs() <= 0 {
		t.Fatalf("clocks/throughput %+v load=%d worker=%d", tp, r.loadDurationMs(), r.workerDurationMs())
	}
}

// fraudTestCLI and identityTestCLI drive the same descriptors the cmd
// mains ship (FraudCLI/IdentityCLI), overriding only the run/print pair, so
// a wrong name, summary, vocabulary, or flag table in a main fails a test.
func fraudTestCLI(run func(Config) (int, error)) CLI[int] {
	return FraudCLI(run, func(int, string, io.Writer) {})
}

func identityTestCLI(run func(Config) (int, error)) CLI[int] {
	return IdentityCLI(run, func(int, string, io.Writer) {})
}

// runCLIForTest drives the shared CLI frame in-process: the descriptor, the
// arguments and both output streams are inputs, so no subprocess or os.Args
// mutation is needed.
func runCLIForTest(t *testing.T, cli CLI[int], args []string) (int, string) {
	t.Helper()
	var stdout, stderr bytes.Buffer
	code := RunCLI(cli, args, &stdout, &stderr)
	return code, stderr.String()
}

func TestCLIHelpListsSharedAndWorkloadFlags(t *testing.T) {
	failRun := func(Config) (int, error) {
		t.Fatal("help must not run the workload")
		return 0, nil
	}
	for _, tc := range []struct {
		name      string
		cli       CLI[int]
		summary   string
		own       string // this workload's own flag
		other     string // the sibling workload's own flag, absent here
		scaleUnit string
	}{
		{"fraud", fraudTestCLI(failRun), "synthetic fraud graph", "--fanout-k", "--nest-depth", "accounts"},
		{"identity", identityTestCLI(failRun), "synthetic IAM identity graph", "--nest-depth", "--fanout-k", "users"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			code, stderr := runCLIForTest(t, tc.cli, []string{"--help"})
			if code != 0 {
				t.Fatalf("--help exit code %d", code)
			}
			for _, want := range []string{"--uri", "--scale", "--ops", "--help", tc.own, tc.scaleUnit, tc.summary} {
				if !strings.Contains(stderr, want) {
					t.Fatalf("help output missing %s:\n%s", want, stderr)
				}
			}
			if strings.Contains(stderr, tc.other) {
				t.Fatalf("help output leaks the sibling workload's %s:\n%s", tc.other, stderr)
			}
		})
	}
}

func TestCLIRejectsPositionals(t *testing.T) {
	code, stderr := runCLIForTest(t, fraudTestCLI(func(Config) (int, error) {
		t.Fatal("an unexpected operand must not run the workload")
		return 0, nil
	}), []string{"--", "stray"})
	if code != 2 {
		t.Fatalf("positional exit code %d", code)
	}
	if !strings.Contains(stderr, "unexpected positional argument(s): stray") {
		t.Fatalf("stderr: %s", stderr)
	}
}

func TestCLIUnknownFlagExitsUsage(t *testing.T) {
	code, _ := runCLIForTest(t, fraudTestCLI(func(Config) (int, error) {
		t.Fatal("an unknown flag must not run the workload")
		return 0, nil
	}), []string{"--nope"})
	if code != 2 {
		t.Fatalf("unknown-flag exit code %d", code)
	}
}

func TestCLIWarnsOnWalDir(t *testing.T) {
	code, stderr := runCLIForTest(t, fraudTestCLI(func(Config) (int, error) {
		return 0, nil
	}), []string{"--wal-dir", "/tmp/unused-wal", "--ops", "0", "--warmup", "0"})
	if code != 0 {
		t.Fatalf("exit code %d", code)
	}
	if !strings.Contains(stderr, "no WAL directory") {
		t.Fatalf("expected the --wal-dir warning, got: %s", stderr)
	}
}

// Both halves of the report's phase/kind vocabulary use one policy: the two
// legal values classify, and a rogue value fails loudly instead of being
// silently bucketed.
func TestMeasuredOpPolicy(t *testing.T) {
	if isMeasured(&OpRecord{Phase: PhaseWarmup}) {
		t.Fatal("a warmup op is not measured")
	}
	if !isMeasured(&OpRecord{Phase: PhaseMeasured}) {
		t.Fatal("a measured op is measured")
	}
	defer func() {
		if recover() == nil {
			t.Fatal("a rogue phase must panic")
		}
	}()
	isMeasured(&OpRecord{Phase: OpPhase("rogue")})
}

// The unmeasured memory block is the JS twin's emptyMemory(): every
// counter renders as zero and logical_limit_bytes as null, not omitted.
func TestUnmeasuredMemoryShape(t *testing.T) {
	raw, err := json.Marshal(MemoryReport{})
	if err != nil {
		t.Fatal(err)
	}
	var m map[string]any
	if err := json.Unmarshal(raw, &m); err != nil {
		t.Fatal(err)
	}
	for _, key := range []string{
		"after_open", "start", "finish", "graph", "workload_delta",
		"logical_used_start_bytes", "logical_used_bytes", "logical_limit_bytes",
	} {
		if _, ok := m[key]; !ok {
			t.Fatalf("empty memory is missing %q: %s", key, raw)
		}
	}
	if m["logical_limit_bytes"] != nil {
		t.Fatalf("logical_limit_bytes must be null, got %v", m["logical_limit_bytes"])
	}
}

func TestApplyLogicalGraphSizeLimitRestores(t *testing.T) {
	t.Setenv(logicalGraphSizeLimitEnv, "prior")
	restore, err := applyLogicalGraphSizeLimit("20mib")
	if err != nil {
		t.Fatal(err)
	}
	if got := os.Getenv(logicalGraphSizeLimitEnv); got != "20mib" {
		t.Fatalf("during run: %q", got)
	}
	restore()
	if got := os.Getenv(logicalGraphSizeLimitEnv); got != "prior" {
		t.Fatalf("after restore: %q", got)
	}
}

func TestApplyLogicalGraphSizeLimitClearsUnset(t *testing.T) {
	// t.Setenv captures the pre-test state (unset included) and restores
	// it at cleanup — the unset path is exercised with no hand-rolled
	// save/restore of the test's own.
	t.Setenv(logicalGraphSizeLimitEnv, "placeholder")
	os.Unsetenv(logicalGraphSizeLimitEnv)
	restore, err := applyLogicalGraphSizeLimit("20mib")
	if err != nil {
		t.Fatal(err)
	}
	restore()
	if _, ok := os.LookupEnv(logicalGraphSizeLimitEnv); ok {
		t.Fatalf("%s survived the restore", logicalGraphSizeLimitEnv)
	}
}

func TestPrintReportWritesToInjectedStream(t *testing.T) {
	r := &Report[FraudSnapshot, FraudReportConfig, FraudPlanted]{
		SchemaVersion: reportSchemaVersion,
		Name:          "test",
		Config:        FraudReportConfig{Ops: 3, Workers: 1},
		Throughput:    Throughput{OpsPerSec: 2},
	}
	var out bytes.Buffer
	PrintFraud(r, "report.json", &out)
	if got := out.String(); !strings.Contains(got, "wrote report.json (3 measured ops, 1 workers") {
		t.Fatalf("renderer output: %s", got)
	}
}

// LABS-1664: reporting_path's destination is always the org root, and the
// root is itself an ordinary user — a source that equals it plans a
// same-node shortestPath, which the engine correctly rejects with 51N23.
// Pin that no planned op ever does.
func TestReportingPathNeverPlansTheRootAsItsSource(t *testing.T) {
	// The knobs come from the one default config, not a second copy of it.
	cfg := DefaultIdentity()
	work := newIdentityWork(generateIdentity(cfg.GraphSeed, cfg.Scale, cfg.NestDepth, cfg.GroupK), cfg)
	if !slices.Contains(work.ordinaryUsers, rootUserID) {
		t.Fatal("user-0 must be in the ordinary pool for this pin to mean anything")
	}
	for range 2000 {
		planned := planReportingPath(work)
		op, ok := planned.(stmtOp)
		if !ok {
			t.Fatalf("reporting_path planned a %T, not a statement op", planned)
		}
		from, ok := op.params["from"].(string)
		if !ok {
			t.Fatalf("reporting_path bound a %T source, not a user id", op.params["from"])
		}
		if from == rootUserID {
			t.Fatalf("reporting_path planned the org root as its own source: %v", op.params)
		}
	}
}
