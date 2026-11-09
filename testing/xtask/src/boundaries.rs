use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path};

use serde_json::Value;

const TEST_SUPPORT_DIR: &str = "testing/relate-test-support";
/// The wire kit (ADR-0430's shared half: the bounded reads and the v1
/// wire facts) — the one `extensions/*` package a surface may link (the
/// CLI host's own dep); the importer children are spawn-only, never
/// linkable into a product binary (ADR-0431 §11).
const WIRE_KIT_DIR: &str = "extensions/relate-wire";
/// The engine leaf crates an `extensions/*` child may link (ADR-0434: "the
/// engine's leaf crates only — a first-party child carries no `Database`").
/// Every other `relate/*` member — the `Database` crate itself above
/// all — is an R7 refusal: a child linking it is exactly the direction the
/// group exists to prevent. `relate-core` is the one engine reach today
/// (the temporal parser the git child uses).
/// Write-engine crates own durability. They talk to a query engine only
/// through `relate-engine-api` (GraphExport / GraphImport), so a normal or
/// build edge onto a query-engine crate is an R8 refusal.
const WRITE_ENGINE_DIRS: &[&str] = &["relate/relate-storage", "relate/relate-write-local"];
/// Query-engine crates own execution and the live graph layout.
const QUERY_ENGINE_DIRS: &[&str] = &["relate/relate-executor"];

