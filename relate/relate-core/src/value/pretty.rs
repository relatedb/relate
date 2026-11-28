//! Neo4j's `AnyValue.prettyPrint()`: the value rendering live Neo4j embeds
//! in error text that echoes a call's inputs. Extracted from
//! `value/mod.rs` for the file-size rule (the `depth.rs`/`ordering.rs`
//! precedent); the method stays inherent, so no caller changes.
//!
//! Every shape below was probed 2026-11-27 on neo4j:2026.10.1-enterprise
//! through `duration_between(<value>, 'abc')`, whose rejection renders the
//! whole input list: scalars wrap as `Type(literal)` — `String("abc")`
//! (raw, unescaped), `Long(123)`, `Double(1.500000e+00)`,
//! `Boolean('true')` — containers wrap as `List{…}` / `Map{k -> v, …}`
//! (sorted keys), entities print their bare ids — `(<id>)`, `-[<id>]-`,
//! `Path{(0)-[<id>]-(1)}` — a byte array prints `ByteArray[120, 121]`, a
//! UUID wraps its bare hyphenated form, and temporals and points print
//! their display forms (`2021-01-01`, `point({x: 1.0, y: 2.0, crs:
//! 'cartesian'})`), the same rendering [`Value::to_display_string`]
//! carries.

use super::{Coord, Node, Relationship, Value};

impl Value {
    /// Live Neo4j's `AnyValue.prettyPrint()`, the rendering its error
    /// messages embed when they echo a call's input values (the
    /// `Invalid call signature for …: Provided input was […]` family).
    /// The rendering is probed live, not derived — see the module docs.
    pub fn pretty_print(&self) -> String {
        self.pretty_print_at(0)
    }

    /// [`Value::pretty_print`] with the ADR-0462 `depth` guard: a container
    /// nested past [`super::MAX_VALUE_DEPTH`] renders the depth marker
    /// instead of recursing — the same safety valve every other render
    /// walk carries, through the one shared [`super::guarded_container`].
    fn pretty_print_at(&self, depth: u32) -> String {
        match self {
            // Null never reaches an operand-echoing message (null
            // propagation answers first); Neo4j's pretty print of NO_VALUE
            // is `NULL`, kept for totality.
            Value::Null => "NULL".into(),
            Value::Bool(b) => format!("Boolean('{b}')"),
            Value::Int(i) => format!("Long({i})"),
            Value::Float(f) => format!("Double({})", double_scientific(*f)),
            // Live does not escape the payload (probed: `String("a"b'c")`).
            Value::String(s) => format!("String(\"{s}\")"),
            Value::List(xs) => super::guarded_container(
                depth,
                |joined| format!("List{{{joined}}}"),
                |d| xs.iter().map(|v| v.pretty_print_at(d)).collect(),
            ),
            // A `BTreeMap`, so the iteration order is already the sorted
            // one live prints (probed: `{b:2,a:1}` renders `Map{a -> …, b
            // -> …}`); keys print raw, like live's.
            Value::Map(m) => super::guarded_container(
                depth,
                |joined| format!("Map{{{joined}}}"),
                |d| {
                    m.iter()
                        .map(|(k, v)| format!("{k} -> {}", v.pretty_print_at(d)))
                        .collect()
                },
            ),
            Value::Node(n) => pretty_node(n),
            Value::Relationship(r) => pretty_rel(r),
            Value::Path(p) => pretty_path(p),
            // The display form is live's own pretty print for temporals
            // (probed: `2021-01-01T00:00:00+01:00[Europe/Stockholm]`,
            // `12:30:00Z`, `PT1M30S`).
            Value::Temporal(t) => t.full_seconds_string(),
            // Same for points: `point({x: 1.0, y: 2.0, crs: 'cartesian'})`.
            Value::Point(pt) => super::point_display(pt),
            Value::Uuid(u) => format!("UUID({})", super::uuid_hyphenated(*u)),
            Value::Bytes(b) => format!("ByteArray[{}]", super::bytes_int_list(b)),
            // Engine-only scalar (live 2026.10.1 has no vector values —
            // `vec` is an unknown function there, probed): the container
            // family's wrapped form, coordinates pretty-printed like any
            // other element.
            // The packed coordinates (ADR-0706) pretty-print as the
            // scalars they are — the same Long/Double forms the walk
            // above gives them — with the container family's wrapped
            // form. No child Values, so the guard cannot stop a
            // recursion; the family render is direct.
            Value::Vector { coords, .. } => {
                // The scalars' own pretty forms (the Int/Float arms
                // above), one spelling through the shared helper.
                let joined = coords
                    .iter()
                    .map(coord_pretty)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("Vector{{{joined}}}")
            }
        }
    }
}

/// `(<id>)` — the bare node id, no labels or properties (probed).
fn pretty_node(n: &Node) -> String {
    format!("({})", n.id)
}

/// `-[<id>]-` — the bare relationship id, no type (probed).
fn pretty_rel(r: &Relationship) -> String {
    format!("-[{}]-", r.id)
}

