//! The ADR-0402 zero-effect write policy: the one batch gate both note sites
//! derive from, the fold that carries a run's effect into the transaction
//! ledger, and the two pinned stderr lines.
//!
//! The criterion itself is the engine's, not this crate's: `relate`'s
//! [`StatementEffect`] records whether a mutation operator reached execution
//! and whether the engine applied a change — the two facts a note may read.
//! The CLI adds only what is a CLI decision: batch sessions speak (the
//! interactive shell stays silent), a plan-producing run takes no
//! per-statement note (its plan table is its own report, while the effect it
//! carries still counts in the transaction ledger), a failed run reports its
//! failure instead of a note while its effect still folds in, and the exact
//! bytes a wrapper script may grep for live here beside the other
//! per-statement policies (`row_cap`, `timeout`).

use relate::StatementEffect;

use crate::exec_query::StatementRun;
use crate::{errors, telemetry};

/// The ADR-0402 Decision 4 commit summary: printed once when a `.commit`
/// closes a transaction that attempted a write but applied nothing.
/// Byte-pinned like the per-statement note below.
pub(crate) const ZERO_EFFECT_COMMIT_NOTE: &str =
    "note: .commit changed nothing; the transaction applied no changes";

/// The ADR-0402 Decision 1 gate, the one criterion both note sites derive
/// from: only a batch session reports (the interactive shell stays silent — a
/// `MATCH` that finds nothing is routine at a prompt), and only about a write
/// that applied nothing. This is also the whole of the `.commit` summary's
/// gate (Decision 4): a transaction has no plan artifact of its own, so a
/// plan-producing statement inside it folded its effect in and kept only its
/// own note out (Decision 2), which [`reports_zero_effect_statement_note`]
/// owns. One criterion, so the two sites cannot disagree about when the
/// policy speaks.
pub(crate) fn reports_zero_effect_write(
    source: telemetry::QuerySource,
    effect: StatementEffect,
) -> bool {
    source.is_batch() && effect.changed_nothing()
}

/// The ADR-0402 Decision 1 gate for the per-statement note, with the two facts
/// only the statement side knows: a plan-producing run takes no note of its
/// own (Decision 2), and only a run that succeeded reports — a failed statement
/// (a render failure after execution included) reports its failure instead,
/// while the effect it carries still folds into the transaction ledger.
/// `EXPLAIN` never executed, and `PROFILE` printed its plan table instead —
/// though `PROFILE` keeps its effect for the transaction ledger, which this
/// gate does not touch.
pub(crate) fn reports_zero_effect_statement_note(
    source: telemetry::QuerySource,
    effect: StatementEffect,
    plan_produced: bool,
    succeeded: bool,
) -> bool {
    succeeded && !plan_produced && reports_zero_effect_write(source, effect)
}

/// The ADR-0402 Decision 4 commit summary gate: a successful `.commit` of a
/// batch transaction that attempted a write and applied nothing. The success
/// test lives here with the policy, so the two note sites share one shape
/// instead of the caller re-deriving it.
pub(crate) fn commit_note(
    code: i32,
    source: telemetry::QuerySource,
    ledger: StatementEffect,
) -> Option<&'static str> {
    (code == 0 && reports_zero_effect_write(source, ledger)).then_some(ZERO_EFFECT_COMMIT_NOTE)
}

/// The ADR-0402 Decision 3 line for a batch statement at `at`: numbered
/// (`note: write statement 2 of 3 changed nothing; the graph is unchanged`)
/// in a multi-statement batch, unnumbered in a single-statement one. The only
/// renderer, and the only place the note's bytes are assembled: the optional
/// ADR-0199 position is the sentence's one substitution slot, so the two
/// forms cannot drift into different wording.
pub(crate) fn zero_effect_write_note(at: Option<errors::Position>) -> String {
    let at = at.map(|at| format!(" {at}")).unwrap_or_default();
    format!("note: write statement{at} changed nothing; the graph is unchanged")
}

