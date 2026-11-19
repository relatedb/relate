//! The `01N60` statement-scoped notification (ADR-0363): an `EXPLAIN`ed
//! statement that references a parameter the caller did not supply warns,
//! naming every unbound parameter. The ADR-0308 refusal that the plain and
//! `PROFILE` paths raise for the same statement lives in
//! `parameter_missing.rs`; this file holds the notice's half of the fact.

mod common;

use common::{connection, has_notification, only_notification, params};
use relate::Value;
use relate_model::wire::NotificationKind;

/// One notice names every unbound parameter, in the same first-appearance
/// order the refusal lists them in; supplying them clears it.
#[test]
fn explain_reports_the_missing_parameters_as_one_notice() {
    let mut conn = connection();

    let one = conn.query("EXPLAIN RETURN $foo").unwrap();
    let notice = only_notification(&one, NotificationKind::ParameterMissing);
    assert_eq!(notice.gql_status(), "01N60");
    assert_eq!(notice.severity().as_str(), "WARNING");
    assert_eq!(
        notice.operator_id(),
        None,
        "the parameter fact is statement-scoped"
    );
    assert_eq!(
        notice.description(),
        "The undefined parameter(s) $foo prevent plan reuse."
    );

    // Two names render as one notice naming both, in the planner's
    // first-appearance order — here `$foo` before `$bar`, the reverse of
    // the alphabetical order this notice rendered while it sorted the
    // names itself, so the whole string is asserted.
    let two = conn.query("EXPLAIN RETURN $foo, $bar").unwrap();
    assert_eq!(
        only_notification(&two, NotificationKind::ParameterMissing).description(),
        "The undefined parameter(s) $foo, $bar prevent plan reuse."
    );

    // A partly supplied statement names only the name still unbound.
    let partial = conn
        .query_with_params(
            "EXPLAIN RETURN $foo, $bar",
            params(&[("foo", Value::Int(1))]),
        )
        .unwrap();
    assert_eq!(
        only_notification(&partial, NotificationKind::ParameterMissing).description(),
        "The undefined parameter(s) $bar prevent plan reuse."
    );

    // Fully supplied: no notice at all.
    let supplied = conn
        .query_with_params(
            "EXPLAIN RETURN $foo, $bar",
            params(&[("foo", Value::Int(1)), ("bar", Value::Int(2))]),
        )
        .unwrap();
    assert!(
        !has_notification(&supplied, NotificationKind::ParameterMissing),
        "{:?}",
        supplied.notifications()
    );
}

/// The fact is role-independent: a write's right-hand side counts exactly as
/// a read's seek constant does, because either leaves the plan uncacheable
/// without the value.
#[test]
fn a_parameter_in_a_write_is_reported_missing_too() {
    let explained = connection()
        .query("EXPLAIN CREATE (n:Thing {name: $name}) RETURN n")
        .unwrap();
    assert_eq!(
        only_notification(&explained, NotificationKind::ParameterMissing).description(),
        "The undefined parameter(s) $name prevent plan reuse."
    );
}

/// A name that is not identifier-shaped renders through the one `$name`
/// spelling, escaped exactly as every other surface escapes it. The doubled
/// backtick is the discriminating case: a hand-rolled `$`name`` would render
/// `` $`a`b` `` — which this assertion rejects — while the canonical
/// renderer doubles the embedded backtick.
#[test]
fn a_quoted_parameter_name_renders_through_the_canonical_spelling() {
    let explained = connection().query("EXPLAIN RETURN $`a``b`").unwrap();
    assert_eq!(
        only_notification(&explained, NotificationKind::ParameterMissing).description(),
        "The undefined parameter(s) $`a``b` prevent plan reuse."
    );
}
