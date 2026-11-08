//! The component-field map policy's boundary inputs (W4b): the epoch
//! spelling families, the presence notion the validator and the reader share,
//! an unreadable source field, the zero-offset rendering, and the temporal
//! fixes round 2's short-zone/epoch/civil-source cases. Each expected value
//! was verified against live
//! Neo4j 2026.09 where the reference engine defines one; the two deliberate
//! divergences (a null form/epoch sibling is ignored rather than selected)
//! are the engine's documented "a null field is ignored by every reader"
//! rule and are stated on the tests that pin them.

mod common;

use relate::Value;

use common::{connection, err_text, scalar_one, text};

/// B3: the lowercase epoch spelling is the same field as the camelCase one —
/// it selects the epoch form and takes part in the exclusion. (Neo4j agrees:
/// `datetime({epochseconds: 1})` constructs, and mixing the two spellings is
/// 22N14.)
#[test]
fn epoch_field_spellings_are_case_insensitive() {
    let mut c = connection();
    assert_eq!(
        text(&mut c, "RETURN toString(datetime({epochseconds: 1})) AS x"),
        "1970-01-01T00:00:01Z"
    );
    assert_eq!(
        text(
            &mut c,
            "RETURN toString(datetime({epochmillis: 2000})) AS x"
        ),
        "1970-01-01T00:00:02Z"
    );
    let err = err_text(c.query("RETURN datetime({epochSeconds: 1, epochmillis: 2}) AS x"));
    assert!(
        err.contains("epochSeconds cannot be selected together with epochMillis"),
        "{err}"
    );
}

/// B4: a source field holding a non-temporal value is the typed
/// unreadable-source rejection, never a silently absent field the
/// constructor defaults over (which answered `0000-01-01`).
#[test]
fn a_non_temporal_source_field_is_rejected() {
    let mut c = connection();
    for (query, expected) in [
        ("RETURN date({date: 42}) AS x", "Cannot get the date of: 42"),
        (
            "RETURN localdatetime({time: 42}) AS x",
            "Cannot get the time of: 42",
        ),
        (
            "RETURN datetime({year: 2020, month: 1, day: 1, time: 42}) AS x",
            "Cannot get the time of: 42",
        ),
    ] {
        let err = err_text(c.query(query));
        assert!(err.contains(expected), "{query}: {err}");
    }
}

/// B5: a null-valued form field does not select the form, so it cannot shift
/// the source date's year; the same field with a value does select it (the
/// week form's ISO week-year default).
#[test]
fn a_null_form_field_does_not_select_the_form() {
    let mut c = connection();
    assert_eq!(
        text(
            &mut c,
            "RETURN toString(date({date: date('2021-01-01'), week: null})) AS x"
        ),
        "2021-01-01"
    );
    assert_eq!(
        text(
            &mut c,
            "RETURN toString(date({date: date('2021-01-01'), week: 5})) AS x"
        ),
        "2020-01-31"
    );
}

/// B6: the validator answers which epoch field is present and the reader
/// reads exactly that one — a null-valued sibling neither wins the read nor
/// fails it.
#[test]
fn the_selected_epoch_field_is_the_one_read() {
    let mut c = connection();
    assert_eq!(
        text(
            &mut c,
            "RETURN toString(datetime({epochMillis: null, epochSeconds: 5})) AS x"
        ),
        "1970-01-01T00:00:05Z"
    );
    assert_eq!(
        text(&mut c, "RETURN toString(datetime({epochseconds: 5})) AS x"),
        "1970-01-01T00:00:05Z"
    );
}

/// B8: one zero-offset rule — the accessors spell a zero offset `Z`, the same
/// spelling the ISO suffix uses, while a non-zero offset keeps its numeric
/// form (verified against Neo4j 2026.09).
#[test]
fn the_timezone_accessor_renders_a_zero_offset_as_z() {
    let mut c = connection();
    for query in [
        "time('12:00Z').timezone",
        "time('12:00+00:00').timezone",
        "time('12:00').timezone",
    ] {
        assert_eq!(
            scalar_one(&mut c, &format!("RETURN {query} AS x")),
            Value::String("Z".into()),
            "{query}"
        );
    }
    assert_eq!(
        scalar_one(&mut c, "RETURN time('12:00+01:00').timezone AS x"),
        Value::String("+01:00".into())
    );
    assert_eq!(
        scalar_one(&mut c, "RETURN time('12:00Z').offset AS x"),
        Value::String("Z".into())
    );
}

