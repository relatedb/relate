//! The builtin registry: the name-to-implementation table (the const rows
//! plus the generated clock-access rows), the per-row argument contract, and
//! the determinism classification. The parent module's `function()` is the
//! dispatch that reads this table.

use std::collections::BTreeMap;

use relate_core::temporal::{clock_spelling_rows, BetweenUnit, ClockKind, ClockMode};
use relate_core::{
    ArityContract, Result, Value, CHAR_LENGTH_ARITY, FORMAT_ARITY, NULLIF_ARITY, VECTOR_ARITY,
};
use relate_ir::Expr;

use crate::overlay::ExecGraph;
use crate::row::Row;

use super::{
    bar, convert, entity, list, math, property, scalar, spatial, string, temporal, vector,
};

/// Whether a builtin samples the clock or entropy on each call, so two
/// evaluations in one statement can return different values. This is the
/// one classification site: it reads the same rows the dispatch does,
/// case-insensitively, and callers must not re-derive the list. The clock
/// and truncate shapes sample the clock whatever their argument is (the
/// one-argument truncate truncates the current value), so their determinism
/// is derived from the shape; the callers treat a false positive as "no
/// fast path", never as an error.
pub(crate) fn is_nondeterministic(name: &str) -> bool {
    lookup_builtin(&name.to_ascii_lowercase())
        .is_some_and(|row| row.nondeterministic || row.implementation.samples_clock())
}

