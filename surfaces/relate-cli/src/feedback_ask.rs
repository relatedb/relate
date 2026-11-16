//! The feedback grant ask (ADR-0588 §3 as revised): one line at a person's
//! terminal, default yes, on the shared fail-closed answer kit
//! ([`crate::prompt`]). One module for both ask surfaces — the first-run
//! ask (`relate feedback`'s state view) and the windowed resurface
//! (`skill install`, `relate upgrade`) — so their preconditions and
//! persistence are one shape, never two copies that drift.
//!
//! The ask record is one stamp on the feedback crate's state
//! (`feedback_ask_last`): the first-run ask reads "ever answered" from its
//! presence, the windowed ask compares it to the grant window, and every
//! answered ask — grant or decline — writes it.

use std::io::BufRead;
use std::path::Path;

use chrono::{DateTime, Utc};
use relate_feedback::authorization::{Authorization, FeedbackConsent, DEFAULT_AUTHORIZATION_DAYS};
use relate_feedback::{opt_out, sender, state};

// The windowed ask's surface types, imported only with the surfaces that
// compile it (the bare-serve and minimal bundles have neither).
#[cfg(any(feature = "skill", feature = "upgrade"))]
use crate::agent_mode::FeedbackSurface;
#[cfg(any(feature = "skill", feature = "upgrade"))]
use std::io::IsTerminal;

/// Persist the consent record, reporting a write failure as
/// `Error: feedback {verb}: writing {path}` with exit 1. `verb` is the
/// person's surface the write rode on (`enable`, `disable`, or `ask` —
/// the install/upgrade ask), so a failure never names a surface its
/// caller is not.
pub(crate) fn save(record: &FeedbackConsent, dir: &Path, verb: &str) -> Result<(), i32> {
    record.save(dir).map_err(|e| {
        crate::print_error(format!(
            "Error: feedback {verb}: writing {}: {e}",
            state::config_path(dir).display()
        ));
        1
    })
}

/// The ask's one line and its answer: default **yes**, fail closed — an
/// empty answer or an explicit `y`/`yes` grants (the shared classifier's
/// whole yes vocabulary), anything else (`n`, `no`, `nope`, `No.`, garbage)
/// declines. `None` is an unread answer: the ask is not spent, and neither
/// grant nor decline is recorded. `now` is the ask's one clock reading,
/// supplied by the caller, so the window, the grant and the stamp are one
/// instant.
fn prompt_grant(
    record: &mut FeedbackConsent,
    endpoint: &str,
    now: DateTime<Utc>,
    stdin: &mut dyn BufRead,
) -> Option<()> {
    eprint!(
        "Agent feedback reports go to the Relate feedback service (Mixpanel) and a grant \
         lasts {DEFAULT_AUTHORIZATION_DAYS} days. Allow agent reports? [Y/n] "
    );
    let answer = crate::prompt::read_answer(stdin)?;
    if crate::prompt::answer_is_yes(&answer) {
        record.authorize(now, endpoint.to_string());
    } else {
        record.declined = true;
    }
    Some(())
}

/// The ask's precondition, one home: the destination a yes would record, or
/// `None` when there is nothing to ask about — no destination resolved (a
/// yes could record nothing), or an opt-out ladder that resolves off (it
/// suppresses every agent send, so it suppresses the ask too).
fn ask_destination(dir: &Path, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    // A refused override is skipped like an absent destination.
    let endpoint = match sender::resolve_feedback_endpoint(env) {
        Ok(Some(endpoint)) => endpoint,
        _ => return None,
    };
    opt_out::resolve(env, state::load_config_telemetry(dir))
        .enabled()
        .then_some(endpoint)
}

