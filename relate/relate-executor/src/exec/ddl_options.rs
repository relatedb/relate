//! The schema-DDL `OPTIONS` module — the one owner of every option rule a
//! `CREATE INDEX`/`CREATE CONSTRAINT` carries: the outer map's key/class
//! policy and its literal-vs-parameter resolution (ADR-0489), the
//! `CREATE INDEX` target shape, and the per-kind `indexConfig` vocabulary.
//!
//! Every rejection here is either the statement *shape* rule (Neo4j's
//! front-end `SemanticError.*` constructions, surfaced as `SyntaxError`) or
//! the run-time argument validator's (`ArgumentError`). For the
//! `indexConfig` *value* path it is always the latter: live Neo4j reports
//! `Neo.ClientError.General.InvalidArguments` for a non-map or foreign
//! config and `Neo.ClientError.Statement.ArgumentError` for an unknown
//! setting or analyzer name — both the openCypher `ArgumentError` class
//! (live-verified 2026-11-02/03 on neo4j:2026.09-enterprise).

use std::collections::{BTreeMap, HashSet};
use std::hash::Hash;

use relate_core::{Error, Result, Value};
use relate_parser::{IndexKind, OptionsForm};

use crate::eval::eval;
use crate::exec::fulltext::Analyzer;
use crate::overlay::ExecGraph;
use crate::row::Row;

// ---------------------------------------------------------------------
// The shared `indexConfig` vocabulary (ADR-0489)
// ---------------------------------------------------------------------

/// The one outer `OPTIONS` key every index kind admits and records; the
/// inner per-kind schema lives in [`create_config`] and the vector
/// feature module, the stored read in `exec::vector::stored_config`.
pub(crate) const KEY_INDEX_CONFIG: &str = "indexConfig";

/// The one non-MAP `indexConfig` rejection: every index kind's config slot
/// rejects a non-map the same way, so the sentence is shared rather than
/// re-authored per kind (live Neo4j reports
/// `Neo.ClientError.General.InvalidArguments` for all of them, ADR-0489).
pub(crate) const INDEX_CONFIG_NOT_A_MAP: &str = "Invalid input for 'indexConfig'. Expected a MAP.";

/// One navigation of the `indexConfig` option — Map or absent, anything
/// else is rejected at this boundary, never silently dropped. A borrow: the
/// one mutating consumer (the vector CREATE defaults) clones at its point of
/// mutation.
pub(crate) fn index_config(
    options: &BTreeMap<String, Value>,
) -> Result<Option<&BTreeMap<String, Value>>> {
    Ok(index_config_or(options, &|_| {
        // ADR-0489: a rejected `indexConfig` value is the DDL argument
        // validator's failure (Neo4j `Neo.ClientError.General.
        // InvalidArguments`), not a statement-shape syntax error.
        Error::argument_error(INDEX_CONFIG_NOT_A_MAP)
    })?
    .map(|(_, map)| map))
}

/// [`index_config`] with the caller's own non-map rejection: a surface whose
/// authored text names the offending value builds it here rather than
/// navigating the key a second time. The found value rides with the map, so
/// a rejection naming the whole `indexConfig` value needs no re-rendering.
pub(crate) fn index_config_or<'a>(
    options: &'a BTreeMap<String, Value>,
    rejected: &dyn Fn(&Value) -> Error,
) -> Result<Option<(&'a Value, &'a BTreeMap<String, Value>)>> {
    match options.get(KEY_INDEX_CONFIG) {
        None => Ok(None),
        Some(value @ Value::Map(map)) => Ok(Some((value, map.as_ref()))),
        Some(value) => Err(rejected(value)),
    }
}

// ---------------------------------------------------------------------
// Outer OPTIONS key policy and resolution (ADR-0489)
// ---------------------------------------------------------------------

