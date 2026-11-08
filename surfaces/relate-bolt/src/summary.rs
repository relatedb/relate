//! Bolt SUCCESS metadata maps: HELLO (ADR-0293, ADR-0312), RUN and the
//! result summaries (ADR-0309), bookmarks (ADR-0310), `qid` (ADR-0311) and
//! the ROUTE routing table (ADR-0269). The session decides when each is
//! sent; this module decides its keys and writes the SUCCESS message.

use std::io::Write;
use std::time::Duration;

use relate::{QueryResult, QueryStats};
use relate_model::wire::STAT_COUNTERS;

use crate::fields::NotificationConfig;
use crate::packstream::{encode_error, try_struct_msg, Pack};
use crate::protocol_notification_summary::notifications_summary_pair;
use crate::protocol_plan_summary::plan_summary_pair;
use crate::value_codec::ValueCodec;

/// One SUCCESS metadata map, in wire order.
pub(crate) type Metadata = Vec<(Pack, Pack)>;

/// Send one SUCCESS (0x70) message with `extra` as its metadata map: the
/// SUCCESS sender lives beside the metadata builders (ADR-0309).
pub(crate) fn send_success<W: Write>(stream: &mut W, extra: Metadata) -> std::io::Result<()> {
    let msg = try_struct_msg(0x70, vec![Pack::Map(extra)]).map_err(encode_error)?;
    stream.write_all(&msg)?;
    stream.flush()
}

fn entry(key: &str, value: Pack) -> (Pack, Pack) {
    (Pack::String(key.into()), value)
}

/// A PackStream Int from an unsigned count: the wire's `Int` is an i64 and
/// every count and millisecond total here is invariant-bound below it (the
/// executor accumulates i64 counters, and no elapsed duration reaches 2^63
/// ms), so an oversized value is a contract break and fails loudly rather
/// than fabricating a saturated maximum (AGENTS.md silent-fallback rule).
fn exact_int(n: u128) -> Pack {
    Pack::Int(i64::try_from(n).expect("an elapsed or count total fits an i64"))
}

/// Whole milliseconds, the unit of `t_first` and `t_last`.
fn millis(elapsed: Duration) -> Pack {
    exact_int(elapsed.as_millis())
}

/// Float seconds, the unit of the ADR-0491 phase keys:
/// `parsing_time`, `planning_time` and `plan_execution_time`
/// are PackStream Float seconds — tools parse them as doubles, so an
/// integer-millisecond number must never ride under these names beside
/// the `t_*` keys. The engine exposes the split only through its
/// millisecond accessors (`QueryResult::parse_ms` and friends — the one
/// public path, ADR-0491), so this rescales that one f64 fact; the
/// `Duration`→number conversion point stays the engine's.
fn seconds(ms: f64) -> Pack {
    Pack::Float(ms / 1000.0)
}

/// The HELLO `connection.recv_timeout_seconds` hint (ADR-0312): twice the
/// keep-alive interval, in whole seconds rounded up, so a client that waits
/// that long has missed two NOOPs.
pub(crate) fn recv_timeout_hint(keep_alive_interval: Duration) -> i64 {
    (keep_alive_interval * 2).as_secs_f64().ceil() as i64
}

/// HELLO SUCCESS: `server` and `connection_id` (ADR-0293) and the
/// receive-timeout hint (ADR-0312), with the caller's version-gated
/// `hints` entries beside it. Version-free by design: the version-gated
/// entries — the 5.x telemetry opt-out hint (ADR-0484 decision 5) —
/// arrive from the caller that owns the decision, `session_5x`'s HELLO
/// through the one-entry helper below, the same caller-supplied shape
/// `patch_bolt` uses.
pub(crate) fn hello(
    server: &str,
    connection_id: &str,
    keep_alive_interval: Duration,
    hints: Metadata,
) -> Metadata {
    let mut all_hints = vec![entry(
        "connection.recv_timeout_seconds",
        Pack::Int(recv_timeout_hint(keep_alive_interval)),
    )];
    all_hints.extend(hints);
    vec![
        entry("server", Pack::String(server.into())),
        entry("connection_id", Pack::String(connection_id.into())),
        entry("hints", Pack::Map(all_hints)),
    ]
}

/// The 5.x HELLO SUCCESS `telemetry.enabled` hint (ADR-0484 decision 5):
/// the server collects nothing, and the hint says so. It is the one
/// `hints` entry `session_5x`'s HELLO supplies — that handler runs only
/// behind the speaks_5x fork, so every HELLO it answers carries it, and
/// `summary::hello` stays version-free.
pub(crate) fn telemetry_opt_out_hint() -> (Pack, Pack) {
    entry("telemetry.enabled", Pack::Bool(false))
}

