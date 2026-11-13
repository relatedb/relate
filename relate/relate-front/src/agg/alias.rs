//! The one function-alias table (`FUNCTION_ALIASES`) and its lookups: the
//! alternate lowercase spellings that resolve to canonical names, split out
//! of the aggregation helpers so the table has its own module.

use relate_ir::AGGREGATES;

/// One accepted alternate lowercase spelling, the canonical lowercase name
/// it resolves to, and the spelling the lowering emits into IR: for an
/// aggregate the display spelling (so an unaliased `RETURN
/// percentile_cont(x)` keeps the Cypher-canonical column name
/// `percentileCont(x)`), for a scalar the canonical name itself.
#[derive(Clone, Copy, Debug)]
pub struct FunctionAlias {
    pub spelling: &'static str,
    pub canonical: &'static str,
    pub display: &'static str,
}

const fn aggregate(
    spelling: &'static str,
    canonical: &'static str,
    display: &'static str,
) -> FunctionAlias {
    FunctionAlias {
        spelling,
        canonical,
        display,
    }
}

const fn scalar(spelling: &'static str, canonical: &'static str) -> FunctionAlias {
    FunctionAlias {
        spelling,
        canonical,
        display: canonical,
    }
}

/// The one function-alias table: CIP-128's snake_case aggregate spellings
/// and CIP-245's snake_case GQL/SQL scalar spellings, scalar rows target names outside `AGGREGATES`.
/// (`toString` rendering is not a spelling policy: the executor's value
/// layer renders temporals at full precision by runtime type, ADR-0448.)
pub(super) const FUNCTION_ALIASES: &[FunctionAlias] = &[
    aggregate("collect_list", "collect", "collect"),
    aggregate("stdev_samp", "stdev", "stDev"),
    aggregate("stddev_samp", "stdev", "stDev"),
    aggregate("stdev_pop", "stdevp", "stDevP"),
    aggregate("stddev_pop", "stdevp", "stDevP"),
    aggregate("percentile_cont", "percentilecont", "percentileCont"),
    aggregate("percentile_disc", "percentiledisc", "percentileDisc"),
    scalar("path_length", "length"),
    scalar("duration_between", "duration.between"),
    scalar("local_time", "localtime"),
    scalar("local_datetime", "localdatetime"),
    scalar("zoned_time", "time"),
    scalar("zoned_datetime", "datetime"),
];

/// The alias row for a lowercase spelling: the one lookup every
/// alias-resolving helper below shares.
fn alias_row(spelling: &str) -> Option<&'static FunctionAlias> {
    FUNCTION_ALIASES.iter().find(|a| a.spelling == spelling)
}

/// The aggregate rows of the alias table: those whose canonical name is a
/// member of the canonical `AGGREGATES` set (the one source of truth).
pub fn aggregate_aliases() -> impl Iterator<Item = &'static FunctionAlias> {
    FUNCTION_ALIASES
        .iter()
        .filter(|a| AGGREGATES.contains(&a.canonical))
}

/// The canonical lowercase aggregate name for an accepted lowercase
/// spelling, or the input unchanged when it is not an aggregate alias.
pub fn canonical_agg_name(name: &str) -> &str {
    aggregate_aliases()
        .find(|a| a.spelling == name)
        .map_or(name, |a| a.canonical)
}

/// The IR name a lowercase alias spelling lowers to, `None` when `name` is
/// not an alias.
pub(crate) fn lowered_alias_name(name: &str) -> Option<&'static str> {
    alias_row(name).map(|a| a.display)
}