/// A builtin implementation.
pub(super) type Builtin = fn(&ExecGraph<'_>, &str, Vec<Value>) -> Result<Value>;

/// A form-row implementation: like [`Builtin`], but the call's arguments
/// arrive **unevaluated** — a form's trailing argument is syntax the form
/// lowers itself (a property-name identifier, a distance-metric literal),
/// never a value, so the dispatcher hands it the argument expressions.
pub(super) type Form = fn(&ExecGraph<'_>, &Row, &BTreeMap<String, Value>, &[Expr]) -> Result<Value>;

/// The implementation shape a builtin row carries, split on the one
/// policy the dispatcher acts on: a form row takes its arguments
/// unevaluated, and every value-taking shape wraps an [`Applied`] row —
/// the dispatcher evaluates the arguments exactly once, in order, before
/// any implementation runs.
#[derive(Clone, Copy)]
pub(super) enum BuiltinImplementation {
    /// A form row: the implementation receives the call's unevaluated
    /// argument expressions ([`Form`]).
    Form(Form),
    /// A value-taking row: the arguments are evaluated before the
    /// implementation runs (the dispatcher's one evaluation site).
    Applied(Applied),
}

/// The value-taking implementation shapes whose type is part of the
/// registration: the plain form for every name, and the row shapes that
/// carry their own data — the clock-access spelling names its kind and
/// mode, the truncate spelling its kind, the `duration.between`/
/// `duration.in*` family its [`BetweenUnit`] — so no implementation
/// re-derives it from the call name.
#[derive(Clone, Copy)]
pub(super) enum Applied {
    Plain(Builtin),
    Clock(ClockKind, ClockMode),
    Truncate(ClockKind),
    /// A `duration.between`/`duration.in*` row: the unit is part of the
    /// registration, so the implementation never re-derives it from the
    /// call name.
    Between(BetweenUnit),
}

impl BuiltinImplementation {
    /// Whether the implementation reads the clock on every call: the clock
    /// accessors do by definition, and the one-argument truncate truncates
    /// the current value. Derived from the shape, so a row cannot mislabel
    /// its own sampling.
    fn samples_clock(self) -> bool {
        matches!(
            self,
            BuiltinImplementation::Applied(Applied::Clock(..) | Applied::Truncate(_))
        )
    }
}

/// One builtin row: implementation plus its per-call determinism
/// classification (LABS-198: the shared `CALL { }` fast path refuses any
/// body calling a nondeterministic builtin, since it would sample once
/// instead of per outer row) and its least argument count (LABS-667: a
/// call below it is an arity error, not a null). Both flags live on the
/// row so a new builtin forces each decision where the implementation is
/// registered.
#[derive(Clone, Copy)]
pub(super) struct BuiltinRow {
    pub(super) name: &'static str,
    pub(super) implementation: BuiltinImplementation,
    pub(super) nondeterministic: bool,
    pub(super) arity: Arity,
}

/// A builtin's argument contract (LABS-667) as data — the least and
/// (optionally) the greatest argument count a call may supply, and the
/// requirement text a call outside the range violates. Every constant is
/// verified against live Neo4j 2026.09 (CYPHER 25); registration names one
/// of them, so the decision is made where the implementation lives and the
/// catalogue (LABS-758) adds constants, not match arms.
#[derive(PartialEq, Clone, Copy, Debug)]
pub(super) struct Arity {
    pub(super) min: usize,
    pub(super) max: Option<usize>,
    pub(super) requirement: &'static str,
    /// The names verified against live Neo4j for this contract — the spec
    /// the lockstep test pins the registered rows to; empty for the floor,
    /// whose every-row behavior the per-row test checks instead.
    pub(super) verified: &'static [&'static str],
}

/// No argument at all (e, pi, rand, randomUuid, timestamp).
pub(super) const ARITY_EXACT_ZERO: Arity = Arity {
    min: 0,
    max: Some(0),
    requirement: "requires no arguments",

    verified: &["e", "pi", "rand", "randomuuid", "timestamp"],
};
/// The temporal constructors and their GQL aliases: the clock read, the
/// single-argument form, or (value, pattern) — the two-argument form
/// verified live (\`time(null, null)\` returns a value; three is 42I13; a
/// non-map second argument is a TYPE error, the implementation's business).
pub(super) const ARITY_AT_MOST_TWO_TEMPORAL: Arity = Arity {
    min: 0,
    max: Some(2),
    requirement: "requires at most two arguments",

    verified: &[
        "date",
        "datetime",
        "local_date_time",
        "local_datetime",
        "local_time",
        "localdatetime",
        "localtime",
        "time",
        "zoned_date_time",
        "zoned_datetime",
        "zoned_time",
    ],
};
/// The clock-access spellings: the optional timezone argument, one at most
/// (verified live: `date.realtime()` and `date.realtime(tz)` are the two
/// accepted shapes, `date.realtime(42)` is 42I51/42001 and two arguments is
/// 42I13/42001). Its rows are generated from the clock spelling registry, so
/// the lockstep test pins them to that registry instead of a `verified`
/// list — the one arity whose spec is derived rather than hand-verified.
pub(super) const ARITY_AT_MOST_ONE: Arity = Arity {
    min: 0,
    max: Some(1),
    requirement: "requires at most one argument",

    verified: &[],
};
/// The `<kind>.truncate` spellings: a unit, an optional value, and an
/// optional fields map — four arguments are refused (verified live).
pub(super) const ARITY_ONE_TO_THREE: Arity = Arity {
    min: 1,
    max: Some(3),
    requirement: "requires one to three arguments",

    verified: &[
        "date.truncate",
        "datetime.truncate",
        "localdatetime.truncate",
        "localtime.truncate",
        "time.truncate",
    ],
};
/// `range`: the start and end, with an optional step — one argument and four
/// are both refused (verified live: `range(1)` is "Insufficient parameters"
/// and `range(1, 2, 3, 4)` "Too many parameters", both SyntaxError).
pub(super) const ARITY_TWO_TO_THREE: Arity = Arity {
    min: 2,
    max: Some(3),
    requirement: "requires two to three arguments",

    verified: &["range"],
};
/// `uuid`: no argument, the STRING/UUID echo, or CIP-246's two-integer
/// construction — three refused (verified live).
pub(super) const ARITY_AT_MOST_TWO: Arity = Arity {
    min: 0,
    max: Some(2),
    requirement: "requires at most two arguments",

    verified: &["uuid"],
};
/// `duration.between` and the three `duration.in*` siblings: exactly two
/// temporal bounds (verified live: `duration.between(x)` and
/// `duration.between(x, y, z)` are both 42I13, named with the called
/// spelling).
pub(super) const ARITY_EXACTLY_TWO: Arity = Arity {
    min: 2,
    max: Some(2),
    requirement: "requires exactly two arguments",

    verified: &[
        "duration.between",
        "duration.indays",
        "duration.inmonths",
        "duration.inseconds",
    ],
};
/// isnan, elementId, db.nameFromElementId, and the `toString` family: both
/// sides refused. `toString`/`toStringOrNull` are here rather than in the
/// floor because their implementations used to re-check the count after
/// argument evaluation — an arity failure must be reported before any
/// argument's own error.
pub(super) const ARITY_EXACTLY_ONE: Arity = Arity {
    min: 1,
    max: Some(1),
    requirement: "requires exactly one argument",

    verified: &[
        "db.namefromelementid",
        "elementid",
        "isnan",
        "tostring",
        "tostringornull",
    ],
};
/// `split`: the source string and the delimiter, both required (verified
/// live: `split('a,b', ',')`; one argument or three is 42I13).
pub(super) const ARITY_EXACTLY_TWO_STRINGS: Arity = Arity {
    min: 2,
    max: Some(2),
    requirement: "requires exactly two arguments",

    verified: &["split"],
};
/// `bar`: value, min, max and width, all four required. Engine-only (not in
/// the Cypher-25 corpus, so there is no live-Neo4j spec): `verified` stays
/// empty and the lockstep test pins the one registered name instead.
pub(super) const ARITY_EXACTLY_FOUR_ENGINE_ONLY: Arity = Arity {
    min: 4,
    max: Some(4),
    requirement: "requires exactly four arguments",

    verified: &[],
};
/// `property_exists` (CIP-247) and `vector_norm` (CIP-214): exactly two
/// arguments, the second of which is the literal the form lowers itself
/// (the property-name identifier, the distance metric).
pub(super) const ARITY_EXACTLY_TWO_FORMS: Arity = Arity {
    min: 2,
    max: Some(2),
    requirement: "requires two arguments",

    verified: &["property_exists", "vector_norm"],
};
/// `vector_distance` (CIP-214): the two vectors plus the distance-metric
/// literal the form lowers itself.
pub(super) const ARITY_EXACTLY_THREE_FORMS: Arity = Arity {
    min: 3,
    max: Some(3),
    requirement: "requires three arguments",

    verified: &["vector_distance"],
};
/// Everything else's floor; the upper bounds are the LABS-758 catalogue.
pub(super) const ARITY_AT_LEAST_ONE: Arity = Arity {
    min: 1,
    max: None,
    requirement: "requires at least one argument",

    verified: &[],
};

impl Arity {
    /// The requirement a call with `got` arguments violates, or `None` when
    /// the count is within the contract — derived from the bounds, so the
    /// constants are the whole policy.
    pub(super) fn violation(&self, got: usize) -> Option<&'static str> {
        if got < self.min || self.max.is_some_and(|max| got > max) {
            Some(self.requirement)
        } else {
            None
        }
    }
}