/// The outer `OPTIONS` allowlist every schema DDL command admits, in both
/// spellings. Cypher 25 removed `indexProvider`, so `indexConfig` is the
/// only key. A literal spelling is the front end's syntax-class rejection
/// before its value expression is lowered (an absent parameter in the value
/// slot must not change the class); a whole-map parameter carrying a foreign
/// key is the argument validator's rejection after resolution — the same
/// allowlist, the class differing by provenance.
pub(crate) const ADMITTED_OPTION_KEYS: &[&str] = &[KEY_INDEX_CONFIG];

/// Live's rejection text for a non-admitted `OPTIONS` key: name the key and
/// the admitted spelling(s), rendered from [`ADMITTED_OPTION_KEYS`] so the
/// text and the allowlist cannot drift. `indexProvider` was deprecated and
/// then removed upstream, so the schema DDL resolvers read the same text
/// for every non-admitted key. The *class* stays by provenance — a literal
/// key is the front end's `SyntaxError`, a whole-map parameter's is the
/// argument validator's `ArgumentError` — matching live 2026.10.1's driver
/// codes; the GQL status (22N04) rides inside live's message, which is why
/// this text names no code of its own.
pub(crate) fn rejected_option_key_message(key: &str) -> String {
    let expected = ADMITTED_OPTION_KEYS
        .iter()
        .map(|admitted| format!("'{admitted}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("Invalid input '{key}' for 'OPTIONS'. Expected {expected}.")
}

/// The literal-spelling rejection the schema DDL resolvers share: the front
/// end's syntax class carrying the pinned text.
fn literal_option_key(key: &str) -> Error {
    Error::syntax(rejected_option_key_message(key))
}

/// The parameter-spelling twin: the argument validator's class, its message
/// key lowercased as live does.
fn parameter_option_key(key: &str) -> Error {
    Error::argument_error(rejected_option_key_message(&key.to_lowercase()))
}

/// The schema DDL resolvers' one key policy (the index and constraint
/// twins): the shared allowlist, the syntax class for a literal foreign
/// key, the argument class for a whole-map parameter's.
fn ddl_option_key_policy() -> OptionKeyPolicy<'static> {
    OptionKeyPolicy {
        admitted: ADMITTED_OPTION_KEYS,
        literal: &literal_option_key,
        parameter: &parameter_option_key,
    }
}

/// The one whole-map `OPTIONS $param` → map invariant: an absent parameter
/// is `ParameterMissing` (ADR-0308), a non-MAP is the run-time argument
/// validator's `ArgumentError` carrying the caller's text (ADR-0489). The
/// schema DDL resolvers and the graph-type resolver share it, so no caller
/// re-spells the two rejections.
pub(crate) fn parameter_map<'a>(
    params: &'a BTreeMap<String, Value>,
    parameter: &str,
    not_a_map: &str,
) -> Result<&'a BTreeMap<String, Value>> {
    match params.get(parameter) {
        Some(Value::Map(map)) => Ok(map),
        None => Err(Error::parameters_missing([parameter])),
        Some(_) => Err(Error::argument_error(not_a_map)),
    }
}

/// The outer-map key policy [`resolve_options`] applies: the admitted keys
/// and the rejection each spelling raises for a foreign key. The class and
/// wording differ by surface -- the statement's own front end rejects a
/// literal foreign key as a syntax error, a whole-map parameter's foreign
/// key is the argument validator's, and the graph-type body reports its
/// kinds' own messages in both spellings -- so the caller supplies them.
pub(crate) struct OptionKeyPolicy<'a> {
    /// The keys the calling command admits.
    pub(crate) admitted: &'a [&'a str],
    /// The literal-spelling rejection for a foreign key.
    pub(crate) literal: &'a dyn Fn(&str) -> Error,
    /// The resolved-parameter-spelling rejection for a foreign key.
    pub(crate) parameter: &'a dyn Fn(&str) -> Error,
}

