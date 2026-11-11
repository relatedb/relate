//! The per-name planner-side rule table for builtin calls: one
//! [`FunctionMeta`] row per lowercased name, carrying the arity contract
//! (42I13), the argument-shape rule, the ADR-0175 integer-argument sites,
//! and the binding-kind / scope rules. Split from the expression checker
//! (`shadowing` sets the split precedent) so the checker file is the
//! traversal/predicate policy and this file is the table machinery. The
//! temporal constructor family — whose many alias spellings a row per name
//! would duplicate from `is_constructor` — is the one family rule
//! [`apply_function_meta`] runs before the row lookup, so the dispatcher stays
//! the single place per-name call rules fire.

use relate_core::{
    builtin_arity, integer_args, ArityContract, Error, Result, Value,
    FUNCTION_REQUIRES_ONE_ARGUMENT,
};
use relate_ir::Expr;

use super::literal::{reject_non_integer_literal, static_arg_value};
use crate::binding::{Binding, Env, EnvBridge};

/// Env-free per-name call rules: a rule judges the call's argument list and
/// returns the authored rejection. Rows live in [`FUNCTION_META`].
type CallRule = fn(&[Expr]) -> Result<()>;

/// Env-dependent per-name call rules: the half that cannot live in the
/// rule column of [`FUNCTION_META`] because it reads the binding scope.
type EnvCallRule = fn(&Env, &[Expr]) -> Result<()>;

/// One planner-side rule for a builtin call, keyed by its lowercased name.
///
/// One table, so a name's arity contract, argument-shape rule and
/// integer-argument sites are decided in one row instead of four
/// independent name lookups. The columns are consulted in the order
/// [`apply_function_meta`] documents; a name appears at most once (the
/// `function_meta_rows_are_sorted_and_unique` test).
struct FunctionMeta {
    name: &'static str,
    /// 42I13: the argument-count contract a call must satisfy, with the
    /// authored message.
    arity: Option<ArityContract>,
    /// The call's argument-list shape rule.
    rule: Option<CallRule>,
    /// ADR-0175 integer-argument sites: (argument index, authored detail
    /// from [`relate_core::integer_args`]).
    int_sites: &'static [(usize, &'static str)],
    /// The binding kinds the first argument's variable may not be bound to,
    /// with the authored message — the `type`/`length`/`properties` family.
    rejected_kinds: Option<(&'static [Binding], &'static str)>,
    /// The env-dependent rule for shapes the kind gate cannot express.
    env_rule: Option<EnvCallRule>,
}

const fn shape_rule(name: &'static str, rule: CallRule) -> FunctionMeta {
    FunctionMeta {
        name,
        arity: None,
        rule: Some(rule),
        int_sites: &[],
        rejected_kinds: None,
        env_rule: None,
    }
}

const fn arity_row(name: &'static str, arity: ArityContract) -> FunctionMeta {
    FunctionMeta {
        name,
        arity: Some(arity),
        rule: None,
        int_sites: &[],
        rejected_kinds: None,
        env_rule: None,
    }
}

const fn int_sites_row(
    name: &'static str,
    sites: &'static [(usize, &'static str)],
) -> FunctionMeta {
    FunctionMeta {
        name,
        arity: None,
        rule: None,
        int_sites: sites,
        rejected_kinds: None,
        env_rule: None,
    }
}

const fn kind_row(
    name: &'static str,
    rejected: &'static [Binding],
    message: &'static str,
) -> FunctionMeta {
    FunctionMeta {
        name,
        arity: None,
        rule: None,
        int_sites: &[],
        rejected_kinds: Some((rejected, message)),
        env_rule: None,
    }
}

const LIST_CONVERSION_ARITY: &str = "list conversion requires one argument";

/// The shared one-argument contract the planner-only rows below reuse.
const ONE_ARGUMENT: ArityContract = ArityContract::exact(1, FUNCTION_REQUIRES_ONE_ARGUMENT);

