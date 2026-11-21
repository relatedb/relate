//! The live handle's tests (ADR-0079, ADR-0545, ADR-0615): the notice
//! and opt-out lifecycle, the exit tail's join/backoff/version-drain
//! behavior, and the arming latch. Split from `handle.rs` (the repo's
//! directory-module pattern) so the production surface stays one
//! readable file.

use super::*;
use crate::telemetry::StartupKind;
use std::time::{Duration, Instant};

fn enabled() -> EffectiveState {
    EffectiveState::Default
}

fn disabled_state() -> EffectiveState {
    EffectiveState::Env(false)
}

/// An enabled handle against `endpoint` with no config dir — the drain
/// thread never spawns and no spool is written, so endpoint-resolution
/// tests exercise no filesystem at all.
fn at_endpoint(endpoint: &str) -> Telemetry {
    let pairs = [("RELATE_MIXPANEL_ENDPOINT", endpoint)];
    let env = crate::telemetry::env_of(&pairs);
    Telemetry::enable(enabled(), &env, None)
}

fn refused() -> Telemetry {
    // Port 1 refuses connections immediately.
    at_endpoint("http://127.0.0.1:1")
}

/// An enabled handle spooling into `dir` against a refused endpoint:
/// the drain thread fails offline and the exit write stays local, the
/// exact shape a short run has in production. The one construction
/// helper for every with-`dir` test — the notice tests included.
fn spooling_in(dir: &std::path::Path) -> Telemetry {
    crate::telemetry::handle_in(dir, "http://127.0.0.1:1")
}

#[test]
fn flush_writes_the_backlog_to_the_spool_and_returns_promptly() {
    // ADR-0545 §1: the exit path is one local append — never a network
    // wait. Against a refused endpoint the pre-ADR-0545 flush waited
    // out its whole budget; this must return after the local write
    // with every event on disk. The 1 s bound (not the sub-ms
    // expected cost) covers debug builds and loaded CI runners.
    let dir = tempfile::tempdir().unwrap();
    let mut t = spooling_in(dir.path());
    t.emit(Event::Startup {
        kind: StartupKind::Oneshot,
    });
    t.emit(Event::Help);
    let start = Instant::now();
    t.flush();
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "flush took {:?} against a refused endpoint",
        start.elapsed()
    );
    let text = spool::spool_contents(dir.path());
    assert_eq!(text.lines().count(), 2);
    assert!(text.contains("RELATE-CLI_STARTUP"));
    assert!(text.contains("RELATE-CLI_HELP"));
}

#[test]
fn emit_drops_beyond_the_backlog_cap() {
    // The backlog carries ADR-0079 §5's bounded-drop semantics for the
    // in-memory leg: past the cap the event is dropped, so a runaway
    // emitter cannot grow the exit append without bound.
    let dir = tempfile::tempdir().unwrap();
    let mut t = spooling_in(dir.path());
    for _ in 0..(BACKLOG_CAP * 4) {
        t.emit(Event::Help);
    }
    t.flush();
    assert_eq!(
        spool::spool_contents(dir.path()).lines().count(),
        BACKLOG_CAP
    );
}

#[test]
fn a_run_without_a_config_dir_records_nothing() {
    // No config dir: neither recording nor delivering is possible
    // (ADR-0079 §3's degraded mode), so the run takes the disabled
    // shape — no live pipeline whose every effect would be
    // discarded — while still reporting the opt-out state unchanged.
    let mut t = refused();
    assert!(t.live.is_none(), "no config dir must mean no pipeline");
    assert_eq!(t.effective_state(), enabled());
    t.emit(Event::Help); // no-op
    let start = Instant::now();
    t.flush();
    assert!(
        start.elapsed() < Duration::from_millis(100),
        "flush took {:?} with no config dir",
        start.elapsed()
    );
}

#[test]
fn disabled_telemetry_swallows_everything() {
    let mut t = Telemetry::disabled(disabled_state());
    assert!(!t.effective_state().enabled());
    assert_eq!(t.effective_state(), EffectiveState::Env(false));
    t.emit(Event::Help); // no-op
    t.flush(); // returns immediately: no pipeline at all
}

