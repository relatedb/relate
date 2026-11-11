//! The stored vector-index option schema, one home: the key table, the
//! storable-metric narrowing, the dimension bound, the one parse, and
//! the DDL defaults. Split from the similarity kernels (stellar round 3,
//! std-6): the scoring concern lives in [`crate::vector_similarity`],
//! the config admission here. The two entry pipelines that navigate the
//! kind-neutral outer `OPTIONS` (`indexConfig`) live with their exec
//! consumers — the CREATE side in `exec::ddl_options`, the stored
//! read in `exec::vector` — so this root module never imports up into
//! `exec` (LABS-845); they call the one parse and the defaults here,
//! each folding errors onto its own contract.

use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};
use relate_ir::VectorMetric;

/// The storable metric — the two index-config metrics the vector
/// index's scan kernels implement, narrowed from the one metric-name
/// table [`VectorMetric`] owns (the vector *functions* vocabulary) by
/// the one [`TryFrom`] at the parse boundary. A new function variant
/// joins the vocabulary without becoming storable; widening the subset
/// is a compiler-guided edit (this enum, its narrowing, and the
/// exhaustive two-arm dispatch in [`crate::vector_similarity::similarity_values`]).
/// The canonical spelling for the DDL defaults comes back through the
/// total [`From`], so the vocabulary table stays the one authority on
/// names. [`StorableMetric::Cosine`] is the documented default (an
/// absent key — catalogs written before the DDL defaulted it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StorableMetric {
    Cosine,
    Euclidean,
}

impl TryFrom<VectorMetric> for StorableMetric {
    type Error = Error;

    /// The one narrowing: an index-config metric, or the typed DDL
    /// rejection (a user-input failure, so Neo4j's `ArgumentError`
    /// class — ADR-0489). The four function-only variants are named,
    /// not catch-alled — a future variant fails to compile here until
    /// it is either admitted or listed as function-only.
    fn try_from(metric: VectorMetric) -> Result<Self> {
        match metric {
            VectorMetric::Cosine => Ok(Self::Cosine),
            VectorMetric::Euclidean => Ok(Self::Euclidean),
            VectorMetric::EuclideanSquared
            | VectorMetric::Manhattan
            | VectorMetric::Dot
            | VectorMetric::Hamming => Err(Error::argument_error("InvalidInputValue")),
        }
    }
}

impl From<StorableMetric> for VectorMetric {
    /// The total widening: every storable metric is a vocabulary metric.
    fn from(metric: StorableMetric) -> Self {
        match metric {
            StorableMetric::Cosine => Self::Cosine,
            StorableMetric::Euclidean => Self::Euclidean,
        }
    }
}

impl StorableMetric {
    /// The canonical stored spelling, from the one vocabulary table.
    fn name(self) -> &'static str {
        VectorMetric::from(self).name()
    }
}

const KEY_DIMENSIONS: &str = "vector.dimensions";
const KEY_SIMILARITY: &str = "vector.similarity_function";
const KEY_QUANTIZATION: &str = "vector.quantization.enabled";
const KEY_QUANTIZATION_TYPE: &str = "vector.quantization.type";
const KEY_HNSW_M: &str = "vector.hnsw.m";
const KEY_HNSW_EF: &str = "vector.hnsw.ef_construction";
const KEY_SEARCH_EXPANSION: &str = "vector.default_search_expansion_factor";

/// The two quantization-type spellings the DDL defaults branch on. The
/// domain stays open (a stored config may carry any string), so these are
/// named constants, not an enum: only `NONE` changes the expansion-factor
/// default, and `BINARY` is what an absent key defaults to.
const QUANTIZATION_NONE: &str = "NONE";
const QUANTIZATION_BINARY: &str = "BINARY";

/// The leaf shape an admission-only option key's value must carry,
/// mirroring the ontology validator's `validate_vector_option` contract
/// (relate-ontology/src/validate/ext.rs):
/// `vector.quantization.enabled` is `opt_bool` and `vector.hnsw.m` /
/// `vector.hnsw.ef_construction` are `opt_int`. Every other value-carrying
/// key's domain (the dimension range, the metric name set, the quantization
/// spelling) is beyond a shape, so its row carries a [`KeyRule`] domain parse
/// instead; there is deliberately no shape-less [`KeyShape`] sentinel variant
/// that `admits` everything.
#[derive(Clone, Copy, PartialEq)]
enum KeyShape {
    Bool,
    Int,
    /// A number: an `INTEGER` or a `FLOAT` (the DDL spells the expansion
    /// factor as a float, the ontology validator accepts either).
    Number,
}