const LEAF_ENGINE_DIRS: &[&str] = &[
    "relate/relate-core",
    "relate/relate-model",
    "relate/relate-error",
    "relate/relate-parser",
    "relate/relate-splitter",
    "relate/relate-catalog",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DepKind {
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
pub struct Dep {
    pub kind: DepKind,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub dir: String,
    pub deps: Vec<Dep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Engine,
    Surfaces,
    Testing,
    /// The first-party extension crates (ADR-0431 §11): the importer
    /// children — subprocesses the CLI host spawns, never libraries the
    /// engine or a surface links into a product binary — and the wire kit,
    /// the one group member a surface links on the product path
    /// (`WIRE_KIT_DIR` above).
    Extensions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    R1,
    R2,
    R3,
    /// `extensions/*` dependency directions (ADR-0431 §11's placement
    /// rules): a first-party extension child may reach the engine's leaf
    /// crates and its own group's kit, and no further — never a surface
    /// (the host is not linkable into a child, ADR-0430 §6's trust shape)
    /// and never the testing group beyond test-support dev deps.
    R7,
    R5,
    /// A write-engine crate (the storage toolkit and a local or future
    /// clustering write engine) must not take a normal or build dependency
    /// on a query-engine crate. The shared contracts live in
    /// `relate-engine-api`. A dev dependency is allowed so the toolkit's
    /// tests can build the in-memory graph.
    R8,
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Rule::R1 => "R1",
            Rule::R2 => "R2",
            Rule::R3 => "R3",
            Rule::R7 => "R7",
            Rule::R5 => "R5",
            Rule::R8 => "R8",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    Edge {
        from: String,
        to: String,
        kind: DepKind,
        rule: Rule,
    },
    Misplaced {
        package: String,
        dir: String,
    },
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Violation::Edge {
                from,
                to,
                kind,
                rule,
            } => write!(f, "{from} -> {to} ({kind}): {rule}"),
            Violation::Misplaced { package, dir } => {
                write!(f, "{package} ({dir}): {}", Rule::R5)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub packages: usize,
    pub edges: usize,
    pub violations: Vec<Violation>,
}

pub fn parse_metadata(metadata: &Value) -> Result<Vec<Package>, String> {
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

fn classify(dir: &str) -> Option<Group> {
    let mut parts = dir.split('/');
    let group = match parts.next()? {
        "relate" => Group::Engine,
        "surfaces" => Group::Surfaces,
        "testing" => Group::Testing,
        "extensions" => Group::Extensions,
        _ => return None,
    };
    match (parts.next(), parts.next()) {
        (Some(name), None) if !name.is_empty() => Some(group),
        _ => None,
    }
}

fn rule_for(from: Group, kind: DepKind, to: Group, to_dir: &str) -> Option<Rule> {
    let test_support = to_dir == TEST_SUPPORT_DIR;
    // The one extensions-group package a surface may link: the wire kit.
    let linkable_extension = to_dir == WIRE_KIT_DIR;
    // The engine leaf crates an extensions child may reach (ADR-0434).
    let leaf_engine = to == Group::Engine && LEAF_ENGINE_DIRS.contains(&to_dir);
    match (from, kind) {
        (Group::Engine, DepKind::Normal | DepKind::Build) => {
            (to != Group::Engine).then_some(Rule::R1)
        }
        (Group::Engine, DepKind::Dev) => (to != Group::Engine && !test_support).then_some(Rule::R2),
        // Surfaces reach the extensions group for the wire kit only on
        // the product path (ADR-0431 §11): a surface linking an importer
        // child would re-create the compiled-in importer surface the
        // extraction retired. Dev deps are test ties — the e2e suites'
        // pins against the importer crates' own constants — and may
        // reach any member, because a dev edge never ships in a binary.
        (Group::Surfaces, DepKind::Normal | DepKind::Build) => (to == Group::Testing
            || (to == Group::Extensions && !linkable_extension))
            .then_some(Rule::R3),
        (Group::Surfaces, DepKind::Dev) => {
            (to == Group::Testing && !test_support).then_some(Rule::R3)
        }
        // First-party children reach the engine's LEAF crates (the
        // Database-free set, `LEAF_ENGINE_DIRS`) and their own group's
        // kit, nothing further (ADR-0434).
        (Group::Extensions, DepKind::Normal | DepKind::Build) => {
            (!(leaf_engine || to == Group::Extensions)).then_some(Rule::R7)
        }
        (Group::Extensions, DepKind::Dev) => {
            (!(leaf_engine || to == Group::Extensions || test_support)).then_some(Rule::R7)
        }
        (Group::Testing, _) => None,
    }
}

pub fn check(packages: &[Package]) -> Report {
    let by_dir: BTreeMap<&str, &Package> = packages.iter().map(|p| (p.dir.as_str(), p)).collect();
    let mut violations = Vec::new();
    let mut edges = BTreeSet::new();

    for package in packages {
        let from_group = classify(&package.dir);
        if from_group.is_none() {
            violations.push(Violation::Misplaced {
                package: package.name.clone(),
                dir: package.dir.clone(),
            });
        }
        for dep in &package.deps {
            let Some(target) = dep.path.as_deref().and_then(|p| by_dir.get(p)) else {
                continue;
            };
            if edges.insert((package.name.as_str(), target.name.as_str(), dep.kind)) {
                let (Some(from), Some(to)) = (from_group, classify(&target.dir)) else {
                    continue;
                };
                if let Some(rule) = rule_for(from, dep.kind, to, &target.dir) {
                    violations.push(Violation::Edge {
                        from: package.name.clone(),
                        to: target.name.clone(),
                        kind: dep.kind,
                        rule,
                    });
                }
                if WRITE_ENGINE_DIRS.contains(&package.dir.as_str())
                    && QUERY_ENGINE_DIRS.contains(&target.dir.as_str())
                    && matches!(dep.kind, DepKind::Normal | DepKind::Build)
                {
                    violations.push(Violation::Edge {
                        from: package.name.clone(),
                        to: target.name.clone(),
                        kind: dep.kind,
                        rule: Rule::R8,
                    });
                }
            }
        }
    }

    Report {
        packages: packages.len(),
        edges: edges.len(),
        violations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(dir: &str, deps: &[(&str, DepKind)]) -> Package {
        Package {
            name: dir.rsplit('/').next().unwrap().to_string(),
            dir: dir.to_string(),
            deps: deps
                .iter()
                .map(|(path, kind)| Dep {
                    kind: *kind,
                    path: Some(path.to_string()),
                })
                .collect(),
        }
    }

    fn workspace(extra: Package) -> Vec<Package> {
        vec![
            pkg("relate/relate-core", &[]),
            pkg("relate/relate", &[("relate/relate-core", DepKind::Normal)]),
            pkg("relate/relate-executor", &[]),
            pkg("surfaces/relate-formatter", &[]),
            // The wire kit and the importer children (ADR-0431 §11): the
            // extensions group's members.
            pkg("extensions/relate-wire", &[]),
            pkg(
                "extensions/relate-git-to-graph",
                &[
                    ("extensions/relate-wire", DepKind::Normal),
                    ("relate/relate-core", DepKind::Normal),
                ],
            ),
            pkg("testing/relate-test-support", &[]),
            pkg("testing/relate-query-latency", &[]),
            extra,
        ]
    }

    fn violations(extra: Package) -> Vec<String> {
        check(&workspace(extra))
            .violations
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn engine_normal_dep_on_surface_is_rejected() {
        let v = violations(pkg(
            "relate/relate-storage",
            &[("surfaces/relate-formatter", DepKind::Normal)],
        ));
        assert_eq!(v, ["relate-storage -> relate-formatter (normal): R1"]);
    }

    #[test]
    fn engine_build_dep_on_testing_is_rejected() {
        let v = violations(pkg(
            "relate/relate-storage",
            &[("testing/relate-test-support", DepKind::Build)],
        ));
        assert_eq!(v, ["relate-storage -> relate-test-support (build): R1"]);
    }

    #[test]
    fn write_engine_normal_dep_on_query_engine_is_rejected() {
        let v = violations(pkg(
            "relate/relate-storage",
            &[("relate/relate-executor", DepKind::Normal)],
        ));
        assert_eq!(v, ["relate-storage -> relate-executor (normal): R8"]);
    }

    #[test]
    fn write_engine_dev_dep_on_query_engine_is_allowed() {
        let v = violations(pkg(
            "relate/relate-storage",
            &[("relate/relate-executor", DepKind::Dev)],
        ));
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn engine_dev_dep_on_test_support_is_allowed() {
        let v = violations(pkg(
            "relate/relate-storage",
            &[
                ("testing/relate-test-support", DepKind::Dev),
                ("relate/relate-core", DepKind::Dev),
            ],
        ));
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn engine_dev_dep_on_other_testing_package_is_rejected() {
        let v = violations(pkg(
            "relate/relate-storage",
            &[("testing/relate-query-latency", DepKind::Dev)],
        ));
        assert_eq!(v, ["relate-storage -> relate-query-latency (dev): R2"]);
    }

    #[test]
    fn engine_dev_dep_on_surface_is_rejected() {
        let v = violations(pkg(
            "relate/relate-storage",
            &[("surfaces/relate-formatter", DepKind::Dev)],
        ));
        assert_eq!(v, ["relate-storage -> relate-formatter (dev): R2"]);
    }

    #[test]
    fn surface_normal_dep_on_testing_is_rejected() {
        let v = violations(pkg(
            "surfaces/relate-cli",
            &[("testing/relate-test-support", DepKind::Normal)],
        ));
        assert_eq!(v, ["relate-cli -> relate-test-support (normal): R3"]);
    }

    #[test]
    fn surface_deps_on_engine_and_surfaces_are_allowed() {
        let v = violations(pkg(
            "surfaces/relate-cli",
            &[
                ("relate/relate", DepKind::Normal),
                ("surfaces/relate-formatter", DepKind::Build),
                ("testing/relate-test-support", DepKind::Dev),
            ],
        ));
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn surface_dev_dep_on_other_testing_package_is_rejected() {
        let v = violations(pkg(
            "surfaces/relate-cli",
            &[("testing/relate-query-latency", DepKind::Dev)],
        ));
        assert_eq!(v, ["relate-cli -> relate-query-latency (dev): R3"]);
    }

    #[test]
    fn testing_dep_on_surface_is_allowed() {
        let v = violations(pkg(
            "testing/relate-datasets",
            &[
                ("surfaces/relate-formatter", DepKind::Normal),
                ("relate/relate", DepKind::Build),
                ("testing/relate-query-latency", DepKind::Dev),
            ],
        ));
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn member_outside_the_three_folders_is_rejected() {
        let v = violations(pkg("legacy/relate-old", &[]));
        assert_eq!(v, ["relate-old (legacy/relate-old): R5"]);
    }

    #[test]
    fn member_nested_too_deep_is_rejected() {
        let v = violations(pkg("surfaces/nested/relate-deep", &[]));
        assert_eq!(v, ["relate-deep (surfaces/nested/relate-deep): R5"]);
    }

    #[test]
    fn duplicate_edges_are_counted_once() {
        let report = check(&workspace(pkg(
            "relate/relate-storage",
            &[
                ("surfaces/relate-formatter", DepKind::Normal),
                ("surfaces/relate-formatter", DepKind::Normal),
            ],
        )));
        assert_eq!(report.packages, 9);
        // The fixture's own edges (relate→core, git-to-graph→wire and
        // →core) plus the deduplicated storage→formatter pair.
        assert_eq!(report.edges, 4);
        assert_eq!(report.violations.len(), 1);
    }

    // --- the extensions group's placement rules (ADR-0431 §11) ---

    /// A first-party child reaches the engine's leaf crates and its own
    /// group's kit — the wire kit and the sibling importer's lib — and
    /// nothing further.
    #[test]
    fn extension_children_reach_the_engine_and_their_group() {
        let v = violations(pkg(
            "extensions/relate-fs-to-graph",
            &[
                ("relate/relate-core", DepKind::Normal),
                ("extensions/relate-wire", DepKind::Normal),
                ("extensions/relate-git-to-graph", DepKind::Normal),
                ("testing/relate-test-support", DepKind::Dev),
            ],
        ));
        assert!(v.is_empty(), "{v:?}");
    }

    /// A first-party child may not link a surface (the host is not
    /// linkable into a child, ADR-0430 §6) nor a testing package beyond
    /// test support.
    #[test]
    fn extension_children_never_link_surfaces_or_testing() {
        for (target, name, kind, rule) in [
            (
                "surfaces/relate-formatter",
                "relate-formatter",
                DepKind::Normal,
                "R7",
            ),
            (
                "surfaces/relate-formatter",
                "relate-formatter",
                DepKind::Build,
                "R7",
            ),
            (
                "surfaces/relate-formatter",
                "relate-formatter",
                DepKind::Dev,
                "R7",
            ),
            (
                "testing/relate-query-latency",
                "relate-query-latency",
                DepKind::Dev,
                "R7",
            ),
            (
                "testing/relate-query-latency",
                "relate-query-latency",
                DepKind::Normal,
                "R7",
            ),
            // The engine's Database crate and its non-leaf internals are
            // refusals too (ADR-0434: leaf crates only — a child carries
            // no Database).
            ("relate/relate", "relate", DepKind::Normal, "R7"),
            (
                "relate/relate-executor",
                "relate-executor",
                DepKind::Normal,
                "R7",
            ),
        ] {
            let v = violations(pkg("extensions/relate-fs-to-graph", &[(target, kind)]));
            assert_eq!(
                v,
                [format!("relate-fs-to-graph -> {name} ({kind}): {rule}")],
                "{target} ({kind})"
            );
        }
    }

    /// The one extensions-group package a surface links on the product
    /// path is the wire kit (the CLI host's dep); a surface linking an
    /// importer child would re-create the compiled-in importer surface
    /// the extraction retired. Dev deps are test ties — pins against the
    /// importer crates' own constants — and may reach any member.
    #[test]
    fn surfaces_link_only_the_wire_kit_from_the_extensions_group() {
        let v = violations(pkg(
            "surfaces/relate-cli",
            &[
                ("extensions/relate-wire", DepKind::Normal),
                ("extensions/relate-git-to-graph", DepKind::Dev),
            ],
        ));
        assert!(v.is_empty(), "{v:?}");
        for kind in [DepKind::Normal, DepKind::Build] {
            let v = violations(pkg(
                "surfaces/relate-cli",
                &[("extensions/relate-git-to-graph", kind)],
            ));
            assert_eq!(
                v,
                [format!("relate-cli -> relate-git-to-graph ({kind}): R3")],
                "{kind}"
            );
        }
    }

    #[test]
    fn an_extensions_member_nested_too_deep_is_rejected() {
        let v = violations(pkg("extensions/nested/relate-deep", &[]));
        assert_eq!(v, ["relate-deep (extensions/nested/relate-deep): R5"]);
    }

    #[test]
    fn parses_cargo_metadata_json() {
        let metadata = serde_json::json!({
            "workspace_root": "/ws",
            "workspace_members": ["path+file:///ws/relate/a#0.1.0", "path+file:///ws/surfaces/b#0.1.0"],
            "packages": [
                {
                    "id": "path+file:///ws/relate/a#0.1.0",
                    "name": "a",
                    "manifest_path": "/ws/relate/a/Cargo.toml",
                    "dependencies": [
                        { "name": "b", "kind": null, "path": "/ws/surfaces/b" },
                        { "name": "serde_json", "kind": null },
                        { "name": "b", "kind": "dev", "path": "/ws/surfaces/b" }
                    ]
                },
                {
                    "id": "path+file:///ws/surfaces/b#0.1.0",
                    "name": "b",
                    "manifest_path": "/ws/surfaces/b/Cargo.toml",
                    "dependencies": [
                        { "name": "a", "kind": "build", "path": "/ws/relate/a" }
                    ]
                },
                {
                    "id": "registry+https://example/serde_json#1.0.0",
                    "name": "serde_json",
                    "manifest_path": "/registry/serde_json/Cargo.toml",
                    "dependencies": []
                }
            ]
        });
        let packages = parse_metadata(&metadata).unwrap();
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].dir, "relate/a");
        assert_eq!(
            packages[0].deps[0],
            Dep {
                kind: DepKind::Normal,
                path: Some("surfaces/b".to_string())
            }
        );
        let report = check(&packages);
        assert_eq!(report.edges, 3);
        let v: Vec<String> = report.violations.iter().map(ToString::to_string).collect();
        assert_eq!(v, ["a -> b (normal): R1", "a -> b (dev): R2"]);
    }
}
