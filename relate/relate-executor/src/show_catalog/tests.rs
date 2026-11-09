//! The catalog renderers' unit tests — split out of `show_catalog.rs`
//! so the production file stays well under the 1k-line boundary.

use super::*;

use crate::function_registry::FunctionCategory;
use crate::procedure_registry::is_ontology_procedure;
use crate::Graph;

/// The one required-cell reader: a missing or non-STRING cell panics
/// naming the row and column.
fn text<'a>(row: &'a Row, column: &str, name: &str) -> &'a str {
    row[column]
        .as_str()
        .unwrap_or_else(|| panic!("{name} has no {column} cell"))
}

fn name_of(row: &Row) -> &str {
    row["name"]
        .as_str()
        .expect("every catalog row carries a string name")
}

/// A role column's list cell as strings.
fn roles<'a>(row: &'a Row, column: &str, name: &str) -> Vec<&'a str> {
    let cell = &row[column];
    cell.as_list()
        .unwrap_or_else(|| panic!("{name}.{column} is not a list: {cell:?}"))
        .iter()
        .map(|v| {
            v.as_str()
                .unwrap_or_else(|| panic!("{name}.{column} holds a non-STRING role"))
        })
        .collect()
}

fn is_ontology_row(row: &Row) -> bool {
    is_ontology_procedure(name_of(row))
}

/// A row's `signature` cell starts with its own `name` cell: the renderer
/// names a row from its registry name for both cells — including the
/// hand-written `special_signature` tails — so a disagreement means the
/// row carries two names.
fn assert_signature_names_row(row: &Row) {
    let name = name_of(row);
    let signature = text(row, "signature", name);
    assert!(
        signature.starts_with(&format!("{name}(")),
        "{name} renders `{signature}`"
    );
}

/// The boolean-cell reader, the twin of [`text`]: a missing or non-BOOL
/// cell panics naming the row and column.
fn flag(row: &Row, column: &str) -> bool {
    let name = name_of(row);
    row[column]
        .as_bool()
        .unwrap_or_else(|| panic!("{name} has no {column} cell"))
}

/// The catalog-metadata floor both renderers share (ADR-0497 §6/§8): a
/// listed row carries a non-empty description and a signature naming its
/// own row, never the `(...) :: ANY` placeholder — a placeholder or blank
/// cell is a catalogue gap, not a display detail.
fn assert_row_carries_catalog_metadata(row: &Row) {
    let name = name_of(row);
    let description = text(row, "description", name);
    assert!(!description.is_empty(), "{name} has an empty description");
    let signature = text(row, "signature", name);
    assert!(
        !signature.contains("(...) :: ANY"),
        "{name} keeps the placeholder signature `{signature}`"
    );
    assert_signature_names_row(row);
}

/// The catalog rows whose rendered name is spelled in mixed case, one
/// entry per rendered row (no dedup: a name that lost its spelling on an
/// arity sibling row must fail the count, not hide behind it), sorted:
/// the display-spelling surface of one preview set.
fn mixed_case_names(rows: &[Row]) -> Vec<&str> {
    let mut names: Vec<&str> = rows
        .iter()
        .map(name_of)
        .filter(|name| name.chars().any(|c| c.is_ascii_uppercase()))
        .collect();
    names.sort_unstable();
    names
}

#[test]
fn show_functions_hides_the_preview_gated_names_while_their_flags_are_disabled() {
    // ADR-0497 §4: the catalogue and the admission gate read the one
    // `function_preview_feature` table, so a disabled name is listed
    // nowhere.
    let disabled: Vec<String> = function_catalog_rows(Graph::default().preview())
        .iter()
        .map(|row| name_of(row).to_ascii_lowercase())
        .collect();
    // Derived from the one table, never hand-copied: `SHOW FUNCTIONS`
    // renders display spellings (`coll.hasAnyOf`), so the lowercased
    // row names are compared against the accessor's lowercase entries.
    let gated: Vec<&str> = relate_front::validate::gated_function_names().collect();
    for name in &gated {
        assert!(
            !disabled.contains(&name.to_string()),
            "{name} must not be listed without its opt-in"
        );
    }

    let mut enabled = Graph::default();
    enabled.configure_preview(relate_front::validate::every_name_gate());
    let listed: Vec<String> = function_catalog_rows(enabled.preview())
        .iter()
        .map(|row| name_of(row).to_ascii_lowercase())
        .collect();
    for name in &gated {
        assert!(
            listed.contains(&name.to_string()),
            "{name} must be listed with its opt-in"
        );
    }
    // Enabling the gates adds exactly the gated names and leaves every
    // other row identical and in order.
    assert_eq!(listed.len(), disabled.len() + gated.len());
    let other_rows: Vec<String> = listed
        .iter()
        .filter(|name| !gated.contains(&name.as_str()))
        .cloned()
        .collect();
    assert_eq!(other_rows, disabled);
}