/// A row whose argument count `relate_core`'s [`ArityContract`] owns: the
/// front end, the planner and this pre-evaluation gate read one count, and
/// this row only states the layer's 42I13 wording. The contract's own
/// `verified` names live on [`Arity`].
const fn shared_arity(
    contract: ArityContract,
    requirement: &'static str,
    verified: &'static [&'static str],
) -> Arity {
    Arity {
        min: contract.min(),
        max: Some(contract.max()),
        requirement,
        verified,
    }
}

/// `nullif()`: exactly two, from the shared contract.
const ARITY_NULLIF: Arity =
    shared_arity(NULLIF_ARITY, "requires exactly two arguments", &["nullif"]);

/// `format()`: one or two, from the shared contract.
const ARITY_FORMAT: Arity =
    shared_arity(FORMAT_ARITY, "requires one or two arguments", &["format"]);

/// `vector()`: exactly three, from the shared contract.
const ARITY_VECTOR: Arity = shared_arity(VECTOR_ARITY, "requires three arguments", &["vector"]);

/// `char_length()`/`character_length()`: exactly one, from the shared contract.
const ARITY_CHAR_LENGTH: Arity = shared_arity(
    CHAR_LENGTH_ARITY,
    "requires one argument",
    &["char_length", "character_length"],
);