/// A config directory whose state file cannot be written gives the
/// disabled shape: the run records nothing rather than reporting under an
/// install ID that changes every run.
#[test]
fn an_unrecordable_state_file_takes_the_disabled_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    // A directory where state.json should be makes the write fail.
    std::fs::create_dir_all(state::state_path(&dir)).unwrap();
    let pairs = [("RELATE_MIXPANEL_ENDPOINT", "http://127.0.0.1:1")];
    let env = crate::telemetry::env_of(&pairs);
    let mut t = Telemetry::enable(enabled(), &env, Some(dir.clone()));
    assert!(
        !t.claim_first_run_notice(),
        "no notice state rides a pipeline that cannot record"
    );
    t.emit(Event::Startup {
        kind: StartupKind::Oneshot,
    });
    t.flush();
    assert_eq!(
        spool::spool_paths(&dir).count(),
        0,
        "no spool file is written"
    );
}

#[test]
fn emit_after_flush_is_a_safe_noop() {
    // A live pipeline (a config dir resolves), so the flush really
    // takes a backlog and the later emit meets the post-flush shape.
    let dir = tempfile::tempdir().unwrap();
    let mut t = spooling_in(dir.path());
    t.emit(Event::Startup {
        kind: StartupKind::Oneshot,
    });
    t.flush();
    assert_eq!(spool::spool_contents(dir.path()).lines().count(), 1);
    // No panic, no output, no blocking; a second flush has nothing
    // left to take, and the late event reaches no spool.
    t.emit(Event::Help);
    let start = Instant::now();
    t.flush();
    assert!(start.elapsed() < Duration::from_millis(100));
    assert_eq!(spool::spool_contents(dir.path()).lines().count(), 1);
}

#[test]
fn effective_state_is_reported() {
    let t = refused();
    assert_eq!(t.effective_state(), enabled());
}

#[test]
fn rejection_warning_pins_the_adr_text() {
    assert_eq!(
            rejection_warning("http://evil.example/track"),
            "warning: ignoring RELATE_MIXPANEL_ENDPOINT=http://evil.example/track: http:// is allowed only for loopback hosts; telemetry disabled for this run"
        );
}

#[test]
fn rejected_endpoint_writes_no_state_file() {
    // Rejection must behave like an opt-out: the endpoint is resolved
    // before any state-file I/O, so no `state.json` is created.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let t = crate::telemetry::handle_in(dir, "http://evil.example/track");
    // The opt-out state is reported unchanged, but nothing runs.
    assert!(t.live.is_none());
    assert!(!state::state_path(dir).exists());
}

#[test]
fn rejected_override_disables_the_pipeline() {
    // A real rejection (a non-loopback `http://` override beside a
    // resolved config dir): no pipeline, the opt-out state reported
    // unchanged, and `emit`/`flush` no-op without writing a spool.
    let tmp = tempfile::tempdir().unwrap();
    let mut t = crate::telemetry::handle_in(tmp.path(), "http://evil.example/track");
    assert_eq!(t.effective_state(), enabled());
    assert!(t.live.is_none());
    t.emit(Event::Help);
    t.flush();
    assert_eq!(spool::spool_paths(tmp.path()).count(), 0);
}

#[test]
fn first_run_notice_shown_exactly_once() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let mut t = spooling_in(dir);
    assert!(t.claim_first_run_notice(), "first check shows the notice");
    assert!(
        !t.claim_first_run_notice(),
        "second check is suppressed by the notice_shown flag"
    );
    assert!(state::load_or_create_state(dir).0.notice_shown);
    // A later process (fresh handle, same dir) sees the persisted flag.
    let mut t2 = spooling_in(dir);
    assert!(!t2.claim_first_run_notice());
}

#[test]
fn first_run_notice_is_structurally_absent_when_disabled() {
    // The notice state rides the live pipeline, so a disabled handle
    // cannot claim even in principle — there is no field to
    // manufacture. The end-to-end suppression (an opted-out install
    // never printing the notice) is pinned by the e2e suite.
    let mut t = Telemetry::disabled(disabled_state());
    assert!(!t.claim_first_run_notice());
    assert!(!t.claim_first_run_notice());
}

// --- the delivery-lifetime revision (ADR-0615): join, query-shape
// skip, version drain, backoff ---

/// The event names a capture sink recorded, from its flattened JSON
/// array bodies.
fn captured_events(sink: &relate_test_support::loopback::CaptureSink) -> Vec<String> {
    crate::telemetry::event_names(sink.json_bodies().iter())
}

/// Record this binary's version in `dir`'s state, so a handle armed
/// there is not the once-per-version drain run.
fn with_recorded_version(dir: &std::path::Path) {
    let (mut record, _) = state::load_or_create_state(dir);
    record.stamp_cli_version(dir, envelope::VERSION).unwrap();
}

