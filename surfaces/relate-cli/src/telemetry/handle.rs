//! The live telemetry handle (ADR-0079), compiled with the `telemetry`
//! feature.
//!
//! Shape (ADR-0545): [`Telemetry::new`] resolves the opt-out layers,
//! loads (or creates) the install ID, and — when enabled and a config
//! directory resolves — arms the pipeline: a bounded in-memory backlog
//! for this run's events, the run's own spool file, and a detached
//! drain thread that starts at the first recorded event and claims
//! whatever earlier runs left in the spool, POSTing it while this run's
//! own work proceeds. Starting the drain at the first event — not at
//! construction — is what makes `telemetry off` one ordered transition
//! (ADR-0545 §5): the run that turns telemetry off records nothing
//! after disarming, so it never starts a delivery it is about to
//! forbid. A run with no resolvable config directory can neither record
//! nor deliver (the install ID is ephemeral, the spool has no home), so
//! it takes the disabled shape while still reporting the opt-out state.
//! [`Telemetry::emit`] appends to the backlog (cap 64,
//! drop-on-overflow): it never blocks and never touches the network.
//! [`Telemetry::flush`] writes the backlog to the run's spool file —
//! one local write, no thread handoff, no network wait — so no exit is
//! ever perceptibly slowed (ADR-0079 §5, ADR-0545 §1). The next enabled
//! run's drain delivers it: at-least-once, made effectively-once by the
//! per-event `$insert_id` the sink dedups on. Every failure — config
//! I/O, HTTP, serialization — is swallowed: no panics and no output,
//! with one sanctioned exception: the ADR-0263 rejection warning
//! described below is a single stderr line.
//!
//! Endpoint resolution lives in `relate_feedback::client` (ADR-0588): when
//! `RELATE_MIXPANEL_ENDPOINT` is set it is used as the full POST URL, but
//! ADR-0263 rejects a non-loopback `http://` value — a cleartext endpoint
//! would leak events — and a rejected override disables telemetry for the
//! run with one stderr warning. Otherwise the compiled-in default endpoint
//! is used. Likewise `RELATE_MIXPANEL_TOKEN` overrides the compiled-in
//! [`MIXPANEL_TOKEN`] (ADR-0079 §7).

use std::path::PathBuf;

use serde_json::Value;

use relate_feedback::client::{self, Client, Reply};
use relate_feedback::consent::EffectiveState;
use relate_feedback::{consent, state};

use super::envelope::{self, BaseContext};
use super::spool;
use super::Event;

/// Mixpanel project token (ADR-0079 §7): the shared Neo4j Labs CLI project —
/// the same one neo4j-cli reports into; events are namespaced by the
/// `RELATE-CLI_` prefix so the tools coexist. This is a client-side
/// ingestion token, public in every shipped binary by design. Overridable
/// via `RELATE_MIXPANEL_TOKEN`.
pub const MIXPANEL_TOKEN: &str = "4bfb2414ab973c741b6f067bf06d5575";

/// Events one run holds in memory before the exit append; beyond this the
/// event is dropped (ADR-0079 §5 best-effort).
const BACKLOG_CAP: usize = 64;

/// Events per drain POST (ADR-0545 §2): keeps each request body small, so
/// one claimed file drains in a few bounded requests.
const DRAIN_BATCH: usize = 50;

/// The enabled pipeline: the event base context, the run's in-memory
/// backlog, the run's spool file, and the drain inputs. One value, so
/// the pieces are born together in `enable` and torn down together in
/// `flush` — a half-built handle is not representable, and `Live`
/// exists exactly when the run can both record and deliver.
struct Live {
    base: BaseContext,
    /// This run's pending envelopes, appended to by `emit` and taken by
    /// `flush`. Plainly owned: the only emitter is the owning thread
    /// (the drain thread touches spool files, never the handle), so
    /// there is no lock to poison and no second accessor to
    /// serialize against.
    backlog: Vec<Value>,
    /// The run's spool file — one per process, so concurrent runs never
    /// share a target (ADR-0545 §1).
    spool_file: PathBuf,
    /// The config directory, the drain thread's claim ground.
    dir: PathBuf,
    /// The resolved endpoint (ADR-0263-checked), the drain's POST
    /// target — held here because the drain starts at the first
    /// recorded event, not at construction.
    endpoint: String,
    /// Whether the drain thread has started. It spawns at the first
    /// `emit`, so a run that records nothing — the disarmed `off`
    /// command above all — never starts a delivery (ADR-0545 §5).
    drain_started: bool,
    /// The `state.json` contents loaded once at construction — the
    /// first-run-notice policy's inputs (ADR-0079 §6): `notice_shown`
    /// suppresses repeats, and the install ID has already fed the base
    /// context. Inside `Live` because a run that cannot record also
    /// cannot claim a notice, and `dir` above is the save target.
    notice: state::State,
}

