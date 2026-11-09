use super::report::with_version_context;
use super::*;
use relate_core::Error;

#[cfg(parquet_codec)]
#[test]
fn manifest_classification() {
    assert_eq!(manifest::classify(1, false), Classification::Legacy(1));
    assert_eq!(manifest::classify(2, false), Classification::Legacy(2));
    // The format follows the content (ADR-0120 §8): only the pair the
    // writer emits is current.
    assert_eq!(
        manifest::classify(manifest::CURRENT_VERSION, false),
        Classification::Current
    );
    assert_eq!(
        manifest::classify(manifest::ONTOLOGY_VERSION, true),
        Classification::Current
    );
    // Readable but content-mismatched: migrate-on-open normalizes to the
    // content's version.
    assert_eq!(
        manifest::classify(manifest::CURRENT_VERSION, true),
        Classification::Legacy(manifest::CURRENT_VERSION)
    );
    assert_eq!(
        manifest::classify(manifest::ONTOLOGY_VERSION, false),
        Classification::Legacy(manifest::ONTOLOGY_VERSION)
    );
    // Both write pairs are current (ADR-0534): legacy and wide, with and
    // without ontology.
    assert_eq!(manifest::classify(3, false), Classification::Current);
    assert_eq!(manifest::classify(4, true), Classification::Current);
    assert_eq!(manifest::classify(5, false), Classification::Current);
    assert_eq!(manifest::classify(6, true), Classification::Current);
    // Content-mismatched pairs are readable legacy: a wide manifest
    // selecting no ontology, an ontology-less wide one, and the crossed
    // pairs.
    assert_eq!(manifest::classify(5, true), Classification::Legacy(5));
    assert_eq!(manifest::classify(6, false), Classification::Legacy(6));
    assert_eq!(manifest::classify(7, false), Classification::Unreadable(7));
    assert_eq!(manifest::classify(7, true), Classification::Unreadable(7));
    assert_eq!(
        manifest::classify(99, false),
        Classification::Unreadable(99)
    );
}

#[test]
fn value_encoding_classification_normalizes_the_absent_marker_once() {
    // Absent means encoding 1 (ADR-0127 Decision 1): readable, legacy.
    assert_eq!(
        value_encoding::classify(None, false),
        Classification::Legacy(value_encoding::ABSENT)
    );
    assert_eq!(
        value_encoding::classify(Some(value_encoding::CURRENT_VERSION), false),
        Classification::Current
    );
    // The format follows the content (ADR-0120 §8): encoding 3 is current
    // only with the inline ontology, encoding 2 only without it.
    assert_eq!(
        value_encoding::classify(Some(value_encoding::ONTOLOGY_VERSION), true),
        Classification::Current
    );
    assert_eq!(
        value_encoding::classify(Some(value_encoding::ONTOLOGY_VERSION), false),
        Classification::Legacy(value_encoding::ONTOLOGY_VERSION)
    );
    assert_eq!(
        value_encoding::classify(Some(value_encoding::CURRENT_VERSION), true),
        Classification::Legacy(value_encoding::CURRENT_VERSION)
    );
    assert_eq!(
        value_encoding::classify(Some(4), false),
        Classification::Unreadable(4)
    );
    assert_eq!(
        value_encoding::classify(Some(4), true),
        Classification::Unreadable(4)
    );
}

#[test]
fn value_encoding_gate_returns_the_stores_actual_version_when_current() {
    // A Current-classified store comes back with the version its content
    // wrote, not the no-ontology default: encoding 3 stays 3 (ADR-0120 §8).
    let (version, marker) =
        value_encoding::gate(Some(value_encoding::ONTOLOGY_VERSION), true).unwrap();
    assert_eq!(version, value_encoding::ONTOLOGY_VERSION);
    assert_eq!(marker, SnapshotMarker::Current);

    let (version, marker) =
        value_encoding::gate(Some(value_encoding::CURRENT_VERSION), false).unwrap();
    assert_eq!(version, value_encoding::CURRENT_VERSION);
    assert_eq!(marker, SnapshotMarker::Current);
}

#[test]
fn wal_magic_classification() {
    assert_eq!(wal::classify(wal::CURRENT_MAGIC), Classification::Current);
    assert!(wal::is_readable(wal::CURRENT_MAGIC));
    // ADR-0120 §4: `MDBWAL02` (segments written while the store holds an
    // ontology) is the second readable magic.
    assert_eq!(
        wal::classify(wal::ONTOLOGY_MAGIC),
        Classification::Legacy(*wal::ONTOLOGY_MAGIC)
    );
    assert!(wal::is_readable(wal::ONTOLOGY_MAGIC));
    // `MDBWAL03` is the still-future magic that fails closed.
    assert_eq!(
        wal::classify(b"MDBWAL03"),
        Classification::Unreadable(*b"MDBWAL03")
    );
    assert!(!wal::is_readable(b"MDBWAL03"));
}

