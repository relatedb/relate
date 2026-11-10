//! Differential comparison (ADR-0144): normalize one Neo4j Query API v2
//! row (JSON) and one relate row (`CypherValue`) to a comparable form
//! and compare the two result sets.
//!
//! Normalization rules:
//! - Bag comparison (order-insensitive) unless the statement has an
//!   `ORDER BY` — then rows compare positionally.
//! - Neo4j node/relationship/point/graph entities are normalized to their
//!   property maps (ids are engine-local and never match across engines);
//!   entity identity columns therefore compare by content, not by id.
//! - Integers compare exactly; the relative/absolute epsilon applies only
//!   when at least one side is a genuine float. Non-finite floats fold to
//!   their `"NaN"`/`"Inf"`/`"-Inf"` string spellings (ADR-0280) — never
//!   to null.
//! - Temporal values compare via their ISO strings (both sides serialize
//!   ISO 8601). Points compare via the engine's `Display` spelling
//!   (`point({srid, x, y[, z]})`): the relate side runs
//!   `PointValue::Display` directly, the Neo4j side parses the Query API
//!   v2 EWKT spelling (`SRID=7203;POINT (1.2 3.4)` — typed JSON or the
//!   plain-JSON bare string) into a `PointValue` and runs the same
//!   `Display`, so a cross-engine point compares by value (F-1-1).
//! - Paths canonicalize to the interleaved walk order
//!   `[node, rel, node, …, node]` on both sides — the Query API v2 wire
//!   shape — before positional array comparison.
//! - `CALL db.schema.visualization()` compares through
//!   [`compare_rows_schema_viz`] (LABS-257, called by the suite on the
//!   procedure name — never shape-sniffed): constraint reprs fold to their
//!   canonical name/type/schema form, entry lists sort by `name`, and the
//!   constraints/indexes cells sort by content.

use relate_model::value::PointValue;
use relate_model::CypherValue;
use serde_json::Value as Json;

/// Cap for one rendered JSON payload in a mismatch message, in chars;
/// longer values are cut with an elision marker so one wide row cannot
/// flood the report.
pub(crate) const ELIDE_AT: usize = 240;

/// The head and tail shares of [`ELIDE_AT`] a cut payload keeps: the head
/// shows the value's shape, the tail is where deep differences live (a
/// different relationship at the end of a path). A head-only cut elides
/// exactly the part that differs, which is how a shortestPath tie was
/// misread as a projection-shape difference (LABS-1299).
const ELIDE_HEAD: usize = 160;
const ELIDE_TAIL: usize = ELIDE_AT - ELIDE_HEAD;

/// Cut `s` to [`ELIDE_AT`] chars — [`ELIDE_HEAD`] head and [`ELIDE_TAIL`]
/// tail — with a `…(+N chars)…` marker naming how much was removed.
pub(crate) fn elide(s: &str) -> String {
    let count = s.chars().count();
    if count <= ELIDE_AT {
        return s.to_string();
    }
    let head: String = s.chars().take(ELIDE_HEAD).collect();
    let tail: String = s.chars().skip(count - ELIDE_TAIL).collect();
    format!("{head}…(+{} chars)…{tail}", count - ELIDE_HEAD - ELIDE_TAIL)
}

/// Inside a schema-visualization entry map (a `:LABELS`/`:TYPE` + `name`
/// map), the `constraints`/`indexes` cell lists are catalog-order artifacts
/// too: sort string lists in those cells (after the constraint-repr fold)
/// so two engines' catalogs compare by content.
fn sort_schema_cells(map: &mut serde_json::Map<String, Json>) {
    let entry = map.contains_key(":LABELS") || map.contains_key(":TYPE");
    if !entry || !map.contains_key("name") {
        return;
    }
    for key in ["constraints", "indexes"] {
        if let Some(Json::Array(items)) = map.get_mut(key) {
            if items.iter().all(|i| i.is_string()) {
                items.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
            }
        }
    }
}

