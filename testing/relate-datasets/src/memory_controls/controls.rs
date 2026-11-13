//! The processing-memory controls' invariant cases (ADR-0333): the mixed
//! Bolt/HTTP connection cap, the unlimited-connection ceiling, and the
//! concurrent-logical-workspace limit.
//!
//! The family's shared harness — its run inputs and report, the serve-child
//! lifecycle, the retry connect, the case finisher and the probe dispatch —
//! is [`crate::memory_controls::harness`]; this module is only the cases the memory
//! probe adds on top of it.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::memory_controls::harness::{
    connect_addr, finish, CaseReport, Config, Sample, ServeLimits, Server,
};
use crate::processing_graph;
use relate_test_support::bolt::client::Session;

const REPEATS: usize = 3;
const HOLD_MS: u64 = 400;
/// The holder keeps its reservation for this long so the review write
/// arrives while that reservation is still charged, including when the
/// rest of the suite is compiling on the same machine.
const OVERLAP_HOLD_MS: u64 = 15_000;

pub(crate) fn case_connections(cfg: &Config) -> Result<CaseReport, String> {
    let store = cfg.store.join("connections");
    let server = Server::start(
        &cfg.serve_bin,
        &store,
        ServeLimits {
            max_connections: Some(2),
            http: true,
            hold_ms: Some(HOLD_MS),
            ..ServeLimits::default()
        },
    )?;
    let (baseline, cold, logical) = load_ledger(&server, cfg.controls.ledger(), true, u64::MAX)?;
    let session_cost = server.session_cost(cold.rss)?;
    let mut idles = Vec::new();
    for _ in 0..REPEATS {
        let mut held = connect_addr(server.bolt()).map_err(|err| {
            format!(
                "bolt session under the mixed cap failed: {err}\n{}",
                server.log_text()
            )
        })?;
        held.run(processing_graph::record_review())?;
        let http = server
            .http
            .clone()
            .ok_or_else(|| "serve did not publish an http address".to_string())?;
        let (started_tx, started_rx) = mpsc::channel();
        let request = thread::spawn(move || {
            http_query(&http, processing_graph::point_read(), Some(started_tx))
        });
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| format!("http query did not connect\n{}", server.log_text()))?;
        // The Bolt write already logged one admit hold. The HTTP read logs
        // the second while it holds the remaining connection slot.
        server.expect_log_count("query admit hold:", 2)?;
        if Session::connect(server.bolt()).is_ok() {
            drop(held);
            let http_result = request
                .join()
                .map_err(|_| "http query thread panicked".to_string());
            return Err(format!(
                "the mixed Bolt/HTTP cap did not refuse the next connection\nhttp: {http_result:?}\n{}",
                server.log_text()
            ));
        }
        server.expect_log("bolt: --max-connections 2 reached")?;
        drop(held);
        let _reopened = connect_when_admitted(server.bolt())?;
        request
            .join()
            .map_err(|_| "http query thread panicked".to_string())?
            .map_err(|err| format!("held http query failed: {err}"))?;
        drop(_reopened);
        idles.push(server.sample()?.rss);
    }
    finish(
        "connection-cap",
        &server,
        baseline,
        cold,
        logical,
        session_cost,
        2,
        idles,
        "accept-then-close",
    )
}

pub(crate) fn case_unlimited(cfg: &Config) -> Result<CaseReport, String> {
    let store = cfg.store.join("unlimited");
    let server = Server::start(
        &cfg.serve_bin,
        &store,
        ServeLimits {
            // Pass 0 through. An absent flag is the server default of 128.
            max_connections: Some(0),
            ..ServeLimits::default()
        },
    )?;
    let (baseline, cold, logical) = load_ledger(&server, cfg.controls.ledger(), true, u64::MAX)?;
    let mut idles = Vec::new();
    for _ in 0..2 {
        let mut open = Vec::with_capacity(129);
        for n in 1..=129 {
            open.push(connect_addr(server.bolt()).map_err(|err| {
                format!(
                    "connection {n} was refused without a cap: {err}\n{}",
                    server.log_text()
                )
            })?);
        }
        open[0].run(processing_graph::point_read())?;
        open[0].run(processing_graph::record_review())?;
        drop(open);
        idles.push(server.sample()?.rss);
    }
    finish(
        "unlimited-connections",
        &server,
        baseline,
        cold,
        logical,
        0,
        0,
        idles,
        "connection 129 accepted",
    )
}

/// Smallest pool whose admission ceiling holds three quanta: the held
/// statement, its cached plan, and the review write. The write alone
/// still fits once the holder releases its statement reservation.
fn pool_for_the_overlap() -> u64 {
    let quantum = relate_core::tokens::CHARGE_QUANTUM_BYTES;
    let need = 3 * quantum;
    let mut capacity = need;
    while capacity - capacity / 8 < need {
        capacity += 1;
    }
    capacity
}

