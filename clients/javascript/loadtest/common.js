"use strict";

const fs = require("fs");
const path = require("path");

const { Database, RelateError } = require("../index");

const BATCH = 200;
const REPORT_SCHEMA_VERSION = 5;
const ENGINE_VERSION = "0.1.0";

function mulberry32(a) {
  return function rng() {
    let t = (a += 0x6d2b79f5);
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function intRange(rng, n) {
  return Math.floor(rng() * n);
}

function writeQuery(conn, cypher, params) {
  let last;
  for (let i = 0; i < 8; i++) {
    try {
      return conn.query(cypher, params);
    } catch (e) {
      last = e;
      // The retry decision is the engine's own classification (ADR-0502 §2):
      // the typed error's `retryable` flag — the write-write conflict and
      // the retryable workspace cap are true, everything else fails loudly.
      // (v1 matched the conflict by message text; the flag replaced it.)
      if (!(e instanceof RelateError && e.retryable)) throw e;
    }
  }
  throw last;
}

function unwind(conn, rows, cypher) {
  for (let i = 0; i < rows.length; i += BATCH) {
    writeQuery(conn, cypher, { rows: rows.slice(i, i + BATCH) });
  }
}

/// The op-exec reporting policy both workloads consult, stated once (the
/// JS twin of the Go ops.go helpers, LABS-487): every op's exec returns its
/// statement count, its row count, and — only for the op that grows the
/// pools — its payload fields. The statement count is the op's TRUE
/// count, never floored (a 0 is the op's own data, surfacing a broken
/// planner), and a failed op reports 0 statements — the count is work
/// that executed, not the plan (runMixedLoadtest's catch below records
/// that 0). The payload is gated at the producer: a
/// thrown query error propagates to runMixedLoadtest's catch before a
/// result exists, so a payload always means created. readOp and writeOp
/// construct the single-statement ops the fused catalog rows return;
/// writeOp's `extra` spreads the op's payload fields into its result.
function readOp(cypher, params) {
  return {
    exec(conn) {
      return { statements: 1, rows: conn.query(cypher, params).rows.length };
    },
  };
}

function writeOp(cypher, params, extra = {}) {
  return {
    exec(conn) {
      writeQuery(conn, cypher, params);
      return { statements: 1, rows: 0, ...extra };
    },
  };
}

function resetUri(uri, walDir) {
  if (walDir) {
    fs.rmSync(walDir, { recursive: true, force: true });
  }
  if (!uri || uri === ":memory:") return;
  fs.rmSync(uri.replace(/^file:\/\//, ""), { recursive: true, force: true });
}

function pickWeighted(rng, catalog) {
  const total = catalog.reduce((s, c) => s + c.weight, 0);
  let x = intRange(rng, total);
  for (const c of catalog) {
    if (x < c.weight) return c;
    x -= c.weight;
  }
  return catalog[0];
}

function pickByWriteFraction(rng, writeFraction, catalog) {
  const wantWrite = rng() < writeFraction;
  const pool = catalog.filter((c) => (c.kind === "write") === wantWrite);
  return pickWeighted(rng, pool.length ? pool : catalog);
}

function validateWriteFraction(writeFraction) {
  if (!(writeFraction >= 0 && writeFraction <= 1)) {
    throw new Error(`--write-fraction must be between 0.0 and 1.0 (got ${writeFraction})`);
  }
  return writeFraction;
}

function percentiles(xs) {
  if (!xs.length) return null;
  const v = xs.slice().sort((a, b) => a - b);
  const pct = (p) => v[Math.min(v.length - 1, Math.round((v.length - 1) * p))];
  const sum = v.reduce((s, x) => s + x, 0);
  return { min: v[0], max: v[v.length - 1], mean: sum / v.length, p50: pct(0.5), p95: pct(0.95), p99: pct(0.99) };
}

function summarize(ops, catalog) {
  const out = [];
  for (const c of catalog) {
    const measured = ops.filter((o) => o.phase === "measured" && o.usecase === c.id);
    if (!measured.length) continue;
    const errors = measured.filter((o) => !o.ok).length;
    const lats = measured.filter((o) => o.ok).map((o) => o.latency_ns);
    const row = {
      id: c.id,
      kind: c.kind,
      description: c.description,
      ops: measured.length,
      errors,
      statements: measured.reduce((s, o) => s + o.statements, 0),
      related: c.related,
      latency_ns: percentiles(lats),
    };
    out.push(row);
  }
  return out;
}

function throughputFrom(ops, workerMs) {
  const secs = Math.max(workerMs, 1) / 1000;
  let read_ops = 0;
  let write_ops = 0;
  for (const o of ops) {
    if (o.phase !== "measured") continue;
    if (o.kind === "read") read_ops++;
    else write_ops++;
  }
  const n = read_ops + write_ops;
  return {
    ops_per_sec: n / secs,
    reads_per_sec: read_ops / secs,
    writes_per_sec: write_ops / secs,
    read_ops,
    write_ops,
  };
}

function writeReport(file, report) {
  const dir = path.dirname(file);
  if (dir && dir !== ".") fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(file, JSON.stringify(report, null, 2));
}

function clock() {
  const d = new Date();
  return { rfc: d.toISOString(), unixMs: d.getTime() };
}

function scalarInt(conn, cypher) {
  const r = conn.query(cypher);
  const v = r.rows[0] && Object.values(r.rows[0])[0];
  return Number(v || 0);
}

function parseArgs(argv, defaults) {
  const out = { ...defaults };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--help" || a === "-h") {
      out.help = true;
      continue;
    }
    if (a === "--reset") {
      out.reset = true;
      continue;
    }
    const m = a.match(/^--([^=]+)(?:=(.*))?$/);
    if (!m) throw new Error(`unexpected argument: ${a}`);
    const key = m[1];
    const raw = m[2] !== undefined ? m[2] : argv[++i];
    if (raw === undefined) throw new Error(`missing value for --${key}`);
    const camel = key.replace(/-([a-z])/g, (_, c) => c.toUpperCase());
    if (!(camel in defaults)) throw new Error(`unknown flag --${key}`);
    const prev = out[camel];
    out[camel] = typeof prev === "number" ? Number(raw) : raw;
  }
  if (out.workers < 1) out.workers = 1;
  if (typeof out.writeFraction === "number") validateWriteFraction(out.writeFraction);
  return out;
}

function warnWalDir(walDir) {
  if (walDir) {
    console.error("warning: --wal-dir is ignored (C ABI relate_database_open has no WAL directory)");
  }
}

function applyLogicalGraphSizeLimit(size) {
  if (size) process.env.RELATE_LOGICAL_GRAPH_SIZE_LIMIT = String(size);
}

function emptyMemory() {
  return {
    after_open: { rss_bytes: 0, peak_rss_bytes: 0, rss_per_node: 0, rss_per_relationship: 0, rss_per_entity: 0 },
    start: { rss_bytes: 0, peak_rss_bytes: 0, rss_per_node: 0, rss_per_relationship: 0, rss_per_entity: 0 },
    finish: { rss_bytes: 0, peak_rss_bytes: 0, rss_per_node: 0, rss_per_relationship: 0, rss_per_entity: 0 },
    graph: { rss_bytes: 0, bytes_per_node: 0, bytes_per_relationship: 0, bytes_per_entity: 0 },
    workload_delta: { rss_bytes: 0, nodes: 0, relationships: 0, rss_bytes_per_new_entity: 0 },
    logical_used_start_bytes: 0,
    logical_used_bytes: 0,
    logical_limit_bytes: null,
  };
}

function runSerial(warmup, ops, exec) {
  const records = [];
  const total = warmup + ops;
  for (let seq = 1; seq <= total; seq++) {
    const phase = seq <= warmup ? "warmup" : "measured";
    records.push(exec(seq, phase, 0));
  }
  return records;
}

function printMix(r, out, extra) {
  console.log(`wrote ${out} (${r.config.ops} measured ops, ${r.config.workers} workers, ${r.usecases.length} usecases)`);
  console.log(`load ${r.load_duration_ms} ms  workers ${r.worker_duration_ms} ms  total ${r.duration_ms} ms`);
  console.log(
    `throughput ${r.throughput.ops_per_sec.toFixed(1)} ops/s  writes ${r.throughput.writes_per_sec.toFixed(1)}/s (${r.throughput.write_ops} ops)  reads ${r.throughput.reads_per_sec.toFixed(1)}/s (${r.throughput.read_ops} ops)`
  );
  if (extra) console.log(extra);
  for (const u of r.usecases) {
    if (!u.latency_ns) continue;
    const pad = u.id.padEnd(22);
    console.log(
      `  ${pad} ${String(u.ops).padStart(4)} ops  p50=${(u.latency_ns.p50 / 1e6).toFixed(3)}ms p95=${(u.latency_ns.p95 / 1e6).toFixed(3)}ms errors=${u.errors}`
    );
  }
}

/// The shared mixed-loadtest orchestration (fraud and identity differ only
/// in their workload pieces): setup, load phase, the timed run with its
/// try/catch record builder, and the report envelope. Each script supplies
/// its spec: { name, configExtras, generate, load, snapshot, makeWork,
/// catalog, applyResult }. The catalog is fused: every row carries its own
/// plan(work) returning an op object with its own exec(conn) step, so the
/// orchestration dispatches through the picked row itself — no id-keyed
/// plan or exec switch here or in the workload scripts (LABS-429, LABS-487).
function runMixedLoadtest(cfg, spec) {
  const writeFraction = validateWriteFraction(cfg.writeFraction ?? 0.5);
  warnWalDir(cfg.walDir);
  applyLogicalGraphSizeLimit(cfg.logicalGraphSizeLimit);
  const started = clock();
  const wall0 = Date.now();
  const load0 = Date.now();
  const graph = spec.generate(cfg);
  if (cfg.reset) resetUri(cfg.uri, cfg.walDir);
  const db = Database.open(cfg.uri);
  const conn = db.connect();
  spec.load(conn, graph);
  const start = spec.snapshot(conn);
  const loadMs = Date.now() - load0;
  const work = spec.makeWork(graph, cfg.workloadSeed);
  const worker0 = Date.now();
  const records = runSerial(cfg.warmup, cfg.ops, (seq, phase, worker) => {
    const uc = pickByWriteFraction(work.rng, writeFraction, spec.catalog);
    const planned = uc.plan(work);
    const t0 = process.hrtime.bigint();
    // One record literal for both outcomes; only the outcome-specific fields differ.
    const record = (outcome) => ({
      seq, phase, usecase: uc.id, kind: uc.kind, related: uc.related,
      latency_ns: Number(process.hrtime.bigint() - t0), worker, ...outcome,
    });
    try {
      const out = planned.exec(conn);
      spec.applyResult(work, out);
      return record({ statements: out.statements, ok: true, rows: out.rows });
    } catch (e) {
      return record({ statements: 0, ok: false, error: String(e.message || e), rows: 0 });
    }
  });
  const workerMs = Date.now() - worker0;
  const finish = spec.snapshot(conn);
  const finished = clock();
  const report = {
    schema_version: REPORT_SCHEMA_VERSION,
    name: spec.name,
    started_at: started.rfc,
    finished_at: finished.rfc,
    started_at_unix_ms: started.unixMs,
    finished_at_unix_ms: finished.unixMs,
    duration_ms: Date.now() - wall0,
    load_duration_ms: loadMs,
    worker_duration_ms: workerMs,
    engine: "relate",
    engine_version: ENGINE_VERSION,
    config: {
      uri: cfg.uri,
      scale: cfg.scale,
      graph_seed: cfg.graphSeed,
      workload_seed: cfg.workloadSeed,
      ops: cfg.ops,
      warmup: cfg.warmup,
      workers: cfg.workers,
      write_fraction: writeFraction,
      ...spec.configExtras(cfg),
    },
    planted: graph.planted,
    dataset: { start, finish },
    throughput: throughputFrom(records, workerMs),
    memory: emptyMemory(),
    usecases: summarize(records, spec.catalog),
    ops: records,
  };
  if (cfg.out) writeReport(cfg.out, report);
  return report;
}

module.exports = {
  runMixedLoadtest,
  BATCH,
  REPORT_SCHEMA_VERSION,
  ENGINE_VERSION,
  mulberry32,
  intRange,
  writeQuery,
  unwind,
  readOp,
  writeOp,
  resetUri,
  pickWeighted,
  pickByWriteFraction,
  validateWriteFraction,
  percentiles,
  summarize,
  throughputFrom,
  writeReport,
  clock,
  scalarInt,
  parseArgs,
  warnWalDir,
  applyLogicalGraphSizeLimit,
  emptyMemory,
  runSerial,
  printMix,
};