#[test]
fn show_procedures_hides_the_ontology_family_while_the_preview_is_disabled() {
    // ADR-0378: a default executor graph carries the empty preview set,
    // so no ontology-family row is discoverable.
    let disabled = procedure_catalog_rows(Graph::default().preview());
    assert!(
        disabled.iter().all(|row| !is_ontology_row(row)),
        "a disabled store lists no db.ontology.* row"
    );

    let mut enabled = Graph::default();
    enabled.configure_preview(PreviewFeatures::parse("ontology").unwrap());
    let listed = procedure_catalog_rows(enabled.preview());
    // Enabling the gate adds exactly the registry's ontology-family rows
    // that the catalog lists — the count applies the catalog's own
    // `listed` filter as well as the family predicate — and leaves every
    // other row identical and in order.
    let catalog_family: Vec<_> = BUILTIN_PROCEDURE_SIGNATURES
        .iter()
        .filter(|signature| signature.listed)
        .filter(|signature| is_ontology_procedure(signature.name))
        .collect();
    assert_eq!(listed.len(), disabled.len() + catalog_family.len());
    let other_rows: Vec<Row> = listed
        .iter()
        .filter(|row| !is_ontology_row(row))
        .cloned()
        .collect();
    assert_eq!(other_rows, disabled);

    // Each listed row carries the registry's own name, mode,
    // description and rendered signature: the gate changes availability
    // only.
    for signature in catalog_family {
        let row = listed
            .iter()
            .find(|row| name_of(row) == signature.name)
            .unwrap_or_else(|| panic!("{} must be listed while enabled", signature.name));
        assert_eq!(row["mode"], CypherValue::Str(signature.mode.to_string()));
        assert_eq!(
            row["description"],
            CypherValue::Str(signature.description.to_string())
        );
        assert_eq!(
            row["signature"],
            CypherValue::Str(render_procedure_signature(
                signature.name,
                signature.args,
                signature.returns,
            ))
        );
    }
}

/// ADR-0497 §6: every `SHOW PROCEDURES` row the catalog lists renders a
/// non-empty description and the CIP-113 signature of its own name —
/// the catalog is the whole inventory, so a row with a placeholder or a
/// blank signature is a catalogue gap rather than a display detail.
#[test]
fn every_listed_row_renders_a_description_and_a_cip113_signature() {
    let rows = procedure_catalog_rows(Graph::default().preview());
    assert!(!rows.is_empty());
    for row in &rows {
        assert_row_carries_catalog_metadata(row);
    }
}

/// ADR-0497 §8: every `SHOW FUNCTIONS` row of a default store carries the
/// brief columns — a non-empty description and a CIP-113 signature
/// rendered from typed parameters (never the old `(...) :: ANY`
/// placeholder). The category cell is the registry row's own
/// `FunctionCategory` (rendered verbatim by `function_row`), and the
/// row-per-advertised-name pairing is pinned by
/// `function_registry::the_metadata_tables_cover_exactly_the_advertised_registry`.
#[test]
fn every_function_catalog_row_carries_full_metadata() {
    let rows = function_catalog_rows(Graph::default().preview());
    assert!(!rows.is_empty());
    for row in &rows {
        assert_row_carries_catalog_metadata(row);
        let name = name_of(row);
        let signature = text(row, "signature", name);
        assert!(
            !signature.contains("(...) :: ANY"),
            "{name} keeps the placeholder signature `{signature}`"
        );
    }
}

/// The `aggregating` cell is the row's own `Aggregating` category, never a
/// second name-based lookup: every listed row's cell agrees with its
/// category and with the engine's aggregate vocabulary, so a registry row
/// cannot claim a classification the aggregation machinery does not fold
/// (or miss one it does). The aggregate rows are the aggregate catalogue,
/// gated aliases included.
#[test]
fn the_aggregating_cell_is_the_rows_own_aggregate_category() {
    let mut every_gate = Graph::default();
    every_gate.configure_preview(relate_front::validate::every_name_gate());
    let rows = function_catalog_rows(every_gate.preview());
    for row in &rows {
        let name = name_of(row);
        let category = text(row, "category", name);
        let aggregating = flag(row, "aggregating");
        assert_eq!(
            aggregating,
            category == FunctionCategory::Aggregating.as_str(),
            "{name}'s aggregating cell disagrees with its `{category}` category"
        );
        assert_eq!(
            aggregating,
            relate_front::agg::is_aggregate_name(name),
            "{name}'s aggregating cell disagrees with the aggregate vocabulary"
        );
    }
    let mut aggregates: Vec<&str> = rows
        .iter()
        .filter(|row| flag(row, "aggregating"))
        .map(name_of)
        .collect();
    aggregates.sort_unstable();
    assert_eq!(
        aggregates,
        [
            "avg",
            "collect",
            "collect_list",
            "count",
            "max",
            "min",
            "percentileCont",
            "percentileDisc",
            "percentile_cont",
            "percentile_disc",
            "stDev",
            "stDevP",
            "stddev_pop",
            "stddev_samp",
            "stdev_pop",
            "stdev_samp",
            "sum",
        ]
    );
}

