//! The build's `--message-format=json` stream, typed: the evidence the
//! verdict reads, the diagnostics a failure prints, and the verdict
//! itself.

use std::path::{Path, PathBuf};

use crate::workspace::Package;

/// The typed facts one patched build's `--message-format=json` stream
/// carries: every compiler artifact's manifest path, and how many of
/// the artifacts were example targets. One evidence channel for the
/// green — no prose, no substring match on cargo's wording.
#[derive(Debug)]
pub(super) struct BuildEvidence {
    /// The manifest path of every `compiler-artifact` record.
    pub(super) artifact_manifests: Vec<PathBuf>,
    /// How many artifacts were example targets.
    pub(super) examples: usize,
}

/// One `--message-format=json` line, typed: the artifact records the
/// verdict reads, the diagnostics a failure prints, and the protocol's
/// known non-artifact records. There is no catch-all: a reason cargo
/// does not document is the deserialize error this type promises,
/// never a silent skip.
#[derive(serde::Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
enum CargoMessage {
    CompilerArtifact {
        manifest_path: PathBuf,
        target: ArtifactTarget,
    },
    CompilerMessage {
        message: Diagnostic,
    },
    BuildScriptExecuted,
    BuildFinished,
}

/// The target block of a compiler artifact; only the kinds are read.
#[derive(serde::Deserialize)]
struct ArtifactTarget {
    kind: Vec<TargetKind>,
}

/// A target kind, typed at the wire boundary: the example count never
/// matches cargo's raw strings.
#[derive(serde::Deserialize)]
#[serde(from = "String")]
enum TargetKind {
    Example,
    Other,
}

impl From<String> for TargetKind {
    fn from(kind: String) -> Self {
        if kind == "example" {
            Self::Example
        } else {
            Self::Other
        }
    }
}

/// One compiler diagnostic as cargo renders it, ready to print.
#[derive(serde::Deserialize)]
struct Diagnostic {
    rendered: String,
}

/// One `--message-format=json` line as its typed record. Blank lines
/// never reach here; anything else that does not deserialize is the
/// tool breaking its protocol, refused by name — never skipped.
fn parse_cargo_message(line: &str) -> Result<CargoMessage, String> {
    serde_json::from_str(line).map_err(|e| {
        format!(
            "cargo emitted a line that is not the JSON --message-format=json promises: {e}: {line}"
        )
    })
}

/// The stream's records. The protocol's line shape lives here once:
/// blank lines are skipped, every other line must deserialize. The
/// evidence parser and the failure-diagnostics reader both fold over
/// this — neither re-spells the walk.
fn cargo_records(stdout: &str) -> Result<Vec<CargoMessage>, String> {
    let mut records = Vec::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        records.push(parse_cargo_message(line)?);
    }
    Ok(records)
}

/// The verdict's evidence: every `compiler-artifact` record's manifest,
/// and how many of them were example targets. Records that are not
/// artifacts (build scripts' `build-script-executed`, `compiler-message`,
/// `build-finished`) carry no artifact evidence and are skipped.
pub(super) fn parse_build_evidence(stdout: &str) -> Result<BuildEvidence, String> {
    let mut artifact_manifests = Vec::new();
    let mut examples = 0;
    for record in cargo_records(stdout)? {
        let CargoMessage::CompilerArtifact {
            manifest_path,
            target,
        } = record
        else {
            continue;
        };
        if target
            .kind
            .iter()
            .any(|kind| matches!(kind, TargetKind::Example))
        {
            examples += 1;
        }
        artifact_manifests.push(manifest_path);
    }
    Ok(BuildEvidence {
        artifact_manifests,
        examples,
    })
}

/// The rendered compiler diagnostics a *failed* build's JSON stream
/// carries — the half cargo writes to stdout, where stderr holds only
/// its summary. Strict on the protocol, like the evidence parser: an
/// unreadable stream is an error the caller annotates onto the build
/// failure, never silently swallowed.
pub(super) fn build_diagnostics(stdout: &[u8]) -> Result<Vec<String>, String> {
    let stdout = std::str::from_utf8(stdout)
        .map_err(|e| format!("the failed build's JSON stream is not valid UTF-8: {e}"))?;
    let mut diagnostics = Vec::new();
    for record in cargo_records(stdout)? {
        if let CargoMessage::CompilerMessage { message } = record {
            diagnostics.push(message.rendered);
        }
    }
    Ok(diagnostics)
}