/// The one spelling of the row-driven arity rejection (LABS-667): it names
/// the function, the requirement, and the count — the Neo4j 42I13 shape.
pub(super) fn arity_rejection(name: &str, requirement: &str, got: usize) -> String {
    format!("InvalidNumberOfArguments: {name}() {requirement}, got {got}")
}

/// Row constructor keeping the table a flat name/implementation/flag
/// listing.
/// The row constructor: every registration states its argument contract
/// (LABS-667) — no defaulting, so a builtin cannot reach the table without
/// the arity decision being made where its implementation is registered.
pub(super) const fn row(
    name: &'static str,
    implementation: Builtin,
    nondeterministic: bool,
    arity: Arity,
) -> BuiltinRow {
    BuiltinRow {
        name,
        implementation: BuiltinImplementation::Applied(Applied::Plain(implementation)),
        nondeterministic,
        arity,
    }
}

/// Row constructor for a `duration.between`/`duration.in*` spelling: the row
/// carries the unit, mirroring [`truncate_row`].
pub(super) const fn between_row(name: &'static str, unit: BetweenUnit) -> BuiltinRow {
    BuiltinRow {
        name,
        implementation: BuiltinImplementation::Applied(Applied::Between(unit)),
        nondeterministic: false,
        arity: ARITY_EXACTLY_TWO,
    }
}

/// Row constructor for a form whose arguments are syntax rather than
/// values: the implementation receives the unevaluated arguments and
/// lowers its own literal argument (`property_exists`'s property-name
/// identifier, the vector metric forms' distance-metric literal).
pub(super) const fn form_row(name: &'static str, form: Form, arity: Arity) -> BuiltinRow {
    BuiltinRow {
        name,
        implementation: BuiltinImplementation::Form(form),
        nondeterministic: false,
        arity,
    }
}

/// Row constructor for a `<kind>.truncate` spelling: the row carries the
/// truncated kind.
pub(super) const fn truncate_row(name: &'static str, kind: ClockKind) -> BuiltinRow {
    BuiltinRow {
        name,
        implementation: BuiltinImplementation::Applied(Applied::Truncate(kind)),
        nondeterministic: false,
        arity: ARITY_ONE_TO_THREE,
    }
}

