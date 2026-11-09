//! SHOW FUNCTIONS / SHOW PROCEDURES static catalogs (CIP discoverability).
use crate::catalog_meta::{DeprecatedBy, ProcArgMeta};
use crate::function_registry::{function_metas, FunctionMeta, CATALOG_ONLY_FUNCTIONS};
use crate::procedure_registry::BUILTIN_PROCEDURE_SIGNATURES;
use indexmap::IndexMap;
use relate_core::PreviewFeatures;
use relate_front::KNOWN_FUNCTIONS;
use relate_model::CypherValue;

type Row = IndexMap<String, CypherValue>;

/// The `rolesExecution`/`rolesBoostedExecution` cells are server RBAC facts:
/// the single-user embedded engine copies live Neo4j 2026.10.1's constant
/// answer. Every built-in function is executable by PUBLIC plus the five
/// roles, with none boosted; a procedure is admin-only exactly when its
/// `admin` flag says so, and its boosted set is always `admin`.
const FUNCTION_ROLES_EXECUTION: &[&str] = &[
    "PUBLIC",
    "admin",
    "architect",
    "editor",
    "publisher",
    "reader",
];
const PROCEDURE_ROLES_EXECUTION: &[&str] = &["PUBLIC", "admin"];
const ADMIN_ROLES_EXECUTION: &[&str] = &["admin"];
const PROCEDURE_ROLES_BOOSTED: &[&str] = &["admin"];

/// One role column's list cell, through the one shared
/// [`string_list`](crate::catalog_meta::string_list) spelling.
fn role_list(roles: &[&str]) -> CypherValue {
    crate::catalog_meta::string_list(roles.iter().map(|r| r.to_string()))
}

/// One entry of a function's/procedure's `argumentDescription`/
/// `returnDescription` map column (CIP-84): `name`/`description`/`type`/
/// `isDeprecated` always present, `default` only when the parameter
/// actually has a default value (CIP-84 show-functions-deprecation.feature
/// [11]/show-procedures-deprecation.feature [10]/[12]). The entries live in
/// the canonical `catalog_meta` shape both registries share.
fn arg_meta_value(a: &ProcArgMeta) -> CypherValue {
    let mut m = IndexMap::new();
    m.insert("name".to_string(), CypherValue::Str(a.name.to_string()));
    m.insert(
        "description".to_string(),
        CypherValue::Str(a.description.to_string()),
    );
    m.insert("type".to_string(), CypherValue::Str(a.type_str.to_string()));
    m.insert("isDeprecated".to_string(), CypherValue::Bool(a.deprecated));
    if let Some(d) = a.default {
        m.insert("default".to_string(), CypherValue::Str(d.to_string()));
    }
    CypherValue::Map(m)
}

