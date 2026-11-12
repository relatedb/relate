//! The workspace's package graph — the one vocabulary every maintenance
//! command reads: the cargo runner every invocation goes through, the
//! one non-zero-exit judge (`expect_success`), `cargo metadata` loaded
//! and parsed into the member packages (`Package`, `Dep`, `DepKind`),
//! and the load+parse composition (`workspace_graph`) callers invoke
//! instead of re-spelling it.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DepKind {
    Normal,
    Dev,
    Build,
}

impl fmt::Display for DepKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DepKind::Normal => "normal",
            DepKind::Dev => "dev",
            DepKind::Build => "build",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dep {
    pub(crate) kind: DepKind,
    pub(crate) path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Package {
    /// Cargo's package id (`path+file://…`): the resolved graph's key,
    /// so the check never round-trips an id through a display name.
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) dir: String,
    pub(crate) deps: Vec<Dep>,
}

/// The `cargo metadata --no-deps` document the workspace graph is read
/// from, typed: a schema change is a deserialize error, never a silent
/// fold. `dependencies` carries no default — cargo always emits it, and
/// a missing array must not read as "no edges".
#[derive(Debug, serde::Deserialize)]
struct Metadata {
    workspace_root: PathBuf,
    workspace_members: Vec<String>,
    packages: Vec<MetadataPackage>,
}

#[derive(Debug, serde::Deserialize)]
struct MetadataPackage {
    id: String,
    name: String,
    manifest_path: PathBuf,
    dependencies: Vec<MetadataDependency>,
}

#[derive(Debug, serde::Deserialize)]
struct MetadataDependency {
    /// `null` is a normal dependency; `"dev"` and `"build"` are the
    /// other two. An unknown string is a schema breach, refused by
    /// serde — never folded into a default.
    kind: Option<DepKind>,
    /// Absent for a registry dependency; a workspace path otherwise.
    path: Option<PathBuf>,
}

