//! The live telemetry handle (ADR-0079), compiled with the `telemetry`
//! feature.
//!
//! Shape (ADR-0545, revised by ADR-0615 — the delivery-lifetime
//! revision): [`Telemetry::new`] resolves the opt-out layers, loads (or
//! creates) the install ID, and — when enabled and a config directory
//! resolves — arms the pipeline: a bounded in-memory backlog for this
//! run's events, the run's own spool file, and the drain thread, whose
//! machinery lives in `drain.rs`. The drain thread starts when the
//! pipeline arms it — at the first recorded event, or when a non-query
//! exit calls
//! [`deliver_backlog_at_exit`](Telemetry::deliver_backlog_at_exit)
//! before its work begins — and claims whatever earlier runs left in
//! the spool, POSTing it while this run's own work proceeds. Starting
//! the drain at the arming point, not after the command's work, is what
//! gives a lifecycle command's own runtime (`upgrade`'s download,
//! `pull`'s copy, `serve`'s lifetime) to the delivery; the `telemetry
//! off` transition is the carve-out — a run about to forbid delivery
//! never starts one (ADR-0545 §5).
//!
//! Exit is where the revision bites. `process::exit` kills a detached
//! thread mid-POST, so next-run-only delivery never delivers on runs
//! that outlive neither their POSTs (sub-second one-shots) nor their
//! event (lifecycle commands used to emit only at the end). Now:
//! a query-session exit keeps ADR-0545 §1's pure local write — the
//! drain keeps its head start and dies at exit, the backlog riding the
//! next run; a non-query exit **joins** the drain with a small budget
//! (`drain::JOIN_BUDGET`) — the thread had the whole run as a head
//! start, so the join pays the POST tail, not the POST; and the first
//! run of a new CLI version joins with an elevated, pending-scaled
//! budget regardless of shape — the once-per-version drain that clears
//! a machine's accumulated backlog after an upgrade, the one sanctioned
//! network wait on a query-session exit (once per version per machine).
//! A join that fails or runs out stamps a backoff window
//! (`drain::DRAIN_BACKOFF`) in `state.json`; exits inside the window
//! skip the wait, so an unreachable sink does not stall every exit — the
//! pre-ADR-0545 stall, back for the join, is what the window prevents.
//! The run's own events still spool at exit and ride the next run
//! (sweep-before-flush: the drain's pass covers only the backlog, so a
//! clean machine pays nothing).
//!
//! [`Telemetry::emit`] appends to the backlog (cap 64,
//! drop-on-overflow): it never blocks and never touches the network.
//! Every failure — config I/O, HTTP, serialization — is swallowed: no
//! panics and no output, with one sanctioned exception: the ADR-0263
//! rejection warning described below is a single stderr line.
//!
//! Endpoint resolution lives in `relate_feedback::client` (ADR-0588): when
//! `RELATE_MIXPANEL_ENDPOINT` is set it is used as the full POST URL, but
//! ADR-0263 rejects a non-loopback `http://` value — a cleartext endpoint
//! would leak events — and a rejected override disables telemetry for the
//! run with one stderr warning. Otherwise the compiled-in default endpoint
//! is used. Likewise `RELATE_MIXPANEL_TOKEN` overrides the compiled-in
//! [`MIXPANEL_TOKEN`] (ADR-0079 §7).

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use relate_feedback::client;
use relate_feedback::opt_out::EffectiveState;
use relate_feedback::{opt_out, state};

use super::drain;
use super::envelope::{self, BaseContext};
use super::spool;
use super::Event;

/// Mixpanel project token (ADR-0079 §7): the shared Neo4j Labs CLI project —
/// the same one neo4j-cli reports into; events are namespaced by the
/// `RELATE-CLI_` prefix so the tools coexist. This is a client-side
/// ingestion token, public in every shipped binary by design. Overridable
/// via `RELATE_MIXPANEL_TOKEN`. The literal's one home is the feedback
/// crate (ADR-0588 §3, single-project revision) — the agent-feedback stream
/// reports into the same project under its own event name — so this is a
/// re-export, not a second spelling.
pub const MIXPANEL_TOKEN: &str = relate_feedback::sender::PROJECT_TOKEN;

/// Events one run holds in memory before the exit append; beyond this the
/// event is dropped (ADR-0079 §5 best-effort).
const BACKLOG_CAP: usize = 64;

