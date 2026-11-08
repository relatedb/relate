//! The canonical known-function registry: every function name the engine
//! accepts on lowered IR, advertised (`KNOWN_FUNCTIONS`, the `SHOW
//! FUNCTIONS` source) or unadvertised (`UNADVERTISED_FUNCTIONS`, e.g. the
//! lowering's internal renames). The planner binds against
//! [`is_known_function`], so a name `SHOW FUNCTIONS` advertises can no
//! longer be rejected by the binder (and vice versa). Unknown calls are
//! rejected by the planner on the lowered IR (`UnknownFunction`).
//!
//! Six registry names are additionally gated behind an ADR-0371 preview
//! feature ([`PREVIEW_FUNCTIONS`], ADR-0497): they stay in the registry so
//! an admitted call binds, but [`validate_function_admission`] rejects them
//! — as ordinary unknown functions — unless the store enabled their flag,
//! and `SHOW FUNCTIONS` filters their rows the same way.

use relate_core::{PreviewFeature, PreviewFeatures};
use relate_error::{EngineError, ErrorKind};
use relate_parser::{Expr, Statement};

use crate::ast::tree::walk_statement_tree;

/// The preview gate's rejection, byte-identical to
/// [`relate_core::Error::unknown_function`]'s pinned 42N48 text: the
/// gate must reject exactly like the binder, so a caller cannot tell the
/// two apart.
fn unknown_function_error(name: &str) -> EngineError {
    EngineError::semantic(
        ErrorKind::UnknownFunction,
        relate_error::unknown_function_detail(name),
    )
}

/// Known built-in function names.
///
/// Entries must be lowercase: `is_known_function` lowercases the call name
/// before the lookup, so a mixed-case entry can never match a real call (it
/// would be advertised but always rejected). The user-facing casing is a
/// rendering fact, owned by the executor's registry keys (the display
/// spellings of ADR-0497 §11) — never by this matching table.
pub const KNOWN_FUNCTIONS: &[&str] = &[
    "id",
    "elementid",
    "db.namefromelementid",
    "normalize",
    "labels",
    "type",
    "keys",
    "properties",
    "size",
    "isempty",
    "length",
    "head",
    "tail",
    "last",
    "reverse",
    "range",
    "coalesce",
    "tostring",
    "tostringlist",
    "uuid",
    "uuid.mostsignificantbits",
    "uuid.leastsignificantbits",
    "randomuuid",
    "tointeger",
    "tofloat",
    "toboolean",
    "tolower",
    "toupper",
    "upper",
    "lower",
    "trim",
    "ltrim",
    "rtrim",
    "btrim",
    "replace",
    "substring",
    "split",
    "left",
    "right",
    "abs",
    "ceil",
    "floor",
    "round",
    "sqrt",
    "sign",
    "rand",
    "exists",
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "collect",
    "stdev",
    "stdevp",
    "percentilecont",
    "percentiledisc",
    "collect_list",
    "stdev_samp",
    "stdev_pop",
    // CIP-245's GQL spellings: the canonical advertised pair is
    // `stdev_samp`/`stdev_pop` (the corpus catalogue's rows), so these two
    // are preview-gated (`gql-stddev-aliases`, ADR-0497).
    "stddev_samp",
    "stddev_pop",
    "percentile_cont",
    "percentile_disc",
    "nodes",
    "relationships",
    "startnode",
    "endnode",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "atan2",
    "log",
    "log10",
    "exp",
    "cot",
    "cosh",
    "coth",
    "sinh",
    "tanh",
    "haversin",
    "isnan",
    "timestamp",
    "degrees",
    "radians",
    "date",
    "datetime",
    "localdatetime",
    "localtime",
    "time",
    "duration",
    "format",
    "point",
    "point.distance",
    "vector",
    "vector_dimension_count",
    "vector.similarity.cosine",
    "vector.similarity.euclidean",
    "vector_distance",
    "vector_norm",
    "tofloatlist",
    "tobooleanlist",
    "tointegerlist",
    "point.withinbbox",
    "datetime.fromepoch",
    "datetime.fromepochmillis",
    "date.truncate",
    "datetime.truncate",
    "localdatetime.truncate",
    "localtime.truncate",
    "time.truncate",
    "duration.between",
    "duration.inmonths",
    "duration.indays",
    "duration.inseconds",
    "pi",
    "e",
    "none",
    "any",
    "all",
    "single", // quantifiers parsed as functions sometimes
    "reduce",
    "shortestpath",
    "allshortestpaths",
    "tobooleanornull",
    "tofloatornull",
    "tointegerornull",
    "tostringornull",
    "char_length",
    "character_length",
    "nullif",
    "valuetype",
    "linenumber",
    "file",
    "string.join",
    "string.regexreplace",
    "string.indexof",
    // GQL function aliases (CIP-245)
    "ceiling",
    "ln",
    "local_time",
    "local_datetime",
    "zoned_time",
    "zoned_datetime",
    "duration_between",
    "path_length",
    // GQL collection functions (CIP-222)
    "coll.flatten",
    "coll.sort",
    "coll.indexof",
    "coll.insert",
    "coll.remove",
    "coll.max",
    "coll.min",
    "coll.distinct",
    // GQL collection functions (CIP-274, CIP-286): preview-gated
    // (`coll-zip` / `coll-has`, ADR-0497) — the live probe records the
    // `coll.zip` name as unreachable and CIP-286 is unreleased, so neither
    // family may be default surface.
    "coll.zip",
    "coll.hasanyof",
    "coll.hasallof",
    "coll.hasnoneof",
    // GQL compliance (CIP-263, CIP-247)
    "cardinality",
    "property_exists",
    // The two adds (ADR-0497): CIP-217's `allReduce` parses to its own AST
    // node, and the corpus's `toBooleanList` row (ShowFunctions.feature:231)
    // converts element-wise — so each needs its catalogue row plus, for the
    // latter, its dispatch.
    "allreduce",
];

