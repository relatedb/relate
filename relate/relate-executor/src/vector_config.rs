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

/// The quantization type a vector index carries — the closed vocabulary
/// the one parse admits (Neo4j dev: `SCALAR`, `BINARY` and `NONE`,
/// case-insensitive; `INT8` and every other spelling is the typed
/// supported-values rejection, REL8-1062). The sibling of
/// [`StorableMetric`]: narrowed at the same parse boundary, carried typed,
/// so no consumer can hold a value the vocabulary does not contain, and
/// the canonical stored spelling comes back through [`Self::name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuantizationType {
    Scalar,
    Binary,
    None,
}

impl QuantizationType {
    /// Every member with its spelling — the one vocabulary table
    /// [`from_spelling`](Self::from_spelling) parses, [`name`](Self::name)
    /// renders and [`supported_values`](Self::supported_values) lists, so
    /// a new variant cannot parse without also rendering and advertising
    /// (the [`VectorMetric::ALL`] precedent).
    const ALL: [(&'static str, Self); 3] = [
        ("SCALAR", Self::Scalar),
        ("BINARY", Self::Binary),
        ("NONE", Self::None),
    ];

    /// The canonical (uppercase) stored spelling, from the table's own
    /// row.
    fn name(self) -> &'static str {
        Self::ALL
            .into_iter()
            .find(|(_, member)| *member == self)
            .map(|(spelling, _)| spelling)
            .expect("every QuantizationType is listed in ALL")
    }

    /// The parse of one `vector.quantization.type` value: the
    /// case-insensitive spelling of a member, or `None` for every other
    /// string (INT8 included — the older scalar-quantization name, which
    /// Relate once normalized instead of rejecting).
    fn from_spelling(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|(spelling, _)| spelling.eq_ignore_ascii_case(name))
            .map(|(_, member)| member)
    }

    /// The supported-values list the out-of-vocabulary rejection renders,
    /// derived from [`Self::ALL`] — the vocabulary's one spelling.
    fn supported_values() -> String {
        Self::ALL
            .iter()
            .map(|(spelling, _)| format!("'{spelling}'"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The leaf shape an admission-only option key's value must carry,
/// mirroring the ontology validator's `validate_vector_option` contract
/// (relate-ontology/src/validate/ext.rs): `vector.hnsw.m` /
/// `vector.hnsw.ef_construction` are `opt_int` and
/// `vector.default_search_expansion_factor` accepts a number. Every other
/// value-carrying key's domain (the dimension range, the metric name set,
/// the quantization vocabulary) is beyond a shape, so its row carries a
/// [`KeyRule`] domain parse instead; there is deliberately no shape-less
/// [`KeyShape`] sentinel variant that `admits` everything. The legacy
/// `vector.quantization.enabled` Bool is not a shape row: its value is
/// kept, not merely admitted, so it carries its own
/// [`KeyRule::LegacyQuantization`] identity.
#[derive(Clone, Copy, PartialEq)]
enum KeyShape {
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
            (Self::Int, Value::Int(_)) | (Self::Number, Value::Int(_) | Value::Float(_))
        )
    }
}

/// What one row of [`CONFIG_KEYS`] owns: a value-carrying key's domain
/// parse — the dimension range ([`KeyRule::Dimension`]), the metric name
/// set ([`KeyRule::Metric`]), the quantization vocabulary
/// ([`KeyRule::Quantization`]) or the legacy quantization Bool
/// ([`KeyRule::LegacyQuantization`]) — or an admission-only key's leaf
/// shape ([`KeyRule::Shape`]). Every row is a validator.
#[derive(Clone, Copy)]
enum KeyRule {
    Dimension,
    Metric,
    Quantization,
    /// The legacy `vector.quantization.enabled` spelling: a Bool admitted
    /// under its own identity — not a shape-sniffed Bool row — because its
    /// value is kept, to map onto the type when the `.type` row is absent
    /// (true → SCALAR, false → NONE, REL8-1062).
    LegacyQuantization,
    Shape(KeyShape),
}

/// The seven-key allowlist of a stored vector index's `indexConfig`, one
/// table — each row owns its whole policy: the four value-carrying keys'
/// domain parses and the three admission-only keys' leaf shapes. The
/// admission-only rows' stored values have no behavioural effect (the
/// scan ignores them — LABS-708 is admission only), and an absent key
/// stays absent (the pre-LABS-708 stored shape, so defaulting is
/// unchanged). The unknown-key scan and the parse pass in
/// [`parse_config`] both consult this one table; the row order is the
/// check order.
const CONFIG_KEYS: [(&str, KeyRule); 7] = [
    (KEY_DIMENSIONS, KeyRule::Dimension),
    (KEY_SIMILARITY, KeyRule::Metric),
    (KEY_QUANTIZATION, KeyRule::LegacyQuantization),
    (KEY_QUANTIZATION_TYPE, KeyRule::Quantization),
    (KEY_HNSW_M, KeyRule::Shape(KeyShape::Int)),
    (KEY_HNSW_EF, KeyRule::Shape(KeyShape::Int)),
    (KEY_SEARCH_EXPANSION, KeyRule::Shape(KeyShape::Number)),
];

/// The DDL bound on `vector.dimensions` and a `vector()` call's dimension,
/// the `usize` image of the one shared [`relate_model::VECTOR_DIMENSION_RANGE`]
/// the parser and the front end consult.
pub(crate) const VECTOR_DIMENSIONS_MIN: usize =
    *relate_model::VECTOR_DIMENSION_RANGE.start() as usize;
pub(crate) const VECTOR_DIMENSIONS_MAX: usize =
    *relate_model::VECTOR_DIMENSION_RANGE.end() as usize;

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
    let mut quantization: Option<QuantizationType> = None;
    // The legacy `vector.quantization.enabled` spelling's parsed value —
    // the newer `.type` spelling wins when both are present (the
    // admission of both keys together is unchanged).
    let mut legacy: Option<QuantizationType> = None;
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
            // fabricates a default over a malformed key; the vocabulary is
            // the closed SCALAR/BINARY/NONE set, case-insensitively
            // (REL8-1062) — INT8 and every other spelling is the typed
            // rejection in the config families' message shape.
            KeyRule::Quantization => match value {
                Value::String(name) => {
                    quantization =
                        Some(QuantizationType::from_spelling(name).ok_or_else(|| {
                            Error::argument_error(format!(
                                "Invalid input for '{KEY_QUANTIZATION_TYPE}': '{name}'. \
                                 Expected one of {}.",
                                QuantizationType::supported_values()
                            ))
                        })?);
                }
                _ => return Err(Error::argument_error("InvalidInputValue")),
            },
            // The legacy Bool is kept, already mapped onto the type it
            // stands for (true → SCALAR, false → NONE) — the vocabulary
            // is never re-encoded as a Bool.
            KeyRule::LegacyQuantization => match value {
                Value::Bool(enabled) => {
                    legacy = Some(if *enabled {
                        QuantizationType::Scalar
                    } else {
                        QuantizationType::None
                    })
                }
                _ => return Err(Error::argument_error("InvalidInputValue")),
            },
            KeyRule::Shape(shape) => {
                if !shape.admits(value) {
                    return Err(Error::argument_error("InvalidInputValue"));
                }
            }
        }
    }
    // The type resolves from the `.type` row, else from the legacy
    // `enabled` spelling, else the DDL default (BINARY) — the precedence
    // is explicit and independent of the table's row order.
    let quantization = quantization.or(legacy).unwrap_or(QuantizationType::Binary);
    Ok(StoredVectorConfig {
        dimension,
        metric,
        quantization,
    })
}