/// The verdict: every patched source must have been compiled from the
/// workspace. Each closure member's workspace manifest — and the
/// extracted facade's own — must be among the artifacts' manifest paths;
/// a `[patch.crates-io]` entry cargo did not apply compiles its crate
/// from the registry cache instead, and that path (or the missing
/// artifact) is the failure named here.
pub(super) fn verify_workspace_sources(
    evidence: &BuildEvidence,
    root: &Path,
    closure: &[&Package],
    facade_manifest: &Path,
) -> Result<(), String> {
    let mut wanted: Vec<PathBuf> = closure
        .iter()
        .map(|package| root.join(&package.dir).join("Cargo.toml"))
        .collect();
    wanted.push(facade_manifest.to_path_buf());
    for manifest in &wanted {
        if !evidence.artifact_manifests.contains(manifest) {
            return Err(format!(
                "the patched build did not compile {} from the workspace — a [patch.crates-io] entry went unused and the crate came from the registry, or was not compiled at all",
                manifest.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_check::fixtures::{
        artifact_record, verification_closure, verification_evidence,
    };

    #[test]
    fn build_evidence_collects_artifacts_counts_examples_and_skips_non_artifacts() {
        let stdout = format!(
            "{}\n\
             {}\n\
             {}\n\
             {}\n\
             {}\n\
             {}\n",
            serde_json::json!({
                "reason": "compiler-message",
                "message": { "rendered": "a warning" },
            }),
            "",
            artifact_record("/ws/relate/relate-core/Cargo.toml", "lib"),
            artifact_record("/tmp/pkg/relate-1.0.0/Cargo.toml", "example"),
            serde_json::json!({ "reason": "build-script-executed", "package_id": "x", "env": [] }),
            serde_json::json!({ "reason": "build-finished", "success": true }),
        );
        let evidence = parse_build_evidence(&stdout).unwrap();
        assert_eq!(evidence.artifact_manifests.len(), 2);
        assert!(evidence
            .artifact_manifests
            .contains(&PathBuf::from("/tmp/pkg/relate-1.0.0/Cargo.toml")));
        assert_eq!(evidence.examples, 1);
    }

    #[test]
    fn a_non_json_line_is_refused_never_skipped() {
        let stdout = format!(
            "   Compiling relate v1.0.0\n{}\n",
            artifact_record("/ws/relate/relate/Cargo.toml", "lib")
        );
        let error = parse_build_evidence(&stdout).unwrap_err();
        assert!(error.contains("not the JSON"), "{error}");
    }

    #[test]
    fn an_unknown_reason_is_refused_not_folded() {
        let stdout = "{\"reason\":\"future-cargo-record\"}\n";
        let error = parse_build_evidence(stdout).unwrap_err();
        assert!(error.contains("unknown variant"), "{error}");
    }

    #[test]
    fn build_diagnostics_carry_the_rendered_compiler_messages() {
        let stdout = format!(
            "{}\n{}\n",
            serde_json::json!({
                "reason": "compiler-message",
                "message": { "rendered": "error[E0425]: cannot find value `x`" },
            }),
            serde_json::json!({ "reason": "build-finished", "success": false }),
        );
        assert_eq!(
            build_diagnostics(stdout.as_bytes()).unwrap(),
            ["error[E0425]: cannot find value `x`"]
        );
        let error = build_diagnostics(b"not the json protocol\n").unwrap_err();
        assert!(error.contains("not the JSON"), "{error}");
    }

    #[test]
    fn every_workspace_source_present_passes_and_carries_the_example_count() {
        let packages = verification_closure();
        let evidence = verification_evidence(&packages, "/ws/relate/relate-model/Cargo.toml");
        let closure: Vec<&Package> = packages.iter().collect();
        verify_workspace_sources(
            &evidence,
            Path::new("/ws"),
            &closure,
            Path::new("/tmp/pkg/relate-1.0.0/Cargo.toml"),
        )
        .unwrap();
        assert_eq!(evidence.examples, 24);
    }

    #[test]
    fn a_source_compiled_from_the_registry_fails_by_name() {
        // relate-model fell back: no artifact carries its workspace
        // manifest (its record names the registry cache instead).
        let packages = verification_closure();
        let evidence = verification_evidence(
            &packages,
            "/.cargo/registry/src/relate-model-1.0.0/Cargo.toml",
        );
        let closure: Vec<&Package> = packages.iter().collect();
        let error = verify_workspace_sources(
            &evidence,
            Path::new("/ws"),
            &closure,
            Path::new("/tmp/pkg/relate-1.0.0/Cargo.toml"),
        )
        .unwrap_err();
        assert!(
            error.contains("relate-model") && error.contains("registry"),
            "{error}"
        );
    }
}