#[test]
fn bundle_classification_checks_magic_and_version_as_a_pair() {
    assert_eq!(
        bundle::classify(bundle::CURRENT_MAGIC, bundle::CURRENT_VERSION),
        Classification::Current
    );
    // A newer VERSION under the current magic is unreadable.
    assert_eq!(
        bundle::classify(bundle::CURRENT_MAGIC, 2),
        Classification::Unreadable((*bundle::CURRENT_MAGIC, 2))
    );
    // A foreign magic is unreadable even at a known VERSION.
    assert_eq!(
        bundle::classify(b"MDBCLB02", 1),
        Classification::Unreadable((*b"MDBCLB02", 1))
    );
}

#[test]
fn annotate_legacy_failure_names_the_version_only_for_legacy_markers() {
    let value_encoding = annotate_legacy_failure(
        Error::storage("failed to parse graph JSON: corrupt"),
        SnapshotMarker::Legacy(Subsystem::ValueEncoding, 1),
    );
    assert_eq!(
        value_encoding.to_string(),
        "storage error: failed to parse graph JSON: corrupt (store snapshot is at value_encoding 1)"
    );

    #[cfg(parquet_codec)]
    {
        let manifest = annotate_legacy_failure(
            Error::io("missing shard"),
            SnapshotMarker::Legacy(Subsystem::Manifest, 2),
        );
        assert_eq!(
            manifest.to_string(),
            "io error: missing shard (store snapshot is at manifest version 2)"
        );
    }

    // A current store keeps its error verbatim.
    let current = annotate_legacy_failure(Error::storage("boom"), SnapshotMarker::Current);
    assert_eq!(current, Error::storage("boom"));
}

#[test]
fn migration_report_records_the_one_migration() {
    let mut report = MigrationReport::default();
    assert_eq!(report.migration(), None);
    assert_eq!(report.to_string(), "no migration recorded");
    // Display prints the recorded pair — the ontology half a single
    // per-subsystem constant could never know.
    report.record(
        Subsystem::ValueEncoding,
        1,
        value_encoding::ONTOLOGY_VERSION,
    );
    assert_eq!(
        report.migration(),
        Some((
            Subsystem::ValueEncoding,
            1,
            value_encoding::ONTOLOGY_VERSION
        ))
    );
    assert_eq!(report.to_string(), "value_encoding 1→3");

    // A store's snapshot is one codec's marker, so a second record never
    // comes; should one anyway, it replaces — there is no list.
    #[cfg(parquet_codec)]
    {
        report.record(
            Subsystem::Manifest,
            1,
            u64::from(manifest::WIDE_ONTOLOGY_VERSION),
        );
        assert_eq!(
            report.migration(),
            Some((
                Subsystem::Manifest,
                1,
                u64::from(manifest::WIDE_ONTOLOGY_VERSION)
            ))
        );
        assert_eq!(report.to_string(), "manifest 1→6");
    }
}

#[cfg(manifest_codec)]
#[test]
fn manifest_published_version_follows_the_codec_and_the_content() {
    use crate::codec::SnapshotCodecKind;
    // Parquet publishes the wide pair (ADR-0553), the experimental codecs
    // the legacy pair (ADR-0537); the ontology half follows the content
    // (ADR-0120 §8).
    #[cfg(parquet_codec)]
    assert_eq!(
        manifest::published_with(SnapshotCodecKind::Parquet, false),
        manifest::WIDE_VERSION
    );
    #[cfg(parquet_codec)]
    assert_eq!(
        manifest::published_with(SnapshotCodecKind::Parquet, true),
        manifest::WIDE_ONTOLOGY_VERSION
    );
    #[cfg(rkyv_codec)]
    assert_eq!(
        manifest::published_with(SnapshotCodecKind::Rkyv, false),
        manifest::CURRENT_VERSION
    );
}

#[test]
fn version_context_preserves_the_error_variant_without_a_doubled_prefix() {
    let storage = with_version_context(
        Error::storage("failed to parse graph JSON: corrupt"),
        format_args!("store snapshot is at value_encoding 1"),
    );
    assert!(matches!(storage, Error::Storage(_)));
    let text = storage.to_string();
    assert_eq!(
        text,
        "storage error: failed to parse graph JSON: corrupt (store snapshot is at value_encoding 1)"
    );
    assert_eq!(text.matches("storage error:").count(), 1, "{text}");

    let io = with_version_context(
        Error::io("missing shard"),
        format_args!("manifest version 2"),
    );
    assert!(matches!(io, Error::Io(_)));
    assert_eq!(
        io.to_string(),
        "io error: missing shard (manifest version 2)"
    );

    // Variants without a free-text payload pass through untouched.
    let conflict = with_version_context(Error::Conflict, format_args!("manifest version 2"));
    assert_eq!(conflict, Error::Conflict);
}