/// The workspace members `cargo metadata` reports, parsed into the
/// package graph: cargo's id, name, workspace-relative directory, and
/// dependencies (kind, plus the workspace-relative path for
/// workspace-internal edges), sorted by name. Members are the traversal's
/// roots, so a member id with no package entry is refused by name —
/// never dropped.
fn parse_metadata(metadata: &Metadata) -> Result<Vec<Package>, String> {
    let mut by_id: BTreeMap<&str, &MetadataPackage> = BTreeMap::new();
    for package in &metadata.packages {
        if by_id.insert(package.id.as_str(), package).is_some() {
            return Err(format!(
                "cargo metadata: duplicate package id {}",
                package.id
            ));
        }
    }
    let mut out = Vec::new();
    for id in &metadata.workspace_members {
        let package = by_id
            .get(id.as_str())
            .ok_or_else(|| format!("cargo metadata: workspace member {id} has no package entry"))?;
        let dir = package.manifest_path.parent().ok_or_else(|| {
            format!(
                "cargo metadata: package {} has an unparented manifest_path",
                package.name
            )
        })?;
        let deps = package
            .dependencies
            .iter()
            .map(|dep| Dep {
                kind: dep.kind.unwrap_or(DepKind::Normal),
                path: dep
                    .path
                    .as_deref()
                    .map(|path| relative(&metadata.workspace_root, path)),
            })
            .collect();
        out.push(Package {
            id: package.id.clone(),
            name: package.name.clone(),
            dir: relative(&metadata.workspace_root, dir),
            deps,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// A path rendered workspace-relative, with `/` separators — the
/// directory form every consumer of the graph keys on.
fn relative(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// The cargo binary every command invokes: `$CARGO` when run through
/// `cargo run`, else `cargo` on PATH.
fn cargo_binary() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

/// The host target triple cargo builds for by default, from the
/// toolchain's own verbose version output. `cargo metadata
/// --filter-platform` needs it to name the graph `cargo build` resolves
/// on this machine, not every platform's declared edges.
pub(crate) fn host_triple() -> Result<String, String> {
    let output = run_cargo(&["-Vv"])?;
    expect_success(&output, "`cargo -Vv`")?;
    let stdout = std::str::from_utf8(&output.stdout)
        .map_err(|e| format!("`cargo -Vv` is not valid UTF-8: {e}"))?;
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
        .ok_or_else(|| "`cargo -Vv` carries no host target triple".to_string())
}

/// A path cargo must receive as a command argument. A non-UTF-8 path is
/// refused loudly — never lossily rewritten into a different path. The
/// one conversion policy for every path handed to cargo.
pub(crate) fn cargo_arg(path: &Path) -> Result<&str, String> {
    path.to_str().ok_or_else(|| {
        format!(
            "{} is not valid UTF-8 — cargo cannot take it as an argument",
            path.display()
        )
    })
}

/// Run `$CARGO` (else `cargo`) with `args`, capturing the output. A
/// spawn failure is an error; the exit status is judged by the caller,
/// with the caller's own error text.
pub(crate) fn run_cargo(args: &[&str]) -> Result<Output, String> {
    let cargo = cargo_binary();
    Command::new(&cargo)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run `{cargo} {}`: {e}", args.join(" ")))
}

/// The one non-zero-child-exit judge: a failed child exit becomes a
/// clear error carrying its stderr. Every child spawn — cargo or any
/// other — passes through this; the policy is spelled once, and no
/// caller re-implements it.
pub(crate) fn expect_success(output: &Output, command: &str) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{command} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// `cargo metadata --format-version 1 --no-deps` for the workspace at
/// `root`, parsed as JSON. The manifest path pins the graph to the same
/// root every caller joins its paths onto — the graph can never come
/// from a different workspace than the check's paths claim.
fn load_metadata(root: &Path) -> Result<Metadata, String> {
    let manifest = root.join("Cargo.toml");
    let manifest_arg = cargo_arg(&manifest)?;
    let output = run_cargo(&[
        "metadata",
        "--format-version",
        "1",
        "--no-deps",
        "--manifest-path",
        manifest_arg,
    ])?;
    expect_success(&output, "`cargo metadata`")?;
    serde_json::from_slice(&output.stdout).map_err(|e| format!("invalid cargo metadata: {e}"))
}

/// The workspace package graph at `root`: metadata loaded and parsed in
/// one call — the composition every command spells.
pub(crate) fn workspace_graph(root: &Path) -> Result<Vec<Package>, String> {
    load_metadata(root).and_then(|m| parse_metadata(&m))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The typed `cargo metadata` document: `members` are the
    /// workspace_members, `packages` the packages array.
    fn metadata(
        members: &[&str],
        packages: &[serde_json::Value],
    ) -> Result<Metadata, serde_json::Error> {
        serde_json::from_value(serde_json::json!({
            "workspace_root": "/ws",
            "workspace_members": members,
            "packages": packages,
        }))
    }

    #[test]
    fn parses_workspace_members_and_their_edges() {
        let metadata = metadata(
            &[
                "path+file:///ws/relate/a#0.1.0",
                "path+file:///ws/surfaces/b#0.1.0",
            ],
            &[
                serde_json::json!({
                    "id": "path+file:///ws/relate/a#0.1.0",
                    "name": "a",
                    "manifest_path": "/ws/relate/a/Cargo.toml",
                    "dependencies": [
                        { "name": "b", "kind": null, "path": "/ws/surfaces/b" },
                        { "name": "serde_json", "kind": null },
                        { "name": "b", "kind": "dev", "path": "/ws/surfaces/b" }
                    ]
                }),
                serde_json::json!({
                    "id": "path+file:///ws/surfaces/b#0.1.0",
                    "name": "b",
                    "manifest_path": "/ws/surfaces/b/Cargo.toml",
                    "dependencies": [
                        { "name": "a", "kind": "build", "path": "/ws/relate/a" }
                    ]
                }),
                serde_json::json!({
                    "id": "registry+https://example/serde_json#1.0.0",
                    "name": "serde_json",
                    "manifest_path": "/registry/serde_json/Cargo.toml",
                    "dependencies": []
                }),
            ],
        )
        .unwrap();
        let packages = parse_metadata(&metadata).unwrap();
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].id, "path+file:///ws/relate/a#0.1.0");
        assert_eq!(packages[0].dir, "relate/a");
        assert_eq!(
            packages[0].deps[0],
            Dep {
                kind: DepKind::Normal,
                path: Some("surfaces/b".to_string())
            }
        );
    }

    #[test]
    fn malformed_dependency_metadata_is_refused_not_folded() {
        // `dependencies` has no serde default: a missing array must not
        // read as "no edges", or both gates green while checking none.
        let missing = metadata(
            &["path+file:///ws/relate/a#0.1.0"],
            &[serde_json::json!({
                "id": "path+file:///ws/relate/a#0.1.0",
                "name": "a",
                "manifest_path": "/ws/relate/a/Cargo.toml",
            })],
        )
        .unwrap_err();
        assert!(
            missing.to_string().contains("missing field `dependencies`"),
            "{missing}"
        );
        let kind = metadata(
            &["path+file:///ws/relate/a#0.1.0"],
            &[serde_json::json!({
                "id": "path+file:///ws/relate/a#0.1.0",
                "name": "a",
                "manifest_path": "/ws/relate/a/Cargo.toml",
                "dependencies": [{ "name": "b", "kind": "optional" }],
            })],
        )
        .unwrap_err();
        assert!(kind.to_string().contains("unknown variant"), "{kind}");
        let path = metadata(
            &["path+file:///ws/relate/a#0.1.0"],
            &[serde_json::json!({
                "id": "path+file:///ws/relate/a#0.1.0",
                "name": "a",
                "manifest_path": "/ws/relate/a/Cargo.toml",
                "dependencies": [{ "name": "b", "kind": null, "path": 7 }],
            })],
        )
        .unwrap_err();
        assert!(path.to_string().contains("invalid type"), "{path}");
    }

    #[test]
    fn a_workspace_member_without_a_package_entry_is_an_error() {
        let metadata = metadata(&["path+file:///ws/relate/missing#0.1.0"], &[]).unwrap();
        let error = parse_metadata(&metadata).unwrap_err();
        assert!(error.contains("has no package entry"), "{error}");
    }

    #[test]
    fn an_unparented_manifest_path_is_an_error() {
        let metadata = metadata(
            &["path+file:///ws/relate/a#0.1.0"],
            &[serde_json::json!({
                "id": "path+file:///ws/relate/a#0.1.0",
                "name": "a",
                "manifest_path": "",
                "dependencies": [],
            })],
        )
        .unwrap();
        let error = parse_metadata(&metadata).unwrap_err();
        assert!(error.contains("unparented manifest_path"), "{error}");
    }
}
