//! The patched extracted manifest: the one `[patch.crates-io]` table
//! the check runs under, and its TOML quoting.

use std::fs;
use std::path::Path;

use crate::workspace::{cargo_arg, Package};

/// One TOML basic-string writer for the patch table's paths: control
/// characters are refused (a basic string cannot carry them), and
/// backslash and double quote — the two characters that end one — are
/// escaped. The only quoted values in the table are these paths.
pub(super) fn toml_basic_string(value: &str) -> Result<String, String> {
    if value.chars().any(char::is_control) {
        return Err(format!(
            "a patch path contains a control character a TOML basic string cannot carry: {value:?}"
        ));
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

/// Render the `[patch.crates-io]` table mapping every closure member to
/// its workspace path. Paths are absolute — the scratch directory the
/// manifest is read from moves with every run — and quoted by the one
/// TOML basic-string writer.
pub(super) fn patch_manifest(root: &Path, closure: &[&Package]) -> Result<String, String> {
    let mut out = String::from("\n[patch.crates-io]\n");
    for package in closure {
        let path = root.join(&package.dir);
        out.push_str(&format!(
            "{} = {{ path = {} }}\n",
            package.name,
            toml_basic_string(cargo_arg(&path)?)?
        ));
    }
    Ok(out)
}

/// Whether the manifest already carries a `[patch]` table: a table
/// header, or the dotted `patch.` key form. A comment or string that
/// merely mentions the word is not a table.
fn carries_patch_table(manifest: &str) -> bool {
    manifest.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("[patch") || line.starts_with("patch.")
    })
}

/// Read the extracted manifest, refusing one that already carries a
/// `[patch]` table: a second table would corrupt what packaging wrote.
pub(super) fn read_extracted_manifest(manifest: &Path) -> Result<String, String> {
    let current = fs::read_to_string(manifest)
        .map_err(|e| format!("failed to read {}: {e}", manifest.display()))?;
    if carries_patch_table(&current) {
        return Err(format!(
            "{} already carries a [patch] table",
            manifest.display()
        ));
    }
    Ok(current)
}

/// Write the extracted manifest with the one `[patch.crates-io]` table
/// the check runs under — the original packaging wrote, untouched, plus
/// the table.
pub(super) fn write_patched_manifest(
    manifest: &Path,
    original: &str,
    patch: &str,
) -> Result<(), String> {
    fs::write(manifest, format!("{original}{patch}"))
        .map_err(|e| format!("failed to write {}: {e}", manifest.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_check::fixtures::pkg;
    use crate::package_check::scratch_dir;

    #[test]
    fn patch_manifest_maps_every_closure_member_to_its_workspace_path() {
        let root = Path::new("/ws");
        let core = pkg("relate-core", "relate/relate-core", &[]);
        let model = pkg("relate-model", "relate/relate-model", &[]);
        let closure = vec![&core, &model];
        let manifest = patch_manifest(root, &closure).unwrap();
        for (name, dir) in [
            ("relate-core", "relate/relate-core"),
            ("relate-model", "relate/relate-model"),
        ] {
            let line = format!(
                "{name} = {{ path = {} }}\n",
                toml_basic_string(root.join(dir).to_str().unwrap()).unwrap()
            );
            assert!(manifest.contains(&line), "missing {line}in:\n{manifest}");
        }
        // The header plus one line per closure member, nothing else.
        assert_eq!(manifest.lines().count(), closure.len() + 2);
    }

    #[test]
    fn toml_basic_strings_escape_the_two_string_enders_and_refuse_controls() {
        assert_eq!(toml_basic_string("/ws/a\"b").unwrap(), "\"/ws/a\\\"b\"");
        assert_eq!(toml_basic_string("/ws/a\\b").unwrap(), "\"/ws/a\\\\b\"");
        assert!(toml_basic_string("/ws/a\u{7}b").is_err());
    }

    #[test]
    fn the_patch_table_guard_matches_headers_not_mentions() {
        assert!(!carries_patch_table(
            "# a description mentioning [patch] in prose\n[package]\nname = \"relate\"\n"
        ));
        assert!(carries_patch_table(
            "[package]\nname = \"relate\"\n[patch.crates-io]\n"
        ));
        assert!(carries_patch_table("  [patch]\n"));
        assert!(carries_patch_table(
            "patch.crates-io.relate-core = { path = \"/ws/relate-core\" }\n"
        ));
    }

    #[test]
    fn writes_the_manifest_with_the_table_once() {
        let scratch = scratch_dir().unwrap();
        let manifest = scratch.path().join("Cargo.toml");
        fs::write(&manifest, "[package]\nname = \"relate\"\n").unwrap();
        let original = read_extracted_manifest(&manifest).unwrap();
        write_patched_manifest(&manifest, &original, "\n[patch.crates-io]\n").unwrap();
        let written = fs::read_to_string(&manifest).unwrap();
        assert_eq!(
            written,
            "[package]\nname = \"relate\"\n\n[patch.crates-io]\n"
        );
        assert!(read_extracted_manifest(&manifest).is_err());
    }
}