/// The empty-spool guard (ADR-0615): nothing to deliver spawns
/// nothing — no thread, no client, nothing resident — so a fresh
/// server or a scripted one-shot pays one directory read; the first
/// backlog is what arms the machinery.
#[test]
fn an_empty_spool_never_starts_the_drain() {
    // The negative case, its own directory: nothing pending, the
    // latch records the decision once and no drain is ever armed.
    let clean = tempfile::tempdir().unwrap();
    with_recorded_version(clean.path());
    let mut t = spooling_in(clean.path());
    t.emit(Event::Startup {
        kind: StartupKind::Oneshot,
    });
    assert!(
        matches!(
            t.live.as_ref().expect("live pipeline").drain,
            DrainState::NothingPending
        ),
        "an empty spool arms nothing: no thread, no client, no TLS"
    );
    t.emit(Event::Help);
    assert!(
        matches!(
            t.live.as_ref().expect("live pipeline").drain,
            DrainState::NothingPending
        ),
        "the latch holds: the second event does not re-scan"
    );
    // The positive case, its own directory: the seed is the only
    // pending file, so the arming is attributed to it alone.
    let seeded = tempfile::tempdir().unwrap();
    with_recorded_version(seeded.path());
    crate::telemetry::seed_spool(seeded.path(), "seeded");
    let mut next = spooling_in(seeded.path());
    next.emit(Event::Help);
    assert!(
        matches!(
            next.live.as_ref().expect("live pipeline").drain,
            DrainState::Armed { .. }
        ),
        "a pending file is what arms the machinery"
    );
}

/// The join guarantee (ADR-0615): a non-query exit that opened the
/// delivery window delivers the backlog before its flush returns —
/// no follow-up run — while the run's own events still spool for the
/// next one (sweep-before-flush).
#[test]
fn a_joined_exit_delivers_the_backlog_before_flush_returns() {
    let dir = tempfile::tempdir().unwrap();
    let (sink, port) = relate_test_support::loopback::spawn_capture_server("200 OK", "1");
    with_recorded_version(dir.path());
    crate::telemetry::seed_spool(dir.path(), "seeded");
    let mut t = crate::telemetry::handle_in(dir.path(), &format!("http://127.0.0.1:{port}"));
    t.deliver_backlog_at_exit();
    t.emit(Event::Startup {
        kind: StartupKind::Oneshot,
    });
    t.flush();
    assert_eq!(
        captured_events(&sink),
        ["seeded"],
        "the join delivered the backlog before flush returned"
    );
    assert!(
        spool::spool_contents(dir.path()).contains("RELATE-CLI_STARTUP"),
        "the run's own event spooled for the next run, not this one"
    );
}

/// The query-session exit keeps ADR-0545 §1's pure local write
/// (ADR-0615): against a sink slower than any sub-second run, a
/// query-shaped exit neither waits nor delivers — the backlog rides
/// the next run. This is the skip-one-shots rule the revision
/// protects.
#[test]
fn a_query_shaped_exit_neither_waits_nor_delivers() {
    let dir = tempfile::tempdir().unwrap();
    let (sink, port) = relate_test_support::loopback::spawn_capture_server_with_delay(
        "200 OK",
        "1",
        std::time::Duration::from_millis(600),
    );
    with_recorded_version(dir.path());
    crate::telemetry::seed_spool(dir.path(), "seeded");
    let mut t = crate::telemetry::handle_in(dir.path(), &format!("http://127.0.0.1:{port}"));
    t.emit(Event::Startup {
        kind: StartupKind::Oneshot,
    });
    let start = std::time::Instant::now();
    t.flush();
    assert!(
        start.elapsed() < std::time::Duration::from_millis(250),
        "a query-shaped exit must not wait for the drain: {:?}",
        start.elapsed()
    );
    assert_eq!(
        sink.request_count(),
        0,
        "the exit itself delivers nothing; the backlog rides the next run"
    );
}