/// RB1: a short zone name (`CET`) is a tz-database name on every map form.
/// The component-field path used to parse it as a numeric offset, so the
/// calendar-fields form failed `invalid timezone offset 'CET'` while the
/// clock form constructed — one typed variant, two meanings. Both forms now
/// construct through the one zone seam (Neo4j 2026.09 answers
/// `time({hour: 12, timezone: 'CET'})` as `12:00:00+01:00`).
#[test]
fn a_short_zone_name_resolves_on_every_map_form() {
    let mut c = connection();
    assert_eq!(
        text(
            &mut c,
            "RETURN toString(time({hour: 12, timezone: 'CET'})) AS x"
        ),
        "12:00:00+01:00"
    );
    assert_eq!(
        text(
            &mut c,
            "RETURN toString(datetime({year: 2020, month: 1, day: 1, timezone: 'CET'})) AS x"
        ),
        "2020-01-01T00:00:00+01:00"
    );
}

/// RB2: a proleptic civil datetime supplies its own time of day as a
/// `time`/`datetime` source; it used to fall through the reader's missing
/// civil arm and silently default to midnight. Its date half stays
/// unrepresentable as a `NaiveDate`, so the date reader answers the authored
/// unreadable-source rejection.
#[test]
fn a_civil_datetime_source_supplies_its_time_but_not_a_date() {
    let mut c = connection();
    assert_eq!(
        scalar_one(
            &mut c,
            "RETURN time(localdatetime('4294967397-01-01T12:30')).hour AS x",
        ),
        Value::Int(12)
    );
    assert_eq!(
        text(
            &mut c,
            "RETURN toString(localtime({time: localdatetime('4294967397-01-01T12:30')})) AS x"
        ),
        "12:30:00"
    );
    let err =
        err_text(c.query("RETURN date({date: localdatetime('4294967397-01-01T12:30')}) AS x"));
    assert!(err.contains("Cannot get the date of: +4294967397"), "{err}");
}

/// RB3: the epoch router reads the same non-null presence notion as every
/// other rule, so a null-valued epoch spelling neither selects the epoch form
/// nor hides the component fields beside it. (Neo4j itself rejects the null
/// epoch spelling; ignoring a null field is the engine's documented, stated
/// divergence.)
#[test]
fn a_null_epoch_field_leaves_the_component_form() {
    let mut c = connection();
    assert_eq!(
        text(
            &mut c,
            "RETURN toString(datetime({epochMillis: null, year: 2020, month: 1, day: 1})) AS x"
        ),
        "2020-01-01T00:00:00Z"
    );
}

/// RB4: a `localtime` literal never carries an offset; its own parser says so
/// (`cannot parse time`), instead of the kind falling through a shared guard
/// into `unknown temporal kind localtime`.
#[test]
fn localtime_rejects_an_offset_with_its_own_parse_error() {
    let mut c = connection();
    let err = err_text(c.query("RETURN localtime('12:00+01:00') AS x"));
    assert!(err.contains("cannot parse time"), "{err}");
    assert!(!err.contains("unknown temporal kind"), "{err}");
    // The offset-carrying `time` literal still parses.
    assert_eq!(
        text(&mut c, "RETURN toString(time('12:00+01:00')) AS x"),
        "12:00:00+01:00"
    );
}

/// RB5: a user-controlled offset field can saturate the signed total to
/// `i64::MIN`, whose `abs()` panics in debug builds. The magnitude check is a
/// typed rejection, never an engine panic.
#[test]
fn an_extreme_offset_is_a_typed_rejection_not_a_panic() {
    let mut c = connection();
    let err = err_text(c.query("RETURN datetime('2020-01-01T00:00-9223372036854775807:01') AS x"));
    assert!(err.contains("invalid timezone offset"), "{err}");
}