/// LABS-257 follow-up: `db.schema.visualization()` renders constraints with
/// server-internal fields that can never match across engines — Neo4j adds
/// `id=30`, `ownedIndex=31`, `graphTypeDependence='UNDESIGNATED'` to the
/// relate spelling `Constraint( name='…', type='…', schema=(…) )`.
/// Fold both to the canonical name/type/schema form (the name is carried by
/// pull, so it is the stable key). Anchored on the known segment keys
/// (`name=`, `, type=`, `, schema=`) rather than a plain delimiter split,
/// so a composite schema like `(:Label {a, b})` — whose `", "` sits inside
/// the value — is never cut in half (F-1-14).
fn normalize_constraint_repr(s: &str) -> String {
    let Some(rest) = s.strip_prefix("Constraint( ") else {
        return s.to_string();
    };
    let rest = rest.trim_end_matches(')').trim_end();
    // A segment's value runs until the next known segment key (", id=" /
    // ", name=" / ", type=" / ", schema=" / ", ownedIndex=" /
    // ", graphTypeDependence="); the first field can also start at 0.
    let field = |key: &str| -> Option<&str> {
        // At position 0 the key starts the string; elsewhere the key is
        // preceded by ", ".
        // One needle for both the find and the self-skip compare
        // (F-2-8/9: it used to be derived twice per field).
        let needle = format!(", {key}");
        let value_start = if rest.starts_with(key) {
            key.len()
        } else {
            rest.find(&needle)? + needle.len()
        };
        // The value ends at the first following segment key other than
        // the key itself.
        let mut end = rest.len();
        for other in [
            ", id=",
            ", name=",
            ", type=",
            ", schema=",
            ", ownedIndex=",
            ", graphTypeDependence=",
        ] {
            if other != needle.as_str() {
                if let Some(i) = rest[value_start..].find(other) {
                    end = end.min(value_start + i);
                }
            }
        }
        Some(rest[value_start..end].trim())
    };
    match (field("name="), field("type="), field("schema=")) {
        (Some(n), Some(t), Some(sc)) => format!("Constraint( name={n}, type={t}, schema={sc} )"),
        _ => s.to_string(), // unrecognized shape: leave it diffable
    }
}

/// A label/relationship entry list from `db.schema.visualization()` (its
/// order is engine-internal: Neo4j walks its catalog, relate walks its
/// label ids — neither is a query contract). Sort such a list by its
/// `name` cell so the two catalogs compare by content. Only fires when
/// EVERY element is a label/rel entry map, so real result lists are never
/// reordered.
fn sort_schema_entries(arr: &mut [Json]) {
    fn name_of(v: &Json) -> Option<&str> {
        let map = v.as_object()?;
        if map.contains_key(":LABELS") || map.contains_key(":TYPE") {
            map.get("name")?.as_str()
        } else {
            None
        }
    }
    if arr.is_empty() || !arr.iter().all(|v| name_of(v).is_some()) {
        return;
    }
    arr.sort_by(|a, b| name_of(a).cmp(&name_of(b)));
}

/// A normalized, comparable value: numbers keep their float form, entities
/// and temporal values fold to `Json` shapes, lists fold recursively.
fn normalize_cypher(v: &CypherValue) -> Json {
    match v {
        CypherValue::Null => Json::Null,
        CypherValue::Bool(b) => Json::Bool(*b),
        CypherValue::Int(i) => Json::from(*i),
        // Non-finite floats encode as "NaN"/"Inf"/"-Inf" strings
        // (ADR-0280), never null — reuse the canonical wire encoding.
        CypherValue::Float(f) => relate_model::wire::json_f64(*f),
        CypherValue::Str(s) => Json::String(s.clone()),
        CypherValue::List(items) => Json::Array(items.iter().map(normalize_cypher).collect()),
        CypherValue::Map(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                out.insert(k.clone(), normalize_cypher(val));
            }
            Json::Object(out)
        }
        // Identity is engine-local: compare entities by labels/rel-type and
        // property content. Labels are sorted — their order is engine-local.
        CypherValue::Node {
            labels, properties, ..
        } => {
            let mut out = serde_json::Map::new();
            let mut labels = labels.clone();
            labels.sort();
            out.insert(":LABELS".into(), labels.into_iter().collect());
            for (k, val) in properties {
                out.insert(k.clone(), normalize_cypher(val));
            }
            Json::Object(out)
        }
        CypherValue::Relationship {
            rel_type,
            properties,
            ..
        } => {
            let mut out = serde_json::Map::new();
            out.insert(":TYPE".into(), Json::String(rel_type.clone()));
            for (k, val) in properties {
                out.insert(k.clone(), normalize_cypher(val));
            }
            Json::Object(out)
        }
        // Paths canonicalize to the interleaved walk order
        // [node, rel, node, rel, …, node] — the same shape the Query API
        // v2 wire (and `normalize_neo4j`) preserves. A well-formed path
        // has `nodes.len() == relationships.len() + 1`, so the trailing
        // node emits no following relationship.
        CypherValue::Path {
            nodes,
            relationships,
        } => {
            let mut items = Vec::with_capacity(nodes.len() + relationships.len());
            for (i, n) in nodes.iter().enumerate() {
                items.push(normalize_cypher(n));
                if let Some(r) = relationships.get(i) {
                    items.push(normalize_cypher(r));
                }
            }
            Json::Array(items)
        }
        // Temporal values serialize via their Display — the canonical
        // ISO-8601 spelling the Query API v2 carries in `_value`.
        CypherValue::Date(_)
        | CypherValue::Time(_, _)
        | CypherValue::DateTime(_, _)
        | CypherValue::LocalDateTime(_)
        | CypherValue::Duration(_) => Json::String(v.to_string()),
        // Point: the engine's Display spelling (`point({srid, x, y[, z]})`).
        // The Neo4j side parses its EWKT wire spelling into a `PointValue`
        // and runs the same `Display` (see `point_from_ewkt`), so both
        // normalizers canonicalize a point through one shared helper (F-1-1).
        CypherValue::Point(p) => Json::String(p.to_string()),
        // Geometry and Vector use the canonical structured wire encoding —
        // never a Display string (stringifying a Vector via Display is the
        // bug relate-model/src/wire.rs calls out).
        CypherValue::Geometry(_) | CypherValue::Vector { .. } => {
            relate_model::wire::value_to_wire_json(v)
        }
    }
}