/// The HELLO SUCCESS `patch_bolt` entry (ADR-0329): the negotiated patch
/// names, echoed in the applied set's order, built through the same
/// `entry` helper as every other SUCCESS key. It is the one entry the
/// caller appends to its metadata; the caller builds it only when a patch
/// applied — the key is absent otherwise.
pub(crate) fn patch_bolt(names: &[&str]) -> (Pack, Pack) {
    entry(
        "patch_bolt",
        Pack::List(
            names
                .iter()
                .map(|name| Pack::String((*name).into()))
                .collect(),
        ),
    )
}

/// RUN SUCCESS (ADR-0309): `fields`, the ADR-0224 `columnTypes` extension
/// key and `t_first`, plus `qid` inside an explicit transaction (ADR-0311).
pub(crate) fn run(
    columns: &[String],
    column_types: &[String],
    t_first: Duration,
    qid: Option<i64>,
) -> Metadata {
    let strings = |names: &[String]| Pack::List(names.iter().cloned().map(Pack::String).collect());
    let mut metadata = vec![
        entry("fields", strings(columns)),
        entry("columnTypes", strings(column_types)),
        entry("t_first", millis(t_first)),
    ];
    if let Some(qid) = qid {
        metadata.push(entry("qid", Pack::Int(qid)));
    }
    metadata
}

/// A PULL or DISCARD that leaves records unread: exactly `{has_more: true}`
/// (ADR-0309).
pub(crate) fn has_more() -> Metadata {
    vec![entry("has_more", Pack::Bool(true))]
}

/// The SUCCESS that ends a result (ADR-0309, ADR-0357): `bookmark` when
/// the result's auto-commit statement committed — the engine's
/// [`QueryResult::bookmark`], the store's commit sequence (ADR-0310,
/// ADR-0502 §7), absent on a read, an empty write and every statement
/// inside an explicit transaction — `stats` when any counter is non-zero,
/// then `type`, `t_first`, `t_last`, the ADR-0491 phase split
/// (`parsing_time`, `planning_time`, `plan_execution_time` — PackStream
/// Float seconds, the ADR-0491 contract keys, beside `t_last` and never
/// in RUN SUCCESS) and `db` — Neo4j's placement, so a
/// strict client that reads only the drained result's summary
/// (cypher-shell) renders its `Time` from here — plus the query
/// notifications (ADR-0363 decision 11, filtered by the statement's
/// ADR-0484 decision 8 config) and the EXPLAIN/PROFILE plan pair
/// (ADR-0344/0345, ADR-0353, ADR-0357) when the result carries them. Both
/// pairs are built here from the result through their one policies; the
/// plan value encodes under `codec` — the connection's value codec
/// (ADR-0484), the same policy its RECORD rows used, so no engine value
/// reaches the wire under a second one. The notifications entry is omitted
/// when the list is empty or the filter dropped it all, and both keys are
/// absent from RUN SUCCESS. A
/// path-invariant break in the plan value is an engine contract break kept
/// loud through the returned io error.
pub(crate) fn ending(
    result: &QueryResult,
    t_last: Duration,
    codec: ValueCodec,
    notification_config: &NotificationConfig,
) -> std::io::Result<Metadata> {
    // The plan pair can fail (a path-invariant break), so build both pairs
    // before any metadata is assembled.
    let plan = plan_summary_pair(result.plan_root(), codec)?;
    let notifications = notifications_summary_pair(result.notifications(), notification_config);
    let mut metadata = Vec::new();
    if let Some(bookmark) = &result.bookmark {
        metadata.push(entry("bookmark", Pack::String(bookmark.clone())));
    }
    if let Some(stats) = stats(&result.stats) {
        metadata.push(entry("stats", stats));
    }
    metadata.push(entry(
        "type",
        Pack::String(result.header.query_type.as_str().into()),
    ));
    metadata.push(entry("t_first", millis(result.header.elapsed)));
    metadata.push(entry("t_last", millis(t_last)));
    // ADR-0491: the statement's PARSE/PLAN/EXECUTE split rides the ending
    // SUCCESS beside `t_last`, under the ADR-0491 contract's keys and unit
    // (Float seconds) — the names and types established console tooling
    // already renders. The split is read from the same result whose total
    // `t_first` reports, so the three are additive context whose sum never
    // exceeds that total; RUN SUCCESS stays without them (its ADR-0309
    // contract is unchanged).
    metadata.push(entry("parsing_time", seconds(result.parse_ms())));
    metadata.push(entry("planning_time", seconds(result.plan_ms())));
    metadata.push(entry("plan_execution_time", seconds(result.execute_ms())));
    // LABS-1120: the fine sub-split rides the same family (Float seconds,
    // sub-window names of the three above) plus the plan-cache outcome —
    // the fact that explains a hit's near-zero `parsing_time`.
    metadata.push(entry("lex_parse_time", seconds(result.lex_parse_ms())));
    metadata.push(entry("rewrite_time", seconds(result.rewrite_ms())));
    metadata.push(entry("lower_time", seconds(result.lower_ms())));
    metadata.push(entry(
        "result_generation_time",
        seconds(result.result_generation_ms()),
    ));
    metadata.push(entry(
        "plan_cache",
        Pack::String(result.plan_cache().as_str().into()),
    ));
    metadata.push(entry(
        "db",
        Pack::String(result.header.namespace.to_string()),
    ));
    if let Some(pair) = notifications {
        metadata.push(pair);
    }
    if let Some(pair) = plan {
        metadata.push(pair);
    }
    Ok(metadata)
}