/// Telemetry handle. Cheap to keep around when disabled: no thread, no
/// state file, no network, no spool. Dropping without
/// [`flush`](Self::flush) flushes.
pub struct Telemetry {
    /// The resolved opt-out state and which layer decided it — the
    /// reportable answer for `relate telemetry status` (ADR-0079 §6),
    /// never the pipeline's presence: a run whose endpoint override was
    /// rejected, or whose config directory did not resolve, reports an
    /// enabled state while recording nothing.
    state: EffectiveState,
    /// The live pipeline, present exactly when the run can both record
    /// and deliver: enabled by the opt-out layers, an accepted endpoint,
    /// and a resolved config directory. It carries the notice state
    /// (ADR-0079 §6) for the same reason — a run that cannot record
    /// cannot claim a first-run notice.
    live: Option<Live>,
}

impl Telemetry {
    /// Resolve the opt-out layers (`DO_NOT_TRACK` → `RELATE_TELEMETRY` →
    /// `config.json`), load or create the install ID under the XDG config
    /// directory, and — when enabled — spawn the drain thread and arm the
    /// spool. Never fails; the only output is the single ADR-0263 stderr
    /// warning when a cleartext non-loopback endpoint override is rejected.
    pub fn new() -> Self {
        let env = crate::config_dir::env_var;
        let dir = crate::config_dir::config_dir();
        let config_telemetry = match dir.as_deref() {
            Some(dir) => state::load_config_telemetry(dir),
            None => state::ConfigTelemetry::Missing,
        };
        let state = consent::resolve(&env, config_telemetry);
        // When disabled, touch nothing: no state file, no thread, no
        // network, no spool.
        if !state.enabled() {
            return Self::disabled(state);
        }
        Self::enable(state, &env, dir)
    }

    /// The enabled path of [`new`](Self::new), split out so tests can drive
    /// endpoint-override rejection with an explicit env and config dir.
    /// Endpoint resolution runs before the state file is loaded so a rejected
    /// override leaves no `state.json` behind — exactly like the opt-out
    /// early return in [`new`](Self::new).
    fn enable(
        state: EffectiveState,
        env: &impl Fn(&str) -> Option<String>,
        dir: Option<std::path::PathBuf>,
    ) -> Self {
        // A rejected cleartext non-loopback override disables telemetry for
        // the run rather than silently redirecting or leaking events
        // (ADR-0263); the opt-out state is reported unchanged.
        let endpoint = match client::resolve_endpoint(env("RELATE_MIXPANEL_ENDPOINT")) {
            Ok(url) => url,
            Err(url) => {
                eprintln!("{}", rejection_warning(&url));
                return Self::disabled(state);
            }
        };
        // No config directory: the run can neither record nor deliver —
        // the install ID is ephemeral and the spool has no home
        // (ADR-0079 §3's degraded mode, ADR-0545 §1). The disabled shape
        // reports the opt-out state unchanged; a live pipeline whose
        // every effect is discarded would only hide that.
        let Some(dir) = dir else {
            return Self::disabled(state);
        };
        // The state file is loaded exactly once, here: the install ID feeds
        // the base context and the notice state rides the pipeline. A state
        // that cannot be recorded is not worth reporting under: an install ID
        // that changes every run can correlate nothing, so the run takes the
        // same disabled shape as one without a config directory.
        let (notice, persistence) = state::load_or_create_state(&dir);
        if persistence == state::Persistence::Ephemeral {
            return Self::disabled(state);
        }
        let token = env("RELATE_MIXPANEL_TOKEN").unwrap_or_else(|| MIXPANEL_TOKEN.to_string());
        Self {
            state,
            live: Some(Live {
                base: BaseContext::detect(token, notice.install_id.clone()),
                backlog: Vec::new(),
                spool_file: spool::spool_file(&dir),
                endpoint,
                dir,
                drain_started: false,
                notice,
            }),
        }
    }

    /// The no-pipeline handle: no thread, no state file, no spool, no
    /// notice state. Also the shape of a run whose endpoint override was
    /// rejected or whose config directory did not resolve — `state` is
    /// still the reportable opt-out answer. `pub(crate)` for the
    /// command's tests, which drive the persist/clear legs against an
    /// inert handle.
    pub(crate) fn disabled(state: EffectiveState) -> Self {
        Self { state, live: None }
    }

    /// The resolved state and which layer decided it (for
    /// `relate telemetry status`, ADR-0079 §6).
    pub fn effective_state(&self) -> EffectiveState {
        self.state
    }

    /// One-time first-run-notice policy (ADR-0079 §6): returns `true`
    /// exactly once per install — on the first run with telemetry
    /// enabled, interactive or not — setting `notice_shown` in
    /// `state.json` as it does. The notice state rides the live
    /// pipeline, so a run that cannot record (disabled by any opt-out
    /// layer, a rejected endpoint, no config directory) never claims
    /// and writes nothing.
    pub fn claim_first_run_notice(&mut self) -> bool {
        let Some(live) = &mut self.live else {
            return false;
        };
        // A claim whose record could not be written is not reported: the
        // notice shows again next run instead of being lost.
        live.notice.claim_notice(&live.dir).unwrap_or_default()
    }