/// Function names the binder accepts on lowered IR but which are *not* part
/// of the user-facing registry (`KNOWN_FUNCTIONS`, the `SHOW FUNCTIONS`
/// source): the binder knows each one for a form the engine itself produces,
/// while `SHOW FUNCTIONS` never lists it and no reference server registers
/// it. Members:
///
/// - `graph.byname` -- valid only as a `USE` graph selector; it is listed
///   by `SHOW FUNCTIONS` through the executor's catalog-only entries, not
///   through `KNOWN_FUNCTIONS`.
/// - `string.interp` -- the CIP-261 interpolated string literal's lowered
///   form (`s'...{expr}...'`); users write the literal, never the call.
/// - `trim_spec` -- the comma spelling of the GQL trim special form
///   `trim(trimSpecification [, trimCharacterString], input)`; users write
///   `trim`/`ltrim`/`rtrim`/`btrim` (or the `... FROM ...` spelling), never
///   this internal name.
/// - `trim_leading`/`trim_trailing` -- the `trim(LEADING ...)` /
///   `trim(TRAILING ...)` lowering's target names (ADR-0497): the parser
///   compiles the special form to them, and the catalogue spellings users
///   write are `ltrim`/`rtrim`. Because the parser produces the same call
///   node either way, moving the spellings here is what removes their
///   `SHOW FUNCTIONS` rows while the lowering's own form keeps binding.
/// - `bar` -- the engine-only text bar `bar(value, min, max, width)`
///   (ADR-0542's admission class): callable in every build, not in the
///   Cypher-25 corpus, so it is never a `SHOW FUNCTIONS` row.
pub(crate) const UNADVERTISED_FUNCTIONS: &[&str] = &[
    "bar",
    "graph.byname",
    "string.interp",
    "trim_spec",
    "trim_leading",
    "trim_trailing",
];

/// Every function-name gate enabled, derived from [`PREVIEW_FUNCTIONS`]
/// itself: the one fixture the admission, listing and registry-coverage
/// tests start from, so a newly gated name is added in one place.
pub fn every_name_gate() -> PreviewFeatures {
    PREVIEW_FUNCTIONS
        .iter()
        .fold(PreviewFeatures::none(), |set, gated| {
            set.with_feature(gated.feature)
        })
}

/// The preview-gated function names, lowercase as the registry spells them
/// (ADR-0497 §4): the one list every consumer derives from the table, so a
/// gated entry is added or dropped in one place. `SHOW FUNCTIONS` renders
/// the display spellings (`coll.hasAnyOf`), so a comparison against
/// catalogue rows must be case-insensitive.
pub fn gated_function_names() -> impl Iterator<Item = &'static str> {
    PREVIEW_FUNCTIONS.iter().map(|gated| gated.name)
}