/// The one `OPTIONS` resolution sequence (ADR-0489), shared by
/// `CREATE CONSTRAINT`, `CREATE INDEX` and the graph-type body, one arm per
/// form:
///
/// - `Absent` yields no options;
/// - `Literal` checks every key against the policy *before* any value
///   expression is lowered (so a shape rejection wins over an absent
///   parameter in a value slot), then lowers and evaluates the values;
/// - `Parameter` resolves the whole map (absent → `ParameterMissing`,
///   non-MAP → `ArgumentError`), then checks its keys against the policy.
pub(crate) fn resolve_options(
    graph: &ExecGraph<'_>,
    form: &OptionsForm,
    params: &BTreeMap<String, Value>,
    policy: &OptionKeyPolicy<'_>,
    not_a_map: &'static str,
) -> Result<BTreeMap<String, Value>> {
    let mut options = BTreeMap::new();
    match form {
        OptionsForm::Absent => {}
        OptionsForm::Literal(entries) => {
            if let Some((key, _)) = entries
                .iter()
                .find(|(key, _)| !policy.admitted.contains(&key.as_str()))
            {
                return Err((policy.literal)(key));
            }
            for (key, expression) in entries {
                options.insert(
                    key.clone(),
                    eval(
                        graph,
                        &Row::new(),
                        &relate_front::lower_expr(expression)?,
                        params,
                    )?,
                );
            }
        }
        OptionsForm::Parameter(parameter) => {
            options = parameter_map(params, parameter, not_a_map)?.clone();
            // The resolved map has no value expressions, so its keys are
            // checked after resolution; that provenance is the argument
            // validator's class.
            if let Some(key) = options
                .keys()
                .find(|key| !policy.admitted.contains(&key.as_str()))
            {
                return Err((policy.parameter)(key));
            }
        }
    }
    Ok(options)
}