/// Name-to-implementation table for scalar builtins, keyed by the lowercased
/// call name. The clock-access spellings are generated beside it
/// ([`builtins`]), never listed here.
pub(super) const BUILTINS: &[BuiltinRow] = &[
    row("abs", math::bi_abs, false, ARITY_AT_LEAST_ONE),
    row("acos", math::bi_acos, false, ARITY_AT_LEAST_ONE),
    row("asin", math::bi_asin, false, ARITY_AT_LEAST_ONE),
    row("atan", math::bi_atan, false, ARITY_AT_LEAST_ONE),
    row("atan2", math::bi_atan2, false, ARITY_AT_LEAST_ONE),
    row("bar", bar::bi_bar, false, ARITY_EXACTLY_FOUR_ENGINE_ONLY),
    row("btrim", string::bi_btrim, false, ARITY_AT_LEAST_ONE),
    row(
        "cardinality",
        list::bi_cardinality,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row("ceil", math::bi_ceil, false, ARITY_AT_LEAST_ONE),
    row("ceiling", math::bi_ceil, false, ARITY_AT_LEAST_ONE),
    row(
        "char_length",
        string::bi_char_length,
        false,
        ARITY_CHAR_LENGTH,
    ),
    row(
        "character_length",
        string::bi_char_length,
        false,
        ARITY_CHAR_LENGTH,
    ),
    row("coalesce", scalar::bi_coalesce, false, ARITY_AT_LEAST_ONE),
    row(
        "coll.distinct",
        list::bi_coll_distinct,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "coll.flatten",
        list::bi_coll_flatten,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "coll.hasallof",
        list::bi_coll_has_all_of,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "coll.hasanyof",
        list::bi_coll_has_any_of,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "coll.hasnoneof",
        list::bi_coll_has_none_of,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "coll.indexof",
        list::bi_coll_indexof,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "coll.insert",
        list::bi_coll_insert,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row("coll.max", list::bi_coll_max, false, ARITY_AT_LEAST_ONE),
    row("coll.min", list::bi_coll_min, false, ARITY_AT_LEAST_ONE),
    row(
        "coll.remove",
        list::bi_coll_remove,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row("coll.sort", list::bi_coll_sort, false, ARITY_AT_LEAST_ONE),
    row("coll.zip", list::bi_coll_zip, false, ARITY_AT_LEAST_ONE),
    row("cos", math::bi_cos, false, ARITY_AT_LEAST_ONE),
    row("cosh", math::bi_cosh, false, ARITY_AT_LEAST_ONE),
    row("cot", math::bi_cot, false, ARITY_AT_LEAST_ONE),
    row("coth", math::bi_coth, false, ARITY_AT_LEAST_ONE),
    row("date", temporal::bi_date, true, ARITY_AT_MOST_TWO_TEMPORAL),
    truncate_row("date.truncate", ClockKind::Date),
    row(
        "datetime",
        temporal::bi_date,
        true,
        ARITY_AT_MOST_TWO_TEMPORAL,
    ),
    row(
        "datetime.fromepoch",
        temporal::bi_datetime_fromepoch,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "datetime.fromepochmillis",
        temporal::bi_datetime_fromepochmillis,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    truncate_row("datetime.truncate", ClockKind::DateTime),
    row(
        "db.namefromelementid",
        entity::bi_db_namefromelementid,
        false,
        ARITY_EXACTLY_ONE,
    ),
    row("degrees", math::bi_degrees, false, ARITY_AT_LEAST_ONE),
    row("duration", temporal::bi_date, false, ARITY_AT_LEAST_ONE),
    between_row("duration.between", BetweenUnit::Full),
    between_row("duration.indays", BetweenUnit::Days),
    between_row("duration.inmonths", BetweenUnit::Months),
    between_row("duration.inseconds", BetweenUnit::Seconds),
    row("e", math::bi_e, false, ARITY_EXACT_ZERO),
    row("elementid", entity::bi_elementid, false, ARITY_EXACTLY_ONE),
    row("endnode", entity::bi_endnode, false, ARITY_AT_LEAST_ONE),
    row("exists", scalar::bi_exists, false, ARITY_AT_LEAST_ONE),
    row("exp", math::bi_exp, false, ARITY_AT_LEAST_ONE),
    row("floor", math::bi_floor, false, ARITY_AT_LEAST_ONE),
    row("format", temporal::bi_format, false, ARITY_FORMAT),
    row("haversin", math::bi_haversin, false, ARITY_AT_LEAST_ONE),
    row("head", list::bi_head, false, ARITY_AT_LEAST_ONE),
    row("id", entity::bi_id, false, ARITY_AT_LEAST_ONE),
    row("isempty", list::bi_isempty, false, ARITY_AT_LEAST_ONE),
    row("isnan", math::bi_isnan, false, ARITY_EXACTLY_ONE),
    row("keys", entity::bi_keys, false, ARITY_AT_LEAST_ONE),
    row("labels", entity::bi_labels, false, ARITY_AT_LEAST_ONE),
    row("last", list::bi_last, false, ARITY_AT_LEAST_ONE),
    row("left", string::bi_left, false, ARITY_AT_LEAST_ONE),
    row("length", list::bi_length, false, ARITY_AT_LEAST_ONE),
    row("ln", math::bi_ln, false, ARITY_AT_LEAST_ONE),
    row(
        "local_date_time",
        temporal::bi_date,
        true,
        ARITY_AT_MOST_TWO_TEMPORAL,
    ),
    row(
        "local_datetime",
        temporal::bi_date,
        true,
        ARITY_AT_MOST_TWO_TEMPORAL,
    ),
    row(
        "local_time",
        temporal::bi_date,
        true,
        ARITY_AT_MOST_TWO_TEMPORAL,
    ),
    row(
        "localdatetime",
        temporal::bi_date,
        true,
        ARITY_AT_MOST_TWO_TEMPORAL,
    ),
    truncate_row("localdatetime.truncate", ClockKind::LocalDateTime),
    row(
        "localtime",
        temporal::bi_date,
        true,
        ARITY_AT_MOST_TWO_TEMPORAL,
    ),
    truncate_row("localtime.truncate", ClockKind::LocalTime),
    row("log", math::bi_ln, false, ARITY_AT_LEAST_ONE),
    row("log10", math::bi_log10, false, ARITY_AT_LEAST_ONE),
    row("lower", string::bi_lower, false, ARITY_AT_LEAST_ONE),
    row("ltrim", string::bi_ltrim, false, ARITY_AT_LEAST_ONE),
    row("nodes", entity::bi_nodes, false, ARITY_AT_LEAST_ONE),
    row("normalize", string::bi_normalize, false, ARITY_AT_LEAST_ONE),
    row("nullif", scalar::bi_nullif, false, ARITY_NULLIF),
    row(
        "octet_length",
        string::bi_octet_length,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row("pi", math::bi_pi, false, ARITY_EXACT_ZERO),
    row("point", spatial::bi_point, false, ARITY_AT_LEAST_ONE),
    row(
        "point.distance",
        spatial::bi_distance,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "point.withinbbox",
        spatial::bi_point_withinbbox,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "properties",
        entity::bi_properties,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    form_row(
        "property_exists",
        property::property_exists,
        ARITY_EXACTLY_TWO_FORMS,
    ),
    row("radians", math::bi_radians, false, ARITY_AT_LEAST_ONE),
    row("rand", math::bi_rand, true, ARITY_EXACT_ZERO),
    row("randomuuid", entity::bi_randomuuid, true, ARITY_EXACT_ZERO),
    row("range", list::bi_range, false, ARITY_TWO_TO_THREE),
    row(
        "relationships",
        entity::bi_relationships,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row("replace", string::bi_replace, false, ARITY_AT_LEAST_ONE),
    row("reverse", list::bi_reverse, false, ARITY_AT_LEAST_ONE),
    row("right", string::bi_right, false, ARITY_AT_LEAST_ONE),
    row("round", math::bi_round, false, ARITY_AT_LEAST_ONE),
    row("rtrim", string::bi_rtrim, false, ARITY_AT_LEAST_ONE),
    row("sign", math::bi_sign, false, ARITY_AT_LEAST_ONE),
    row("sin", math::bi_sin, false, ARITY_AT_LEAST_ONE),
    row("sinh", math::bi_sinh, false, ARITY_AT_LEAST_ONE),
    row("size", list::bi_size, false, ARITY_AT_LEAST_ONE),
    row("split", string::bi_split, false, ARITY_EXACTLY_TWO_STRINGS),
    row("sqrt", math::bi_sqrt, false, ARITY_AT_LEAST_ONE),
    row("startnode", entity::bi_startnode, false, ARITY_AT_LEAST_ONE),
    row(
        "string.indexof",
        string::bi_string_indexof,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "string.interp",
        string::bi_string_interp,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "string.join",
        string::bi_string_join,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "string.regexreplace",
        string::bi_string_regexreplace,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row("substring", string::bi_substring, false, ARITY_AT_LEAST_ONE),
    row("tail", list::bi_tail, false, ARITY_AT_LEAST_ONE),
    row("tan", math::bi_tan, false, ARITY_AT_LEAST_ONE),
    row("tanh", math::bi_tanh, false, ARITY_AT_LEAST_ONE),
    row("time", temporal::bi_date, true, ARITY_AT_MOST_TWO_TEMPORAL),
    truncate_row("time.truncate", ClockKind::Time),
    row("timestamp", temporal::bi_timestamp, true, ARITY_EXACT_ZERO),
    row("tobool", convert::bi_tobool, false, ARITY_AT_LEAST_ONE),
    row("toboolean", convert::bi_tobool, false, ARITY_AT_LEAST_ONE),
    // ADR-0497's `toBooleanList` add: the corpus's element-wise list
    // conversion, dispatched through the shared list-conversion body.
    row(
        "tobooleanlist",
        convert::bi_tobooleanlist,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "tobooleanornull",
        convert::bi_tobooleanornull,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row("tofloat", convert::bi_tofloat, false, ARITY_AT_LEAST_ONE),
    row(
        "tofloatlist",
        convert::bi_tofloatlist,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "tofloatornull",
        convert::bi_tofloatornull,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row("toint", convert::bi_toint, false, ARITY_AT_LEAST_ONE),
    row("tointeger", convert::bi_toint, false, ARITY_AT_LEAST_ONE),
    row(
        "tointegerlist",
        convert::bi_tointegerlist,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "tointegerornull",
        convert::bi_tointegerornull,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row("tolower", string::bi_lower, false, ARITY_AT_LEAST_ONE),
    row("tostring", convert::bi_tostring, false, ARITY_EXACTLY_ONE),
    row(
        "tostringlist",
        convert::bi_tostringlist,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "tostringornull",
        convert::bi_tostringornull,
        false,
        ARITY_EXACTLY_ONE,
    ),
    row("toupper", string::bi_upper, false, ARITY_AT_LEAST_ONE),
    row("trim", string::bi_trim, false, ARITY_AT_LEAST_ONE),
    row(
        "trim_leading",
        string::bi_trim_leading,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row("trim_spec", string::bi_trim_spec, false, ARITY_AT_LEAST_ONE),
    row(
        "trim_trailing",
        string::bi_trim_trailing,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row("type", entity::bi_type, false, ARITY_AT_LEAST_ONE),
    row("upper", string::bi_upper, false, ARITY_AT_LEAST_ONE),
    row("uuid", entity::bi_uuid, true, ARITY_AT_MOST_TWO),
    row(
        "uuid.leastsignificantbits",
        entity::bi_uuid_leastsignificantbits,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "uuid.mostsignificantbits",
        entity::bi_uuid_mostsignificantbits,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row("valuetype", scalar::bi_valuetype, false, ARITY_AT_LEAST_ONE),
    row("vector", vector::bi_vector, false, ARITY_VECTOR),
    row(
        "vector.similarity.cosine",
        vector::bi_vector_cosinesimilarity,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "vector.similarity.euclidean",
        vector::bi_vector_similarity_euclidean,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    row(
        "vector_dimension_count",
        vector::bi_vector_dimension_count,
        false,
        ARITY_AT_LEAST_ONE,
    ),
    form_row(
        "vector_distance",
        vector::vector_distance,
        ARITY_EXACTLY_THREE_FORMS,
    ),
    form_row("vector_norm", vector::vector_norm, ARITY_EXACTLY_TWO_FORMS),
    row(
        "zoned_date_time",
        temporal::bi_date,
        true,
        ARITY_AT_MOST_TWO_TEMPORAL,
    ),
    row(
        "zoned_datetime",
        temporal::bi_date,
        true,
        ARITY_AT_MOST_TWO_TEMPORAL,
    ),
    row(
        "zoned_time",
        temporal::bi_date,
        true,
        ARITY_AT_MOST_TWO_TEMPORAL,
    ),
];

/// The whole builtin table: the const rows plus the clock-access spellings,
/// whose kind, mode, contract and determinism are all derived from the clock
/// spelling registry — so a row cannot carry semantics its name does not
/// name, and there is no second registration to drift.
pub(super) fn builtins() -> &'static [BuiltinRow] {
    static TABLE: std::sync::OnceLock<Vec<BuiltinRow>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut rows = BUILTINS.to_vec();
        rows.extend(clock_spelling_rows().map(|(name, kind, mode)| BuiltinRow {
            name,
            implementation: BuiltinImplementation::Applied(Applied::Clock(kind, mode)),
            // Determinism is derived from the implementation shape
            // ([`is_nondeterministic`]), like the truncate rows.
            nondeterministic: false,
            arity: ARITY_AT_MOST_ONE,
        }));
        rows.sort_by_key(|row| row.name);
        rows
    })
}

pub(super) fn lookup_builtin(name: &str) -> Option<&'static BuiltinRow> {
    builtins()
        .binary_search_by_key(&name, |row| row.name)
        .ok()
        .map(|i| &builtins()[i])
}