/// RB7: the epoch arm applies its component overrides over the instant's wall
/// clock in the resolved zone — the behavior `validate_epoch` documents and
/// Neo4j 2026.09 answers (`{epochSeconds: 0, year: 2020}` is
/// `2020-01-01T00:00Z`; `{epochMillis: 0, minute: 5, timezone: '+02:00'}` is
/// `1970-01-01T02:05+02:00`).
#[test]
fn epoch_component_fields_override_the_instant() {
    let mut c = connection();
    assert_eq!(
        text(
            &mut c,
            "RETURN toString(datetime({epochSeconds: 0, year: 2020})) AS x"
        ),
        "2020-01-01T00:00:00Z"
    );
    assert_eq!(
        text(
            &mut c,
            "RETURN toString(datetime({epochMillis: 0, minute: 5, timezone: '+02:00'})) AS x"
        ),
        "1970-01-01T02:05:00+02:00"
    );
}

/// C5: one stored zero offset has one spelling — a winter IANA wall clock
/// renders `Z[Europe/London]` (Neo4j 2026.09), a zero fixed offset answers
/// `Z` through the `timezone` accessor, and a `GMT` spelling keeps its own
/// text (Neo4j answers `GMT`).
#[test]
fn a_zero_offset_has_one_spelling() {
    let mut c = connection();
    assert_eq!(
        text(
            &mut c,
            "RETURN toString(datetime('2020-01-01T00:00[Europe/London]')) AS x"
        ),
        "2020-01-01T00:00:00Z[Europe/London]"
    );
    assert_eq!(
        scalar_one(
            &mut c,
            "RETURN datetime({year: 2020, month: 1, day: 1, timezone: '+00:00'}).timezone AS x",
        ),
        Value::String("Z".into())
    );
    assert_eq!(
        scalar_one(
            &mut c,
            "RETURN datetime({year: 2020, month: 1, day: 1, timezone: 'GMT'}).timezone AS x",
        ),
        Value::String("GMT".into())
    );
}

/// LABS-871: the offset grammar `parse_offset` owns carries at most one
/// leading sign; a second leading sign is the typed `invalid timezone
/// offset` rejection, never a sign silently folded away: `--01:00` used
/// to construct `-01:00` and `+-01:00` used to construct `+01:00`, on
/// the map form and the `UTC`/`GMT`-prefixed form alike. A `+-` literal
/// tail rejects through the same offset grammar; a `--` literal tail
/// never reaches it — the literal split declines a dash not preceded by
/// a digit, so the naive-body parse answers first.
#[test]
fn a_second_leading_sign_in_an_offset_is_a_typed_rejection() {
    let mut c = connection();
    for expr in [
        "datetime({year: 2020, month: 1, day: 1, timezone: '--01:00'})",
        "datetime({year: 2020, month: 1, day: 1, timezone: '+-01:00'})",
        "datetime({year: 2020, month: 1, day: 1, timezone: '++01:00'})",
        "datetime({year: 2020, month: 1, day: 1, timezone: '-+01:00'})",
        "datetime({year: 2020, month: 1, day: 1, timezone: 'UTC+-01:00'})",
        "datetime('2020-01-01T00:00:00+-01:00')",
        "time('12:00:00+-01:00')",
    ] {
        let err = err_text(c.query(&format!("RETURN {expr} AS x")));
        assert!(err.contains("invalid timezone offset"), "{expr}: {err}");
    }
    // A `--` literal tail never reaches the offset grammar: the literal
    // split declines it, so the body parse's own error answers.
    let err = err_text(c.query("RETURN datetime('2020-01-01T00:00:00--01:00') AS x"));
    assert!(err.contains("cannot parse time"), "{err}");
    // The single-sign forms still construct on both the map and literal
    // spellings.
    assert_eq!(
        text(
            &mut c,
            "RETURN toString(datetime({year: 2020, month: 1, day: 1, timezone: '-01:00'})) AS x"
        ),
        "2020-01-01T00:00:00-01:00"
    );
    assert_eq!(
        text(&mut c, "RETURN toString(time('12:00:00-01:00')) AS x"),
        "12:00:00-01:00"
    );
}
