//! The normalization suite (the vault identity fold's composition
//! ADR): a filename a normalizing copy tool left decomposed (macOS
//! `tar` writes NFD; the engine writes NFC) is the same name as its
//! composed spelling — the vault opens over it, a normalization-only
//! duplicate folds like a case-only one, and the checkpoint renames
//! the file to the composed canonical form.

use super::{node_by_name, open_markdown};
use relate_executor::Graph;

/// Open a markdown store at `dir`, read-only semantics of the test
/// harness: one open, no checkpoint.
fn open(dir: &std::path::Path) -> (crate::StoreTarget, Graph) {
    open_markdown(dir).unwrap()
}

#[test]
fn a_decomposed_filename_is_its_composed_key_value() {
    // The tar case: the engine wrote the file with the composed `Adöe`
    // (U+00F6) value, the copy tool left the filename decomposed
    // (`o` + U+0308). Before the fold composed, the key check read the
    // two as different names and the vault refused to open.
    let dir = tempfile::tempdir().unwrap();
    let person = dir.path().join("Person");
    std::fs::create_dir_all(&person).unwrap();
    std::fs::write(
        // `Ado\u{0308}e.md` — decomposed filename on disk.
        person.join("Ado\u{0308}e.md"),
        // Composed value in the frontmatter.
        "---\ntype: Person\nname: Ad\u{00F6}e\n---\nbody",
    )
    .unwrap();
    let (_store, graph) = open(dir.path());
    // Panics if the decomposed-named file did not load as its node.
    node_by_name(&graph, "Ad\u{00F6}e");
}

#[test]
fn a_normalization_only_duplicate_folds_like_a_case_only_one() {
    // Two files whose names differ only by normalization are one name
    // — the same data-quality fold a case-only pair takes: the first
    // spelling in path order wins, the later drops.
    let dir = tempfile::tempdir().unwrap();
    let person = dir.path().join("Person");
    std::fs::create_dir_all(&person).unwrap();
    std::fs::write(
        person.join("Ad\u{00F6}e.md"),
        "---\ntype: Person\nname: Ad\u{00F6}e\n---\nfirst",
    )
    .unwrap();
    // The composed twin: the same name as the decomposed file below —
    // without it the two fixtures derive different identities, no
    // duplicate pair exists, and this test cannot fail for the fold it
    // documents.
    std::fs::write(
        person.join("Ad\u{00F6}e2.md"),
        "---\ntype: Person\nname: Ad\u{00F6}e2\n---\nsecond",
    )
    .unwrap();
    std::fs::write(
        person.join("Ado\u{0308}e2.md"),
        "---\ntype: Person\nname: Ado\u{0308}e2\n---\nthird",
    )
    .unwrap();
    // `Ad\u{00F6}e2` and `Ado\u{0308}e2` are one name under the
    // composed fold: the first spelling in path order wins (whose
    // *name* still composes to the same identity) and the other drops
    // as the fold's duplicate — so the vault carries two nodes, and a
    // fold that stops composing carries three and fails here.
    let (_store, graph) = open(dir.path());
    node_by_name(&graph, "Ad\u{00F6}e");
    node_by_name(&graph, "Ad\u{00F6}e2");
    assert_eq!(graph.all_node_ids().len(), 2);
}

#[test]
fn a_checkpoint_renames_a_decomposed_filename_to_the_composed_form() {
    // The heal: the plan derives the filename from the canonical
    // (composed) key value, so the checkpoint writes the composed name
    // and deletes the decomposed spelling — the delete-before-write
    // order that is a rename on every filesystem, the same order a
    // case-only spelling takes. On a normalization-insensitive
    // filesystem (default APFS) both spellings resolve to one physical
    // file, so the proof is the stored directory entry's own bytes,
    // never a path lookup.
    let dir = tempfile::tempdir().unwrap();
    let person = dir.path().join("Person");
    std::fs::create_dir_all(&person).unwrap();
    std::fs::write(
        person.join("Ado\u{0308}e.md"),
        "---\ntype: Person\nname: Ad\u{00F6}e\n---\nbody",
    )
    .unwrap();
    let (store, graph) = open(dir.path());
    crate::checkpoint(&store, &graph, None).unwrap();
    let stored: Vec<String> = std::fs::read_dir(&person)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        stored.contains(&"Ad\u{00F6}e.md".to_string()),
        "the composed canonical spelling is stored: {stored:?}"
    );
    assert!(
        !stored.iter().any(|name| name == "Ado\u{0308}e.md"),
        "the decomposed spelling is gone — the rename healed the vault: {stored:?}"
    );
    // Release the writer lease before the reopen proves the healed
    // vault loads.
    drop(graph);
    drop(store);
    let (_store, reloaded) = open(dir.path());
    assert_eq!(reloaded.all_node_ids().len(), 1);
}

#[test]
fn a_decomposed_root_file_heals_instead_of_refusing() {
    // The rust round-6 pin: the plan's composed root path must find a
    // root file the walk recorded under a decomposed spelling — an
    // exact-byte existence lookup would trip the root-file refusal
    // ("no existing root file") instead of healing.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Ado\u{0308}e.md"),
        "---\nname: Ado\u{0308}e\n---\nroot note\n",
    )
    .unwrap();
    let (store, graph) = open(dir.path());
    crate::checkpoint(&store, &graph, None).unwrap();
    let stored: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        stored.contains(&"Ad\u{00F6}e.md".to_string()),
        "the composed root spelling is stored: {stored:?}"
    );
    assert!(
        !stored.iter().any(|name| name == "Ado\u{0308}e.md"),
        "the decomposed root spelling is gone — the heal renamed it: {stored:?}"
    );
}

#[test]
fn a_decomposed_raw_file_passes_through_verbatim() {
    // The rust round-6 pin, raw half: a raw-frontmatter file whose
    // on-disk name is decomposed must find its passthrough through the
    // identity fold — an exact-byte membership lookup would miss it,
    // and the checkpoint would re-render the file, discarding the
    // user's raw text (ADR-0406 §6: never rewritten).
    let dir = tempfile::tempdir().unwrap();
    let person = dir.path().join("Person");
    std::fs::create_dir_all(&person).unwrap();
    let raw: &[u8] = b"---\nbroken: [unclosed\n---\n\nthe user's raw text\n";
    std::fs::write(person.join("Ado\u{0308}e.md"), raw).unwrap();
    let (store, graph) = open(dir.path());
    crate::checkpoint(&store, &graph, None).unwrap();
    // On a normalization-insensitive filesystem (default APFS) both
    // spellings resolve to one physical file, so the proof is the
    // stored directory entry's own bytes, never a path lookup — the
    // sibling heal test's rule.
    let stored: Vec<String> = std::fs::read_dir(&person)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        stored.contains(&"Ad\u{00F6}e.md".to_string()),
        "the composed raw spelling is stored: {stored:?}"
    );
    assert!(
        !stored.iter().any(|name| name == "Ado\u{0308}e.md"),
        "the decomposed raw spelling is gone: {stored:?}"
    );
    let passed = std::fs::read(person.join("Ad\u{00F6}e.md")).unwrap();
    assert_eq!(
        passed, raw,
        "the raw bytes pass through verbatim under the composed name"
    );
}