/// One preview-gated function name (ADR-0497 §4): the feature that admits
/// it. A gated name is catalogue surface exactly while its feature is
/// enabled.
struct PreviewFunction {
    feature: PreviewFeature,
    name: &'static str,
}

/// The preview-gated function names (ADR-0497 §4): each is admitted only
/// while its feature is enabled. The one name -> feature owner — admission
/// ([`validate_function_admission`]) and listing
/// ([`function_passes_name_gate`]) both read it, so a name cannot be
/// rejected while advertised or admitted
/// while unlisted. The table is temporary by construction: a row is deleted
/// when its CIP ships in a reference release (the registry row then becomes
/// default surface) or when its scenario is re-pinned.
const PREVIEW_FUNCTIONS: &[PreviewFunction] = &[
    PreviewFunction {
        feature: PreviewFeature::CollectionZip,
        name: "coll.zip",
    },
    PreviewFunction {
        feature: PreviewFeature::CollectionHas,
        name: "coll.hasanyof",
    },
    PreviewFunction {
        feature: PreviewFeature::CollectionHas,
        name: "coll.hasallof",
    },
    PreviewFunction {
        feature: PreviewFeature::CollectionHas,
        name: "coll.hasnoneof",
    },
    PreviewFunction {
        feature: PreviewFeature::GqlStddevAliases,
        name: "stddev_samp",
    },
    PreviewFunction {
        feature: PreviewFeature::GqlStddevAliases,
        name: "stddev_pop",
    },
];

/// The gated name's row, if `name` is preview-gated (case-insensitive, like
/// every function-call match; the unadvertised lowering internals are never
/// gated).
fn gated_function(name: &str) -> Option<&'static PreviewFunction> {
    PREVIEW_FUNCTIONS
        .iter()
        .find(|gated| gated.name.eq_ignore_ascii_case(name))
}

/// The preview feature gating function `name` (ADR-0497 §4): `Some(feature)`
/// when the name is one of [`PREVIEW_FUNCTIONS`], `None` for every
/// default-dialect built-in. This is the admission side of the table.
pub(crate) fn function_preview_feature(name: &str) -> Option<PreviewFeature> {
    gated_function(name).map(|gated| gated.feature)
}

/// Whether `name` passes the preview-name gate under `preview` (ADR-0497
/// §4): every name that is not preview-gated does; a gated name does only
/// while its feature is enabled. This is the listing side of the same table
/// the admission gate reads. It says nothing about whether the name exists —
/// a caller deciding catalogue membership asks `KNOWN_FUNCTIONS` (see the
/// executor's `advertised_function_names`).
pub fn function_passes_name_gate(name: &str, preview: PreviewFeatures) -> bool {
    match gated_function(name) {
        Some(gated) => preview.enabled(gated.feature),
        None => true,
    }
}

/// The function-name admission gate (ADR-0497 §4): every call in the
/// statement tree whose name is preview-gated is rejected with the ordinary
/// [`UnknownFunction`](ErrorKind::UnknownFunction) rejection unless `preview`
/// enables its feature. The statement's parsed spellings are what the gate
/// reads, so the lowering's name rewrites (`stddev_samp` ->
/// `stDev`, `trim(LEADING ...)` -> `trim_leading`) cannot smuggle a gated
/// name past it; the aggregate alias table therefore needs no gate of its
/// own.
///
/// The disabled form is deliberately not [`preview_feature_disabled_text`]'s
/// authored "set RELATE_PREVIEW" error: a name that must not exist
/// cannot be named in an error without advertising it, and 42N48 is what a
/// reference server answers for an unregistered name (ADR-0497 §4).
pub(crate) fn validate_function_admission(
    stmt: &Statement,
    preview: PreviewFeatures,
) -> Result<(), EngineError> {
    walk_statement_tree(
        stmt,
        &mut |_| Ok(()),
        &mut |_| Ok(()),
        &mut |expr| match expr {
            Expr::FunctionCall(call) => match call.name.with_str(function_preview_feature) {
                Some(feature) if !preview.enabled(feature) => {
                    Err(call.name.with_str(unknown_function_error))
                }
                _ => Ok(()),
            },
            _ => Ok(()),
        },
    )
}

