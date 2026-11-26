//! End-to-end behavior suites through the public API (`Database` /
//! `Connection`), split by concern. Machinery-internal tests live next to
//! their modules (`binding`, `commit`, `checkpoint`, `procedures`).

mod adoption;
mod bookmarks;
mod catalog;
mod ephemeral;
mod follow;
mod key_statistics_counts;
mod label_stats;
mod memory;
mod namespace;
#[cfg(feature = "parquet")]
mod open_peak;
mod persist;
mod persisted_sentinel;
mod plan_cache;
mod plan_cache_stats_epoch;
mod point;
mod preview;
mod processing_limits;
mod queries;
mod read_admission;
mod resize;
mod shortest_path;
mod stack;
mod support;
mod token_changes;
mod txn;

/// The store's id for an interned name — the one spelling the test suites
/// share. Callers resolve a name only after the statement that interns it has
/// committed, so a miss is a fixture bug, never a real absence. The resolve
/// runs under the publication read guard (`GraphCell`'s contract, ADR-0137 —
/// a `--follow` reload may replace the engine mid-read), the same guarded
/// reach [`crate::Shared::commit_ts`] takes.
pub(crate) fn token_id(shared: &crate::Shared, name: &str) -> u32 {
    use relate_names::TokenResolver;

    let _read = shared
        .read_guard()
        .expect("a fixture's store is healthy and its publication unlocked");
    match shared.graph.resolve(name) {
        relate_names::Token::Id(id) => id.0,
        other => panic!("`{name}` must be interned by now, resolved as {other:?}"),
    }
}

/// Codec expectations in these suites are keyed on this crate's own
/// `parquet` feature, so a codec leaking into a lean build fails here.
#[test]
fn parquet_codec_is_compiled_exactly_with_the_parquet_feature() {
    assert_eq!(
        crate::SnapshotCodecKind::Parquet.is_compiled(),
        cfg!(feature = "parquet")
    );
}