/// Fold one completed statement run into the transaction ledger and report
/// its zero-effect note (ADR-0402 Decisions 3-4). The fold is unconditional:
/// the effect is the execution's fact, not the render's, so a run whose render
/// failed after the engine applied its change still reaches the ledger and
/// silences a later `.commit` summary. The note is a successful run's report
/// only — a failed run reports its failure instead — and a plan artifact keeps
/// its own statement's note out without touching the fold.
pub(crate) fn note_run(
    ledger: &mut StatementEffect,
    source: telemetry::QuerySource,
    run: &StatementRun,
    at: Option<errors::Position>,
) -> Option<String> {
    ledger.merge(run.effect);
    reports_zero_effect_statement_note(source, run.effect, run.plan_produced, run.code == 0)
        .then(|| zero_effect_write_note(at))
}

#[cfg(test)]
mod note_run_tests {
    use super::*;
    use telemetry::QuerySource;

    /// A run without an `Exec` bundle: the policy under test reads only the
    /// three `StatementRun` facts.
    fn run(code: i32, effect: StatementEffect) -> StatementRun {
        StatementRun {
            code,
            effect,
            plan_produced: false,
            hint_code: None,
        }
    }

    /// ADR-0402 Decision 4: the effect is the execution's fact. A write that
    /// applied its change and then failed to render (exit 1) keeps the effect,
    /// folds into the ledger, takes no note — and silences the `.commit`
    /// summary, so a persisted write is never called a no-op.
    #[test]
    fn a_failed_run_folds_its_effect_without_a_note() {
        let at = Some(errors::Position::new(2, 3));
        let mut ledger = StatementEffect::None;
        let applied = run(1, StatementEffect::Applied);
        assert_eq!(
            note_run(&mut ledger, QuerySource::Oneshot, &applied, at),
            None,
            "a failed run reports its failure, not a note"
        );
        assert_eq!(ledger, StatementEffect::Applied, "the effect is kept");
        assert!(
            !reports_zero_effect_write(QuerySource::Oneshot, ledger),
            "the applied write silences the .commit summary"
        );
    }