/// The one planner-side rule table for builtin calls, sorted by name.
const FUNCTION_META: &[FunctionMeta] = &[
    arity_row("coll.distinct", ONE_ARGUMENT),
    arity_row("coll.max", ONE_ARGUMENT),
    arity_row("coll.min", ONE_ARGUMENT),
    arity_row("coll.sort", ONE_ARGUMENT),
    arity_row("cosh", ONE_ARGUMENT),
    arity_row("coth", ONE_ARGUMENT),
    shape_rule("exists", reject_removed_exists_property_form),
    arity_row("haversin", ONE_ARGUMENT),
    arity_row("head", ONE_ARGUMENT),
    arity_row("isempty", ONE_ARGUMENT),
    arity_row("last", ONE_ARGUMENT),
    int_sites_row("left", &[(1, integer_args::LEFT_LENGTH.type_error)]),
    kind_row(
        "length",
        &[Binding::Node, Binding::Rel, Binding::Scalar],
        "InvalidArgumentType: length() needs a path, string, or list",
    ),
    arity_row("lower", ONE_ARGUMENT),
    arity_row("octet_length", ONE_ARGUMENT),
    shape_rule("point", reject_invalid_point_literals),
    kind_row(
        "properties",
        &[Binding::Scalar, Binding::List, Binding::Path],
        "InvalidArgumentType: properties() needs a node, relationship, or map",
    ),
    int_sites_row(
        "range",
        &[
            (0, integer_args::RANGE_START.type_error),
            (1, integer_args::RANGE_END.type_error),
            (2, integer_args::RANGE_STEP.type_error),
        ],
    ),
    arity_row("reverse", ONE_ARGUMENT),
    int_sites_row("right", &[(1, integer_args::RIGHT_LENGTH.type_error)]),
    arity_row("sinh", ONE_ARGUMENT),
    FunctionMeta {
        name: "size",
        arity: Some(ONE_ARGUMENT),
        rule: None,
        int_sites: &[],
        rejected_kinds: None,
        env_rule: Some(rule_size_argument),
    },
    int_sites_row(
        "substring",
        &[
            (1, integer_args::SUBSTRING_START.type_error),
            (2, integer_args::SUBSTRING_LENGTH.type_error),
        ],
    ),
    arity_row("tail", ONE_ARGUMENT),
    arity_row("tanh", ONE_ARGUMENT),
    arity_row(
        "timestamp",
        ArityContract::exact(0, "timestamp() takes no arguments"),
    ),
    arity_row(
        "tobooleanlist",
        ArityContract::exact(1, LIST_CONVERSION_ARITY),
    ),
    arity_row(
        "tofloatlist",
        ArityContract::exact(1, LIST_CONVERSION_ARITY),
    ),
    arity_row(
        "tointegerlist",
        ArityContract::exact(1, LIST_CONVERSION_ARITY),
    ),
    arity_row("tolower", ONE_ARGUMENT),
    arity_row(
        "tostringlist",
        ArityContract::exact(1, LIST_CONVERSION_ARITY),
    ),
    arity_row("toupper", ONE_ARGUMENT),
    kind_row(
        "type",
        &[Binding::Node],
        "InvalidArgumentType: type() needs a relationship",
    ),
    arity_row("upper", ONE_ARGUMENT),
    arity_row(
        "valuetype",
        ArityContract::exact(1, "valueType() requires one argument"),
    ),
];