/// The stored vector index's parsed options.
#[derive(Debug)]
pub(crate) struct StoredVectorConfig {
    pub dimension: Option<usize>,
    /// The stored metric — the parse's [`TryFrom`] narrowing is the
    /// type, so no consumer can hold a metric the scan kernels would
    /// reject.
    pub metric: StorableMetric,
    /// The quantization type, one of the closed SCALAR/BINARY/NONE
    /// vocabulary the parse admits (an absent key and the legacy
    /// `enabled` spelling resolve into the same vocabulary, REL8-1062) —
    /// carried typed, the [`StorableMetric`] sibling, so no consumer can
    /// hold a value the vocabulary does not contain.
    pub quantization: QuantizationType,
}

/// The stored read's parse — the one vocabulary parse
/// ([`parse_config`]), preceded by the one legacy fold: a build before
/// the closed vocabulary accepted and uppercased any spelling, so a
/// stored `INT8` (the older scalar-quantization name) is already on
/// disk. The CREATE side rejects that spelling; the stored read maps it
/// onto `SCALAR`, its successor, so an index an older build created
/// stays queryable (the rust-review back-compat remedy, REL8-1062).
/// Every other out-of-vocabulary stored value stays the drift the read's
/// contract describes.
pub(crate) fn parse_stored_config(config: &BTreeMap<String, Value>) -> Result<StoredVectorConfig> {
    if matches!(
        config.get(KEY_QUANTIZATION_TYPE),
        Some(Value::String(stored)) if stored.eq_ignore_ascii_case("INT8")
    ) {
        let mut folded = config.clone();
        folded.insert(
            KEY_QUANTIZATION_TYPE.into(),
            Value::String(QuantizationType::Scalar.name().into()),
        );
        return parse_config(&folded);
    }
    parse_config(config)
}

