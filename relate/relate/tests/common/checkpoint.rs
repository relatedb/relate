//! The ADR-0120 checkpoint format-marker assertion.

use super::store::store_vfs;

/// What a checkpoint's ontology publication must look like — the one
/// typed expectation [`assert_checkpoint_format`] asserts on, so every
/// shape asserts something (no held-but-unchecked hole).
pub enum ExpectedOntology {
    /// The plain format: the codec's ontology field is absent.
    Absent,
    /// The ontology is held: the codec's ontology field is present (a
    /// Parquet manifest publishes by reference — `ontology_path`,
    /// ADR-0120 §5 — so presence is the most it can assert).
    Held,
    /// The ontology is held and published verbatim: the field's value is
    /// exactly `document`.
    Document(&'static str),
}

/// Assert the ADR-0120 §4/§8 checkpoint format markers of a Native store
/// directory, per snapshot codec, against one typed ontology expectation.
/// The ontology state is the format-marker state: the plain format
/// ([`ExpectedOntology::Absent`]) is a Parquet `format_version` 5 (the
/// wide pair, ADR-0553) / a JSON `value_encoding` 2 with no ontology
/// published anywhere, and a held ontology is `format_version` 6 /
/// `value_encoding` 3; the experimental codec writes the legacy pair
/// (ADR-0537), 3/4. Each codec arm
/// asserts on one typed read of its own checkpoint image
/// (`SnapshotCodecKind::checkpoint_image`): the Parquet manifest's
/// `ontology_path` (by reference, ADR-0120 §5), the JSON `graph.json`'s
/// inline `ontology` field, and the markdown sidecar manifest
/// (ADR-0406 §3), whose envelope names the codec and whose inline
/// ontology follows the JSON codec's rules.
pub fn assert_checkpoint_format(
    dir: &std::path::Path,
    codec: relate_storage::SnapshotCodecKind,
    expected: ExpectedOntology,
) {
    // One typed read of the checkpoint image, shared by every arm below.
    let image: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join(codec.checkpoint_image())).unwrap())
            .unwrap();
    // The ontology state is the format-marker state (ADR-0120 §4/§8).
    let held = !matches!(expected, ExpectedOntology::Absent);
    match codec {
        relate_storage::SnapshotCodecKind::Parquet => {
            assert_eq!(image["format_version"], if held { 6 } else { 5 });
            // A Parquet manifest publishes the ontology by reference
            // (`ontology_path`) — the referenced object's bytes are the
            // document, asserted verbatim, never just presence.
            let published = image.get("ontology_path").map(|value| {
                let path = value.as_str().expect("ontology_path is a path");
                relate_storage::read_to_vec(&*store_vfs(dir), path)
                    .unwrap_or_else(|e| panic!("ontology object {path} of {dir:?} reads: {e}"))
            });
            assert_ontology(published, expected);
        }
        relate_storage::SnapshotCodecKind::Json => {
            assert_eq!(image["value_encoding"], if held { 3 } else { 2 });
            assert_ontology(inline_ontology(&image), expected);
        }
        relate_storage::SnapshotCodecKind::Markdown => {
            assert_eq!(image["codec"], "markdown");
            assert_ontology(inline_ontology(&image), expected);
        }
        // The experimental codec's image is the plain codec manifest: the
        // format marker (classifying against the manifest's own ontology
        // flag, ADR-0120 §8) plus the checkpoint theater. The ontology
        // document rides the codec payload, so the image never carries it.
        // The experimental codecs' image is the same plain codec manifest
        // (format marker plus theater); the ontology rides the payload
        // manifest, never the image.
        relate_storage::SnapshotCodecKind::Rkyv | relate_storage::SnapshotCodecKind::Vortex => {
            assert_eq!(image["format_version"], if held { 4 } else { 3 });
            assert_eq!(image["has_ontology"], held);
            assert_ontology(None, ExpectedOntology::Absent);
        }
    }
}

/// The inline ontology text of a checkpoint image (`graph.json` or the
/// markdown sidecar manifest), as published-document bytes.
fn inline_ontology(image: &serde_json::Value) -> Option<Vec<u8>> {
    image
        .get("ontology")
        .and_then(|value| value.as_str())
        .map(str::as_bytes)
        .map(<[u8]>::to_vec)
}

/// The ontology-publication rule every codec arm of
/// [`assert_checkpoint_format`] shares, over the codec's own
/// published-document accessor — the inline field's text, or the
/// referenced object's bytes for a Parquet manifest: absent, present,
/// or the published document verbatim.
fn assert_ontology(published: Option<Vec<u8>>, expected: ExpectedOntology) {
    match expected {
        ExpectedOntology::Absent => {
            assert!(
                published.is_none(),
                "the checkpoint image publishes no ontology: {published:?}"
            );
        }
        ExpectedOntology::Held => {
            assert!(published.is_some(), "the checkpoint publishes its ontology");
        }
        ExpectedOntology::Document(document) => {
            assert_eq!(
                published.as_deref(),
                Some(document.as_bytes()),
                "the published ontology is the document"
            );
        }
    }
}
