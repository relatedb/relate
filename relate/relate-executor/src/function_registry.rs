//! Canonical built-in function registry: the one table of the functions
//! `SHOW FUNCTIONS` describes and the typed deprecation lookup the query
//! notifications read (ADR-0363 decision 5).
//!
//! Mirrors [`crate::procedure_registry`]: the table and its typed lookups
//! live here, while `show_catalog` only renders the rows. A catalog entry is
//! deprecated exactly when its `deprecated_by` option is `Some` (01N01), and
//! it is aggregate exactly when its own category is `Aggregating`, so there
//! is no second flag to keep aligned either way.
//!
//! Every advertised function owns at least one row here — there is no
//! placeholder entry — so a `SHOW FUNCTIONS` row always renders a real
//! description, its registry category and a CIP-113 signature over typed
//! parameters (ADR-0497 §8). The inventory is split by family into sibling
//! modules so no one file carries the whole table; [`FUNCTION_META_TABLES`]
//! is the one place that knows the parts.

use crate::catalog_meta::{DeprecatedBy, ProcArgMeta};

mod aggregate;
mod graph;
mod list;
mod numeric;
mod predicate;
mod scalar;
mod spatial;
mod string;
mod temporal;
mod trig;
mod vector;

/// The registry's classification vocabulary (CIP-84): the corpus catalogue's
/// closed fifteen-name category set, compiler-checked instead of a string
/// literal. `as_str()` is the cell the catalogue renders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FunctionCategory {
    Aggregating,
    Database,
    Graph,
    List,
    Logarithmic,
    Numeric,
    Path,
    Predicate,
    Scalar,
    Spatial,
    String,
    Temporal,
    Trigonometric,
    Uuid,
    Vector,
}

impl FunctionCategory {
    /// The category cell `SHOW FUNCTIONS` renders (the corpus spelling).
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            FunctionCategory::Aggregating => "Aggregating",
            FunctionCategory::Database => "Database",
            FunctionCategory::Graph => "Graph",
            FunctionCategory::List => "List",
            FunctionCategory::Logarithmic => "Logarithmic",
            FunctionCategory::Numeric => "Numeric",
            FunctionCategory::Path => "Path",
            FunctionCategory::Predicate => "Predicate",
            FunctionCategory::Scalar => "Scalar",
            FunctionCategory::Spatial => "Spatial",
            FunctionCategory::String => "String",
            FunctionCategory::Temporal => "Temporal",
            FunctionCategory::Trigonometric => "Trigonometric",
            FunctionCategory::Uuid => "UUID",
            FunctionCategory::Vector => "Vector",
        }
    }
}

/// One catalog row's metadata for a built-in function: its display-spelling
/// name, the category CIP-84 classifies it under, its description and its
/// typed parameters (the signature is derived from `args`/`return_type` by
/// [`render_function_signature`](crate::show_catalog), except for
/// `special_signature`).
///
/// The row is deprecated exactly when `deprecated_by` is `Some` (CIP-157), so
/// the flag and its replacement cannot disagree.
#[derive(Clone, Copy)]
pub(crate) struct FunctionMeta {
    /// The row's **display spelling** as the reference catalogue (Neo4j
    /// 2026.10.1) prints it (ADR-0497 §11): the one name a row owns — the
    /// renderer reads it for the catalogue's `name` cell and the signature —
    /// matched case-insensitively by [`function_metas`].
    pub(crate) name: &'static str,
    /// The registry's classification (CIP-84).
    pub(crate) category: FunctionCategory,
    pub(crate) description: &'static str,
    /// The signature **tail** for a row whose parameters are not an ordinary
    /// parameter list — the text after the name, starting at `(` — for the
    /// list-comprehension-styled quantifiers (`all`/`any`/`none`/`single`),
    /// `reduce` and `allReduce`, whose grammar CIP-113 keeps as a special
    /// case. The renderer prefixes the row's own [`name`](Self::name), so a
    /// tail can never spell a second name.
    pub(crate) special_signature: Option<&'static str>,
    /// The formal parameters, in signature order. Entries carrying a
    /// `default` keep the explicit struct literal so the default stays
    /// visible at its entry.
    pub(crate) args: &'static [ProcArgMeta],
    pub(crate) return_type: &'static str,
    /// The replacement when the function is deprecated (CIP-157's
    /// `deprecatedBy`); `None` means not deprecated.
    pub(crate) deprecated_by: Option<DeprecatedBy>,
}