/// `Path{(0)-[<id>]-(1)}` — the shared chain walk ([`super::Path::walk`])
/// with the bare-id element renderings. Live's pretty print carries no
/// direction arrows, and the pretty rel form (`-[<id>]-`) brings its own
/// dashes — the display/literal walks compose [`super::direction_arrows`]
/// around their dash-less `[:TYPE]` — so this walk's rel seam ignores the
/// current node id.
fn pretty_path(p: &super::Path) -> String {
    format!("Path{{{}}}", p.walk(pretty_node, |r, _| pretty_rel(r)))
}

/// The `Double(…)` payload: Java's `%e` — one integer digit, six
/// fractional digits, a signed exponent zero-padded to two places
/// (`Double(1.500000e+00)`, `Double(1.000000e-07)`,
/// `Double(1.000000e+300)`; `NaN`/`Infinity`/`-Infinity` verbatim — all
/// probed).
///
/// The rounding basis is the f64's **shortest round-trip digits**, not its
/// exact binary expansion: live prints `Double(1.007813e+00)` for
/// 1.0078125 — Java's formatter rounds the decimal string half-up, so
/// 129/128 rounds up where a round-half-even of the same digits would
/// keep 1.007812 (probed). Rust's `{:e}` produces the same shortest
/// digits for every probed shape, so the digits are taken from it and the
/// half-up rounding is done here, on the string.
///
/// One known boundary divergence, documented rather than ported: live's
/// formatter is OpenJDK's legacy `FloatingDecimal`, whose digit weeding
/// sometimes emits more digits than the shortest that round-trips — the
/// smallest subnormal (exact 4.9406564584124654e-324) prints
/// `Double(4.900000e-324)` on live while this rendering prints its
/// shortest correctly-rounded form `Double(5.000000e-324)`. Every
/// probed non-boundary float agrees.
fn double_scientific(f: f64) -> String {
    // The non-finite spellings are the one float policy's own —
    // `float_text` prints NaN/Infinity/-Infinity, the same strings live's
    // formatter does (probed).
    if !f.is_finite() {
        return relate_model::value::float_text(f);
    }
    // Shortest scientific digits: `1.5e0`, `-2.25e0`, `1e20`, `4.9e-324`.
    let rendered = format!("{f:e}");
    let (mantissa, exponent) = rendered
        .split_once('e')
        .expect("LowerExp always carries an exponent");
    let exponent: i64 = exponent.parse().expect("a LowerExp exponent is an integer");
    let (mantissa, exponent) = round_half_up_at_six(mantissa, exponent);
    format!("{mantissa}e{exponent:+03}")
}

/// A packed coordinate's pretty form — the `Value::Int`/`Value::Float`
/// arms' renders, spelled once for the vector arm (ADR-0706).
fn coord_pretty(c: Coord) -> String {
    match c {
        Coord::I(i) => format!("Long({i})"),
        Coord::F(f) => format!("Double({})", double_scientific(f)),
    }
}

/// Round a `{:e}` mantissa string (`"1.0078125"`, `"-1.5"`, `"9"`) half-up
/// at six fractional digits, returning the padded mantissa and, when the
/// rounding carried out of the integer digit (`9.999999…5` → `10.000000`),
/// the exponent shifted up one with the mantissa renormalized to
/// `1.000000`. A `{:e}` mantissa carries exactly one integer digit, so the
/// arithmetic stays a single-digit add; the exponent crosses as the
/// integer it is.
fn round_half_up_at_six(mantissa: &str, exponent: i64) -> (String, i64) {
    let negative = mantissa.starts_with('-');
    let unsigned = mantissa.strip_prefix('-').unwrap_or(mantissa);
    // One integer digit, a dot, and at least seven fractional digits (the
    // seventh is the first discarded one — the half-up decider). Only
    // zeros are appended, so the dot's index does not move.
    let mut digits = unsigned.to_string();
    if !digits.contains('.') {
        digits.push('.');
    }
    let dot = digits.find('.').expect("the dot was just ensured");
    let frac_len = digits.len() - dot - 1;
    digits.push_str(&"0".repeat(7 - frac_len.min(7)));
    let mut int_digit: u32 = digits[..dot]
        .parse()
        .expect("a LowerExp mantissa carries one integer digit");
    let mut frac_kept: u32 = digits[dot + 1..dot + 7]
        .parse()
        .expect("seven padded fractional digits");
    // Half-up: the discarded tail alone decides, never the kept digit's
    // parity (probed: 1.0078125 rounds to 1.007813).
    if digits.as_bytes()[dot + 7] >= b'5' {
        frac_kept += 1;
    }
    let mut exponent_shift = 0i64;
    if frac_kept == 1_000_000 {
        // The fraction carried: 1.999999…5 becomes 2.000000, and the one
        // shape that leaves the single-digit range — 9.999999…5 —
        // renormalizes to 1.000000 with the exponent bumped.
        frac_kept = 0;
        int_digit += 1;
        if int_digit == 10 {
            int_digit = 1;
            exponent_shift = 1;
        }
    }
    let mantissa = format!("{int_digit}.{frac_kept:06}");
    (
        if negative {
            format!("-{mantissa}")
        } else {
            mantissa
        },
        exponent + exponent_shift,
    )
}