/// The one ask both surfaces ride — the module contract's "one shape",
/// made literal: the piped-run gate, the destination, the state load, one
/// clock reading, the surface's own skip rule, the prompt, the persist and
/// the stamp exist once, so the two surfaces cannot drift on preconditions
/// or persistence. What varies per surface is `should_ask` — the
/// suppression predicate over the stamp, the consent record and the ask's
/// one instant — and who owns the record (the caller's, or loaded here by
/// the windowed wrapper).
fn ask(
    dir: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    stdin_is_tty: bool,
    stdin: &mut dyn BufRead,
    record: &mut FeedbackConsent,
    should_ask: impl Fn(&state::State, &FeedbackConsent, DateTime<Utc>) -> bool,
) -> Result<(), i32> {
    if !stdin_is_tty {
        return Ok(());
    }
    let Some(endpoint) = ask_destination(dir, env) else {
        return Ok(());
    };
    let (mut stamp, _) = state::load_or_create_state(dir);
    // One clock read carries the whole ask — the skip rule, the grant and
    // the stamp are one instant, never three.
    let now = Utc::now();
    if !should_ask(&stamp, record, now) {
        return Ok(());
    }
    if prompt_grant(record, &endpoint, now, stdin).is_none() {
        return Ok(());
    }
    // The answer is persisted first; the stamp lands only when it did, so
    // the ask is spent exactly when its answer was recorded.
    save(record, dir, "ask")?;
    stamp_ask(&mut stamp, dir, now);
    Ok(())
}

/// The first-run ask (ADR-0588 §3): once per install — the ask record's
/// presence is the "ever answered" fact — on the person's first interactive
/// `relate feedback`. A recorded decline is never asked again here; the
/// windowed ask may resurface it after a grant window, never sooner. The
/// caller owns the consent record (the state view prints it after the
/// ask), so a failed write propagates as its exit code.
pub(crate) fn ask_first_run(
    dir: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    stdin_is_tty: bool,
    record: &mut FeedbackConsent,
    stdin: &mut dyn BufRead,
) -> Result<(), i32> {
    ask(
        dir,
        env,
        stdin_is_tty,
        stdin,
        record,
        |stamp, record, now| {
            stamp.feedback_ask_last.is_none()
                && !record.declined
                && matches!(record.authorization(now), Authorization::Missing)
        },
    )
}

/// The windowed ask for the install/upgrade surfaces: the same ask, but the
/// record's stamp — not a forever-decline — suppresses it, so a lapsed or
/// declined install is asked about again once the last ask is older than
/// the grant window. Piped runs and an active grant skip it. The record is
/// its own (no caller state to print), and a write failure is reported and
/// swallowed: a completed install or upgrade is never failed by the ask
/// that rode on it.
#[cfg(any(feature = "skill", feature = "upgrade"))]
pub(crate) fn ask_grant_windowed(
    dir: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    stdin_is_tty: bool,
    stdin: &mut dyn BufRead,
) {
    let mut record = FeedbackConsent::load(dir);
    let _ = ask(
        dir,
        env,
        stdin_is_tty,
        stdin,
        &mut record,
        |stamp, record, now| {
            let within_window = stamp.feedback_ask_last.is_some_and(|at| {
                now.signed_duration_since(at) < chrono::Duration::days(DEFAULT_AUTHORIZATION_DAYS)
            });
            !within_window && !matches!(record.authorization(now), Authorization::Authorized { .. })
        },
    );
}

/// The install/upgrade arms' one gate: ask only after a successful run on
/// the person's surface — an agent-driven run is exactly the
/// relayed-consent shape ADR-0588 keeps out. `asking_run` is the arm's own
/// typed fact (an install; an upgrade, never a `--check`).
#[cfg(any(feature = "skill", feature = "upgrade"))]
pub(crate) fn ask_after_command(exit_code: i32, surface: FeedbackSurface, asking_run: bool) {
    if exit_code != 0 || !asking_run || surface != FeedbackSurface::Human {
        return;
    }
    let Some(dir) = crate::config_dir::config_dir() else {
        return;
    };
    let stdin = std::io::stdin();
    let tty = stdin.is_terminal();
    let mut lock = stdin.lock();
    ask_grant_windowed(&dir, &crate::config_dir::env_var, tty, &mut lock);
}

