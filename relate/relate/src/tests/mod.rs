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
mod txn;

/// Codec expectations in these suites are keyed on this crate's own
/// `parquet` feature, so a codec leaking into a lean build fails here.
#[test]
fn parquet_codec_is_compiled_exactly_with_the_parquet_feature() {
    assert_eq!(
        crate::SnapshotCodecKind::Parquet.is_compiled(),
        cfg!(feature = "parquet")
    );
}
