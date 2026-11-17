//! The telemetry drain thread's machinery (ADR-0545 §2 as revised by
//! ADR-0615): the one detached thread an armed pipeline spawns to deliver
//! the spool backlog, the budgets its exits wait with, and the
//! once-per-version drain's scaling. The handle (`handle.rs`) owns the
//! lifecycle decisions — when the thread starts, which exits join it —
//! and this module owns how a started drain runs.
//!
//! The thread claims spool and leftover-claim files oldest first
//! (`spool::claim_all`), POSTs each in bounded batches, deletes a claim
//! only after every batch is confirmed, and signals its one answer —
//! whether everything it claimed was delivered — through the returned
//! channel. It loops until the spool empties or the caller's deadline
//! passes; the steady shape passes the start instant, so it takes
//! exactly one pass, and the once-per-version drain passes its computed
//! budget out. A pass that confirms nothing ends the loop: the same
//! claims would face the same sink inside the same deadline, and
//! retrying them would spin. Every failure is swallowed (ADR-0079 §5);
//! undelivered claims stay for the next run, at-least-once, deduped by
//! the sink's `$insert_id`.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use relate_feedback::client::{Client, Reply};

use super::spool;

/// Events per drain POST (ADR-0545 §2): keeps each request body small, so
/// one claimed file drains in a few bounded requests.
const DRAIN_BATCH: usize = 50;

/// The standard exit-join budget (ADR-0615): the longest a non-query exit
/// waits for the drain's in-flight POSTs to confirm. The thread had the
/// whole run as a head start, so this covers the POST tail, not the POST —
/// a single cold POST to the sink measures ~250 ms and a warm eight-file
/// sweep ~2 s; whatever exceeds the budget stays claimed and rides the
/// next run.
pub(super) const JOIN_BUDGET: Duration = Duration::from_secs(1);

/// The once-per-version drain's budget floor and per-pending-file scale
/// (ADR-0615): the first run of a new CLI version clears the whole
/// accumulated backlog, so its wait scales with what it found — a fresh
/// install with nothing pending pays nothing, a machine with a pile
/// clears it in one bounded run.
const VERSION_DRAIN_BASE: Duration = Duration::from_secs(1);
const VERSION_DRAIN_PER_FILE: Duration = Duration::from_millis(150);

/// The once-per-version drain's ceiling (ADR-0615): one sanctioned
/// network wait on a query-session exit, paid at most once per version
/// per machine.
const VERSION_DRAIN_CAP: Duration = Duration::from_secs(10);

/// How long exits skip the drain wait after an attempt that failed or
/// ran out of budget (ADR-0615): a machine with a pending backlog and an
/// unreachable sink must not pay the budget on every exit — the
/// pre-ADR-0545 exit stall, back for the join, is what this window
/// prevents. The drain thread inside each run still attempts delivery.
pub(super) const DRAIN_BACKOFF: Duration = Duration::from_secs(15 * 60);

/// The once-per-version drain's wait budget, scaled by the pending-file
/// count the run found at arming (ADR-0615). The per-file scale is real
/// time on the sink's round trip, so the count cast is a file count, not
/// a truncation.
pub(super) fn version_drain_budget(pending: usize) -> Duration {
    (VERSION_DRAIN_BASE + VERSION_DRAIN_PER_FILE * pending as u32).min(VERSION_DRAIN_CAP)
}

/// The detached drain thread (see the module docs): claim what earlier
/// runs left in the spool, POST it in bounded batches, and signal whether
/// every claimed batch was confirmed. Best-effort by construction — a
/// thread the OS refuses costs this run its delivery attempt, the spool
/// still lands, and a later run drains it; never a panic (ADR-0079 §5:
/// no telemetry failure may fail a run; `std::thread::spawn` would panic
/// here). The send is best-effort too: a run that never joins drops its
/// receiver.
pub(super) fn spawn_drain(
    endpoint: String,
    dir: PathBuf,
    deadline: Instant,
) -> mpsc::Receiver<Delivery> {
    let (done_tx, done_rx) = mpsc::channel();
    let _ = std::thread::Builder::new().spawn(move || {
        let client = Client::new(endpoint);
        // The signal carries the LAST pass's outcome, not an AND over
        // the whole attempt history: the loop never runs a pass after a
        // pass that took claims and confirmed none (retrying them against
        // the same sink inside the same deadline would spin), so the last
        // pass states this drain's exit state — confirmed, or the failure
        // the backoff window is for.
        let mut outcome = Delivery::Confirmed;
        // The loop's own condition states the nothing-claimable exit;
        // the body keeps going while the deadline holds and the last
        // pass made progress — a partially successful pass (some claims
        // delivered, some retryable) continues, so a budgeted drain
        // delivers what it can instead of ending at the first retryable
        // claim. A pass with no progress stops the loop: its claims stay
        // for the next run, and retrying them against the same sink now
        // would spin. The steady shape's deadline is the start instant,
        // so it takes exactly one pass.
        while let Some((progressed, pass)) = sweep(&dir, &client) {
            outcome = pass;
            if !progressed || Instant::now() >= deadline {
                break;
            }
        }
        let _ = done_tx.send(outcome);
    });
    done_rx
}