/// The `show_expectations` corpus rows this metadata is filled from
/// (`features/correctness/ShowFunctions.feature`): the spot rows assert
/// name, category and description verbatim, and the `Numeric` filter
/// lists the rounding family once per `round` arity (lines 2660-2670).
#[test]
fn function_metadata_matches_the_recorded_corpus_rows() {
    let rows = function_catalog_rows(Graph::default().preview());
    let named =
        |name: &str| -> Vec<&Row> { rows.iter().filter(|row| name_of(row) == name).collect() };
    let one = |name: &str| -> &Row {
        let mut found = named(name);
        assert_eq!(found.len(), 1, "{name} must be listed once");
        found.pop().expect("checked len")
    };

    let abs = one("abs");
    assert_eq!(text(abs, "category", name_of(abs)), "Numeric");
    assert_eq!(
        text(abs, "description", name_of(abs)),
        "Returns the absolute value of an `INTEGER` or `FLOAT`."
    );

    let from_element_id = one("db.nameFromElementId");
    assert_eq!(
        text(from_element_id, "category", name_of(from_element_id)),
        "Database"
    );
    assert_eq!(
        text(from_element_id, "description", name_of(from_element_id)),
        "Resolves the database name for the given element id"
    );

    let tail = one("tail");
    assert_eq!(text(tail, "category", name_of(tail)), "List");
    assert_eq!(
        text(tail, "description", name_of(tail)),
        "Returns all but the first element in a `LIST<ANY>`."
    );

    // Lines 82-83: the arity overloads render as two rows with their own
    // descriptions, transcribed verbatim from the corpus (the second
    // cell really does end in `....`).
    let flatten = named("coll.flatten");
    let mut descriptions: Vec<&str> = flatten
        .iter()
        .map(|row| text(row, "description", name_of(row)))
        .collect();
    descriptions.sort_unstable();
    assert_eq!(
        descriptions,
        [
            "Returns a list flattened to ....",
            "Returns a list flattened to the given nesting depth.",
        ]
    );

    // Lines 2660-2670: the `Numeric` category is exactly the rounding
    // family, with `round` repeated per arity row.
    let mut numeric: Vec<&str> = rows
        .iter()
        .filter(|row| text(row, "category", name_of(row)) == "Numeric")
        .map(name_of)
        .collect();
    numeric.sort_unstable();
    assert_eq!(
        numeric,
        ["abs", "ceil", "ceiling", "floor", "isNaN", "rand", "round", "round", "round", "sign",]
    );
}

/// The display-spelling contract (ADR-0497 §11): the catalog's mixed-case
/// names are exactly the reference catalogue's. The reference is the
/// Cypher-25 corpus's generated `ShowFunctions*` tables, live-verified
/// against Neo4j 2026.10.1 (`SHOW FUNCTIONS YIELD name`): 42 default
/// rows, plus the three preview-gated `coll.has*` rows, which render
/// their CIP camelCase. Every other row renders its lowercase spelling,
/// so a name that loses or gains a spelling — or gains a typo'd one —
/// fails here instead of silently rendering lowercase. That every
/// rendered spelling is still callable in its own casing is the
/// case-insensitive matching fact pinned end-to-end by
/// `canonical_registries::catalog_display_spellings_are_callable_in_their_own_casing`.
#[test]
fn the_catalog_renders_the_reference_display_spellings() {
    /// The 42 default-surface names the reference catalogue spells in
    /// mixed case.
    const DEFAULT: &[&str] = &[
        "allReduce",
        "allShortestPaths",
        "coll.indexOf",
        "db.nameFromElementId",
        "duration.inDays",
        "duration.inMonths",
        "duration.inSeconds",
        "elementId",
        "endNode",
        "graph.byElementId",
        "graph.byName",
        "graph.propertiesByName",
        "isEmpty",
        "isNaN",
        "nullIf",
        "percentileCont",
        "percentileDisc",
        "point.withinBBox",
        "randomUUID",
        "shortestPath",
        "startNode",
        "stDev",
        "stDevP",
        "string.indexOf",
        "string.regexReplace",
        "toBoolean",
        "toBooleanList",
        "toBooleanOrNull",
        "toFloat",
        "toFloatList",
        "toFloatOrNull",
        "toInteger",
        "toIntegerList",
        "toIntegerOrNull",
        "toLower",
        "toString",
        "toStringList",
        "toStringOrNull",
        "toUpper",
        "uuid.leastSignificantBits",
        "uuid.mostSignificantBits",
        "valueType",
    ];
    /// The preview-gated names, rendered in their CIP camelCase: they
    /// join the default set exactly while their flags are on.
    const GATED: &[&str] = &["coll.hasAllOf", "coll.hasAnyOf", "coll.hasNoneOf"];

    let mut default_expected = DEFAULT.to_vec();
    default_expected.sort_unstable();
    let default_rows = function_catalog_rows(Graph::default().preview());
    assert_eq!(mixed_case_names(&default_rows), default_expected);

    let mut every_expected: Vec<&str> = DEFAULT.iter().chain(GATED).copied().collect();
    every_expected.sort_unstable();
    let mut every_gate = Graph::default();
    every_gate.configure_preview(relate_front::validate::every_name_gate());
    let gated_rows = function_catalog_rows(every_gate.preview());
    assert_eq!(mixed_case_names(&gated_rows), every_expected);

    // A row's `signature` cell starts with its own `name` cell: the only
    // hand-written signature text (`special_signature` tails, e.g.
    // `allReduce`) must agree with the row's own name spelling, or the row
    // would carry two names. Covers every rendered row, gated rows
    // included.
    for row in &gated_rows {
        assert_signature_names_row(row);
    }
}