pub(crate) fn case_active_queries(cfg: &Config) -> Result<CaseReport, String> {
    let store = cfg.store.join("active");
    let limits = ServeLimits {
        max_connections: Some(4),
        concurrent_logical_workspace_limit: Some(pool_for_the_overlap()),
        hold_ms: Some(OVERLAP_HOLD_MS),
        ..ServeLimits::default()
    };
    // The ledger load retains far more than the overlap pool. Load with
    // admission off, then reopen under the pool the overlap probes.
    let logical = {
        let server = Server::start(&cfg.serve_bin, &store, ServeLimits::default())?;
        let mut session = connect_addr(server.bolt())?;
        processing_graph::load(&mut session, cfg.controls.ledger(), true, u64::MAX)?;
        drop(session);
        crate::processing_logical::logical_bytes(&server)?
    };
    let baseline = {
        let empty = cfg.store.join("active-empty");
        let server = Server::start(&cfg.serve_bin, &empty, limits)?;
        server.sample()?
    };
    let server = Server::open_existing(&cfg.serve_bin, &store, limits)?;
    let cold = server.sample()?;
    let session_cost = server.session_cost(cold.rss)?;
    let mut idles = Vec::new();
    let mut failure = String::new();
    for _ in 0..REPEATS {
        // Each repeat waits for its own hold line. A count of one matches
        // the previous repeat's line and lets the write run with the pool
        // already free.
        let seen = server.log_text().matches("query admit hold:").count();
        let addr = server.bolt().to_string();
        let holder = thread::spawn(move || {
            let mut session = connect_addr(&addr)?;
            session.run("RETURN 1")
        });
        server.expect_log_count_within(
            "query admit hold:",
            seen + 1,
            Duration::from_millis(OVERLAP_HOLD_MS),
        )?;
        let mut rejected = connect_addr(server.bolt())?;
        let err = match rejected.run(processing_graph::record_review()) {
            Ok(()) => {
                return Err(format!(
                    "the overlapping write should hit concurrent-logical-workspace-limit\n{}",
                    server.log_text()
                ));
            }
            Err(err) => err,
        };
        if !err.contains("concurrent-logical-workspace-limit") {
            return Err(format!(
                "expected concurrent-logical-workspace-limit, got {err}"
            ));
        }
        failure = err;
        holder
            .join()
            .map_err(|_| "holder thread panicked".to_string())??;
        rejected.run(processing_graph::record_review())?;
        if rejected.run_records(processing_graph::point_read())? != 1 {
            return Err("point read after concurrent-logical-workspace-limit failed".into());
        }
        idles.push(server.sample()?.rss);
    }
    finish(
        "concurrent-logical-workspace-limit",
        &server,
        baseline,
        cold,
        logical,
        session_cost,
        4,
        idles,
        &failure,
    )
}

/// Reconnect once a closed session has returned its connection slot.
fn connect_when_admitted(addr: &str) -> Result<Session, String> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match connect_addr(addr) {
            Ok(session) => return Ok(session),
            Err(err) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
                let _ = err;
            }
            Err(err) => return Err(err),
        }
    }
}

fn load_ledger(
    server: &Server,
    ledger: crate::processing_graph::Ledger,
    indexes: bool,
    max_entities: u64,
) -> Result<(Sample, Sample, u64), String> {
    let baseline = server.sample()?;
    let mut session = connect_addr(server.bolt())?;
    processing_graph::load(&mut session, ledger, indexes, max_entities)?;
    let logical = crate::processing_logical::logical_bytes(server)?;
    drop(session);
    Ok((baseline, server.sample()?, logical))
}

fn http_query(
    addr: &str,
    cypher: &str,
    started: Option<mpsc::Sender<()>>,
) -> Result<String, String> {
    let body = format!(
        "{{\"statement\":{}}}",
        serde_json::to_string(cypher).map_err(|err| err.to_string())?
    );
    let mut stream = TcpStream::connect(addr).map_err(|err| format!("http connect: {err}"))?;
    if let Some(started) = started {
        let _ = started.send(());
    }
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|err| err.to_string())?;
    let request = format!(
        "POST /db/neo4j/query/v2 HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|err| err.to_string())?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|err| err.to_string())?;
    let status = response.lines().next().unwrap_or("");
    if !status.starts_with("HTTP/1.1 200") && !status.starts_with("HTTP/1.0 200") {
        return Err(format!("http query failed: {response}"));
    }
    Ok(response)
}