impl KeyShape {
    /// Whether a present value of this shape is admitted. A present null
    /// is rejected as wrong-kind, per the ontology validator's
    /// `read_scalar`.
    fn admits(self, value: &Value) -> bool {
        matches!(
            (self, value),
            (Self::Bool, Value::Bool(_))
                | (Self::Int, Value::Int(_))
                | (Self::Number, Value::Int(_) | Value::Float(_))
        )
    }
}

/// What one row of [`CONFIG_KEYS`] owns: a value-carrying key's domain
/// parse — the dimension range ([`KeyRule::Dimension`]), the metric name
/// set ([`KeyRule::Metric`]) or the quantization spelling
/// ([`KeyRule::Quantization`]) — or an admission-only key's leaf shape
/// ([`KeyRule::Shape`]). Every row is a validator.
#[derive(Clone, Copy)]
enum KeyRule {
    Dimension,
    Metric,
    Quantization,
    Shape(KeyShape),
}

/// The seven-key allowlist of a stored vector index's `indexConfig`, one
/// table — each row owns its whole policy: the three value-carrying keys'
/// domain parses and the four admission-only keys' leaf shapes. The
/// admission-only rows' stored values have no behavioural effect (the
/// scan ignores them — LABS-708 is admission only), and an absent key
/// stays absent (the pre-LABS-708 stored shape, so defaulting is
/// unchanged). The unknown-key scan and the parse pass in
/// [`parse_config`] both consult this one table; the row order is the
/// check order.
const CONFIG_KEYS: [(&str, KeyRule); 7] = [
    (KEY_DIMENSIONS, KeyRule::Dimension),
    (KEY_SIMILARITY, KeyRule::Metric),
    (KEY_QUANTIZATION, KeyRule::Shape(KeyShape::Bool)),
    (KEY_QUANTIZATION_TYPE, KeyRule::Quantization),
    (KEY_HNSW_M, KeyRule::Shape(KeyShape::Int)),
    (KEY_HNSW_EF, KeyRule::Shape(KeyShape::Int)),
    (KEY_SEARCH_EXPANSION, KeyRule::Shape(KeyShape::Number)),
];

/// The DDL bound on `vector.dimensions` and a `vector()` call's dimension.
pub(crate) const VECTOR_DIMENSIONS_MIN: usize = 1;
pub(crate) const VECTOR_DIMENSIONS_MAX: usize = 4096;

/// The dimension a `vector.dimensions` leaf or a `vector()` call's
/// dimension argument may carry: within the bound, as a `usize` — the
/// bound and the type travel together, so no caller casts a
/// separately-checked `i64` (stellar round 3, std-5). `None` outside the
/// bound.
pub(crate) fn vector_dimension(value: i64) -> Option<usize> {
    let dimension = usize::try_from(value).ok()?;
    (VECTOR_DIMENSIONS_MIN..=VECTOR_DIMENSIONS_MAX)
        .contains(&dimension)
        .then_some(dimension)
}

