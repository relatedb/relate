//! Guard the wasm bundle's npm manifest against the files the build
//! emits (REL8-1234). `scripts/build-wasm.sh` copies every file under
//! `surfaces/relate-wasm/js/` into the published `pkg/` bundle beside
//! the wasm-bindgen output, and stamps `pkg/package.json` from
//! `surfaces/relate-wasm/npm/package.json.template`. npm packs the
//! bundle by that manifest's `files` array and resolves imports by its
//! `exports` map, so three inventories must agree:
//!
//! - a `js/` helper the template does not list ships in `pkg/` but is
//!   dropped from the tarball;
//! - a `files` entry nothing produces packs a path no build emits — the
//!   exact regression the ticket reported, a listed `trap.d.ts`/
//!   `opfs.d.ts` absent from the published package, which left
//!   TypeScript consumers on implicit any;
//! - an `exports` subpath pointing at a file outside `files` resolves
//!   to an unpacked file, so the import fails for every consumer.
//!
//! The `js/` directory is the single inventory of hand-maintained files
//! (the build script copies the directory, not a list); every other
//! `files` entry must be accounted for by a producer the script names —
//! the package README stamped from `npm/README.md`, the wasm-bindgen
//! output, or the `snippets/` directory the glue imports.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{slash_path, walk_files};

/// The wasm surface, relative to the workspace root.
const WASM: &str = "surfaces/relate-wasm";
/// Files `wasm-bindgen --target web --out-name relate` emits into `pkg/`
/// beside the `js/` copies. The set is complete: a `files` entry naming
/// any other generated file is a claim this check cannot verify, so it
/// fails instead of assuming.
const BINDGEN_OUTPUTS: [&str; 4] = [
    "relate.js",
    "relate.d.ts",
    "relate_bg.wasm",
    "relate_bg.wasm.d.ts",
];
/// The one directory entry the manifest carries: the glue imports from
/// it and wasm-bindgen fills it, so it is produced by the build and
/// listed without a file name.
const SNIPPETS: &str = "snippets/";
/// The package README's `files` spelling — npm also packs it implicitly,
/// but the template lists it, and the build stamps `pkg/README.md` from
/// `npm/README.md`.
const PACKAGE_README: &str = "README.md";
/// The npm manifest template, relative to the wasm surface — the one
/// spelling, for the reader and for every diagnostic about it.
const MANIFEST: &str = "npm/package.json.template";

/// The parsed template. It is JSON except for the `__VERSION__`
/// placeholder, which never appears inside `files` or `exports`;
/// substituting it makes the document parseable without forgiving
/// anything else — a template that is not otherwise valid JSON is an
/// error, not a violation.
fn template(root: &Path) -> Result<serde_json::Value, String> {
    let path = root.join(WASM).join(MANIFEST);
    let raw = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&raw.replace("__VERSION__", "0.0.0"))
        .map_err(|e| format!("{}: not valid package JSON: {e}", path.display()))
}

/// The manifest's `files` array, as the paths it names — the same
/// element type the produced set uses, so every comparison below is one
/// equality over one spelling.
fn files_entries(manifest: &serde_json::Value) -> Result<BTreeSet<PathBuf>, String> {
    let array = manifest
        .get("files")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("{WASM}/{MANIFEST}: no `files` array"))?;
    array
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(PathBuf::from)
                .ok_or_else(|| format!("{WASM}/{MANIFEST}: a `files` entry is not a string"))
        })
        .collect()
}

/// One export target as the `files` entry it names. npm requires a
/// "./"-relative target, stripped here by components — so `.//x` and
/// `./x` both land on `x`, and a target npm would not resolve is the
/// error it is instead of silently becoming another spelling.
fn export_target(subpath: &str, file: &str) -> Result<PathBuf, String> {
    let relative = Path::new(file).strip_prefix("./").map_err(|_| {
        format!("{WASM}/{MANIFEST} `exports`['{subpath}']: '{file}' is not a './'-relative path")
    })?;
    if relative.as_os_str().is_empty() {
        return Err(format!(
            "{WASM}/{MANIFEST} `exports`['{subpath}']: '{file}' names no file"
        ));
    }
    Ok(relative.to_path_buf())
}