/// The drain's arming state (see `Live::drain`): `Idle` — not yet
/// decided; `NothingPending` — decided against, the spool was empty at
/// the scan; `Armed` — a drain spawn was attempted, carrying everything
/// the exit tail needs: the completion signal, the join budget this
/// run's exit waits with (`None` for a query-session run, whose exit
/// never joins), and whether this is the once-per-version drain (the
/// stamp's condition).
enum DrainState {
    Idle,
    NothingPending,
    Armed {
        rx: mpsc::Receiver<drain::Delivery>,
        join_budget: Option<Duration>,
        version_drain: bool,
    },
}

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
    /// target — held here because the drain starts at the arming point
    /// (the first recorded event, or a non-query exit's
    /// [`deliver_backlog_at_exit`](Telemetry::deliver_backlog_at_exit)),
    /// not at construction.
    endpoint: String,
    /// The drain's arming state, the one latch every start site reads
    /// (ADR-0615). The `Armed` receiver exists even when the OS refuses
    /// the thread (the sender drops, the join reads `Disconnected`),
    /// which the version-stamp condition and that arm both rely on.
    drain: DrainState,
    /// Whether this run is the once-per-version drain (the state file
    /// records a different version than this binary): the first armed
    /// run of the version joins with an elevated, pending-scaled budget
    /// whatever its shape — the one sanctioned network wait on a
    /// query-session exit, once per version per machine.
    version_drain_run: bool,
    /// The loaded `state.json` snapshot — the one record driving the
    /// run's persisted policies: `notice_shown` (the first-run notice),
    /// the install ID (the base context), `cli_version_last_run` (the
    /// version drain's trigger) and `delivery_backoff_until` (the exit
    /// join's gate). Inside `Live` because a run that cannot record also
    /// cannot claim a notice, and `dir` above is the save target.
    record: state::State,
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
    /// directory, and — when enabled — arm the spool, compute the
    /// once-per-version drain, and prepare the drain thread's start (it
    /// spawns at the first recorded event or a non-query exit's delivery
    /// window). Never fails; the only output is the single ADR-0263
    /// stderr warning when a cleartext non-loopback endpoint override is
    /// rejected.
    pub fn new() -> Self {
        let env = crate::config_dir::env_var;
        let dir = crate::config_dir::config_dir();
        let config_telemetry = match dir.as_deref() {
            Some(dir) => state::load_config_telemetry(dir),
            None => state::ConfigTelemetry::Missing,
        };
        let state = opt_out::resolve(&env, config_telemetry);
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
    pub(super) fn enable(
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
        let (record, persistence) = state::load_or_create_state(&dir);
        if persistence == state::Persistence::Ephemeral {
            return Self::disabled(state);
        }
        let token = env("RELATE_MIXPANEL_TOKEN").unwrap_or_else(|| MIXPANEL_TOKEN.to_string());
        // The once-per-version drain (ADR-0615): the state file records
        // the CLI version this config directory last ran under, so a run
        // of a different version is that version's drain run. Its wait
        // scales with the backlog it found at arming — before this run
        // adds its own file — and the run records its version at exit
        // whether or not delivery succeeded (attempt-once), so a machine
        // that cannot deliver yet pays the elevated budget exactly once.
        let version_drain_run = record.cli_version_last_run.as_deref() != Some(envelope::VERSION);
        Self {
            state,
            live: Some(Live {
                base: BaseContext::detect(token, record.install_id.clone()),
                backlog: Vec::new(),
                spool_file: spool::spool_file(&dir),
                endpoint,
                dir,
                drain: DrainState::Idle,
                version_drain_run,
                record,
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
        live.record.claim_notice(&live.dir).unwrap_or_default()
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
        live.start_drain(false);
        let envelope = envelope::build_envelope(&event, &live.base);
        if live.backlog.len() < BACKLOG_CAP {
            live.backlog.push(envelope);
        }
    }

    /// The non-query exits' delivery window (ADR-0615): start the drain
    /// now — the command's or page's own runtime is the window the
    /// thread POSTs in — and arm the exit join, so the exit waits
    /// (bounded by `drain::JOIN_BUDGET`) for the in-flight POSTs to confirm
    /// instead of killing them. Called by the lifecycle dispatch and the
    /// `--version`/help paths; a query-session run never calls it, and
    /// its exit stays ADR-0545 §1's pure local write. The once-per-version
    /// drain joins on its own, whatever the shape.
    pub fn deliver_backlog_at_exit(&mut self) {
        let Some(live) = &mut self.live else {
            return;
        };
        live.start_drain(true);
    }

    /// The exit path (ADR-0545 §1 as revised by ADR-0615): claim the
    /// once-per-version attempt, join the drain where this run's shape
    /// does — a non-query exit with the standard budget, a version-drain
    /// run with its elevated one, never inside the failed-attempt backoff
    /// window — then write this run's own events to its spool file, one
    /// local write beyond what the join already covered. Idempotent: a
    /// second call returns immediately, and `emit` afterwards is a no-op.
    pub fn flush(&mut self) {
        if let Some(live) = self.live.take() {
            live.join_drain_and_spool();
        }
    }

    /// Take the live pipeline without writing anything: the run records
    /// no further events, its exit writes no spool file, and no delivery
    /// is ever started or waited on — the dispatch's carve-out (the
    /// lifecycle hook skips the off transition) never opened the
    /// delivery window, and a disarmed pipeline has no drain to join.
    /// `telemetry off` disarms before persisting, making the command one
    /// ordered transition (ADR-0545 §5): disarm, persist, clear the
    /// pending spool — never a delivery or a recording of its own.
    pub(crate) fn disarm(&mut self) {
        self.live = None;
    }
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        self.flush();
    }
}

impl Live {
    /// Start this run's drain thread, once (ADR-0545 §2 as revised by
    /// ADR-0615): the steady one-sweep shape at every ordinary start, the
    /// loop-until-empty shape with its computed budget on the
    /// once-per-version drain.
    /// `exit_joins` is the one home of the non-query fact (ADR-0615):
    /// `true` from the delivery window `deliver_backlog_at_exit` opens,
    /// `false` from an emit — a query-session run arms no join budget,
    /// so its exit stays the pure local write of ADR-0545 §1, and the
    /// join can never be skipped by an arming that preceded the window.
    fn start_drain(&mut self, exit_joins: bool) {
        if !matches!(self.drain, DrainState::Idle) {
            return;
        }
        // One scan decides everything the drain needs about the present
        // backlog — the arming, the thread's deadline, and the exit
        // join's budget (ADR-0615). Nothing to deliver means no attempt:
        // no thread, no HTTP client, no TLS machinery resident in the
        // process — a clean machine (a fresh server, a scripted
        // one-shot) pays this one read, once per run (the latch records
        // the decision either way), and a long-lived `serve` never holds
        // the delivery stack it has no backlog for. The
        // once-per-version drain's budget is scaled by the count this
        // scan just took; a non-query exit joins with the standard
        // budget; a query-session run arms without a join. The steady
        // deadline is the start instant, so the drain loop takes exactly
        // one pass.
        let pending = spool::spool_paths(&self.dir).count();
        if pending == 0 {
            self.drain = DrainState::NothingPending;
            return;
        }
        let (deadline, join_budget) = if self.version_drain_run {
            let budget = drain::version_drain_budget(pending);
            (Instant::now() + budget, Some(budget))
        } else {
            (Instant::now(), exit_joins.then_some(drain::JOIN_BUDGET))
        };
        self.drain = DrainState::Armed {
            rx: drain::spawn_drain(self.endpoint.clone(), self.dir.clone(), deadline),
            join_budget,
            version_drain: self.version_drain_run,
        };
    }

    /// The exit tail (ADR-0615): the once-per-version claim, the join this
    /// run's shape earned, then the run's own spool write. See
    /// [`Telemetry::flush`] for the shape rules.
    fn join_drain_and_spool(mut self) {
        // The arming state, moved out whole: one `if let` owns the
        // once-per-version claim, the backoff check, and the join — the
        // state is consumed here, never reset to a value it cannot hold
        // at this point.
        let armed = self.drain;
        if let DrainState::Armed {
            rx,
            join_budget,
            version_drain,
        } = armed
        {
            // The join decision: only for a run that armed a budget — the
            // once-per-version drain or a non-query exit — and never inside
            // the failed-attempt backoff window, so a pending backlog with
            // an unreachable sink does not stall every exit. The budget is
            // the arming's own: the thread self-limits at its deadline, so
            // the join either collects an already-buffered signal instantly
            // or covers genuinely in-flight POSTs. The backoff read comes
            // first, because the once-per-version claim must not burn on a
            // backoff-skipped join: inside the window the exit gives the
            // elevated drain neither its wait nor its thread's window (the
            // process exits on top of it), so the claim stays for a run
            // that can actually spend it. Outside the window, a killed or
            // failed attempt still counts — attempt-once.
            let in_backoff = self
                .record
                .delivery_backoff_until
                .is_some_and(|until| until > chrono::Utc::now());
            if version_drain && !in_backoff {
                let _ = self.record.stamp_cli_version(&self.dir, envelope::VERSION);
            }
            if let (true, Some(budget)) = (!in_backoff, join_budget) {
                match rx.recv_timeout(budget) {
                    // Every claim the last pass took was confirmed: nothing
                    // to record.
                    Ok(drain::Delivery::Confirmed) => {}
                    // A claim survived the pass, or the wait ran out: stamp
                    // the backoff window so the next exits skip the wait.
                    // The undelivered claims stay for the next run either
                    // way (at-least-once, deduped by `$insert_id`).
                    Ok(drain::Delivery::Failed) | Err(mpsc::RecvTimeoutError::Timeout) => {
                        let _ = self.record.stamp_delivery_backoff(
                            &self.dir,
                            chrono::Utc::now() + drain::DRAIN_BACKOFF,
                        );
                    }
                    // The thread ended without its signal (a spawn-time
                    // refusal): nothing to wait on, nothing to record.
                    Err(mpsc::RecvTimeoutError::Disconnected) => {}
                }
            }
        }
        // The run's own events, last: the drain's pass covered only the
        // backlog, so this write is the whole of this run's exit cost
        // wherever no join applied (ADR-0545 §1).
        spool::write(&self.spool_file, &self.backlog);
    }
}

/// The single stderr warning emitted when an override is rejected (ADR-0263
/// pins this text). `<url>` is the rejected value, reproduced verbatim.
fn rejection_warning(url: &str) -> String {
    format!(
        "warning: ignoring RELATE_MIXPANEL_ENDPOINT={url}: http:// is allowed only for loopback hosts; telemetry disabled for this run"
    )
}

#[cfg(test)]
mod tests;
