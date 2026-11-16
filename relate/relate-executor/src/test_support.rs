//! The forest-neutral test helpers: `ints`, `map`, `config_with_floor` and
//! `PeakPoolDouble`, shared across module test trees without importing
//! another forest's harness. The `exec` tests re-export these from
//! `exec::test_support`; the write-composing helpers live there instead, so
//! the dependency stays one-directional.

use std::collections::BTreeMap;
use std::sync::Arc;

use proptest::test_runner::Config as ProptestConfig;
use relate_core::tokens::TokenLedger;
use relate_core::Value;

/// A string-keyed map from `&str` pairs — the config/row builder the test
/// modules share (procedure rows, vector index configs), so no test tree
/// re-spells it.
pub(crate) fn map(entries: &[(&str, Value)]) -> BTreeMap<String, Value> {
    entries
        .iter()
        .map(|(key, value)| (key.to_string(), value.clone()))
        .collect()
}

/// A `Value::Map` from `&str` pairs — [`map`]'s value form, so no test
/// tree re-spells the wrap.
pub(crate) fn value_map(entries: &[(&str, Value)]) -> Value {
    Value::Map(map(entries).into())
}

/// A `Value::Int` -- the scalar shorthand the test modules share.
pub(crate) fn int(i: i64) -> Value {
    Value::Int(i)
}

/// A list of integers.
pub(crate) fn ints(xs: &[i64]) -> Value {
    Value::List(xs.iter().map(|&x| Value::Int(x)).collect::<Vec<_>>().into())
}

/// ADR-0268 §3 env scaling with a floor: a proptest's acceptance needs at
/// least `floor` generated cases; `PROPTEST_CASES` can still raise it,
/// never lower it below the contract's floor. The one spelling — every
/// generated-batch contract in the layer (the chunk round-trip, the batch
/// filter's zone differential, the scan-gate differential) and the row
/// stamping differential (eval) read it, never a second copy per module.
/// Forest-neutral like [`map`]/[`ints`]: `exec`'s test harness re-exports
/// it, so a proptest outside `exec` reads it here instead of reaching
/// into another forest's harness.
pub(crate) fn config_with_floor(floor: u32) -> ProptestConfig {
    let base = ProptestConfig::default();
    ProptestConfig::with_cases(base.cases.max(floor))
}

/// The workspace/overlay peak tests' counting pool: the workspace's one
/// shared fake, under this forest's historical name.
pub(crate) use relate_test_support::peak_pool::FixedPool as PeakPoolDouble;

/// A ledger with room for everything: accounting on, nothing refused.
pub(crate) fn roomy_ledger() -> TokenLedger {
    let pool = Arc::new(PeakPoolDouble::new(u64::MAX / 2));
    TokenLedger::new(u64::MAX / 2, 0, pool)
}

/// A ledger whose pool refuses any growth: the next positive delta fails.
pub(crate) fn refusing_ledger() -> TokenLedger {
    let pool = Arc::new(PeakPoolDouble::new(0));
    TokenLedger::new(0, 0, pool)
}

/// The layering guard of the charge-owning set modules: `source` (a module's
/// own text) reaches neither the exec tree nor the pattern walker. Scans the
/// code lines for the module names as path segments, so a grouped import
/// (`use crate::{exec::…}`) cannot evade a qualified-path match, and a
/// `crate::{` group at all is refused: these modules name each import in full.
pub(crate) fn assert_never_imports_exec_tree(file: &str, source: &str) {
    for line in source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
    {
        for forbidden in ["crate::exec", "exec::", "pattern_walk", "crate::{"] {
            assert!(
                !line.contains(forbidden),
                "{file} must not reference `{forbidden}` (the module stays importable by the path engine): {line}"
            );
        }
    }
}