/// The manifest's `exports` map, as the distinct (subpath, target) edges
/// it declares — through a plain string value or through any of an
/// object's conditions (`types`, `default`, …). A set, so two conditions
/// naming the same file are one edge and render one violation; a
/// condition that is not a plain string is an error — the guard must be
/// taught that shape, not skip it.
fn export_targets(manifest: &serde_json::Value) -> Result<BTreeSet<(String, PathBuf)>, String> {
    let exports = manifest
        .get("exports")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| format!("{WASM}/{MANIFEST}: no `exports` map"))?;
    let mut targets = BTreeSet::new();
    for (subpath, value) in exports {
        match value {
            serde_json::Value::String(file) => {
                targets.insert((subpath.clone(), export_target(subpath, file)?));
            }
            serde_json::Value::Object(conditions) => {
                for condition in conditions.values() {
                    let file = condition.as_str().ok_or_else(|| {
                        format!(
                            "{WASM}/{MANIFEST} `exports`['{subpath}']: a condition is not a plain string"
                        )
                    })?;
                    targets.insert((subpath.clone(), export_target(subpath, file)?));
                }
            }
            _ => {
                return Err(format!(
                    "{WASM}/{MANIFEST} `exports`['{subpath}']: neither a string nor a conditions object"
                ))
            }
        }
    }
    Ok(targets)
}

/// The package README's source — the one spelling of that path, which
/// the check and the fixtures both read from here.
fn readme_source_path(root: &Path) -> PathBuf {
    root.join(WASM).join("npm/README.md")
}