/// The `CREATE CONSTRAINT` options shape (ADR-0489): an
/// index-backed constraint admits only `indexConfig`, and any other literal
/// key is the front end's syntax error, rejected before the value
/// expression is lowered and before the schema-conflict checks run. A
/// rejected `indexConfig` *value*, and every rejection reached through a
/// whole-map `OPTIONS $param` (including a non-map parameter), is the
/// argument validator's `ArgumentError`. EXISTENCE and PROPERTY TYPE
/// constraints admit no options at all: any entry is rejected, whether
/// written literally or supplied by the parameter (live-verified 2026-11-02
/// on neo4j:2026.09-enterprise). Running before the conflict gate makes the
/// shape class win over an equivalent rule, matching Neo4j's compile-time
/// ordering.
pub(crate) fn validate_constraint_options(
    graph: &ExecGraph<'_>,
    c: &relate_parser::CreateConstraintClause,
    constraint_type: relate_model::ConstraintType,
    params: &BTreeMap<String, Value>,
) -> Result<()> {
    let options = resolve_options(
        graph,
        &c.options,
        params,
        &ddl_option_key_policy(),
        "Could not create constraint with options: expected a map value.",
    )?;
    // EXISTENCE and PROPERTY TYPE constraints admit no options at all
    // (Neo4j 22N81): an empty map is accepted, any entry is not.
    if matches!(
        constraint_type,
        relate_model::ConstraintType::Exists | relate_model::ConstraintType::PropertyType
    ) {
        return if options.is_empty() {
            Ok(())
        } else {
            Err(Error::argument_error(
                "InvalidOptionsMap: this constraint has no valid options values",
            ))
        };
    }
    // The one admitted value shape: an empty `indexConfig` map, navigated
    // through the module's one accessor.
    if index_config(&options)?.is_some_and(|config| !config.is_empty()) {
        return Err(Error::argument_error("InvalidOptionsMap"));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// CREATE INDEX target shape
// ---------------------------------------------------------------------

/// The kinds that take exactly one property, with the rejection each raises
/// when the count differs (Neo4j's two messages, live-verified): POINT/TEXT
/// report the property-count rule in the front end, VECTOR reports its own
/// unsupported-shape error. One table, consulted once.
fn single_property_rejection(kind: IndexKind) -> Option<&'static str> {
    match kind {
        IndexKind::Point | IndexKind::Text => {
            Some("InvalidArguments: this index type requires one property")
        }
        IndexKind::Vector => Some("UnsupportedIndexOrConstraint"),
        IndexKind::Range | IndexKind::Fulltext | IndexKind::Lookup => None,
    }
}

/// The duplicated-token policy: no token repeats within its vocabulary.
fn has_duplicates<'a, T: Hash + Eq + 'a>(mut items: impl Iterator<Item = &'a T>) -> bool {
    let mut seen = HashSet::new();
    items.any(|item| !seen.insert(item))
}

/// The `CREATE INDEX` target-shape policy: the single-property kinds'
/// property count and the duplicated-token rule (22N76 — the label and the
/// property vocabularies are separate sets, each without duplicates, for
/// every index kind). Runs before the OPTIONS shape so a target rejection
/// wins over a config one.
fn validate_index_target(c: &relate_parser::CreateIndexClause) -> Result<()> {
    if let Some(detail) = single_property_rejection(c.kind) {
        if c.properties.len() != 1 {
            return Err(Error::syntax(detail));
        }
    }
    if has_duplicates(c.labels.iter())
        || has_duplicates(c.properties.iter().chain(&c.with_properties))
    {
        return Err(Error::semantic("IndexContainsDuplicatedTokens"));
    }
    Ok(())
}

/// The `CREATE INDEX` options shape (ADR-0489), the `CREATE CONSTRAINT`
/// twin's counterpart: the target shape, the shared `OPTIONS` resolution
/// (whose allowlist excludes `indexProvider` in both spellings), and the
/// per-kind `indexConfig` policy. Returns the resolved options map the
/// catalog detail records. Runs before the conflict gate so an `IF NOT
/// EXISTS`/equivalent-rule skip can never mask a shape rejection.
pub(crate) fn resolve_index_options(
    graph: &ExecGraph<'_>,
    c: &relate_parser::CreateIndexClause,
    params: &BTreeMap<String, Value>,
) -> Result<BTreeMap<String, Value>> {
    validate_index_target(c)?;
    let mut options = resolve_options(
        graph,
        &c.options,
        params,
        &ddl_option_key_policy(),
        "InvalidInputValue: OPTIONS parameter must be a MAP",
    )?;
    validate_index_config(c.kind, &mut options)?;
    // Point and fulltext indexes record their kind's default config so
    // SHOW INDEXES' `options` and `createStatement` carry Neo4j's shape; a
    // caller-supplied entry wins. The per-kind table is
    // `IndexKindSpec::default_config`, not a match here.
    if let Some(defaults) = crate::redo::IndexKindSpec::from(c.kind).default_config() {
        let mut config = index_config(&options)?.cloned().unwrap_or_default();
        defaults(&mut config);
        options.insert(KEY_INDEX_CONFIG.into(), Value::Map(config.into()));
    }
    Ok(options)
}

// ---------------------------------------------------------------------
// Per-kind indexConfig vocabulary
// ---------------------------------------------------------------------

/// One value shape an `indexConfig` setting admits.
#[derive(Clone, Copy)]
enum ValueShape {
    /// A list of numbers (a POINT index's spatial bound).
    NumericList,
    /// One of the fulltext analyzer names.
    Analyzer,
    /// A boolean.
    Boolean,
}

impl ValueShape {
    fn check(self, key: &str, value: &Value) -> Result<()> {
        match self {
            Self::NumericList => {
                if matches!(
                    value,
                    Value::List(items)
                        if items.iter().all(|item| matches!(item, Value::Float(_) | Value::Int(_)))
                ) {
                    Ok(())
                } else {
                    Err(Error::argument_error(
                        "Invalid input for 'indexConfig'. Expected to be MAP<STRING, LIST<FLOAT>>.",
                    ))
                }
            }
            Self::Analyzer => match value {
                Value::String(name) if Analyzer::from_name(name).is_some() => Ok(()),
                _ => Err(Error::argument_error(format!(
                    "Invalid input for analyzer name: {value:?}"
                ))),
            },
            Self::Boolean => {
                if matches!(value, Value::Bool(_)) {
                    Ok(())
                } else {
                    Err(Error::argument_error(format!(
                        "Invalid input for '{key}'. Expected a boolean."
                    )))
                }
            }
        }
    }
}

/// One index kind's `indexConfig` family: the settings it admits and the
/// prefixes of the families other kinds own. The shared loop in
/// [`validate_config_family`] is the only control flow.
struct ConfigFamily {
    keys: &'static [(&'static str, ValueShape)],
    /// Key prefixes that name another index kind's config — rejected as a
    /// foreign family, never as an unknown setting.
    foreign_prefixes: &'static [&'static str],
    /// Completes "Expected {…}." in the foreign-family rejection.
    foreign_expected: &'static str,
}

