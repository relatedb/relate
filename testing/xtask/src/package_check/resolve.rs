//! The host closure: resolve the patched extracted package and walk the
//! result, so the verdict names exactly the workspace members the build
//! compiles.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::workspace::{cargo_arg, expect_success, host_triple, run_cargo, DepKind, Package};

use super::FACADE;

/// The typed slice of a full `cargo metadata` resolution the closure
/// reads: each node's dependency edges and the package names their ids
/// map to. A missing `resolve` — the `--no-deps` shape — is a
/// deserialize error, never an empty graph.
#[derive(Debug, serde::Deserialize)]
pub(super) struct ResolvedMetadata {
    pub(super) packages: Vec<ResolvedPackage>,
    pub(super) resolve: ResolvedGraph,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct ResolvedPackage {
    pub(super) id: String,
    pub(super) name: String,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct ResolvedGraph {
    pub(super) root: String,
    pub(super) nodes: Vec<ResolvedNode>,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct ResolvedNode {
    pub(super) id: String,
    pub(super) deps: Vec<ResolvedEdge>,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct ResolvedEdge {
    pub(super) pkg: String,
    dep_kinds: Vec<ResolvedDepKind>,
}

#[derive(Debug, serde::Deserialize)]
struct ResolvedDepKind {
    /// `null` is a normal dependency; `"dev"` and `"build"` are the
    /// other two. The vocabulary is the workspace graph's `DepKind`, so
    /// the check never carries a second kind table.
    kind: Option<DepKind>,
}

/// Resolve the patched extracted package and read its graph for the
/// host platform: `--filter-platform` drops packages no host build can
/// reach (another platform's dependencies), so the answer names what
/// `cargo build` compiles here rather than every platform's manifest
/// declarations.
pub(super) fn load_resolved_metadata(manifest: &Path) -> Result<ResolvedMetadata, String> {
    let host = host_triple()?;
    let output = run_cargo(&[
        "metadata",
        "--format-version",
        "1",
        "--filter-platform",
        &host,
        "--manifest-path",
        cargo_arg(manifest)?,
    ])?;
    expect_success(&output, "`cargo metadata` (extracted package)")?;
    serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("invalid cargo metadata for the extracted package: {e}"))
}

/// The extracted package's closure over its *resolved* graph: the
/// workspace members a host build of the patched package compiles,
/// following normal and build edges, the facade excluded. Optional
/// dependencies that stayed inactive and other platforms' target
/// dependencies are absent from the filtered resolve, so the verdict
/// never demands an artifact the build does not produce.
pub(super) fn resolved_closure<'a>(
    metadata: &ResolvedMetadata,
    packages: &'a [Package],
) -> Result<Vec<&'a Package>, String> {
    let by_id: BTreeMap<&str, &Package> = packages.iter().map(|p| (p.id.as_str(), p)).collect();
    let by_name: BTreeMap<&str, &Package> = packages.iter().map(|p| (p.name.as_str(), p)).collect();
    let names: BTreeMap<&str, &str> = metadata
        .packages
        .iter()
        .map(|p| (p.id.as_str(), p.name.as_str()))
        .collect();
    let nodes: BTreeMap<&str, &ResolvedNode> = metadata
        .resolve
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect();
    let root = metadata.resolve.root.as_str();
    let root_name = names
        .get(root)
        .ok_or_else(|| format!("cargo metadata: resolve root {root} names no package"))?;
    if *root_name != FACADE {
        return Err(format!(
            "cargo metadata: resolve root is '{root_name}', expected '{FACADE}'"
        ));
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let node = nodes
            .get(id)
            .ok_or_else(|| format!("cargo metadata: resolve has no node for package {id}"))?;
        for edge in &node.deps {
            if edge.dep_kinds.is_empty() {
                return Err(format!(
                    "cargo metadata: dependency edge to {} carries no dep_kinds",
                    edge.pkg
                ));
            }
            if edge
                .dep_kinds
                .iter()
                .all(|kind| matches!(kind.kind, Some(DepKind::Dev)))
            {
                continue;
            }
            stack.push(edge.pkg.as_str());
        }
    }
    let mut members: BTreeMap<&str, &Package> = BTreeMap::new();
    for id in &seen {
        // The root is the extracted facade itself — the package under
        // test, not a patch target. Its id names the scratch copy, so it
        // never matches a workspace member.
        if *id == root {
            continue;
        }
        if let Some(package) = by_id.get(id) {
            members.insert(package.dir.as_str(), package);
            continue;
        }
        // A resolved package whose name a workspace member carries but
        // whose id differs is path-spelling drift between the workspace
        // metadata and the patched build. Read as an external
        // dependency it would green the check while patching nothing,
        // so it is refused here.
        if let Some(name) = names.get(id) {
            if let Some(workspace) = by_name.get(name) {
                return Err(format!(
                    "cargo metadata: resolved workspace member {name} carries id {id}, expected {}",
                    workspace.id
                ));
            }
        }
    }
    let mut closure: Vec<&Package> = members
        .into_values()
        .filter(|package| package.name != FACADE)
        .collect();
    closure.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(closure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_check::fixtures::{fixture_resolved, fixture_workspace};

    #[test]
    fn closure_follows_normal_and_build_edges_from_the_facade() {
        let packages = fixture_workspace();
        let closure = resolved_closure(&fixture_resolved(), &packages).unwrap();
        let names: Vec<&str> = closure.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "relate-core",
                "relate-executor",
                "relate-markdown",
                "relate-model"
            ]
        );
    }

    #[test]
    fn closure_follows_the_resolution_not_the_manifest_declarations() {
        // The build did not resolve the build-dependency (an inactive
        // optional member, say): the closure must not demand its
        // artifact, even though the manifests declare it.
        let packages = fixture_workspace();
        let mut resolved = fixture_resolved();
        let root = resolved.resolve.root.clone();
        resolved
            .resolve
            .nodes
            .iter_mut()
            .find(|node| node.id == root)
            .unwrap()
            .deps
            .retain(|edge| edge.pkg != "path+file:///ws/relate-model#1.0.0");
        let closure = resolved_closure(&resolved, &packages).unwrap();
        assert!(!closure.iter().any(|p| p.name == "relate-model"));
        assert!(closure.iter().any(|p| p.name == "relate-executor"));
    }

    #[test]
    fn a_resolved_workspace_member_with_a_different_id_is_an_error() {
        // The same name at a different path is drift, not an external
        // dependency: it must never silently leave the check green.
        let old = "path+file:///ws/relate-model#1.0.0";
        let new = "path+file:///ws/elsewhere/relate-model#1.0.0";
        let mut resolved = fixture_resolved();
        for package in &mut resolved.packages {
            if package.id == old {
                package.id = new.to_string();
            }
        }
        for node in &mut resolved.resolve.nodes {
            if node.id == old {
                node.id = new.to_string();
            }
            for edge in &mut node.deps {
                if edge.pkg == old {
                    edge.pkg = new.to_string();
                }
            }
        }
        let error = resolved_closure(&resolved, &fixture_workspace()).unwrap_err();
        assert!(error.contains("expected"), "{error}");
        assert!(error.contains(new), "{error}");
    }
}