impl FunctionMeta {
    /// Compact entry for the common case: an ordinary parameter list, no
    /// deprecation.
    pub(crate) const fn described(
        name: &'static str,
        category: FunctionCategory,
        description: &'static str,
        args: &'static [ProcArgMeta],
        return_type: &'static str,
    ) -> Self {
        Self {
            name,
            category,
            description,
            special_signature: None,
            args,
            return_type,
            deprecated_by: None,
        }
    }

    /// Whether the row is an aggregate: its own CIP-84 category *is* the
    /// `aggregating` catalog column, so a row cannot report one without the
    /// other.
    pub(crate) fn is_aggregating(&self) -> bool {
        matches!(self.category, FunctionCategory::Aggregating)
    }
}

/// Every family's table, in scan order. [`function_metas`] reads this one
/// list, so adding a module means adding its `ENTRIES` here — nothing else
/// names the inventory.
///
/// Each row owns its **display spelling** as its `name` field, spelled as the
/// reference catalogue (Neo4j 2026.10.1) prints it (ADR-0497 §11): the 42
/// default mixed-case names (`allReduce`, `toBooleanList`,
/// `point.withinBBox`, …) plus the three gated `coll.has*` rows carry their
/// reference casing, every other name is the lowercase spelling. The name is
/// the one name a row owns — the renderer reads it for the catalogue's `name`
/// cell and the signature — and matching is case-insensitive, so the casing
/// is a rendering fact, never an admission rule. The set of mixed-case names
/// is pinned by
/// `show_catalog::the_catalog_renders_the_reference_display_spellings`.
const FUNCTION_META_TABLES: &[&[FunctionMeta]] = &[
    scalar::ENTRIES,
    list::ENTRIES,
    numeric::ENTRIES,
    trig::ENTRIES,
    string::ENTRIES,
    temporal::instant::ENTRIES,
    temporal::truncate::ENTRIES,
    temporal::gql::ENTRIES,
    temporal::duration::ENTRIES,
    temporal::epoch::ENTRIES,
    temporal::format::ENTRIES,
    aggregate::ENTRIES,
    predicate::ENTRIES,
    vector::ENTRIES,
    spatial::ENTRIES,
    graph::ENTRIES,
];

/// Every registry row, in table order: the one iteration of the inventory,
/// shared by [`function_metas`] and the tests that walk the whole table.
fn all_function_metas() -> impl Iterator<Item = &'static FunctionMeta> {
    FUNCTION_META_TABLES.iter().flat_map(|table| table.iter())
}

/// The registry rows a function name produces: exactly one for every name, or
/// several when the name has arity overloads, which CIP-113 keeps as separate
/// signature rows (`range`, `normalize`, `round`, `btrim`, `format`,
/// `coll.flatten`). Function calls match case-insensitively (CIP-230), so the
/// lookup compares the call name against each row's display spelling
/// case-insensitively, like the validator does.
pub(crate) fn function_metas(name: &str) -> impl Iterator<Item = &'static FunctionMeta> + use<'_> {
    all_function_metas().filter(move |meta| meta.name.eq_ignore_ascii_case(name))
}

/// CIP-230 reserved-namespace `graph.*` functions (`graph.names`,
/// `graph.byName`, `graph.byElementId`, `graph.propertiesByName`): real
/// Neo4j built-ins that enumerate/target constituent graphs of a *composite*
/// (Fabric) database. A single-graph embedded engine has no composite
/// database concept to execute these against (see MANIFEST.md CIP-61/107/
/// 147/193/212), so they're not in `validate::KNOWN_FUNCTIONS` and can't be
/// called -- but CIP-230's reserved-namespace list still expects them to be
/// *discoverable* via `SHOW FUNCTIONS` (01:[1]), so they get catalog-only
/// entries here, the same "listed but not executable" treatment the
/// `procedure_registry` table gives several procedures.
///
/// Derived from the core selector table ([`relate_core::GRAPH_SELECTOR_FUNCTION_NAMES`]):
/// the catalog-only list is exactly the selector names, in the table's
/// order, so a selector added (or removed) there reaches `SHOW FUNCTIONS`
/// with no second list to update. The arity lockstep test below pins each
/// name's catalog entry to the same table.
pub(crate) const CATALOG_ONLY_FUNCTIONS: &[&str] = &relate_core::GRAPH_SELECTOR_FUNCTION_NAMES;