/// True when `name` (any casing) is a function the engine accepts: every
/// advertised built-in (`KNOWN_FUNCTIONS`, which includes the aggregate
/// names and their CIP-128 aliases) plus the unadvertised lowered-internal
/// names (`UNADVERTISED_FUNCTIONS`). This is the single known-function
/// registry the planner binds against, so `SHOW FUNCTIONS` can no longer
/// advertise a name the binder rejects.
///
/// Preview-gated names stay in the registry so an admitted call binds; the
/// admission gate ([`validate_function_admission`]) is what hides them while
/// their feature is off, and it runs before the binder.
pub fn is_known_function(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    KNOWN_FUNCTIONS.contains(&lower.as_str())
        || UNADVERTISED_FUNCTIONS.contains(&lower.as_str())
        // The clock-access spellings are vocabulary, not separate builtins:
        // their one source of truth is the temporal clock module.
        || relate_core::temporal::clock_access(&lower).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ADR-0497 disposition in one table: the names that stop being
    /// callable (`removed`) versus the spellings that move to the lowering's
    /// unadvertised set (`renamed_internal`) — pinned literally, so a
    /// registry edit that put one back fails here.
    #[test]
    fn the_removed_and_internalised_names_leave_the_registry() {
        for removed in [
            "apoc.coll.sort",
            "distance",
            "rels",
            "typename",
            "vector.add",
            "vector.subtract",
            "vector.cosinesimilarity",
            "vector.dimensions",
            "vector.normalize",
            "vector.manhattan",
            "vector.euclideandistance",
            "vector.dotproduct",
            "point.withindistance",
            "linestring",
            "polygon",
            "spatial.contains",
            "spatial.within",
            "spatial.intersects",
            "spatial.overlaps",
            "spatial.crosses",
            "spatial.touches",
            "spatial.area",
            "spatial.length",
            "spatial.centroid",
            "spatial.towkt",
            "spatial.fromwkt",
            "spatial.togeojson",
            "spatial.fromgeojson",
        ] {
            assert!(
                !is_known_function(removed),
                "{removed} must be an unknown function"
            );
            assert!(
                !KNOWN_FUNCTIONS.contains(&removed),
                "{removed} is advertised"
            );
        }
        // The trim special forms' internal targets: binder-known (the
        // parser lowers `trim(LEADING ...)` to them) but never advertised.
        for internal in ["trim_leading", "trim_trailing"] {
            assert!(is_known_function(internal), "{internal} must keep binding");
            assert!(
                !KNOWN_FUNCTIONS.contains(&internal),
                "{internal} must not be advertised"
            );
            assert!(UNADVERTISED_FUNCTIONS.contains(&internal));
        }
        // The engine-only admission class: binder-known, never advertised.
        assert!(is_known_function("bar"));
        assert!(!KNOWN_FUNCTIONS.contains(&"bar"));
        assert!(UNADVERTISED_FUNCTIONS.contains(&"bar"));
        // The canonical spellings the renames target are untouched.
        for canonical in [
            "ltrim",
            "rtrim",
            "relationships",
            "valuetype",
            "vector_dimension_count",
            "vector_norm",
            "vector.similarity.cosine",
            "vector_distance",
            "point.distance",
        ] {
            assert!(KNOWN_FUNCTIONS.contains(&canonical), "{canonical} lost");
        }
        // The two additions.
        for added in ["tobooleanlist", "allreduce"] {
            assert!(is_known_function(added), "{added} must be callable");
            assert!(KNOWN_FUNCTIONS.contains(&added), "{added} is unadvertised");
        }
    }

    #[test]
    fn every_gated_name_is_a_registry_name_with_one_owner() {
        let all = every_name_gate();
        for gated in PREVIEW_FUNCTIONS {
            assert!(
                KNOWN_FUNCTIONS.contains(&gated.name),
                "{} is gated but not in the registry, so an admitted call \
                 would still be rejected by the binder",
                gated.name
            );
            assert_eq!(
                function_preview_feature(gated.name),
                Some(gated.feature),
                "{}",
                gated.name
            );
            // Matching is case-insensitive, like every function call.
            assert_eq!(
                function_preview_feature(&gated.name.to_ascii_uppercase()),
                Some(gated.feature),
                "{}",
                gated.name
            );
            // The listing side reads the same row: never listed while the
            // flag is off, listed while on.
            assert!(
                !function_passes_name_gate(gated.name, PreviewFeatures::none()),
                "{} must not be listed without its opt-in",
                gated.name
            );
            assert!(
                function_passes_name_gate(gated.name, all),
                "{} must be listed with its opt-in",
                gated.name
            );
        }
        // The gated set is exactly the three features' six names: a
        // registry row added or dropped without the matching table change
        // fails here.
        assert_eq!(PREVIEW_FUNCTIONS.len(), 6);
        let derived: Vec<&str> = gated_function_names().collect();
        let table: Vec<&str> = PREVIEW_FUNCTIONS.iter().map(|gated| gated.name).collect();
        assert_eq!(derived, table, "the accessor must expose every gated name");
        assert_eq!(function_preview_feature("coll.sort"), None);
        assert_eq!(function_preview_feature("stdev_samp"), None);
        assert_eq!(function_preview_feature("db.labels"), None);
        // A default-dialect name is registry surface and passes the name
        // gate under any preview set: the gate only ever withholds gated
        // names.
        assert!(KNOWN_FUNCTIONS.contains(&"coll.sort"));
        assert!(function_passes_name_gate(
            "coll.sort",
            PreviewFeatures::none()
        ));
    }

    /// The admission gate: a gated name is an ordinary unknown function
    /// while its flag is off — at every nesting depth — and binds once the
    /// flag is on. One case table, so a newly gated name is added once and
    /// both sides of the gate are proven for it.
    #[test]
    fn gated_names_are_unknown_functions_until_their_flag_is_enabled() {
        let disabled = PreviewFeatures::none();
        let enabled = every_name_gate();
        // (query, admitted with every name-gate on, rejected without them)
        let cases = [
            ("RETURN coll.zip([1], [2])", true, true),
            ("RETURN coll.hasAnyOf([1], [1])", true, true),
            ("RETURN coll.hasAllOf([1], [1])", true, true),
            ("RETURN coll.hasNoneOf([1], [2])", true, true),
            ("RETURN stddev_samp(1.0)", true, true),
            ("RETURN stddev_pop(1.0)", true, true),
            (
                "CALL { RETURN coll.zip([1], [2]) AS z } RETURN z",
                true,
                true,
            ),
            (
                "RETURN EXISTS { RETURN stddev_samp(1.0) AS s } AS e",
                true,
                true,
            ),
            // Not gated at all: `coll.sort` is CIP-222 default surface in
            // every dialect.
            ("RETURN coll.sort([2, 1])", true, false),
        ];
        for (query, admitted_when_enabled, rejected_when_disabled) in cases {
            let stmt = relate_parser::parse(query).expect("the statement parses");
            let disabled_result = validate_function_admission(&stmt, disabled);
            if rejected_when_disabled {
                let error = disabled_result
                    .expect_err(&format!("{query} must be rejected without the opt-in"))
                    .to_string();
                assert!(error.starts_with("UnknownFunction: "), "{query}: {error}");
            } else {
                assert!(
                    disabled_result.is_ok(),
                    "{query} must not be gated without the opt-in"
                );
            }
            let enabled_result = validate_function_admission(&stmt, enabled);
            if admitted_when_enabled {
                assert!(
                    enabled_result.is_ok(),
                    "{query} must be admitted with its opt-in"
                );
            } else {
                assert!(
                    enabled_result.is_err(),
                    "{query} must stay rejected with the name gates on"
                );
            }
        }
    }

    // `is_known_function` lowercases the call name before `.contains()`,
    // so any KNOWN_FUNCTIONS entry that is not already lowercase can never
    // match a real call -- it would be an advertised-but-always-rejected
    // name (exactly the `vector.cosineSimilarity` regression that shipped a
    // capital `S`). This invariant test forbids that whole class of bug.
    #[test]
    fn every_known_function_entry_is_lowercase() {
        let offenders: Vec<&&str> = KNOWN_FUNCTIONS
            .iter()
            .filter(|name| **name != name.to_lowercase())
            .collect();
        assert!(
            offenders.is_empty(),
            "KNOWN_FUNCTIONS entries must be lowercase (the validator lowercases \
             the call name before lookup); unreachable entries: {offenders:?}"
        );
    }
}