/// Apply the per-name rules for `name`: the temporal constructor family's
/// static construction check, the cross-layer arity contract, then the
/// [`FUNCTION_META`] row, if any.
///
/// The column order is the historical one: the argument-list shape rule,
/// then the ADR-0175 integer-argument sites (a non-INTEGER literal fails
/// at plan time; a non-literal value is handled at runtime), then the
/// 42I13 arity contract, then the binding-kind gate, then the env-dependent
/// rule. No name carries two columns whose relative order would matter
/// today, but the sequence is stated once so a future row cannot reorder
/// it by accident.
pub(super) fn apply_function_meta(env: &Env, name: &str, args: &[Expr]) -> Result<()> {
    // The temporal constructor family is a name-family rule (a row per alias
    // spelling would duplicate `is_constructor`), so it runs here, before the
    // per-name row: this dispatcher stays the one place call rules fire.
    if relate_core::temporal::is_constructor(name) {
        check_static_temporal_construction(name, args)?;
    }
    // The arity contracts the front end also enforces live in `relate-core`
    // (one fact per name), not in this table; a name with a row below and a
    // contract here carries both, in this one order.
    if let Some(contract) = builtin_arity(name) {
        check_arity_contract(contract, args.len())?;
    }
    let Some(meta) = FUNCTION_META.iter().find(|meta| meta.name == name) else {
        return Ok(());
    };
    if let Some(rule) = meta.rule {
        rule(args)?;
    }
    for &(index, what) in meta.int_sites {
        if let Some(arg) = args.get(index) {
            reject_non_integer_literal(arg, what)?;
        }
    }
    if let Some(contract) = meta.arity {
        check_arity_contract(contract, args.len())?;
    }
    if let Some((rejected, message)) = meta.rejected_kinds {
        if let Some(Expr::Variable(variable)) = args.first() {
            if env
                .get_name(variable)
                .is_some_and(|binding| rejected.contains(binding))
            {
                return Err(Error::syntax(message));
            }
        }
    }
    if let Some(rule) = meta.env_rule {
        rule(env, args)?;
    }
    Ok(())
}

/// The 42I13 rejection for a call outside `contract`: the contract's bare
/// detail under the one `InvalidNumberOfArguments` token, composed once for
/// this layer.
fn check_arity_contract(contract: ArityContract, count: usize) -> Result<()> {
    match contract.violation(count) {
        Some(detail) => Err(Error::invalid_number_of_arguments(detail)),
        None => Ok(()),
    }
}

/// `size()` cannot take a pattern predicate, nor a statically-path value.
fn rule_size_argument(env: &Env, args: &[Expr]) -> Result<()> {
    match args.first() {
        Some(Expr::PatternPred { .. }) => Err(Error::syntax(
            "UnexpectedSyntax: size() cannot be applied to a pattern predicate",
        )),
        Some(Expr::Variable(v)) if env.get_name(v) == Some(&Binding::Path) => Err(Error::syntax(
            "InvalidArgumentType: size() cannot be applied to a path",
        )),
        _ => Ok(()),
    }
}

/// A temporal constructor over a fully-static one-argument string form is
/// constructed at compile time: a literal that cannot construct is a
/// front-end rejection, not a row-time one (live Neo4j: `date('2022-1')` is a
/// SyntaxError). The construction's own argument-value class — an out-of-range
/// zone offset, a named zone on a date-less time — is live's ArgumentError and
/// is kept; every other failure (the parse/type faults) is the compile-time
/// SyntaxError class. The field-map form and the CIP-228 `(value, pattern)`
/// form keep their runtime classes (their corpus contract pins run-time
/// invalidity).
fn check_static_temporal_construction(name: &str, args: &[Expr]) -> Result<()> {
    let [arg] = args else {
        return Ok(());
    };
    let Some(value) = static_arg_value(arg) else {
        return Ok(());
    };
    if !matches!(value, Value::String(_)) {
        return Ok(());
    }
    match relate_core::temporal::construct_named(name, std::slice::from_ref(&value)) {
        Ok(_) => Ok(()),
        Err(e @ (Error::ArgumentError(_) | Error::Syntax(_))) => Err(e),
        // The class moves to Syntax; the typed detail must not carry the old
        // class prefix into the new message.
        Err(e) => Err(e.into_syntax()),
    }
}