/// The DDL defaults — live Neo4j dev's own (`SHOW INDEXES` on a freshly
/// created vector index, REL8-1062): the HNSW tuning pair, the search
/// expansion factor, the parsed quantization type, and the parsed
/// metric's canonical spelling. The parse established the shapes, so
/// this only inserts; a user-supplied value wins.
/// `vector.quantization.enabled` is still admitted (the older spelling)
/// but is no longer defaulted — the parse maps it onto the type
/// (true → SCALAR, false → NONE) when the `.type` spelling is absent,
/// and Neo4j reports `vector.quantization.type` instead. The parse's
/// result is taken whole, so a future defaulted field is a
/// compiler-guided edit inside this module, not a new positional
/// parameter threaded by each caller.
pub(crate) fn apply_defaults(config: &mut BTreeMap<String, Value>, parsed: &StoredVectorConfig) {
    config.entry(KEY_HNSW_M.into()).or_insert(Value::Int(16));
    config.entry(KEY_HNSW_EF.into()).or_insert(Value::Int(100));
    // The stored spelling is canonical (uppercase) — Neo4j reports `NONE`
    // even when the statement wrote `'none'`.
    config.insert(
        KEY_QUANTIZATION_TYPE.into(),
        Value::String(parsed.quantization.name().into()),
    );
    // The expansion factor's default follows the quantization type: a
    // non-quantized index defaults to 1.0, a scalar-quantized one to
    // 1.5, a binary-quantized one to 3.0 (live Neo4j dev, REL8-1062;
    // 2026.09 reported 2.0 for BINARY — dev's 3.0 is retained as the
    // newer observation). The match is exhaustive over the typed
    // vocabulary — a member added to the enum is a compiler-guided edit
    // here, never a silent default.
    let expansion_default = match parsed.quantization {
        QuantizationType::None => 1.0,
        QuantizationType::Scalar => 1.5,
        QuantizationType::Binary => 3.0,
    };
    config
        .entry(KEY_SEARCH_EXPANSION.into())
        .or_insert(Value::Float(expansion_default));
    // Canonical spelling, present or defaulted: the parse established
    // the String-ness, so the uppercase normalization folds here.
    config.insert(
        KEY_SIMILARITY.into(),
        Value::String(parsed.metric.name().into()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::map;

    /// The option allowlist admits both `vector.quantization` keys — the
    /// SEARCH acceptance suites' Background sets `type: 'none'`
    /// (LABS-708) — together and separately, validates `type` as a
    /// string, and still rejects everything outside the table. The
    /// `.type` vocabulary is closed (SCALAR/BINARY/NONE
    /// case-insensitively, REL8-1062): INT8 and every other spelling is
    /// the typed supported-values rejection.
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

        // The vocabulary is closed and case-insensitive: every member of
        // the one table parses from its own spelling and its lowercase
        // form, and INT8 — the old scalar-quantization name Relate once
        // normalized — is rejected with the supported-values list,
        // exactly like any other out-of-vocabulary spelling. Iterating
        // the table (the `VectorMetric::ALL` round-trip precedent) keeps
        // a member added to the enum covered by construction, and the
        // uniqueness check keeps the spellings a bijection.
        let mut spellings: Vec<&str> = QuantizationType::ALL.iter().map(|(s, _)| *s).collect();
        spellings.sort_unstable();
        spellings.dedup();
        assert_eq!(
            spellings.len(),
            QuantizationType::ALL.len(),
            "spellings are unique"
        );
        for (spelling, member) in QuantizationType::ALL {
            for form in [spelling, spelling.to_ascii_lowercase().as_str()] {
                let parsed =
                    parse_config(&map(&[(KEY_QUANTIZATION_TYPE, Value::String(form.into()))]))
                        .expect("an in-vocabulary spelling parses");
                assert_eq!(parsed.quantization, member, "{form:?}");
            }
        }
        for rejected in ["int8", "INT8", "Bogus", ""] {
            let error = parse_config(&map(&[(
                KEY_QUANTIZATION_TYPE,
                Value::String(rejected.into()),
            )]))
            .expect_err("an out-of-vocabulary spelling is rejected");
            assert!(
                format!("{error}").contains(&QuantizationType::supported_values()),
                "{rejected:?} rejection lists the supported values: {error}"
            );
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
    use crate::test_support::map;

    /// The stored read's legacy fold: a stored `INT8` (a spelling an
    /// older build accepted and uppercased) maps onto SCALAR, so an
    /// index an older build created stays queryable, while the CREATE
    /// side's rejection of the same spelling is unchanged (REL8-1062).
    #[test]
    fn stored_int8_folds_onto_scalar() {
        for stored in ["INT8", "int8"] {
            let parsed = parse_stored_config(&map(&[(
                KEY_QUANTIZATION_TYPE,
                Value::String(stored.into()),
            )]))
            .expect("the stored legacy spelling folds onto SCALAR");
            assert_eq!(parsed.quantization, QuantizationType::Scalar);
        }
        // Every other out-of-vocabulary stored spelling stays the drift
        // the read's contract describes.
        assert!(parse_stored_config(&map(&[(
            KEY_QUANTIZATION_TYPE,
            Value::String("Bogus".into()),
        )]))
        .is_err());
    }

    /// Neo4j dev's DDL defaults: the HNSW pair, a 3.0 expansion factor and
    /// the BINARY quantization type; NONE moves the expansion factor to
    /// 1.0, SCALAR to 1.5 (REL8-1062), and the stored spelling is
    /// canonical (uppercase). The legacy `enabled` spelling maps onto the
    /// type when `.type` is absent, and both keys present keep today's
    /// precedence: the `.type` spelling wins.
    #[test]
    fn defaults_match_live_neo4j_and_follow_the_quantization_type() {
        let mut config = map(&[]);
        let parsed = parse_config(&config).expect("an empty config parses");
        assert_eq!(parsed.quantization, QuantizationType::Binary);
        apply_defaults(&mut config, &parsed);
        assert_eq!(config.get(KEY_SEARCH_EXPANSION), Some(&Value::Float(3.0)));
        assert_eq!(
            config.get(KEY_QUANTIZATION_TYPE),
            Some(&Value::String(QuantizationType::Binary.name().into()))
        );
        assert_eq!(config.get(KEY_HNSW_M), Some(&Value::Int(16)));
        assert_eq!(config.get(KEY_HNSW_EF), Some(&Value::Int(100)));

        // The parse canonicalizes the spelling, and the defaults branch on
        // that typed value — never on the raw string a statement wrote.
        let mut none = map(&[(KEY_QUANTIZATION_TYPE, Value::String("none".into()))]);
        let parsed = parse_config(&none).expect("a string quantization type parses");
        assert_eq!(parsed.quantization, QuantizationType::None);
        apply_defaults(&mut none, &parsed);
        assert_eq!(
            none.get(KEY_QUANTIZATION_TYPE),
            Some(&Value::String(QuantizationType::None.name().into()))
        );
        assert_eq!(none.get(KEY_SEARCH_EXPANSION), Some(&Value::Float(1.0)));

        // A scalar-quantized index defaults to the 1.5 expansion factor.
        let mut scalar = map(&[(KEY_QUANTIZATION_TYPE, Value::String("Scalar".into()))]);
        let parsed = parse_config(&scalar).expect("an in-vocabulary type parses");
        assert_eq!(parsed.quantization, QuantizationType::Scalar);
        apply_defaults(&mut scalar, &parsed);
        assert_eq!(scalar.get(KEY_SEARCH_EXPANSION), Some(&Value::Float(1.5)));

        // The legacy `enabled` spelling maps onto the type when `.type` is
        // absent: true → SCALAR, false → NONE (REL8-1062).
        for (enabled, canonical, expansion) in [
            (true, QuantizationType::Scalar, 1.5),
            (false, QuantizationType::None, 1.0),
        ] {
            let mut legacy = map(&[(KEY_QUANTIZATION, Value::Bool(enabled))]);
            let parsed = parse_config(&legacy).expect("the legacy spelling is admitted");
            assert_eq!(parsed.quantization, canonical);
            apply_defaults(&mut legacy, &parsed);
            assert_eq!(
                legacy.get(KEY_QUANTIZATION_TYPE),
                Some(&Value::String(canonical.name().into()))
            );
            assert_eq!(
                legacy.get(KEY_SEARCH_EXPANSION),
                Some(&Value::Float(expansion))
            );
        }

        // Both keys present: the `.type` spelling wins, unchanged.
        let both = map(&[
            (KEY_QUANTIZATION, Value::Bool(false)),
            (KEY_QUANTIZATION_TYPE, Value::String("binary".into())),
        ]);
        let parsed = parse_config(&both).expect("both keys are admitted together");
        assert_eq!(parsed.quantization, QuantizationType::Binary);
    }
}