/// Renders a comma-separated `reqParamList, optParamList` (CIP-113's
/// abstract syntax: every parameter as `name :: type` or, when it carries a
/// default, `name = default :: type`). Required parameters are expected to
/// be listed before optional ones in `args` itself -- callers build `args`
/// in that order, so this function doesn't need to reorder anything (CIP-113
/// procedure-signatures.feature [15]).
fn render_param_list(args: &[ProcArgMeta]) -> String {
    args.iter()
        .map(|a| match a.default {
            Some(d) => format!("{} = {} :: {}", a.name, d, a.type_str),
            None => format!("{} :: {}", a.name, a.type_str),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Renders a CIP-113 function signature: `name(paramList) :: returnType`,
/// with no parentheses around the (possibly closed-union) return type and
/// an empty `()` when `args` is empty.
fn render_function_signature(name: &str, args: &[ProcArgMeta], return_type: &str) -> String {
    format!("{}({}) :: {}", name, render_param_list(args), return_type)
}

/// Renders a CIP-113 procedure signature: `name(paramList) :: (ret :: type,
/// ...)`, with the return-type suffix dropped entirely (not `::VOID`,
/// `::()`, or `::NOTHING` -- all three were considered and rejected per the
/// CIP's Revision History) when the procedure yields no columns.
fn render_procedure_signature(name: &str, args: &[ProcArgMeta], returns: &[ProcArgMeta]) -> String {
    let params = render_param_list(args);
    if returns.is_empty() {
        format!("{name}({params})")
    } else {
        let rets = returns
            .iter()
            .map(|r| format!("{} :: {}", r.name, r.type_str))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{name}({params}) :: ({rets})")
    }
}

/// Every advertised function name, in catalog order, filtered by the preview
/// set: the one derivation `function_catalog_rows` renders and the
/// registry-coverage test compares its tables against, so the name sources
/// and the listing predicate are spelled once.
pub(crate) fn advertised_function_names(
    preview: PreviewFeatures,
) -> impl Iterator<Item = &'static str> {
    KNOWN_FUNCTIONS
        .iter()
        .copied()
        .chain(relate_core::temporal::clock_spellings())
        .chain(CATALOG_ONLY_FUNCTIONS.iter().copied())
        .filter(move |&raw| relate_front::validate::function_passes_name_gate(raw, preview))
}

/// Build the function catalog as virtual rows, one per registry row in the
/// known-function registry (`validate::KNOWN_FUNCTIONS`) that also backs
/// function-call validation — so every callable built-in is discoverable
/// and the catalog can't drift out of sync with what actually works. The
/// clock-access spellings are generated from their own vocabulary (they are
/// not registry entries) and the catalog-only `graph.*` names
/// (`CATALOG_ONLY_FUNCTIONS`, CIP-230) ride beside them through the same
/// renderer. A name with arity overloads (`range`, `round`, `coll.flatten`,
/// …) contributes one row per signature (CIP-113).
///
/// `preview` is the store-level preview-feature set (ADR-0371, ADR-0497): a
/// gated name is omitted unless its feature is enabled, so discovery never
/// advertises a name the admission gate would refuse. The predicate is the
/// front's [`function_passes_name_gate`], the listing side of the same table
/// `validate_function_admission` reads.
pub fn function_catalog_rows(preview: PreviewFeatures) -> Vec<Row> {
    let mut metas: Vec<&'static FunctionMeta> = advertised_function_names(preview)
        .flat_map(|raw| {
            let metas: Vec<&'static FunctionMeta> = function_metas(raw).collect();
            // Every advertised name owns at least one registry row; the
            // pairing is pinned by
            // `the_metadata_tables_cover_exactly_the_advertised_registry`.
            assert!(
                !metas.is_empty(),
                "no function_registry row for advertised `{raw}`"
            );
            metas
        })
        .collect();
    // Live Neo4j reports the catalog in the rendered name's byte order
    // (`abs`, `acos`, … `sqrt`, `stDev`, `stDevP`, `startNode`…; 2026.10.1).
    // `FunctionMeta::name` IS the rendered spelling (ADR-0497 §11) — the
    // renderer reads it for the name cell — so sorting the metas sorts the
    // rows; a stable sort keeps an overload name's registry rows in their
    // own order (`coll.flatten`'s depth row first).
    metas.sort_by(|a, b| a.name.cmp(b.name));
    metas.into_iter().map(function_row).collect()
}

/// The CIP-157 `deprecatedBy` cell — the deprecation target's display text,
/// else NULL: the one spelling both catalog renderers read.
fn deprecated_by_cell(deprecated_by: Option<DeprecatedBy>) -> CypherValue {
    deprecated_by
        .map(|deprecated| CypherValue::Str(deprecated.display().to_string()))
        .unwrap_or(CypherValue::Null)
}

/// One function's catalog row, rendered from its [`FunctionMeta`]: the
/// display-spelling name (ADR-0497 §11), the category, the `aggregating`
/// cell, description, CIP-113 signature, argument descriptions and
/// deprecation cells all come from the registry entry, so the renderer never
/// re-derives a name or a classification.
fn function_row(meta: &FunctionMeta) -> Row {
    let mut r = Row::new();
    r.insert("name".to_string(), CypherValue::Str(meta.name.to_string()));
    r.insert(
        "category".to_string(),
        CypherValue::Str(meta.category.as_str().to_string()),
    );
    r.insert("isBuiltIn".to_string(), CypherValue::Bool(true));
    r.insert(
        "aggregating".to_string(),
        CypherValue::Bool(meta.is_aggregating()),
    );
    r.insert(
        "description".to_string(),
        CypherValue::Str(meta.description.to_string()),
    );
    let signature = meta
        .special_signature
        .map(|tail| format!("{}{}", meta.name, tail))
        .unwrap_or_else(|| render_function_signature(meta.name, meta.args, meta.return_type));
    r.insert("signature".to_string(), CypherValue::Str(signature));
    r.insert(
        "argumentDescription".to_string(),
        CypherValue::List(meta.args.iter().map(arg_meta_value).collect()),
    );
    r.insert(
        "returnDescription".to_string(),
        CypherValue::Str(meta.return_type.to_string()),
    );
    r.insert(
        "rolesExecution".to_string(),
        role_list(FUNCTION_ROLES_EXECUTION),
    );
    r.insert("rolesBoostedExecution".to_string(), role_list(&[]));
    r.insert(
        "isDeprecated".to_string(),
        CypherValue::Bool(meta.deprecated_by.is_some()),
    );
    r.insert(
        "deprecatedBy".to_string(),
        deprecated_by_cell(meta.deprecated_by),
    );
    r
}

/// `SHOW PROCEDURES` catalog rows, rendered from the canonical
/// `procedure_registry` table (the same table the executor's `CALL`
/// `SHOW PROCEDURES` catalog rows, rendered from the canonical
/// `procedure_registry` table (the same table the executor's `CALL`
/// dispatch and `Database::validate_calls` bind against), in name order
/// (live Neo4j's own ordering, sorted over the registry). The
/// `catalog-only` entries (`executable: false`) are inventory rows the
/// corpus lists but never calls (`db.resampleIndex`, `dbms.setConfigValue`,
/// the `cdc.*` family, the `dbms.cluster.*` and `fleetManagement.*`
/// families, …): `SHOW PROCEDURES` reports them and a `CALL` refuses them.
///
/// `preview` is the store-level preview-feature set (ADR-0371, ADR-0378):
/// a signature whose gating feature is disabled is omitted, so discovery
/// never advertises a procedure the `CALL` gate would refuse. The mapping
/// is the executor's one [`procedure_preview_feature`](crate::procedure_preview_feature),
/// and with the gate enabled every row is identical to the ungated table.
pub fn procedure_catalog_rows(preview: PreviewFeatures) -> Vec<Row> {
    // Neo4j reports the catalog in name order (`db.cdc.current` before
    // `db.cdc.earliest` …, and `tx.getMetaData` before `tx.setMetaData`),
    // not in registration order.
    let mut listed: Vec<&'static crate::procedure_registry::ProcSignature> =
        BUILTIN_PROCEDURE_SIGNATURES
            .iter()
            .filter(|signature| signature.listed)
            .filter(|signature| {
                crate::procedure_gate::procedure_preview_feature(signature.name)
                    .is_none_or(|feature| preview.enabled(feature))
            })
            .collect();
    listed.sort_unstable_by(|a, b| a.name.cmp(b.name));
    listed.into_iter().map(procedure_row).collect()
}

/// One `SHOW PROCEDURES` row — the procedure renderer twin of
/// [`function_row`]: every cell comes from the registry signature, so the
/// renderer never re-derives a name or a flag.
fn procedure_row(signature: &crate::procedure_registry::ProcSignature) -> Row {
    let mut r = Row::new();
    r.insert(
        "name".to_string(),
        CypherValue::Str(signature.name.to_string()),
    );
    r.insert(
        "description".into(),
        CypherValue::Str(signature.description.to_string()),
    );
    r.insert("mode".into(), CypherValue::Str(signature.mode.to_string()));
    r.insert(
        "worksOnSystem".into(),
        CypherValue::Bool(signature.works_on_system),
    );
    r.insert("admin".into(), CypherValue::Bool(signature.admin));
    r.insert(
        "signature".into(),
        CypherValue::Str(render_procedure_signature(
            signature.name,
            signature.args,
            signature.returns,
        )),
    );
    r.insert(
        "argumentDescription".into(),
        CypherValue::List(signature.args.iter().map(arg_meta_value).collect()),
    );
    r.insert(
        "returnDescription".into(),
        CypherValue::List(signature.returns.iter().map(arg_meta_value).collect()),
    );
    r.insert(
        "rolesExecution".into(),
        role_list(if signature.admin {
            ADMIN_ROLES_EXECUTION
        } else {
            PROCEDURE_ROLES_EXECUTION
        }),
    );
    r.insert(
        "rolesBoostedExecution".into(),
        role_list(PROCEDURE_ROLES_BOOSTED),
    );
    let deprecated = signature.deprecated_by.is_some();
    r.insert("isDeprecated".into(), CypherValue::Bool(deprecated));
    let mut option = IndexMap::new();
    option.insert("deprecated".into(), CypherValue::Bool(deprecated));
    r.insert("option".into(), CypherValue::Map(option));
    r.insert(
        "deprecatedBy".into(),
        deprecated_by_cell(signature.deprecated_by),
    );
    r
}

#[cfg(test)]
mod tests;
