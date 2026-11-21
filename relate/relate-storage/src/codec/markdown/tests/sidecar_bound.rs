//! The sidecar-bound suite (ADR-0648): the markdown sidecar rides its
//! own metadata ceiling — 256 MiB, not the untrusted-input class's
//! 16 MiB — because it is engine-authored state whose size scales with
//! the vault. A store whose sidecar passes the old class ceiling
//! opens; the untrusted-input class itself stays at 16 MiB.

use super::open_markdown;

/// Bootstrap a small markdown store and return its sidecar path.
fn bootstrapped(dir: &std::path::Path) -> std::path::PathBuf {
    let person = dir.join("Person");
    std::fs::create_dir_all(&person).unwrap();
    std::fs::write(
        person.join("Ada.md"),
        "---\ntype: Person\nname: Ada\n---\nbody",
    )
    .unwrap();
    let (store, graph) = open_markdown(dir).unwrap();
    crate::checkpoint(&store, &graph, None).unwrap();
    drop(graph);
    drop(store);
    dir.join(".relate").join("manifest.json")
}

#[test]
fn a_sidecar_past_the_untrusted_class_ceiling_opens() {
    // The old bound refused any sidecar past the ADR-0086 class
    // ceiling (16 MiB) — a wall at roughly 45k files, since the
    // sidecar carries per-file stats and ids. The raise: a sidecar
    // between the class ceiling and the sidecar's own bound opens.
    // The padding rides an unknown JSON field, so the sidecar is still
    // a valid manifest — the way a real one grows is by carrying more
    // files, which serializes to the same parsed shape at larger size.
    let dir = tempfile::tempdir().unwrap();
    let sidecar = bootstrapped(dir.path());
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&sidecar).unwrap()).unwrap();
    // ~17 MiB: one byte past the old class ceiling.
    manifest["padding"] = serde_json::Value::String("x".repeat(17 * 1024 * 1024));
    std::fs::write(&sidecar, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (store, graph) = open_markdown(dir.path()).unwrap();
    drop(store);
    assert_eq!(
        graph.all_node_ids().len(),
        1,
        "the store over the old ceiling opens and loads"
    );
}

#[test]
fn the_untrusted_input_class_keeps_its_own_ceiling() {
    // The raise is the sidecar's alone: the untrusted-input class
    // (a classic root manifest) still refuses past 16 MiB —
    // `snapshot::MAX_JSON_OBJECT` untouched — and the sidecar's bound
    // relates to it per target: raised on native, identical on wasm.
    let class_ceiling: u64 = crate::snapshot::MAX_JSON_OBJECT;
    let sidecar_bound: u64 = super::super::manifest::MAX_MARKDOWN_SIDECAR;
    assert_eq!(
        class_ceiling,
        16 * 1024 * 1024,
        "the ADR-0086 class ceiling is unchanged"
    );
    #[cfg(not(target_arch = "wasm32"))]
    assert!(
        sidecar_bound > class_ceiling,
        "the sidecar's own bound is the raised one"
    );
    #[cfg(target_arch = "wasm32")]
    assert_eq!(
        sidecar_bound, class_ceiling,
        "wasm keeps the class ceiling for the sidecar"
    );
}