/// Stamp the ask record, reporting a stamp failure through the shared
/// diagnostic sink: a failed stamp is a warning, never a failed ask.
fn stamp_ask(stamp: &mut state::State, dir: &Path, now: DateTime<Utc>) {
    if let Err(e) = stamp.stamp_feedback_ask(dir, now) {
        crate::print_error(format!("Error: feedback ask stamp: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::env_of;

    fn endpoint_env() -> impl Fn(&str) -> Option<String> {
        env_of(&[("RELATE_FEEDBACK_ENDPOINT", "http://127.0.0.1:9/track")])
    }

    /// The ask's one line and its answer: one line at a person's terminal,
    /// default yes, on the shared fail-closed answer kit.
    #[test]
    fn the_ask_grants_on_empty_or_yes_and_fails_closed_on_anything_else() {
        for answer in ["\n", "  \n", "y\n", "YES\n"] {
            let mut record = FeedbackConsent::default();
            let mut input = answer.as_bytes();
            prompt_grant(
                &mut record,
                "https://api.mixpanel.com/track",
                Utc::now(),
                &mut input,
            )
            .unwrap();
            assert!(
                matches!(
                    record.authorization(Utc::now()),
                    Authorization::Authorized { .. }
                ),
                "{answer:?} grants"
            );
        }
        // The fail-closed half: an unrecognized answer must never grant —
        // the default-yes revision fails closed, not open.
        for answer in ["n\n", "no\n", "nope\n", "No.\n", "nO\n", "maybe\n", "1\n"] {
            let mut record = FeedbackConsent::default();
            let mut input = answer.as_bytes();
            prompt_grant(
                &mut record,
                "https://api.mixpanel.com/track",
                Utc::now(),
                &mut input,
            )
            .unwrap();
            assert!(record.declined, "{answer:?} declines");
        }
        // An unread answer spends nothing.
        let mut record = FeedbackConsent::default();
        assert!(prompt_grant(
            &mut record,
            "https://api.mixpanel.com/track",
            Utc::now(),
            &mut &b""[..],
        )
        .is_none());
        assert!(record.authorization(Utc::now()) == Authorization::Missing);
    }

    /// The first-run ask: answered once, the one record suppresses it for
    /// good — a fresh decline is never re-asked by this surface.
    #[test]
    fn the_first_run_ask_answers_once() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let mut record = FeedbackConsent::load(dir);
        ask_first_run(dir, &endpoint_env(), true, &mut record, &mut &b"n\n"[..]).unwrap();
        assert!(
            FeedbackConsent::load(dir).declined,
            "an explicit no declines"
        );
        assert!(
            state::load_or_create_state(dir)
                .0
                .feedback_ask_last
                .is_some(),
            "the answered ask stamps the one record"
        );

        // The first-run ask itself never repeats.
        let mut fresh = FeedbackConsent::load(dir);
        ask_first_run(dir, &endpoint_env(), true, &mut fresh, &mut &b"y\n"[..]).unwrap();
        assert!(
            fresh.authorization(Utc::now()) == Authorization::Missing,
            "an answered install is never asked again"
        );
    }

    /// The windowed ask within the window reads nothing: a fresh decline
    /// holds for the window, not forever. The precondition is built from
    /// state directly (a declined record and a now-stamped ask), never by
    /// running the other ask surface.
    #[cfg(any(feature = "skill", feature = "upgrade"))]
    #[test]
    fn the_windowed_ask_within_the_window_reads_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let declined = FeedbackConsent {
            declined: true,
            ..Default::default()
        };
        declined.save(dir).unwrap();
        let (mut stamp, _) = state::load_or_create_state(dir);
        stamp.feedback_ask_last = Some(Utc::now());
        state::save_state(dir, &stamp).unwrap();
        let mut input: &[u8] = b"\n";
        ask_grant_windowed(dir, &endpoint_env(), true, &mut input);
        assert_eq!(
            input, b"\n",
            "the windowed ask within the window reads nothing"
        );
        assert!(
            FeedbackConsent::load(dir).declined,
            "the window holds the decline"
        );
    }

    /// The windowed ask resurfaces once the record's stamp is older than
    /// the grant window — the maintainer's resurface decision.
    #[cfg(any(feature = "skill", feature = "upgrade"))]
    #[test]
    fn the_windowed_ask_resurfaces_after_the_window() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        ask_grant_windowed(dir, &endpoint_env(), true, &mut &b"\n"[..]);
        let granted = FeedbackConsent::load(dir);
        assert!(matches!(
            granted.authorization(Utc::now()),
            Authorization::Authorized { .. }
        ));
        // Aged stamp, revoked grant: the ask returns, and declines.
        // A revoke is the default record saved over the grant.
        FeedbackConsent::default().save(dir).unwrap();
        let (mut aged, _) = state::load_or_create_state(dir);
        aged.feedback_ask_last =
            Some(Utc::now() - chrono::Duration::days(DEFAULT_AUTHORIZATION_DAYS + 1));
        state::save_state(dir, &aged).unwrap();
        ask_grant_windowed(dir, &endpoint_env(), true, &mut &b"n\n"[..]);
        assert!(
            FeedbackConsent::load(dir).declined,
            "the resurfaced ask declines"
        );
    }

    /// The windowed ask never reaches a piped run or an installation whose
    /// opt-out ladder is off; the first-run ask skips the same states.
    #[cfg(any(feature = "skill", feature = "upgrade"))]
    #[test]
    fn the_asks_skip_piped_and_opted_out_and_destinationless_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let mut input: &[u8] = b"\n";
        ask_grant_windowed(dir, &endpoint_env(), false, &mut input);
        assert_eq!(input, b"\n", "a piped run reads nothing");

        std::fs::write(state::config_path(dir), r#"{"telemetry": false}"#).unwrap();
        ask_grant_windowed(dir, &endpoint_env(), true, &mut input);
        assert_eq!(input, b"\n", "an off ladder reads nothing");

        // No destination (the explicit empty-token override): nothing a yes
        // could record, so nothing is asked.
        let no_destination = env_of(&[("RELATE_FEEDBACK_TOKEN", "")]);
        let mut record = FeedbackConsent::load(dir);
        ask_first_run(dir, &no_destination, true, &mut record, &mut input).unwrap();
        assert_eq!(input, b"\n", "the destinationless ask reads nothing");
        assert!(
            state::load_or_create_state(dir)
                .0
                .feedback_ask_last
                .is_none(),
            "no destination spends no ask"
        );
    }

    /// An untrusted config file fails the opt-out ladder closed, so the ask
    /// never runs against it — and records nothing either way.
    /// The first-run ask self-gates on a piped stdin exactly as the
    /// windowed ask does (the deleted piped-state test's property).
    #[test]
    fn the_first_run_ask_skips_a_piped_run() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let mut input: &[u8] = b"\n";
        let mut record = FeedbackConsent::load(dir);
        ask_first_run(dir, &endpoint_env(), false, &mut record, &mut input).unwrap();
        assert_eq!(input, b"\n", "a piped run reads nothing");
        assert!(
            state::load_or_create_state(dir)
                .0
                .feedback_ask_last
                .is_none(),
            "a piped run spends no ask"
        );
    }

    #[test]
    fn an_untrusted_config_file_skips_the_ask() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(state::config_path(dir), "{ not json").unwrap();
        let mut input: &[u8] = b"y\n";
        let mut record = FeedbackConsent::load(dir);
        ask_first_run(dir, &endpoint_env(), true, &mut record, &mut input).unwrap();
        assert_eq!(input, b"y\n", "the ladder fail-closed ask reads nothing");
        assert!(record.authorization(Utc::now()) == Authorization::Missing);
    }
}
