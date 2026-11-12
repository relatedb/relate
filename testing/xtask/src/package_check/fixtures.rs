//! Cross-stage test fixtures: the workspace graph, the resolved graph
//! and the build evidence the stage tests share. Compiled only under
//! `cfg(test)`.

use super::evidence::BuildEvidence;
use super::resolve::ResolvedMetadata;
use crate::workspace::{Dep, DepKind, Package};
use std::path::{Path, PathBuf};

/// A workspace member fixture: `deps` are `(workspace dir, kind)`
/// pairs, `None` marking a registry dependency with no path.
pub(super) fn pkg(name: &str, dir: &str, deps: &[(Option<&str>, DepKind)]) -> Package {
    Package {
        id: format!("path+file:///ws/{name}#1.0.0"),
        name: name.to_string(),
        dir: dir.to_string(),
        deps: deps
            .iter()
            .map(|(path, kind)| Dep {
                kind: *kind,
                path: path.map(str::to_string),
            })
            .collect(),
    }
}

/// The facade with normal, build and dev edges, a transitive chain, a
/// normal cycle back into the facade, and members no closure reaches:
/// the dev-only test support (whose own normal edge cycles back), a
/// dev-only surface, and an unrelated surface.
pub(super) fn fixture_workspace() -> Vec<Package> {
    vec![
        pkg(
            "relate",
            "relate/relate",
            &[
                (Some("relate/relate-core"), DepKind::Normal),
                (Some("relate/relate-model"), DepKind::Build),
                (Some("testing/relate-test-support"), DepKind::Dev),
                (None, DepKind::Normal), // a registry dependency
            ],
        ),
        pkg(
            "relate-core",
            "relate/relate-core",
            &[(Some("relate/relate-executor"), DepKind::Normal)],
        ),
        pkg(
            "relate-executor",
            "relate/relate-executor",
            &[
                (Some("relate/relate-markdown"), DepKind::Normal),
                (Some("surfaces/relate-formatter"), DepKind::Dev),
            ],
        ),
        pkg("relate-markdown", "relate/relate-markdown", &[]),
        pkg("relate-model", "relate/relate-model", &[]),
        pkg(
            "relate-test-support",
            "testing/relate-test-support",
            &[(Some("relate/relate"), DepKind::Normal)],
        ),
        pkg("relate-formatter", "surfaces/relate-formatter", &[]),
        pkg("relate-cli", "surfaces/relate-cli", &[]),
    ]
}

/// The full `--filter-platform` resolution of the packaged facade,
/// shaped like cargo's: the members the host build reaches are nodes;
/// the dev-only members are absent, as they are from a real build's
/// resolve.
pub(super) fn fixture_resolved() -> ResolvedMetadata {
    let path = |name: &str| format!("path+file:///ws/{name}#1.0.0");
    serde_json::from_value(serde_json::json!({
        "packages": [
            { "id": path("relate"), "name": "relate" },
            { "id": path("relate-core"), "name": "relate-core" },
            { "id": path("relate-executor"), "name": "relate-executor" },
            { "id": path("relate-markdown"), "name": "relate-markdown" },
            { "id": path("relate-model"), "name": "relate-model" },
            { "id": path("relate-test-support"), "name": "relate-test-support" },
            { "id": path("relate-formatter"), "name": "relate-formatter" },
            { "id": path("relate-cli"), "name": "relate-cli" },
            { "id": "registry+https://example/serde#1.0.0", "name": "serde" }
        ],
        "resolve": {
            "root": path("relate"),
            "nodes": [
                { "id": path("relate"), "deps": [
                    { "pkg": path("relate-core"), "dep_kinds": [{ "kind": null }] },
                    { "pkg": path("relate-model"), "dep_kinds": [{ "kind": "build" }] },
                    { "pkg": path("relate-test-support"), "dep_kinds": [{ "kind": "dev" }] },
                    { "pkg": "registry+https://example/serde#1.0.0", "dep_kinds": [{ "kind": null }] }
                ] },
                { "id": path("relate-core"), "deps": [
                    { "pkg": path("relate-executor"), "dep_kinds": [{ "kind": null }] }
                ] },
                { "id": path("relate-executor"), "deps": [
                    { "pkg": path("relate-markdown"), "dep_kinds": [{ "kind": null }] },
                    { "pkg": path("relate-formatter"), "dep_kinds": [{ "kind": "dev" }] }
                ] },
                { "id": path("relate-markdown"), "deps": [] },
                { "id": path("relate-model"), "deps": [] },
                { "id": path("relate-test-support"), "deps": [
                    { "pkg": path("relate"), "dep_kinds": [{ "kind": null }] }
                ] },
                { "id": path("relate-formatter"), "deps": [] },
                { "id": path("relate-cli"), "deps": [] },
                { "id": "registry+https://example/serde#1.0.0", "deps": [] }
            ]
        }
    }))
    .unwrap()
}

/// One `compiler-artifact` record for `manifest` with the given target
/// kind, serialized by `serde_json` so escaping is its business.
pub(super) fn artifact_record(manifest: &str, kind: &str) -> String {
    serde_json::json!({
        "reason": "compiler-artifact",
        "manifest_path": manifest,
        "target": { "kind": [kind] },
    })
    .to_string()
}

/// The four closure members the verification fixture mirrors: the
/// facade's normal and build edges, dev-only members left out.
pub(super) fn verification_closure() -> Vec<Package> {
    vec![
        pkg("relate-core", "relate/relate-core", &[]),
        pkg("relate-executor", "relate/relate-executor", &[]),
        pkg("relate-markdown", "relate/relate-markdown", &[]),
        pkg("relate-model", "relate/relate-model", &[]),
    ]
}

/// The artifacts a green build produces for `closure`, plus the
/// extracted facade's own. Every member's workspace manifest is derived
/// from its directory; relate-model's path is the one entry a caller
/// varies (workspace or registry cache).
pub(super) fn verification_evidence(
    closure: &[Package],
    relate_model_manifest: &str,
) -> BuildEvidence {
    let mut artifact_manifests: Vec<PathBuf> = closure
        .iter()
        .map(|package| {
            if package.name == "relate-model" {
                PathBuf::from(relate_model_manifest)
            } else {
                Path::new("/ws").join(&package.dir).join("Cargo.toml")
            }
        })
        .collect();
    artifact_manifests.push("/tmp/pkg/relate-1.0.0/Cargo.toml".into());
    BuildEvidence {
        artifact_manifests,
        examples: 24,
    }
}