/// The point index's `indexConfig` keys: two-dimensional and 3D bounds in
/// the cartesian and WGS-84 coordinate systems (Neo4j's live allowlist,
/// 2026.09).
const POINT_FAMILY: ConfigFamily = ConfigFamily {
    keys: &[
        ("spatial.cartesian.min", ValueShape::NumericList),
        ("spatial.cartesian.max", ValueShape::NumericList),
        ("spatial.cartesian-3d.min", ValueShape::NumericList),
        ("spatial.cartesian-3d.max", ValueShape::NumericList),
        ("spatial.wgs-84.min", ValueShape::NumericList),
        ("spatial.wgs-84.max", ValueShape::NumericList),
        ("spatial.wgs-84-3d.min", ValueShape::NumericList),
        ("spatial.wgs-84-3d.max", ValueShape::NumericList),
    ],
    foreign_prefixes: &["fulltext.", "vector."],
    foreign_expected: "a spatial bound",
};

/// The `indexConfig` key naming a fulltext index's analyzer.
pub(crate) const KEY_FULLTEXT_ANALYZER: &str = "fulltext.analyzer";

/// The analyzer a stored fulltext index was created with: its
/// [`KEY_FULLTEXT_ANALYZER`] setting, the standard analyzer when absent. A
/// stored value the create-time validation would have rejected is catalog
/// drift.
pub(crate) fn index_analyzer(
    options: &BTreeMap<String, Value>,
    index_name: &str,
) -> Result<Analyzer> {
    let drift = || {
        Error::execution(format!(
            "InvalidIndexDefinition: fulltext index {index_name} has an invalid indexConfig option"
        ))
    };
    let config = index_config(options).map_err(|_| drift())?;
    match config.and_then(|config| config.get(KEY_FULLTEXT_ANALYZER)) {
        None => Ok(Analyzer::Standard),
        Some(Value::String(name)) => Analyzer::from_name(name).ok_or_else(drift),
        Some(_) => Err(drift()),
    }
}

/// The fulltext index's two `indexConfig` settings.
const FULLTEXT_FAMILY: ConfigFamily = ConfigFamily {
    keys: &[
        (KEY_FULLTEXT_ANALYZER, ValueShape::Analyzer),
        ("fulltext.eventually_consistent", ValueShape::Boolean),
    ],
    foreign_prefixes: &["spatial.", "vector."],
    foreign_expected: "'fulltext.analyzer' or 'fulltext.eventually_consistent'",
};