    /// Queue an event for delivery. Non-blocking: a full backlog drops the
    /// event; after [`flush`](Self::flush), [`disarm`](Self::disarm), or
    /// when disabled this is a safe no-op. Never panics, never prints.
    /// The first event also starts the drain thread (ADR-0545 §2): no
    /// recorded event, no delivery attempt.
    pub fn emit(&mut self, event: Event) {
        let Some(live) = &mut self.live else {
            return;
        };
        if !live.drain_started {
            live.drain_started = true;
            spawn_drain(live.endpoint.clone(), live.dir.clone());
        }
        let envelope = envelope::build_envelope(&event, &live.base);
        if live.backlog.len() < BACKLOG_CAP {
            live.backlog.push(envelope);
        }
    }

    /// Write the run's pending events to its spool file — one local
    /// write, never a network wait (ADR-0545 §1) — then release the
    /// pipeline; the next enabled run's drain delivers them. Idempotent:
    /// a second call returns immediately, and `emit` afterwards is a
    /// no-op.
    pub fn flush(&mut self) {
        if let Some(live) = self.live.take() {
            spool::write(&live.spool_file, &live.backlog);
        }
    }

    /// Take the live pipeline without writing anything: the run records
    /// no further events, its exit writes no spool file, and — because
    /// the drain starts at the first recorded event — no delivery is
    /// ever started. `telemetry off` disarms before persisting, making
    /// the command one ordered transition (ADR-0545 §5): disarm,
    /// persist, clear the pending spool — never a delivery or a
    /// recording of its own.
    pub(crate) fn disarm(&mut self) {
        self.live = None;
    }
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        self.flush();
    }
}

/// The detached drain thread (ADR-0545 §2): claim what earlier runs left
/// in the spool, POST it in bounded batches while this run's own work
/// proceeds, and delete each claim only after every batch is confirmed.
/// Every failure is swallowed; an undelivered claim stays for the next
/// enabled run (at-least-once, deduped by the sink's `$insert_id`).
fn spawn_drain(endpoint: String, dir: PathBuf) {
    // Best-effort by construction: a thread the OS refuses costs this
    // run its delivery attempt — the spool still lands, a later run
    // drains it — never a panic (ADR-0079 §5: no telemetry failure may
    // fail a run; `std::thread::spawn` would panic here).
    let _ = std::thread::Builder::new().spawn(move || {
        let client = Client::new(endpoint);
        for claim in spool::claim_all(&dir) {
            let delivered = claim
                .events
                .chunks(DRAIN_BATCH)
                .all(|batch| client.post(batch) == Reply::Accepted);
            if delivered {
                spool::remove(&claim.path);
            }
        }
    });
}

/// The single stderr warning emitted when an override is rejected (ADR-0263
/// pins this text). `<url>` is the rejected value, reproduced verbatim.
fn rejection_warning(url: &str) -> String {
    format!(
        "warning: ignoring RELATE_MIXPANEL_ENDPOINT={url}: http:// is allowed only for loopback hosts; telemetry disabled for this run"
    )
}

#[cfg(test)]
mod tests {
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
        let pairs = [("RELATE_MIXPANEL_ENDPOINT", "http://127.0.0.1:1")];
        let env = crate::telemetry::env_of(&pairs);
        Telemetry::enable(enabled(), &env, Some(dir.to_path_buf()))
    }

    /// The one `telemetry-spool-*.jsonl` file under `dir`, its text —
    /// exactly one file, or the test names what it found instead. The
    /// scan is the production sweep's own (`spool::spool_paths`), so
    /// the test sees exactly what a drain would claim.
    fn the_spool_file(dir: &std::path::Path) -> String {
        let mut found: Vec<PathBuf> = spool::spool_paths(dir).collect();
        assert_eq!(found.len(), 1, "expected one spool file, found {found:?}");
        std::fs::read_to_string(found.remove(0)).expect("read spool file")
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
        let text = the_spool_file(dir.path());
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
        assert_eq!(the_spool_file(dir.path()).lines().count(), BACKLOG_CAP);
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
        let mut t = refused();
        t.emit(Event::Startup {
            kind: StartupKind::Oneshot,
        });
        t.flush();
        // No panic, no output, no blocking; a second flush has nothing
        // left to take.
        t.emit(Event::Help);
        let start = Instant::now();
        t.flush();
        assert!(start.elapsed() < Duration::from_millis(100));
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
        let env = |k: &str| {
            (k == "RELATE_MIXPANEL_ENDPOINT").then(|| "http://evil.example/track".to_string())
        };
        let t = Telemetry::enable(enabled(), &env, Some(dir.to_path_buf()));
        // The opt-out state is reported unchanged, but nothing runs.
        assert!(t.live.is_none());
        assert!(!state::state_path(dir).exists());
    }

    #[test]
    fn rejected_override_disables_the_pipeline() {
        // The rejection path reuses `Self::disabled`: no backlog or spool,
        // and the opt-out state is reported unchanged. `emit`/`flush`
        // no-op.
        let mut t = Telemetry::disabled(enabled());
        assert_eq!(t.effective_state(), enabled());
        assert!(t.live.is_none());
        t.emit(Event::Help);
        t.flush();
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
}