/// One pass's outcome, carried by the completion signal to the exit
/// join: `Confirmed` — every claim the pass took was delivered and
/// removed — or `Failed` — a claim survived the pass, the failure the
/// backoff window exists for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Delivery {
    Confirmed,
    Failed,
}

/// One claim's disposition after its batches were POSTed, matched from
/// the client's typed [`Reply`] — never a single-variant comparison:
/// every batch confirmed; a retryable failure (the sink may take it
/// later, so the claim stays for the next run); or the sink's outright
/// refusal, whose own contract says resending it unchanged cannot help.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimOutcome {
    Confirmed,
    Retryable,
    Refused,
}

/// The disposition fold over one claim's batch replies: a claim holding
/// an unconfirmed (retryable) batch survives everything — retryable
/// outranks even a later refusal, so the next run can deliver what was
/// never refused; a refusal stops the fold (later batches would ride a
/// refusal the sink already stated cannot succeed) and, with no earlier
/// unconfirmed batch, refuses the claim attempt-once; otherwise every
/// batch confirmed.
fn claim_outcome(replies: &[Reply]) -> ClaimOutcome {
    let mut outcome = ClaimOutcome::Confirmed;
    for reply in replies {
        match reply {
            Reply::Accepted => {}
            Reply::ServerError | Reply::Unreachable | Reply::TimedOut => {
                outcome = ClaimOutcome::Retryable;
            }
            Reply::Rejected => {
                return if matches!(outcome, ClaimOutcome::Retryable) {
                    ClaimOutcome::Retryable
                } else {
                    ClaimOutcome::Refused
                };
            }
        }
    }
    outcome
}

/// POST one claim's envelopes in bounded batches, stopping at a refusal
/// (the rest would ride it), and fold the typed replies into the
/// claim's disposition.
fn post_claim(client: &Client, claim: &spool::Claim) -> ClaimOutcome {
    let mut replies = Vec::new();
    for batch in claim.events.chunks(DRAIN_BATCH) {
        let reply = client.post(batch);
        let refused = matches!(reply, Reply::Rejected);
        replies.push(reply);
        if refused {
            break;
        }
    }
    claim_outcome(&replies)
}