/// 42I52: Cypher 25 removed the property-existence function form
/// `exists(<property>)` (also the dynamic `exists(n['prop'])`);
/// `exists { <subquery> }`, `exists(<pattern>)`, and `property_exists(...)`
/// stay. Live-verified against neo4j:2026.09-enterprise.
fn reject_removed_exists_property_form(args: &[Expr]) -> Result<()> {
    match args {
        [Expr::Property { .. } | Expr::DynamicProperty { .. } | Expr::Subscript { .. }] => {
            Err(Error::syntax(
                "InvalidPropertyExistence: the property existence syntax `exists(variable.property)` is no longer supported. Please use `variable.property IS NOT NULL` or `property_exists(...)` instead",
            ))
        }
        _ => Ok(()),
    }
}

/// 42I16: a statically-known `point()` argument is checked at compile
/// time, not as a runtime argument-value error. Two shapes are knowable
/// here: a *literal map* must carry a coordinate family (Neo4j rejects
/// `point({xxx: 1, yyy: 2})` as `InvalidPoint`), and a non-map *literal*
/// (`point(5)`, `point('foo')`) fails the signature check — live
/// neo4j:2026.09 reports every non-map spelling, literal or parameter,
/// as `Neo.ClientError.Statement.SyntaxError`; the parameter and
/// variable spellings stay with the executor's runtime backstop
/// (`spatial::point_construct`), which carries the same class.
/// `point(null)` is legal (null), and an entity argument is a `Variable`,
/// not knowable here.
fn reject_invalid_point_literals(args: &[Expr]) -> Result<()> {
    let [arg] = args else {
        return Ok(());
    };
    match arg {
        Expr::Map(pairs) => {
            let has = |key: &str| pairs.iter().any(|(k, _)| k.eq_ignore_ascii_case(key));
            if (has("x") && has("y")) || (has("latitude") && has("longitude")) {
                Ok(())
            } else {
                Err(Error::syntax(
                    "InvalidPoint: the map is not a valid POINT. Use either Cartesian ('x' and 'y') or geographic ('latitude' and 'longitude') coordinates",
                ))
            }
        }
        // The detail text is shared with the executor's runtime backstop
        // (`spatial::point_construct`); the two spellings are kept equal by
        // the class-pin tests. A shared core constant is the follow-up
        // (LABS-1102).
        Expr::Bool(_) | Expr::Int(_) | Expr::Float(_) | Expr::String(_) | Expr::List(_) => Err(
            Error::syntax("InvalidArgumentType: point() requires a map, node, or relationship"),
        ),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relate_names::SmallName;

    /// The table is the whole rule vocabulary: one row per name, sorted so a
    /// reader can find a name and a duplicate cannot hide.
    #[test]
    fn function_meta_rows_are_sorted_and_unique() {
        let names: Vec<&str> = FUNCTION_META.iter().map(|meta| meta.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted, "FUNCTION_META must be sorted and unique");
    }

    /// The kind gate and the arity contract still fire from the table, and
    /// an unrelated name is unaffected.
    #[test]
    fn function_meta_applies_the_row_it_names() {
        let mut env = Env::new();
        env.insert_name(SmallName::from_name("n"), Binding::Node);

        let type_call = Expr::Function {
            name: SmallName::from_name("type"),
            distinct: false,
            args: vec![Expr::Variable(SmallName::from_name("n"))],
        };
        let Expr::Function { args, .. } = &type_call else {
            unreachable!()
        };
        let err = apply_function_meta(&env, "type", args).unwrap_err();
        assert!(
            err.to_string().contains("type() needs a relationship"),
            "{err}"
        );

        let timestamp = Expr::Function {
            name: SmallName::from_name("timestamp"),
            distinct: false,
            args: vec![Expr::Int(1)],
        };
        let Expr::Function { args, .. } = &timestamp else {
            unreachable!()
        };
        let err = apply_function_meta(&env, "timestamp", args).unwrap_err();
        assert!(
            err.to_string().contains("InvalidNumberOfArguments"),
            "{err}"
        );

        assert!(apply_function_meta(&env, "toString", args).is_ok());
    }
}