/// COMMIT SUCCESS: the bookmark the engine's commit returned (ADR-0310,
/// ADR-0502 §7); `{}` when the transaction committed nothing, so no
/// bookmark is fabricated for a commit that crossed no WAL boundary.
pub(crate) fn commit(bookmark: Option<String>) -> Metadata {
    bookmark
        .map(|bookmark| entry("bookmark", Pack::String(bookmark)))
        .into_iter()
        .collect()
}

/// ROUTE SUCCESS (ADR-0269): the `rt` routing table, built through the same
/// `entry` helper as every other SUCCESS key.
pub(crate) fn route(self_address: &str, db: &str) -> Metadata {
    vec![entry("rt", routing_table(self_address, db))]
}

/// The `ttl` (seconds) of the ROUTE routing table (ADR-0269). There is no
/// topology to age out; 300 matches the served idle-timeout default.
pub(crate) const ROUTE_TTL_SECS: i64 = 300;

/// The single-server self routing table answered to ROUTE (ADR-0269):
/// every role points back at this server, listed WRITE, READ, ROUTE as
/// Neo4j lists them (ADR-0318). `self_address` is the address the
/// client dialed, else the connection's local socket address (ADR-0140) —
/// the address decision is made at the transport boundary, so this always
/// advertises exactly one address. The public path stays
/// `protocol::routing_table` (ADR-0281), re-exported by `protocol.rs`.
pub fn routing_table(self_address: &str, db: &str) -> Pack {
    let addresses = vec![Pack::String(self_address.into())];
    let server = |role: &str| {
        Pack::Map(vec![
            (Pack::String("role".into()), Pack::String(role.into())),
            (
                Pack::String("addresses".into()),
                Pack::List(addresses.clone()),
            ),
        ])
    };
    Pack::Map(vec![
        (Pack::String("ttl".into()), Pack::Int(ROUTE_TTL_SECS)),
        (
            Pack::String("servers".into()),
            Pack::List(vec![server("WRITE"), server("READ"), server("ROUTE")]),
        ),
        (Pack::String("db".into()), Pack::String(db.into())),
    ])
}

/// The `stats` map (ADR-0309): Neo4j's kebab-case counters, only the
/// non-zero ones, plus `contains-updates: true`; `None` when every counter
/// is zero. The counter list and its values are the canonical wire
/// snapshot every surface renders (`relate::query_stats_to_wire`,
/// `STAT_COUNTERS`); Bolt renders each counter under its derived
/// `StatCounter::bolt_key()`, the Neo4j kebab-case wire spelling.
pub(crate) fn stats(stats: &QueryStats) -> Option<Pack> {
    let snapshot = relate::query_stats_to_wire(stats);
    let mut counters: Metadata = STAT_COUNTERS
        .iter()
        .map(|counter| (counter.bolt_key(), (counter.count)(&snapshot)))
        .filter(|(_, count)| *count != 0)
        .map(|(key, count)| entry(&key, exact_int(count.into())))
        .collect();
    if counters.is_empty() {
        return None;
    }
    counters.push(entry("contains-updates", Pack::Bool(true)));
    Some(Pack::Map(counters))
}