/// The once-per-version drain (ADR-0615): the first run of a new
/// version joins whatever its shape — a query-shaped run delivers
/// the accumulated backlog at its own exit with the elevated budget —
/// and records the version, so the next run is not a drain run.
#[test]
fn the_version_drain_fires_once_on_any_shape_and_stamps() {
    let dir = tempfile::tempdir().unwrap();
    let (sink, port) = relate_test_support::loopback::spawn_capture_server_with_delay(
        "200 OK",
        "1",
        std::time::Duration::from_millis(300),
    );
    crate::telemetry::seed_spool(dir.path(), "seeded");
    // Fresh state: no recorded version, so this handle is the drain run.
    let mut t = crate::telemetry::handle_in(dir.path(), &format!("http://127.0.0.1:{port}"));
    assert!(
        t.live.as_ref().expect("live pipeline").version_drain_run,
        "a fresh state file makes this the once-per-version drain run"
    );
    t.emit(Event::Startup {
        kind: StartupKind::Oneshot,
    });
    t.flush();
    assert_eq!(
        captured_events(&sink),
        ["seeded"],
        "the version drain delivered the backlog at a query-shaped exit"
    );
    // The version is recorded — attempt-once — and the next run is
    // not a drain run.
    let (recorded, _) = state::load_or_create_state(dir.path());
    assert_eq!(
        recorded.cli_version_last_run.as_deref(),
        Some(envelope::VERSION)
    );
    let later = crate::telemetry::handle_in(dir.path(), &format!("http://127.0.0.1:{port}"));
    assert!(
        !later
            .live
            .as_ref()
            .expect("live pipeline")
            .version_drain_run,
        "the recorded version makes the next run a steady one"
    );
}

/// The version claim survives a backoff-skipped join (the rust
/// review's round-3 medium): inside the backoff window the exit gives
/// the elevated drain neither its wait nor its thread's window, so the
/// once-per-version claim stays for a run that can actually spend it.
#[test]
fn the_version_claim_survives_a_backoff_skipped_join() {
    let dir = tempfile::tempdir().unwrap();
    // A live backoff window and an unrecorded version, with a pending
    // file so the run arms: the join is skipped by the window alone,
    // against a live sink.
    let (mut record, _) = state::load_or_create_state(dir.path());
    record
        .stamp_delivery_backoff(dir.path(), chrono::Utc::now() + chrono::Duration::hours(1))
        .unwrap();
    crate::telemetry::seed_spool(dir.path(), "seeded");
    let (_sink, port) = relate_test_support::loopback::spawn_capture_server("200 OK", "1");
    let mut t = crate::telemetry::handle_in(dir.path(), &format!("http://127.0.0.1:{port}"));
    t.emit(Event::Help);
    t.flush();
    let (recorded, _) = state::load_or_create_state(dir.path());
    assert!(
        recorded.cli_version_last_run.is_none(),
        "the version claim survives a backoff-skipped join"
    );
}

/// The failed-attempt backoff (ADR-0615): a joined exit against an
/// unreachable sink stamps the window, and the next joined exit
/// inside it returns without waiting or delivering — the
/// pre-ADR-0545 exit stall is what the window prevents.
#[test]
fn a_failed_join_stamps_the_backoff_and_the_next_exit_skips_the_wait() {
    let dir = tempfile::tempdir().unwrap();
    with_recorded_version(dir.path());
    crate::telemetry::seed_spool(dir.path(), "seeded");
    // Port 1 refuses connections immediately: the sweep fails fast
    // and confirms nothing, so the join stamps the backoff.
    let mut first = spooling_in(dir.path());
    first.deliver_backlog_at_exit();
    first.emit(Event::Startup {
        kind: StartupKind::Oneshot,
    });
    first.flush();
    let (recorded, _) = state::load_or_create_state(dir.path());
    assert!(
        recorded
            .delivery_backoff_until
            .is_some_and(|until| until > chrono::Utc::now()),
        "the failed join stamps the backoff window"
    );
    // Inside the window, a joined exit against a slow sink returns
    // without the wait and without delivery.
    let (sink, port) = relate_test_support::loopback::spawn_capture_server_with_delay(
        "200 OK",
        "1",
        std::time::Duration::from_millis(600),
    );
    let mut second = crate::telemetry::handle_in(dir.path(), &format!("http://127.0.0.1:{port}"));
    second.deliver_backlog_at_exit();
    second.emit(Event::Help);
    let start = std::time::Instant::now();
    second.flush();
    assert!(
        start.elapsed() < std::time::Duration::from_millis(250),
        "an exit inside the backoff window must not wait: {:?}",
        start.elapsed()
    );
    assert_eq!(
        sink.request_count(),
        0,
        "the skipped join delivers nothing at this exit"
    );
}