/// Live Neo4j 2026.10.1 (`SHOW FUNCTIONS YIELD name`): the catalog is
/// case-sensitively name-sorted, not registration-ordered. The `Numeric`
/// filter therefore lists the rounding family in reference order, and
/// the mixed-case `percentileCont` sorts before `percentile_cont`
/// (`C` < `_`) — a case-insensitive sort would invert the pair.
#[test]
fn function_catalog_renders_the_reference_name_order() {
    let rows = function_catalog_rows(Graph::default().preview());
    let names: Vec<&str> = rows.iter().map(name_of).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "catalog rows are not in name order");

    let numeric: Vec<&str> = rows
        .iter()
        .filter(|row| text(row, "category", name_of(row)) == "Numeric")
        .map(name_of)
        .collect();
    assert_eq!(
        numeric,
        ["abs", "ceil", "ceiling", "floor", "isNaN", "rand", "round", "round", "round", "sign",]
    );

    let position = |name: &str| names.iter().position(|n| *n == name).expect(name);
    assert!(position("percentileCont") < position("percentile_cont"));
}

/// The `rolesExecution`/`rolesBoostedExecution` cells copy live Neo4j
/// 2026.10.1: every built-in function is executable by PUBLIC plus the
/// five roles with nothing boosted, and a procedure is admin-only
/// exactly when its `admin` flag says so (boosted always `admin`). The
/// role cell is derived from the flag, so the two cannot disagree; the
/// registry's `admin` values are live-verified (`SHOW PROCEDURES YIELD
/// name, admin`: 34 admin rows).
#[test]
fn role_cells_match_live_neo4j() {
    let rows = function_catalog_rows(Graph::default().preview());
    for row in &rows {
        let name = name_of(row);
        assert_eq!(
            roles(row, "rolesExecution", name),
            [
                "PUBLIC",
                "admin",
                "architect",
                "editor",
                "publisher",
                "reader"
            ],
            "{name}"
        );
        assert!(
            roles(row, "rolesBoostedExecution", name).is_empty(),
            "{name}"
        );
    }

    let procedures = procedure_catalog_rows(Graph::default().preview());
    assert!(!procedures.is_empty());
    // The admin inventory is pinned independently of the roles derivation
    // below: live 2026.10.1 `SHOW PROCEDURES YIELD name, admin` lists 34
    // admin rows, `db.clearQueryCaches` among them, `db.labels` not.
    let admin_names: Vec<&str> = procedures
        .iter()
        .filter(|row| flag(row, "admin"))
        .map(name_of)
        .collect();
    assert_eq!(admin_names.len(), 34);
    assert!(admin_names.contains(&"db.clearQueryCaches"));
    assert!(!admin_names.contains(&"db.labels"));
    for row in &procedures {
        let name = name_of(row);
        let admin = flag(row, "admin");
        let expected: &[&str] = if admin {
            &["admin"]
        } else {
            &["PUBLIC", "admin"]
        };
        assert_eq!(roles(row, "rolesExecution", name), expected, "{name}");
        assert_eq!(
            roles(row, "rolesBoostedExecution", name),
            ["admin"],
            "{name}"
        );
    }
}
