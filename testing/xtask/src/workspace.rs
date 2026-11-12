//! The workspace's package graph — the one vocabulary every maintenance
//! command reads: the cargo runner every invocation goes through,
//! `cargo metadata` loaded and parsed into the member packages
//! (`Package`, `Dep`, `DepKind`), and the load+parse composition
//! (`workspace_graph`) callers invoke instead of re-spelling it.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path};
use std::process::{Command, Output};

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
    pub(crate) name: String,
    pub(crate) dir: String,
    pub(crate) deps: Vec<Dep>,
}

/// The workspace members `cargo metadata` reports, parsed into the
/// package graph: name, workspace-relative directory, and dependencies
/// (kind, plus the workspace-relative path for workspace-internal
/// edges), sorted by name.
pub(crate) fn parse_metadata(metadata: &Value) -> Result<Vec<Package>, String> {
    let root = metadata
        .get("workspace_root")
        .and_then(Value::as_str)
        .map(Path::new)
        .ok_or("cargo metadata: missing workspace_root")?;
    let members: BTreeSet<&str> = metadata
        .get("workspace_members")
        .and_then(Value::as_array)
        .ok_or("cargo metadata: missing workspace_members")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or("cargo metadata: missing packages")?;

    let mut out = Vec::new();
    for package in packages {
        let id = package.get("id").and_then(Value::as_str).unwrap_or("");
        if !members.contains(id) {
            continue;
        }
        let name = package
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("cargo metadata: package {id} has no name"))?;
        let manifest = package
            .get("manifest_path")
            .and_then(Value::as_str)
            .map(Path::new)
            .ok_or_else(|| format!("cargo metadata: package {name} has no manifest_path"))?;
        let dir = manifest.parent().unwrap_or(manifest);
        let mut deps = Vec::new();
        for dep in package
            .get("dependencies")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let kind = match dep.get("kind").and_then(Value::as_str) {
                None => DepKind::Normal,
                Some("dev") => DepKind::Dev,
                Some("build") => DepKind::Build,
                Some(other) => {
                    return Err(format!(
                        "cargo metadata: package {name} has unknown dependency kind {other:?}"
                    ))
                }
            };
            let path = dep
                .get("path")
                .and_then(Value::as_str)
                .map(|p| relative(root, Path::new(p)));
            deps.push(Dep { kind, path });
        }
        out.push(Package {
            name: name.to_string(),
            dir: relative(root, dir),
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

/// `cargo metadata --format-version 1 --no-deps`, parsed as JSON.
fn load_metadata() -> Result<Value, String> {
    let output = run_cargo(&["metadata", "--format-version", "1", "--no-deps"])?;
    if !output.status.success() {
        return Err(format!(
            "`{} metadata` failed: {}",
            cargo_binary(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("invalid cargo metadata: {e}"))
}

/// The workspace package graph: metadata loaded and parsed in one call —
/// the composition every command spells.
pub(crate) fn workspace_graph() -> Result<Vec<Package>, String> {
    load_metadata().and_then(|m| parse_metadata(&m))
}