/// Normalize a Neo4j Query API v2 value: entities fold to their property
/// maps (identity fields dropped — ids are engine-local), typed temporal
/// JSON (`{"$type":"Date","_value":"2024-03-14"}`-style) to ISO strings.
fn normalize_neo4j(v: &Json) -> Json {
    let Some(map) = v.as_object() else {
        return match v {
            Json::Array(items) => Json::Array(items.iter().map(normalize_neo4j).collect()),
            // Plain-JSON mode (the suite's reference client sends
            // `Accept: application/json`) spells a POINT as a bare EWKT
            // string; fold it to the engine's Display spelling. A string
            // that is not exactly EWKT stays as-is.
            Json::String(s) => match point_from_ewkt(s) {
                Some(p) => Json::String(p.to_string()),
                None => v.clone(),
            },
            other => other.clone(),
        };
    };
    if let Some(t) = map
        .get("__type")
        .or_else(|| map.get("$type"))
        .and_then(Json::as_str)
    {
        // Typed value: the temporal types fold to their ISO-8601 `_value`
        // string so both engines compare equal; unknown tags stay objects
        // and diff visibly.
        if let Some(s) = typed_string(t, map) {
            return Json::String(s);
        }
    }
    // Query API v2 entities (extract.rs shape): a node is
    // `{elementId, labels, properties}`, a relationship
    // `{elementId, startNodeElementId, endNodeElementId, type, properties}`.
    let is_node = map.contains_key("elementId") && map.contains_key("labels");
    let is_rel = map.contains_key("startNodeElementId") && map.contains_key("endNodeElementId");
    // The entity arms compute only their tag (`:LABELS` with sorted labels
    // vs `:TYPE`); the property merge below is the single shared path.
    let tag: Option<(&str, Json)> = if is_node {
        // Labels sorted to match the relate side (order is engine-local).
        let mut labels: Vec<String> = map
            .get("labels")
            .and_then(Json::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Json::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        labels.sort();
        Some((
            ":LABELS",
            Json::Array(labels.into_iter().map(Json::String).collect()),
        ))
    } else if is_rel {
        Some((":TYPE", map.get("type").cloned().unwrap_or(Json::Null)))
    } else {
        None
    };
    let mut out = serde_json::Map::new();
    if let Some((key, value)) = tag {
        out.insert(key.into(), value);
        if let Some(props) = map.get("properties").and_then(Json::as_object) {
            for (k, val) in props {
                out.insert(k.clone(), normalize_neo4j(val));
            }
        }
        return Json::Object(out);
    }
    for (k, val) in map {
        out.insert(k.clone(), normalize_neo4j(val));
    }
    Json::Object(out)
}

/// Canonical string of a Query API v2 typed value. The v2 wire shape is
/// `{"$type": "Date", "_value": "2024-03-14"}` (the `__type` tag spelling
/// is accepted too); the six temporal types carry their ISO-8601 string in
/// the `_value` payload, which is read explicitly — never by scanning for
/// "the first string field" (in a BTreeMap the `$type` tag sorts first and
/// would fold the value to its own type name). A `Point` carries EWKT in
/// `_value` and folds to the engine's `Display` spelling (F-1-1). Unknown
/// type tags return `None`: they stay as objects and will diff visibly.
fn typed_string(t: &str, map: &serde_json::Map<String, Json>) -> Option<String> {
    match t.to_ascii_lowercase().as_str() {
        "date" | "time" | "localtime" | "datetime" | "localdatetime" | "duration" => {
            map.get("_value").and_then(Json::as_str).map(str::to_string)
        }
        // `{"$type": "Point", "_value": "SRID=7203;POINT (1.2 3.4)"}` — the
        // EWKT payload parses into the same `PointValue` the engine side
        // Displays. An unparseable payload returns `None`: the value stays
        // an object and diffs visibly.
        "point" => map
            .get("_value")
            .and_then(Json::as_str)
            .and_then(point_from_ewkt)
            .map(|p| p.to_string()),
        _ => None,
    }
}

/// Parse the Query API v2 EWKT point spelling into a [`PointValue`]:
/// `SRID=<srid>;POINT (<x> <y>)` for 2D, `SRID=<srid>;POINT Z (<x> <y> <z>)`
/// for 3D (the `Z` marker is optional — the coordinate count decides).
/// `None` when the string is not exactly that shape: bare WKT without the
/// `SRID=` prefix cannot recover the CRS, so it stays as-is and diffs
/// visibly. This is the Neo4j half of the F-1-1 shared helper — both
/// normalizers canonicalize a point through `PointValue`'s `Display`.
fn point_from_ewkt(s: &str) -> Option<PointValue> {
    let (srid, wkt) = s.strip_prefix("SRID=")?.split_once(';')?;
    let srid: i32 = srid.parse().ok()?;
    let wkt = wkt.trim_start().strip_prefix("POINT")?.trim_start();
    let wkt = wkt.strip_prefix('Z').map(str::trim_start).unwrap_or(wkt);
    let inner = wkt.strip_prefix('(')?.strip_suffix(')')?;
    let coords: Option<Vec<f64>> = inner.split_whitespace().map(|c| c.parse().ok()).collect();
    match coords?.as_slice() {
        [x, y] => Some(PointValue {
            srid,
            x: *x,
            y: *y,
            z: None,
        }),
        [x, y, z] => Some(PointValue {
            srid,
            x: *x,
            y: *y,
            z: Some(*z),
        }),
        _ => None,
    }
}

/// The verdict of a cross-engine row comparison (F-1-11): one entry point
/// replaces the old strict/tolerant `rows_equivalent`/`rows_equivalent_ex`
/// pair. Every non-exact verdict carries the *strict* failure text, so a
/// pass-with-note or a failure reports what exact comparison saw.
#[derive(Debug, PartialEq, Eq)]
pub enum Relation {
    /// Rows match exactly (positionally when `ordered`, as a bag otherwise).
    Exact,
    /// Rows match once order is ignored: `collect()` list order is
    /// unspecified by Cypher, and with `ordered` an `ORDER BY` tie leaves
    /// the row order unspecified too — an order-only difference is a pass
    /// with a note, not a diff. Carries the strict failure text.
    UpToListOrder(String),
    /// Content genuinely differs. Carries the strict failure text.
    Different(String),
}

/// LABS-257: the comparison for `CALL db.schema.visualization()` — the
/// caller knows the statement is the visualization procedure, so the
/// extra normalization applies exactly here (never shape-sniffed from
/// the data, F-1-11/12): constraint reprs fold to their canonical
/// name/type/schema form (Neo4j's `id`/`ownedIndex`/`graphTypeDependence`
/// are server-internal), the entry lists sort by `name` (the catalog walk
/// order is engine-internal), and the constraints/indexes cells inside an
/// entry map sort by content. Everything else compares exactly like the
/// plain unordered comparison.
pub fn compare_rows_schema_viz(neo4j: &[Json], cypher: &[CypherValue]) -> Relation {
    let mut left = normalize_reference_rows(neo4j);
    let mut right = normalize_engine_rows(cypher);
    for row in left.iter_mut().chain(right.iter_mut()) {
        viz_normalize(row);
    }
    match compare_normalized(&left, &right, false, false) {
        Ok(()) => Relation::Exact,
        Err(strict_why) => match compare_normalized(&left, &right, false, true) {
            Ok(()) => Relation::UpToListOrder(strict_why),
            Err(_) => Relation::Different(strict_why),
        },
    }
}

/// The recursive viz-mode normalization (only reached from
/// [`compare_rows_schema_viz`]): constraint-repr fold on strings, cell
/// sort on entry maps, entry-name sort on lists whose every element is an
/// entry map.
fn viz_normalize(v: &mut Json) {
    match v {
        Json::String(s) => *s = normalize_constraint_repr(s),
        Json::Array(items) => {
            for item in items.iter_mut() {
                viz_normalize(item);
            }
            sort_schema_entries(items);
        }
        Json::Object(map) => {
            for val in map.values_mut() {
                viz_normalize(val);
            }
            sort_schema_cells(map);
        }
        _ => {}
    }
}

/// Compare one Neo4j Query API v2 result set against one relate result
/// set. `ordered` = the statement has an `ORDER BY`, so rows compare
/// positionally; otherwise a multiset (bag) comparison decides, so
/// engine-internal iteration order never fails a semantically equal result.
pub fn compare_rows(neo4j: &[Json], cypher: &[CypherValue], ordered: bool) -> Relation {
    match compare(neo4j, cypher, ordered, false) {
        Ok(()) => Relation::Exact,
        Err(strict_why) => {
            // `collect()` list order is unspecified anywhere, and `ORDER BY`
            // with equal sort keys leaves the row order unspecified (ties) —
            // a pure reordering is `UpToListOrder`, not `Different`.
            if compare(neo4j, cypher, ordered, true).is_ok()
                || (ordered && compare(neo4j, cypher, false, true).is_ok())
            {
                return Relation::UpToListOrder(strict_why);
            }
            Relation::Different(strict_why)
        }
    }
}

/// The comparison behind [`compare_rows`]: normalize both sides, then
/// compare positionally (`ordered`) or as a bag; with `lists_unordered`,
/// list cells compare as multisets (recursively). The failure texts are
/// the strict comparison's — they are what [`Relation`] carries.
fn compare(
    neo4j: &[Json],
    cypher: &[CypherValue],
    ordered: bool,
    lists_unordered: bool,
) -> Result<(), String> {
    compare_normalized(
        &normalize_reference_rows(neo4j),
        &normalize_engine_rows(cypher),
        ordered,
        lists_unordered,
    )
}

/// The one bag-match policy: remove the first entry of `remaining` that
/// `matches` accepts, reporting whether one was taken.
/// [`compare_normalized`]'s bag arm, [`rows_subset`], and [`values_equal`]'s
/// unordered-list arm are the same match-each-left-against-an-unused-right
/// dance — each keeps only its own failure action.
fn take_match<T>(remaining: &mut Vec<T>, matches: impl Fn(&T) -> bool) -> bool {
    match remaining.iter().position(matches) {
        Some(pos) => {
            remaining.remove(pos);
            true
        }
        None => false,
    }
}

/// The comparison on already-normalized rows (shared by [`compare`] and
/// the viz entry point, F-1-3: one comparison implementation).
fn compare_normalized(
    left: &[Json],
    right: &[Json],
    ordered: bool,
    lists_unordered: bool,
) -> Result<(), String> {
    if left.len() != right.len() {
        return Err(format!(
            "row count differs: neo4j {} vs relate {}",
            left.len(),
            right.len()
        ));
    }
    if ordered {
        for (i, (l, r)) in left.iter().zip(right.iter()).enumerate() {
            if !values_equal(l, r, lists_unordered) {
                return Err(format!(
                    "row {i} differs: neo4j {} vs relate {}",
                    elide(&l.to_string()),
                    elide(&r.to_string())
                ));
            }
        }
        return Ok(());
    }
    // Bag comparison: match each left value against an unused right value,
    // completing the whole pass before reporting — a mid-loop snapshot
    // would frame relate rows that later left rows still consume. Both
    // sides render as compact serde_json, elided at the report's cap —
    // never Rust Debug, which spells strings as `String("…")` — and each
    // row is framed by its result-set index, one row per frame, never one
    // nested list: its outer brackets read as an extra value-nesting level
    // once elided, which is how a shortestPath tie was misread as a
    // projection bug (LABS-1299).
    let mut remaining: Vec<(usize, &Json)> = right.iter().enumerate().collect();
    let mut unmatched: Vec<(usize, &Json)> = Vec::new();
    for (i, l) in left.iter().enumerate() {
        if !take_match(&mut remaining, |(_, r)| values_equal(l, r, lists_unordered)) {
            unmatched.push((i, l));
        }
    }
    if !unmatched.is_empty() {
        return Err(format!(
            "neo4j rows with no relate match ({}): {}; relate rows left over ({}): {}",
            unmatched.len(),
            rows_preview(&unmatched),
            remaining.len(),
            rows_preview(&remaining),
        ));
    }
    Ok(())
}

/// How many rows of a mismatch message's difference each side renders; the
/// rest are named by count in the message.
const ROWS_SHOWN: usize = 3;

/// The one row frame: a row's index in its result set and its elided
/// payload — the label every mismatch message in this module names a row
/// by. Never one nested list: its outer brackets read as an extra
/// value-nesting level once elided, which is how a shortestPath tie was
/// misread as a projection bug (LABS-1299).
fn row_frame(i: usize, row: &Json) -> String {
    format!("row {i}: {}", elide(&row.to_string()))
}

/// Rows of one result set, each in its own [`row_frame`], up to
/// [`ROWS_SHOWN`]; the rest are named by count.
fn rows_preview(rows: &[(usize, &Json)]) -> String {
    let mut shown: Vec<String> = rows
        .iter()
        .take(ROWS_SHOWN)
        .map(|(i, r)| row_frame(*i, r))
        .collect();
    if shown.len() < rows.len() {
        shown.push(format!("(+{} more)", rows.len() - shown.len()));
    }
    shown.join("; ")
}

/// Multiset containment: every relate row must have a matching row in
/// the reference's (full) result set. Used for `LIMIT`/`SKIP` without
/// `ORDER BY`, where the engines may legitimately pick different rows but
/// every returned row must still exist on the reference side.
pub fn rows_subset(full: &[Json], subset: &[CypherValue]) -> Result<(), String> {
    let full = normalize_reference_rows(full);
    let subset = normalize_engine_rows(subset);
    let mut remaining: Vec<&Json> = full.iter().collect();
    for (i, s) in subset.iter().enumerate() {
        if !take_match(&mut remaining, |f| values_equal(f, s, false)) {
            return Err(format!(
                "relate {} is not in the reference's full result set",
                row_frame(i, s)
            ));
        }
    }
    Ok(())
}

/// Normalized reference rows (crate-visible for the subset check).
pub(crate) fn normalize_reference_rows(rows: &[Json]) -> Vec<Json> {
    rows.iter().map(normalize_neo4j).collect()
}

/// Normalized relate rows (crate-visible for the subset check).
pub(crate) fn normalize_engine_rows(rows: &[CypherValue]) -> Vec<Json> {
    rows.iter().map(normalize_cypher).collect()
}

/// Exact integer view of a JSON number (i64/u64 unified via i128); `None`
/// for genuine floats.
fn as_integer(n: &serde_json::Number) -> Option<i128> {
    n.as_i64()
        .map(i128::from)
        .or_else(|| n.as_u64().map(i128::from))
}

/// Equality of two normalized values: integers compare exactly, floats
/// with epsilon, other values structurally. With `lists_unordered`, list
/// values compare as multisets (recursively).
fn values_equal(l: &Json, r: &Json, lists_unordered: bool) -> bool {
    match (l, r) {
        (Json::Number(a), Json::Number(b)) => match (as_integer(a), as_integer(b)) {
            // Both integer-representable: exact comparison — no f64
            // rounding, so distinct integers above 2^53 stay distinct.
            (Some(x), Some(y)) => x == y,
            // At least one genuine float: relative/absolute epsilon.
            // `as_f64` is total for stored i64/u64/f64; the NaN fallback
            // never satisfies the epsilon.
            _ => {
                let (x, y) = (
                    a.as_f64().unwrap_or(f64::NAN),
                    b.as_f64().unwrap_or(f64::NAN),
                );
                (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1e-9)
            }
        },
        (Json::Array(a), Json::Array(b)) if lists_unordered => {
            if a.len() != b.len() {
                return false;
            }
            let mut remaining: Vec<&Json> = b.iter().collect();
            a.iter()
                .all(|x| take_match(&mut remaining, |y| values_equal(x, y, lists_unordered)))
        }
        (Json::Array(a), Json::Array(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b)
                    .all(|(x, y)| values_equal(x, y, lists_unordered))
        }
        (Json::Object(a), Json::Object(b)) => {
            a.len() == b.len()
                && a.iter().all(|(k, v)| {
                    b.get(k)
                        .is_some_and(|w| values_equal(v, w, lists_unordered))
                })
        }
        _ => l == r,
    }
}

#[cfg(test)]
mod tests;