/// One sweep of the drain loop: claim what the cap allows and POST it,
/// oldest first. `Some((progressed, Delivery))` — whether the pass
/// removed any claim (delivered or refused: both are progress, so a
/// partially successful pass keeps the loop going while the budget
/// holds), and the pass's delivery outcome; `None` when the pass took
/// nothing, so the spool is drained and the loop ends.
fn sweep(dir: &std::path::Path, client: &Client) -> Option<(bool, Delivery)> {
    let claims = spool::claim_all(dir);
    if claims.is_empty() {
        return None;
    }
    let mut progressed = false;
    let mut outcome = Delivery::Confirmed;
    for claim in &claims {
        match post_claim(client, claim) {
            // Both confirmed ends remove the claim — the delivered one,
            // and the refused one (attempt-once, like the spool's
            // undeliverable content: retained, it would consume a sweep
            // slot forever against a refusal that cannot change). A
            // removal counts as progress (the claim left the spool); a
            // removal that fails leaves the claim pending — no progress
            // on it, and the pass reports Failed so the loop stops
            // re-POSTing a confirmed claim, leaving it for the next run.
            ClaimOutcome::Confirmed | ClaimOutcome::Refused => match spool::remove(&claim.path) {
                Ok(()) => progressed = true,
                Err(_) => outcome = Delivery::Failed,
            },
            ClaimOutcome::Retryable => outcome = Delivery::Failed,
        }
    }
    Some((progressed, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::spool;

    /// The budget's shape (ADR-0615): the floor with nothing pending, the
    /// per-file scale in between, the ceiling past it.
    #[test]
    fn the_version_drain_budget_floors_scales_and_caps() {
        assert_eq!(version_drain_budget(0), VERSION_DRAIN_BASE);
        assert_eq!(
            version_drain_budget(3),
            VERSION_DRAIN_BASE + VERSION_DRAIN_PER_FILE * 3
        );
        assert_eq!(version_drain_budget(10_000), VERSION_DRAIN_CAP);
    }

    /// The drain thread delivers a seeded spool directly: a deadline out
    /// clears it, removes every claim, and signals all-delivered.
    #[test]
    fn spawn_drain_clears_a_seeded_spool_within_its_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let (sink, port) = relate_test_support::loopback::spawn_capture_server("200 OK", "1");
        crate::telemetry::seed_spool(dir.path(), "seeded");
        let done = spawn_drain(
            format!("http://127.0.0.1:{port}"),
            dir.path().to_path_buf(),
            Instant::now() + Duration::from_secs(5),
        );
        assert_eq!(
            done.recv_timeout(Duration::from_secs(5)),
            Ok(Delivery::Confirmed),
            "the seeded spool drains within the deadline"
        );
        let events = crate::telemetry::event_names(sink.json_bodies().iter());
        assert_eq!(events, ["seeded"]);
        assert_eq!(
            spool::spool_paths(dir.path()).count(),
            0,
            "a confirmed claim is removed"
        );
    }

    /// A refused batch is attempt-once (ADR-0615, the typed-Reply
    /// disposition): a sink that answers 4xx stated that resending
    /// unchanged cannot help, so the claim is dropped like undeliverable
    /// content instead of consuming a sweep slot forever — and the pass
    /// still reads confirmed, no backoff for a sink that answers.
    #[test]
    fn a_refused_batch_drops_its_claim_attempt_once() {
        let dir = tempfile::tempdir().unwrap();
        // The fixed-response capture server answers 400 with body 1 —
        // the client classifies the status, so the batch reads Refused.
        let (sink, port) =
            relate_test_support::loopback::spawn_capture_server("400 Bad Request", "1");
        crate::telemetry::seed_spool(dir.path(), "refused");
        let done = spawn_drain(
            format!("http://127.0.0.1:{port}"),
            dir.path().to_path_buf(),
            Instant::now() + Duration::from_secs(5),
        );
        assert_eq!(
            done.recv_timeout(Duration::from_secs(5)),
            Ok(Delivery::Confirmed),
            "a refusal is a disposition, not a backoff: the sink answers"
        );
        assert_eq!(sink.request_count(), 1, "the refused batch was POSTed once");
        assert_eq!(
            spool::spool_paths(dir.path()).count(),
            0,
            "the refused claim is dropped attempt-once, not retained"
        );
    }

    /// The precedence the rust review's medium pinned (ADR-0615): a
    /// claim holding an unconfirmed (retryable) batch survives even a
    /// later refusal — its events were never refused, only undelivered —
    /// while a refusal with no earlier unconfirmed batch refuses the
    /// claim attempt-once.
    #[test]
    fn retryable_outranks_a_later_refusal() {
        use relate_feedback::client::Reply;
        // Unconfirmed before the refusal: the claim survives.
        assert_eq!(
            claim_outcome(&[Reply::Accepted, Reply::TimedOut, Reply::Rejected]),
            ClaimOutcome::Retryable
        );
        assert_eq!(
            claim_outcome(&[Reply::Unreachable, Reply::ServerError, Reply::Rejected]),
            ClaimOutcome::Retryable
        );
        // A refusal with every earlier batch confirmed: attempt-once.
        assert_eq!(
            claim_outcome(&[Reply::Accepted, Reply::Rejected]),
            ClaimOutcome::Refused
        );
        assert_eq!(claim_outcome(&[Reply::Rejected]), ClaimOutcome::Refused);
        // The clean and the retryable passes.
        assert_eq!(
            claim_outcome(&[Reply::Accepted, Reply::Accepted]),
            ClaimOutcome::Confirmed
        );
        assert_eq!(
            claim_outcome(&[Reply::Accepted, Reply::TimedOut]),
            ClaimOutcome::Retryable
        );
    }

    /// The steady shape's now-deadline takes exactly one sweep (ADR-0615):
    /// with more pending files than the sweep cap, one pass delivers the
    /// cap's worth and the rest stay for the next run.
    #[test]
    fn the_steady_deadline_takes_exactly_one_sweep() {
        let dir = tempfile::tempdir().unwrap();
        let (_sink, port) = relate_test_support::loopback::spawn_capture_server("200 OK", "1");
        for _ in 0..(spool::MAX_CLAIMS_PER_SWEEP + 1) {
            crate::telemetry::seed_spool(dir.path(), "e");
        }
        let done = spawn_drain(
            format!("http://127.0.0.1:{port}"),
            dir.path().to_path_buf(),
            Instant::now(),
        );
        assert_eq!(
            done.recv_timeout(Duration::from_secs(5)),
            Ok(Delivery::Confirmed)
        );
        assert_eq!(
            spool::spool_paths(dir.path()).count(),
            1,
            "the sweep cap delivered its fill in one pass; one file stays for the next run"
        );
    }
}