/// The create side of the vector option schema, the read side's mirror
/// (`exec::vector::stored_config`): one adjacent pipeline — navigate the
/// outer `indexConfig` (Map or absent, anything else is rejected at the
/// [`index_config`] boundary), parse (the typed DDL errors), apply the
/// DDL defaults, and return the canonical `indexConfig` value to store
/// under [`KEY_INDEX_CONFIG`]. The parse and the defaults are the root
/// schema's ([`crate::vector_config`]) — a root module this exec file
/// imports downward, never the reverse (LABS-845).
fn create_config(options: &BTreeMap<String, Value>) -> Result<Value> {
    let mut config = index_config(options)?.cloned().unwrap_or_default();
    let parsed = crate::vector_config::parse_config(&config)?;
    crate::vector_config::apply_defaults(&mut config, parsed.metric, &parsed.quantization);
    Ok(Value::Map(config.into()))
}

/// The per-kind `indexConfig` value policy: one variant per validation
/// strategy, so every kind — VECTOR included — has exactly one policy.
enum ConfigPolicy {
    /// The kind consumes no config: absent or empty is fine, anything else
    /// is rejected.
    Nothing { non_empty: &'static str },
    /// A key/value family, validated by the shared loop.
    Family(&'static ConfigFamily),
    /// VECTOR: the typed parse and the DDL defaults are
    /// [`create_config`]'s navigate-and-default pipeline.
    Vector,
}

impl ConfigPolicy {
    /// The kind's value policy — one variant per kind, no absent mode.
    fn for_kind(kind: IndexKind) -> Self {
        match kind {
            IndexKind::Lookup => Self::Nothing {
                non_empty: "Invalid input for index config. Expected nothing.",
            },
            IndexKind::Range | IndexKind::Text => Self::Nothing {
                non_empty: "InvalidIndexConfig",
            },
            IndexKind::Point => Self::Family(&POINT_FAMILY),
            IndexKind::Fulltext => Self::Family(&FULLTEXT_FAMILY),
            IndexKind::Vector => Self::Vector,
        }
    }

    /// Validate the resolved options map's `indexConfig` entry (absent = no
    /// config), navigating the one [`index_config`] boundary itself so every
    /// variant — the mutating VECTOR one included — shares the same entry
    /// point.
    fn validate(&self, options: &mut BTreeMap<String, Value>) -> Result<()> {
        match self {
            Self::Nothing { non_empty } => match index_config(options)? {
                None => Ok(()),
                Some(map) if map.is_empty() => Ok(()),
                Some(_) => Err(Error::argument_error(*non_empty)),
            },
            Self::Family(family) => match index_config(options)? {
                None => Ok(()),
                Some(map) => validate_config_family(map, family),
            },
            Self::Vector => {
                let config = create_config(options)?;
                options.insert(KEY_INDEX_CONFIG.into(), config);
                Ok(())
            }
        }
    }
}

/// Validate (and, for VECTOR, default) the resolved `indexConfig` entry
/// against its index kind — the one config-policy owner. Every kind's
/// policy lives in [`ConfigPolicy::for_kind`]; this is the only dispatcher.
fn validate_index_config(kind: IndexKind, options: &mut BTreeMap<String, Value>) -> Result<()> {
    ConfigPolicy::for_kind(kind).validate(options)
}

/// The shared per-key loop of a config family: lowercase, the foreign-family
/// prefix rejection, the table lookup, and the value shape.
fn validate_config_family(map: &BTreeMap<String, Value>, family: &ConfigFamily) -> Result<()> {
    for (key, value) in map.iter() {
        let lowered = key.to_ascii_lowercase();
        if family
            .foreign_prefixes
            .iter()
            .any(|prefix| lowered.starts_with(prefix))
        {
            return Err(Error::argument_error(format!(
                "Invalid input '{key}' for index config. Expected {}.",
                family.foreign_expected
            )));
        }
        let Some((_, shape)) = family.keys.iter().find(|(name, _)| *name == lowered) else {
            let expected: Vec<&str> = family.keys.iter().map(|(name, _)| *name).collect();
            return Err(Error::argument_error(format!(
                "Invalid input '{key}' for index setting. Expected one of {}.",
                expected.join(", ")
            )));
        };
        shape.check(key, value)?;
    }
    Ok(())
}