/// Refuse a tree whose package README the build cannot stamp. Its
/// absence is this check's error class (exit 2), never a producer that
/// quietly drops out of the set: modelling it as optional would let a
/// manifest and a tree that both dropped it read clean while the build's
/// unconditional `cp` fails.
fn require_readme_source(root: &Path) -> Result<(), String> {
    let path = readme_source_path(root);
    match fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(format!(
            "{}: not a file — the build copies it into pkg/",
            path.display()
        )),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Every file under `js/`, keyed by its path relative to `js/` — the
/// inventory the build copies and the manifest must mirror. `PathBuf`
/// keys, not rendered strings, so two distinct names cannot collapse
/// into one entry; messages render through [`slash_path`].
fn js_tree(root: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let js = root.join(WASM).join("js");
    let mut files = BTreeSet::new();
    walk_files(&js, &mut |_path, rel| {
        files.insert(rel.to_path_buf());
        Ok(())
    })?;
    Ok(files)
}

/// The paths the build emits that are not the hand-maintained `js/`
/// helpers: the wasm-bindgen output, the `snippets/` directory the glue
/// imports, and the package README — which the build copies
/// unconditionally, so it is always in this set (`require_readme_source`
/// refuses a missing source before this is built). Kept apart from the
/// `js/` walk so each inventory's own loop carries its own diagnostic.
fn built() -> BTreeSet<PathBuf> {
    let mut built: BTreeSet<PathBuf> = BINDGEN_OUTPUTS.map(PathBuf::from).into();
    built.insert(PathBuf::from(SNIPPETS));
    built.insert(PathBuf::from(PACKAGE_README));
    built
}

/// Everything the build emits: the `js/` inventory the build script
/// copies wholesale, plus [`built`].
fn produced(js: &BTreeSet<PathBuf>) -> BTreeSet<PathBuf> {
    js.union(&built()).cloned().collect()
}

/// One violation per disagreement among the `js/` inventory, the
/// manifest's `files` array and its `exports` map; an unreadable or
/// unparsable template or `js/` tree is an error. Paths render through
/// [`slash_path`], the crate's one spelling.
pub fn check(root: &Path) -> Result<Vec<String>, String> {
    // Every inventory is read before any comparison: the manifest's
    // `files` and `exports`, the js/ tree, and the README source the
    // build copies.
    let manifest = template(root)?;
    let files = files_entries(&manifest)?;
    let exports = export_targets(&manifest)?;
    let js = js_tree(root)?;
    require_readme_source(root)?;
    let emitted = produced(&js);
    let mut violations = Vec::new();

    // What the build emits must be packed, each kind under its own
    // diagnostic: a js/ helper is the one kind with a subpath export to
    // keep in step, anything else is an artifact the tarball would lose.
    for path in js.difference(&files) {
        violations.push(format!(
            "{WASM}/js/{}: not in {WASM}/{MANIFEST} `files` — the bundle ships it, the \
             tarball drops it; list it (and its subpath export, when it has one) or \
             remove it from js/",
            slash_path(path)
        ));
    }
    for path in built().difference(&files) {
        violations.push(format!(
            "{WASM}/{MANIFEST} `files`: the build produces '{}' and the tarball would \
             lose it — list it",
            slash_path(path)
        ));
    }
    // What the manifest lists must be something the build emits.
    for path in files.difference(&emitted) {
        violations.push(format!(
            "{WASM}/{MANIFEST} `files`: '{}' names nothing the build emits",
            slash_path(path)
        ));
    }
    // The export map must point at packed files.
    for (subpath, target) in &exports {
        if !files.contains(target) {
            violations.push(format!(
                "{WASM}/{MANIFEST} `exports`['{subpath}'] points at '{}', not in `files` \
                 — the subpath resolves to a file the tarball does not pack",
                slash_path(target)
            ));
        }
    }
    Ok(violations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// A fixture workspace root holding `surfaces/relate-wasm/{js,npm}`,
    /// through the crate's one scratch-dir policy (a `TempDir`, removed
    /// on every exit path).
    fn fixture() -> tempfile::TempDir {
        let root = crate::scratch_dir().expect("scratch dir");
        let wasm = root.path().join(WASM);
        fs::create_dir_all(wasm.join("js")).expect("fixture js/");
        fs::create_dir_all(wasm.join("npm")).expect("fixture npm/");
        root
    }

    /// Write the js/ tree.
    fn write_js(root: &tempfile::TempDir, js_files: &[&str]) {
        for name in js_files {
            fs::write(root.path().join(WASM).join("js").join(name), "").expect("fixture js file");
        }
    }

    /// Write the package README the build copies.
    fn write_readme(root: &tempfile::TempDir) {
        fs::write(readme_source_path(root.path()), "").expect("fixture README");
    }

    /// Write the fixture manifest verbatim — for the error cases whose
    /// text is the state under test. The one manifest write path, shared
    /// with the builder below.
    fn write_manifest_text(root: &tempfile::TempDir, text: &str) {
        fs::write(root.path().join(WASM).join(MANIFEST), text).expect("fixture manifest write");
    }

    /// Write the fixture manifest: the given `files` array and export
    /// map, built as JSON — a structural fixture, so a mutation that
    /// misses is an error at the builder, not a silent no-op.
    fn write_manifest(root: &tempfile::TempDir, files: &[&str], exports: Value) {
        let manifest = json!({
            "name": "@relate/relate-wasm",
            "version": "0.0.0",
            "files": files,
            "exports": exports,
        });
        write_manifest_text(
            root,
            &serde_json::to_string_pretty(&manifest).expect("fixture manifest"),
        );
    }

    /// The export map shape the real template carries: the typed root,
    /// a typed subpath export per js/ helper, and the plain-string wasm
    /// entry.
    fn typed_exports(helpers: &[&str]) -> Value {
        let mut exports = serde_json::Map::new();
        exports.insert(
            ".".to_owned(),
            json!({"types": "./relate.d.ts", "default": "./relate.js"}),
        );
        for helper in helpers {
            let types = format!("./{}.d.ts", helper.strip_suffix(".js").unwrap_or(helper));
            exports.insert(
                format!("./{helper}"),
                json!({"types": types, "default": format!("./{helper}")}),
            );
        }
        exports.insert("./relate_bg.wasm".to_owned(), json!("./relate_bg.wasm"));
        Value::Object(exports)
    }

    /// The hand-maintained bundle JS, the js/ inventory the fixtures
    /// write and the build copies.
    const JS_HELPERS: [&str; 4] = ["trap.js", "trap.d.ts", "opfs.js", "opfs.d.ts"];

    /// The clean fixture's `files` array: everything the js/ tree, the
    /// wasm-bindgen output, the snippets directory and the package
    /// README contribute.
    const CLEAN_FILES: [&str; 10] = [
        "relate.js",
        "relate.d.ts",
        "relate_bg.wasm",
        "relate_bg.wasm.d.ts",
        "trap.js",
        "trap.d.ts",
        "opfs.js",
        "opfs.d.ts",
        "snippets/",
        "README.md",
    ];

    /// `CLEAN_FILES` minus one entry — the mutation fixtures differ only
    /// in which entry they drop. An entry that is not in the list is a
    /// typo and fails here, not a fixture that silently stops mutating.
    fn clean_files_without(entry: &str) -> Vec<&'static str> {
        assert!(
            CLEAN_FILES.contains(&entry),
            "not a CLEAN_FILES entry: {entry}"
        );
        CLEAN_FILES
            .into_iter()
            .filter(|candidate| *candidate != entry)
            .collect()
    }

    #[test]
    fn a_tree_and_manifest_that_agree_is_clean() {
        let root = fixture();
        write_js(&root, &JS_HELPERS);
        write_readme(&root);
        write_manifest(&root, &CLEAN_FILES, typed_exports(&["trap.js", "opfs.js"]));
        assert_eq!(check(root.path()), Ok(Vec::new()));
    }

    #[test]
    fn a_js_helper_missing_from_files_is_a_violation() {
        let root = fixture();
        write_js(&root, &JS_HELPERS);
        write_readme(&root);
        // trap.js keeps no subpath export here (a js file need not have
        // one), so the one violation is the files-side drift alone.
        write_manifest(
            &root,
            &clean_files_without("trap.d.ts"),
            typed_exports(&["opfs.js"]),
        );
        let violations = check(root.path()).expect("check ran");
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].contains("js/trap.d.ts"), "{violations:?}");
    }

    #[test]
    fn a_files_entry_nothing_produces_is_a_violation() {
        // The regression the ticket reported: `trap.d.ts` and `opfs.d.ts`
        // listed while the build emitted no such files — here simulated
        // by listing an entry with no js/ counterpart.
        let root = fixture();
        write_js(&root, &JS_HELPERS);
        write_readme(&root);
        let mut files = CLEAN_FILES.to_vec();
        files.push("ghost.js");
        write_manifest(&root, &files, typed_exports(&["trap.js", "opfs.js"]));
        let violations = check(root.path()).expect("check ran");
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].contains("'ghost.js'"), "{violations:?}");
    }

    #[test]
    fn a_produced_file_missing_from_files_is_a_violation() {
        // The other direction: the build emits it, the manifest drops it
        // — the tarball would ship a bundle the glue cannot import from.
        // `snippets/` carries no export subpath, so the one violation is
        // the packing one.
        let root = fixture();
        write_js(&root, &JS_HELPERS);
        write_readme(&root);
        write_manifest(
            &root,
            &clean_files_without("snippets/"),
            typed_exports(&["trap.js", "opfs.js"]),
        );
        let violations = check(root.path()).expect("check ran");
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations[0].contains("the build produces 'snippets/'"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_missing_readme_source_is_an_error() {
        // The README is not an optional producer: the build copies
        // npm/README.md unconditionally, so a tree that dropped it — and
        // a manifest that dropped its entry with it — must not read as
        // clean.
        let root = fixture();
        // No write_readme: the source is the state under test.
        write_js(&root, &JS_HELPERS);
        write_manifest(
            &root,
            &clean_files_without("README.md"),
            typed_exports(&["trap.js", "opfs.js"]),
        );
        let error = check(root.path()).expect_err("a missing README source is an error");
        assert!(error.contains("npm/README.md"), "{error}");
    }

    #[test]
    fn an_export_target_that_is_not_relative_is_an_error() {
        // npm resolves only "./"-relative export targets; a bare name is
        // the template's mistake, not a file to look up.
        let root = fixture();
        write_js(&root, &JS_HELPERS);
        write_readme(&root);
        write_manifest(
            &root,
            &CLEAN_FILES,
            json!({ "./trap.js": { "types": "trap.d.ts", "default": "./trap.js" } }),
        );
        let error = check(root.path()).expect_err("a bare export target is an error");
        assert!(error.contains("is not a './'-relative path"), "{error}");
    }

    #[test]
    fn an_export_pointing_outside_files_is_a_violation() {
        // The subpath typing the ticket was about: the export names a
        // .d.ts the tarball would not pack.
        let root = fixture();
        write_js(&root, &JS_HELPERS);
        write_readme(&root);
        write_manifest(
            &root,
            &clean_files_without("trap.d.ts"),
            typed_exports(&["trap.js", "opfs.js"]),
        );
        let violations = check(root.path()).expect("check ran");
        assert_eq!(violations.len(), 2, "{violations:?}");
        assert!(
            violations
                .iter()
                .any(|v| v.contains("`exports`['./trap.js'] points at 'trap.d.ts'")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_manifest_without_files_is_an_error() {
        let root = fixture();
        // An `exports` map is present, so the failure is unambiguously
        // the missing `files` rather than an incidental read order.
        write_manifest_text(&root, r#"{ "name": "@relate/relate-wasm", "exports": {} }"#);
        let error = check(root.path()).expect_err("a manifest without `files` is an error");
        assert!(error.contains("`files` array"), "{error}");
    }

    #[test]
    fn a_manifest_that_is_not_json_is_an_error() {
        let root = fixture();
        write_manifest_text(&root, "name: not json");
        let error = check(root.path()).expect_err("a non-JSON manifest is an error");
        assert!(error.contains("not valid package JSON"), "{error}");
    }

    #[test]
    fn the_repository_tree_is_clean() {
        // The guard's premise, asserted against the real tree: the
        // shipped template, its export map and the js/ inventory agree
        // today, so the check is load-bearing from the moment it lands,
        // not from the first future drift.
        assert_eq!(check(&crate::workspace_root()), Ok(Vec::new()));
    }
}