/// The parse of a stored vector index's `indexConfig` — the key table
/// stated once, each present key admitted by its own row. The
/// CREATE-side entry (`exec::ddl_options::create_config`) calls this
/// directly (its typed errors are the DDL's user-facing codes, all in
/// Neo4j's `ArgumentError` class — ADR-0489); the stored read
/// (`exec::vector::stored_config`) maps any error onto the drift
/// contract.
pub(crate) fn parse_config(config: &BTreeMap<String, Value>) -> Result<StoredVectorConfig> {
    // The allowlist is the one table: every present key must be one of
    // them — an unknown key wins over any bad value.
    if !config
        .keys()
        .all(|key| CONFIG_KEYS.iter().any(|(name, _)| *name == key.as_str()))
    {
        return Err(Error::argument_error("InvalidInputValue"));
    }
    // One pass over the table, in row order (the check order): the
    // value-carrying rows parse their domains (every user-input failure
    // a typed DDL `ArgumentError`, ADR-0489), the shape rows admit.
    // Absent rows stay absent — the dimension defaults to none, the
    // metric to Cosine.
    let mut dimension = None;
    let mut metric = StorableMetric::Cosine;
    let mut quantization = QUANTIZATION_BINARY.to_string();
    for (name, rule) in CONFIG_KEYS {
        let Some(value) = config.get(name) else {
            continue;
        };
        match rule {
            KeyRule::Dimension => match value {
                Value::Int(value) => {
                    dimension = Some(
                        vector_dimension(*value)
                            .ok_or_else(|| Error::argument_error("NumberOutOfRange"))?,
                    );
                }
                _ => return Err(Error::argument_error("InvalidInputValue")),
            },
            KeyRule::Metric => match value {
                Value::String(metric_name) => {
                    metric = StorableMetric::try_from(
                        VectorMetric::from_name(metric_name)
                            .ok_or_else(|| Error::argument_error("InvalidInputValue"))?,
                    )?;
                }
                _ => return Err(Error::argument_error("InvalidInputValue")),
            },
            // A wrong-shaped value is rejected here, so no later stage ever
            // fabricates a default over a malformed key; the canonical
            // uppercase spelling is fixed once, at the parse boundary.
            KeyRule::Quantization => match value {
                Value::String(name) => quantization = name.to_ascii_uppercase(),
                _ => return Err(Error::argument_error("InvalidInputValue")),
            },
            KeyRule::Shape(shape) => {
                if !shape.admits(value) {
                    return Err(Error::argument_error("InvalidInputValue"));
                }
            }
        }
    }
    Ok(StoredVectorConfig {
        dimension,
        metric,
        quantization,
    })
}

/// The stored vector index's parsed options.
pub(crate) struct StoredVectorConfig {
    pub dimension: Option<usize>,
    /// The stored metric — the parse's [`TryFrom`] narrowing is the
    /// type, so no consumer can hold a metric the scan kernels would
    /// reject.
    pub metric: StorableMetric,
    /// The quantization type in its canonical (uppercase) spelling, parsed
    /// from the same key table as every other value-carrying option (an
    /// absent key parses to [`QUANTIZATION_BINARY`]). Carried typed so the
    /// DDL defaults never fabricate a value for a missing or malformed key.
    pub quantization: String,
}