/// The replacement when `name` resolves to a deprecated built-in function,
/// `None` otherwise. One typed lookup over the same table `SHOW FUNCTIONS`
/// renders, so no caller keeps a second list of deprecated names. A name's
/// arity rows share one `deprecated_by` (the lockstep test below), so the
/// first row that carries a replacement speaks for every sibling.
pub fn builtin_function_deprecation(name: &str) -> Option<DeprecatedBy> {
    function_metas(name).find_map(|meta| meta.deprecated_by)
}

#[cfg(test)]
mod tests {
    use crate::catalog_meta::DeprecationForm;

    use super::*;

    /// The `graph.*` selector arity table in `relate-core` and the
    /// catalog's own argument lists are one fact: a fifth selector added to
    /// either without the other fails here.
    #[test]
    fn graph_selector_arities_match_the_catalog() {
        for (name, arity) in relate_core::GRAPH_SELECTOR_FUNCTIONS {
            let metas: Vec<&FunctionMeta> = function_metas(name).collect();
            assert_eq!(metas.len(), 1, "{name} has no catalog entry");
            assert_eq!(metas[0].args.len(), arity, "{name}");
            assert!(CATALOG_ONLY_FUNCTIONS.contains(&name), "{name}");
        }
    }

    /// The lookup reads the one function catalog: `id` is deprecated with
    /// its `elementId` replacement (while its catalog display keeps the
    /// CIP-157 prose sentence), `elementId` itself is not, and an unknown
    /// name is not.
    #[test]
    fn function_deprecation_lookup_reads_the_function_catalog() {
        let replacement = builtin_function_deprecation("ID").expect("id is deprecated");
        assert_eq!(
            replacement.display(),
            "elementId or consider using an application-generated id"
        );
        assert_eq!(replacement.form(), DeprecationForm::Identifier("elementId"));
        assert!(builtin_function_deprecation("elementId").is_none());
        assert!(builtin_function_deprecation("no.such.function").is_none());
    }

    /// The table covers exactly the functions that can ever be listed — every
    /// name `SHOW FUNCTIONS` can iterate (with every preview gate enabled) has
    /// metadata, and the table invents no name the registry does not admit —
    /// so a catalogue row can never fall back to a placeholder. Rows are
    /// named by their display spellings (ADR-0497 §11), so each lowercases
    /// into the advertised set: a name that names another function — or a
    /// typo'd one — fails here.
    #[test]
    fn the_metadata_tables_cover_exactly_the_advertised_registry() {
        let every_gate = relate_front::validate::every_name_gate();
        let mut advertised: Vec<String> =
            crate::show_catalog::advertised_function_names(every_gate)
                .map(str::to_string)
                .collect();
        advertised.sort_unstable();
        let mut catalogued: Vec<String> = all_function_metas()
            .map(|meta| meta.name.to_ascii_lowercase())
            .collect();
        catalogued.sort_unstable();
        catalogued.dedup();
        assert_eq!(catalogued, advertised);
    }

    /// Arity sibling rows share one deprecation: every row of a name carries
    /// the same `deprecated_by`, so the per-name deprecation lookup
    /// ([`builtin_function_deprecation`]) cannot silently pick one sibling's
    /// replacement over another's.
    #[test]
    fn arity_sibling_rows_share_one_deprecation() {
        let mut by_name: Vec<(String, Option<DeprecatedBy>)> = all_function_metas()
            .map(|meta| (meta.name.to_ascii_lowercase(), meta.deprecated_by))
            .collect();
        by_name.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        for pair in by_name.windows(2) {
            if pair[0].0 == pair[1].0 {
                assert_eq!(
                    pair[0].1, pair[1].1,
                    "{}'s arity rows disagree on their replacement",
                    pair[0].0
                );
            }
        }
    }

    /// The four recased aggregate names and the front's alias table are one
    /// fact in two layers (ADR-0497 §11, CIP-128): `FUNCTION_ALIASES`'s
    /// aggregate-class display — the spelling the lowering emits into IR — must equal
    /// the catalogue name the registry renders, or `RETURN percentile_cont(x)`
    /// and `SHOW FUNCTIONS` would print two different spellings for one
    /// function.
    #[test]
    fn aggregate_alias_display_columns_match_the_registry_keys() {
        for a in relate_front::agg::aggregate_aliases() {
            let (alias, canonical, display) = (a.spelling, a.canonical, a.display);
            let metas: Vec<&FunctionMeta> = function_metas(canonical).collect();
            assert_eq!(metas.len(), 1, "{alias} -> {canonical} has no unique row");
            assert_eq!(
                metas[0].name, display,
                "{canonical}'s catalogue name disagrees with the alias table's display column"
            );
        }
    }
}