    /// The mirror direction (ADR-0402 Decision 3): a successful zero-effect
    /// write prints its note, and a failed one takes no note while its
    /// attempted effect still reaches the summary.
    #[test]
    fn an_attempted_effect_prints_its_note_only_on_success() {
        let at = Some(errors::Position::new(2, 3));
        let mut ledger = StatementEffect::None;
        let succeeded = run(0, StatementEffect::Attempted);
        assert_eq!(
            note_run(&mut ledger, QuerySource::Oneshot, &succeeded, at),
            Some(zero_effect_write_note(at)),
            "a successful zero-effect write reports"
        );
        assert_eq!(ledger, StatementEffect::Attempted);

        let mut ledger = StatementEffect::None;
        let failed = run(1, StatementEffect::Attempted);
        assert_eq!(
            note_run(&mut ledger, QuerySource::Oneshot, &failed, at),
            None,
            "the same write whose render failed takes no note"
        );
        assert_eq!(ledger, StatementEffect::Attempted, "the attempt is kept");
        assert!(
            reports_zero_effect_write(QuerySource::Oneshot, ledger),
            "so the .commit summary still reports the attempted no-op"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use telemetry::QuerySource;

    /// A read: no mutation operator, nothing applied.
    fn read() -> StatementEffect {
        StatementEffect::None
    }

    /// A write that applied nothing.
    fn zero_effect_write() -> StatementEffect {
        StatementEffect::Attempted
    }

    /// A write that applied a change.
    fn effective_write() -> StatementEffect {
        StatementEffect::Applied
    }

    /// ADR-0402 Decision 2: a plan-producing run takes only the note out of
    /// scope — `EXPLAIN` never executes, and `PROFILE` executes and keeps its
    /// effect (the session folds it into the transaction ledger), while its
    /// plan table is the run's own report. A failed run takes no note either;
    /// its effect still folds in (pinned by `note_run`'s tests).
    #[test]
    fn a_plan_producing_run_takes_only_the_note() {
        for source in [QuerySource::Oneshot, QuerySource::Stdin] {
            for effect in [read(), zero_effect_write(), effective_write()] {
                assert!(
                    !reports_zero_effect_statement_note(source, effect, true, true),
                    "a plan artifact adds no note, whatever the effect was"
                );
            }
            assert!(
                reports_zero_effect_statement_note(source, zero_effect_write(), false, true),
                "the same statement without its plan artifact reports"
            );
        }
    }

    /// The ADR-0402 Decision 1-3 decision table: a batch session reports a
    /// zero-effect write; a read, an effective write and the interactive
    /// shell all stay silent — and the engine's facts, not the plan's
    /// conservative query type, decide.
    #[test]
    fn zero_effect_note_decision_table() {
        for source in [QuerySource::Oneshot, QuerySource::Stdin] {
            assert!(reports_zero_effect_statement_note(
                source,
                zero_effect_write(),
                false,
                true
            ));
            assert!(!reports_zero_effect_statement_note(
                source,
                effective_write(),
                false,
                true
            ));
            assert!(!reports_zero_effect_statement_note(
                source,
                read(),
                false,
                true
            ));
            // A failed run reports its failure, never a note.
            assert!(!reports_zero_effect_statement_note(
                source,
                zero_effect_write(),
                false,
                false
            ));
        }
        // The interactive shell never reports, even a zero-effect write.
        assert!(!reports_zero_effect_statement_note(
            QuerySource::Repl,
            zero_effect_write(),
            false,
            true
        ));
        assert!(!reports_zero_effect_statement_note(
            QuerySource::Repl,
            read(),
            false,
            true
        ));
    }

    /// The ADR-0402 Decision 3-4 wording, byte-for-byte.
    #[test]
    fn zero_effect_note_wording_is_pinned() {
        assert_eq!(
            zero_effect_write_note(None),
            "note: write statement changed nothing; the graph is unchanged"
        );
        assert_eq!(
            zero_effect_write_note(Some(errors::Position::new(2, 3))),
            "note: write statement 2 of 3 changed nothing; the graph is unchanged"
        );
        assert_eq!(
            ZERO_EFFECT_COMMIT_NOTE,
            "note: .commit changed nothing; the transaction applied no changes"
        );
    }

    /// The ADR-0402 Decision 4 commit-summary table over the ledger (the
    /// engine effect folded with `merge`): only a transaction that attempted a
    /// write and applied nothing reports.
    #[test]
    fn commit_summary_decision_table() {
        let empty = StatementEffect::None;
        assert!(
            !reports_zero_effect_write(QuerySource::Oneshot, empty),
            "an empty transaction is silent"
        );

        let mut read_only = StatementEffect::None;
        read_only.merge(read());
        assert!(
            !reports_zero_effect_write(QuerySource::Oneshot, read_only),
            "a read-only transaction is silent"
        );

        let mut zero_write = StatementEffect::None;
        zero_write.merge(zero_effect_write());
        assert!(
            reports_zero_effect_write(QuerySource::Oneshot, zero_write),
            "a write with no applied change reports"
        );

        let mut effective = StatementEffect::None;
        effective.merge(effective_write());
        assert!(
            !reports_zero_effect_write(QuerySource::Oneshot, effective),
            "an effective write is silent"
        );

        // A zero-effect write beside an effective one merges to applied.
        let mut mixed = StatementEffect::None;
        mixed.merge(effective_write());
        mixed.merge(zero_effect_write());
        assert!(
            !reports_zero_effect_write(QuerySource::Oneshot, mixed),
            "one applied statement silences the summary"
        );

        // The summary is batch-only like the statement note.
        assert!(
            !reports_zero_effect_write(QuerySource::Repl, zero_write),
            "the interactive shell never prints the summary"
        );
    }

    /// The commit-note gate keeps the success test with the policy: a failed
    /// commit keeps its error line, the shell stays silent, and a read-only
    /// transaction prints nothing.
    #[test]
    fn the_commit_gate_reports_only_a_successful_zero_effect_batch_commit() {
        let zero = zero_effect_write();
        assert_eq!(
            commit_note(0, QuerySource::Oneshot, zero),
            Some(ZERO_EFFECT_COMMIT_NOTE)
        );
        assert_eq!(
            commit_note(1, QuerySource::Oneshot, zero),
            None,
            "a failed commit reports its failure, not a summary"
        );
        assert_eq!(
            commit_note(0, QuerySource::Repl, zero),
            None,
            "the interactive shell stays silent"
        );
        assert_eq!(
            commit_note(0, QuerySource::Oneshot, StatementEffect::None),
            None,
            "a read-only transaction stays silent"
        );
    }
}