/// The DDL defaults — Neo4j 2026.09's own (`SHOW INDEXES` on a freshly
/// created vector index): the HNSW tuning pair, the search expansion
/// factor, the parsed quantization type, and the parsed metric's canonical
/// spelling. The parse established the shapes, so this only inserts; a
/// user-supplied value wins. `vector.quantization.enabled` is still
/// admitted (the older spelling) but is no longer defaulted — Neo4j
/// reports `vector.quantization.type` instead.
pub(crate) fn apply_defaults(
    config: &mut BTreeMap<String, Value>,
    metric: StorableMetric,
    quantization: &str,
) {
    config.entry(KEY_HNSW_M.into()).or_insert(Value::Int(16));
    config.entry(KEY_HNSW_EF.into()).or_insert(Value::Int(100));
    // The stored spelling is canonical (uppercase) — Neo4j reports `NONE`
    // even when the statement wrote `'none'`.
    config.insert(
        KEY_QUANTIZATION_TYPE.into(),
        Value::String(quantization.into()),
    );
    // The expansion factor's default follows the quantization type: a
    // non-quantized index defaults to 1.0, a quantized one to 3.0 (live
    // Neo4j 2026.09).
    let expansion_default = match quantization {
        QUANTIZATION_NONE => 1.0,
        _ => 3.0,
    };
    config
        .entry(KEY_SEARCH_EXPANSION.into())
        .or_insert(Value::Float(expansion_default));
    // Canonical spelling, present or defaulted: the parse established
    // the String-ness, so the uppercase normalization folds here.
    config.insert(KEY_SIMILARITY.into(), Value::String(metric.name().into()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::map;

    /// The option allowlist admits both `vector.quantization` keys — the
    /// SEARCH acceptance suites' Background sets `type: 'none'`
    /// (LABS-708) — together and separately, validates `type` as a
    /// string, and still rejects everything outside the table.
    #[test]
    fn config_admits_both_quantization_keys() {
        for admitted in [
            map(&[
                (KEY_QUANTIZATION, Value::Bool(false)),
                (KEY_QUANTIZATION_TYPE, Value::String("none".into())),
            ]),
            map(&[(KEY_QUANTIZATION, Value::Bool(true))]),
            map(&[(KEY_QUANTIZATION_TYPE, Value::String("scalar".into()))]),
        ] {
            let parsed = parse_config(&admitted).expect("quantization keys are admitted");
            assert!(matches!(parsed.metric, StorableMetric::Cosine));
        }

        // Every value-carrying key rejects a wrong-shaped value, not just
        // `type`: `enabled` is a Bool, the two `hnsw` tuning keys are Ints,
        // `type` is a String (the ontology validator's per-key shapes).
        for (key, rejected) in [
            (KEY_QUANTIZATION, Value::String("yes".into())),
            (KEY_HNSW_M, Value::String("16".into())),
            (KEY_HNSW_EF, Value::Float(16.0)),
            (KEY_QUANTIZATION_TYPE, Value::Bool(false)),
            (KEY_QUANTIZATION_TYPE, Value::Int(0)),
            (KEY_QUANTIZATION_TYPE, Value::Null),
        ] {
            let rejected_config = map(&[(key, rejected.clone())]);
            assert!(
                parse_config(&rejected_config).is_err(),
                "{key} rejects {rejected:?}"
            );
        }

        // The table stays closed.
        let unknown = map(&[("vector.unknown", Value::Int(1))]);
        assert!(parse_config(&unknown).is_err(), "unknown key is rejected");
    }

    /// The storable subset is exactly what the one narrowing admits,
    /// asserted over the one `VectorMetric` vocabulary table itself:
    /// every keyword in the table parses exactly when its metric
    /// narrows, and the oracle consults the same `TryFrom` (stellar
    /// round 3, std-2 — no second hardcoded variant list), so a variant
    /// that joins the table is covered by construction and a widened
    /// narrowing propagates without a lockstep edit. The absent key
    /// defaults to Cosine.
    #[test]
    fn the_storable_subset_is_exactly_the_two_index_metrics() {
        for (keyword, metric) in VectorMetric::ALL {
            let config = map(&[(KEY_SIMILARITY, Value::String(keyword.into()))]);
            let parsed = parse_config(&config);
            let storable = StorableMetric::try_from(metric).is_ok();
            assert_eq!(
                parsed.is_ok(),
                storable,
                "{keyword} parses exactly when the narrowing admits it"
            );
            if storable {
                assert_eq!(
                    VectorMetric::from(parsed.expect("is_ok was asserted above").metric),
                    metric,
                    "{keyword} round-trips as its own variant"
                );
            }
        }
        let absent = map(&[(KEY_DIMENSIONS, Value::Int(4))]);
        assert_eq!(
            parse_config(&absent)
                .expect("an absent key defaults")
                .metric,
            StorableMetric::Cosine
        );
    }
}

#[cfg(test)]
mod default_tests {
    use super::*;

    /// Neo4j 2026.09's DDL defaults: the HNSW pair, a 3.0 expansion factor
    /// and the BINARY quantization type; `vector.quantization.type: NONE`
    /// moves the expansion factor to 1.0, and the stored spelling is
    /// canonical (uppercase).
    #[test]
    fn defaults_match_live_neo4j_and_follow_the_quantization_type() {
        let mut config = BTreeMap::new();
        let parsed = parse_config(&config).expect("an empty config parses");
        assert_eq!(parsed.quantization, QUANTIZATION_BINARY);
        apply_defaults(&mut config, parsed.metric, &parsed.quantization);
        assert_eq!(config.get(KEY_SEARCH_EXPANSION), Some(&Value::Float(3.0)));
        assert_eq!(
            config.get(KEY_QUANTIZATION_TYPE),
            Some(&Value::String(QUANTIZATION_BINARY.into()))
        );
        assert_eq!(config.get(KEY_HNSW_M), Some(&Value::Int(16)));
        assert_eq!(config.get(KEY_HNSW_EF), Some(&Value::Int(100)));

        // The parse canonicalizes the spelling, and the defaults branch on
        // that typed value — never on the raw string a statement wrote.
        let mut none = BTreeMap::new();
        none.insert(KEY_QUANTIZATION_TYPE.into(), Value::String("none".into()));
        let parsed = parse_config(&none).expect("a string quantization type parses");
        assert_eq!(parsed.quantization, QUANTIZATION_NONE);
        apply_defaults(&mut none, parsed.metric, &parsed.quantization);
        assert_eq!(
            none.get(KEY_QUANTIZATION_TYPE),
            Some(&Value::String(QUANTIZATION_NONE.into()))
        );
        assert_eq!(none.get(KEY_SEARCH_EXPANSION), Some(&Value::Float(1.0)));
    }
}
