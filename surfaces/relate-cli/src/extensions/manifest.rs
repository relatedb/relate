//! The extension manifest model (ADR-0429 §2-§4): `extension.json`, the
//! one JSON file every extension package directory carries. Strict and
//! fail-closed — the code-shape rules: an unknown kind, an unknown
//! field, or malformed JSON fails the decode loudly, naming the
//! offending value; no entry is skipped, nothing flows by default.
//! `serde`/`serde_json` are the CLI's existing dependencies (the same
//! parser decodes the mount's JSON-RPC messages, ADR-0442), so the manifest
//! adds none (AGENTS.md R6).

use std::path::PathBuf;

use relate_wire::mcp::CommitFacts;
use serde::Deserialize;

/// The manifest, tagged by `kind` — the closed stratum vocabulary
/// (ADR-0429 §3). The only stratum is `command`; the later ones
/// (`callables`, `starter`, `engine-plugin`) arrive as new variants of
/// this enum. A manifest naming any
/// other value fails the decode with serde's unknown-variant error —
/// the value and the vocabulary both named — never a future-proofed
/// install: a manifest that says `kind: "callables"` today is a
/// refusal (ADR-0429 §3).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum Manifest {
    /// A command-kind extension: mounts as `relate run <name>` and
    /// speaks the MCP stdio mount's wire (ADR-0447).
    Command(CommandManifest),
}

/// The command-stratum payload (ADR-0429 §2): everything the `run`
/// host needs to mount the action — the mount name, the protocol
/// (the MCP revision) at the `command` mount point, the flag table the host
/// parses argv from, and the lifecycle knobs the importers' phase runner
/// takes.
/// Every field is known: an unknown field fails the decode naming it.
/// The optional fields (`positional`, `requires-fresh-store`, `hidden`)
/// carry the defaults noted on each.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct CommandManifest {
    /// The mount name — the `<name>` of `run <name>`. The registry
    /// refuses a load whose name collides with a reserved command
    /// word or another installed extension (ADR-0429 §6).
    pub(crate) name: String,
    /// The package's own version.
    pub(crate) version: String,
    /// The protocol at the `command` mount point (ADR-0429 §4, ADR-0447):
    /// the MCP revision the child speaks, which is the one revision this
    /// host serves. The manifest declares, the host enforces: a value other
    /// than the mount's revision is a usage refusal naming both (exit 2,
    /// before anything runs) — never a negotiation.
    pub(crate) protocol: String,
    /// The flag table (ADR-0429 §5): one entry per flag, tagged by its
    /// kind, each with its default. The host parses argv from this
    /// table; the child never sees raw argv (ADR-0442 §3).
    pub(crate) flags: Vec<FlagSpec>,
    /// The optional positional, like the importers' DIR; absent when
    /// the action takes none.
    #[serde(default)]
    pub(crate) positional: Option<Positional>,
    /// Whether the host runs the ADR-0394 fresh-store check: a
    /// `requires-fresh-store` action's batches CREATE their edges, so
    /// it refuses a run against a store that is not fresh.
    #[serde(default)]
    pub(crate) requires_fresh_store: bool,
    /// The commit policy the host's phase runner takes: `whole-import`
    /// (ADR-0157) or `per-batch` (ADR-0392).
    pub(crate) commit: CommitFacts,
    /// The ADR-0279 visibility policy ported to the `run` listing: a
    /// hidden extension still parses and runs, appears in no listing,
    /// and its flags never reach did-you-mean.
    #[serde(default)]
    pub(crate) hidden: bool,
    /// The executable: a path relative to the package directory. A
    /// path that escapes the package directory is a load refusal,
    /// checked at discovery (ADR-0429 §2) — the executable resolves
    /// only inside the package dir.
    pub(crate) executable: PathBuf,
}

/// One flag-table entry, tagged by `kind` — the closed flag-kind
/// vocabulary (ADR-0429 §2): `string`, `integer`, `boolean`,
/// `repeatable`. The default is the variant's own field, so a default
/// that does not parse as its flag's kind fails the decode — the typed
/// default rides the tag, no post-decode shape check. An unknown kind
/// fails the decode naming the value.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum FlagSpec {
    /// A string-valued flag.
    String(StringFlag),
    /// An integer-valued flag.
    Integer(IntegerFlag),
    /// A boolean flag.
    Boolean(BooleanFlag),
    /// A repeatable string flag, one value per occurrence.
    Repeatable(RepeatableFlag),
}

impl FlagSpec {
    /// The entry's name — the one field all four kinds carry: the host
    /// renders it `--{name}` and keys the child's envelope by it
    /// (ADR-0429 §5).
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::String(declared) => &declared.name,
            Self::Integer(declared) => &declared.name,
            Self::Boolean(declared) => &declared.name,
            Self::Repeatable(declared) => &declared.name,
        }
    }
}

/// One flag's entry: its name and the typed default the host resolves when
/// the flag is absent. The four kinds differ only in the default's type
/// (see the aliases below); the kind spelling rides [`FlagSpec`]'s tag.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Flag<T> {
    /// The flag's name; the host renders `--{name}`.
    pub(crate) name: String,
    /// The value the host resolves when the flag is absent (a repeatable
    /// flag's default is its list of values; each occurrence appends one
    /// more).
    pub(crate) default: T,
}

/// A string flag's entry.
pub(crate) type StringFlag = Flag<String>;
/// An integer flag's entry.
pub(crate) type IntegerFlag = Flag<i64>;
/// A boolean flag's entry.
pub(crate) type BooleanFlag = Flag<bool>;
/// A repeatable string flag's entry.
pub(crate) type RepeatableFlag = Flag<Vec<String>>;

/// The optional positional (ADR-0429 §2): the one positional argument
/// the action takes, like the importers' DIR.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Positional {
    /// The positional's name (the host renders it in the usage line).
    pub(crate) name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A well-formed command manifest exercising every field and all
    /// four flag kinds. `protocol` closes the object: no test edits
    /// it, so every field a test drops or replaces sits mid-object
    /// with its trailing comma.
    const WELL_FORMED: &str = r#"{
        "kind": "command",
        "name": "greet",
        "version": "1.2.3",
        "flags": [
            {"kind": "string", "name": "message", "default": "hello"},
            {"kind": "integer", "name": "count", "default": 3},
            {"kind": "boolean", "name": "shout", "default": false},
            {"kind": "repeatable", "name": "tag", "default": []}
        ],
        "positional": {"name": "DIR"},
        "requires-fresh-store": true,
        "commit": "whole-import",
        "executable": "greet.sh",
        "hidden": false,
        "protocol": "2025-06-18"
    }"#;

    #[test]
    fn decodes_a_well_formed_command_manifest() {
        let manifest: Manifest = serde_json::from_str(WELL_FORMED).unwrap();
        let Manifest::Command(command) = &manifest;
        assert_eq!(command.name, "greet");
        assert_eq!(command.version, "1.2.3");
        assert_eq!(command.protocol, "2025-06-18");
        assert_eq!(command.flags.len(), 4);
        assert_eq!(
            command.positional.as_ref().map(|p| p.name.as_str()),
            Some("DIR")
        );
        assert!(command.requires_fresh_store);
        assert_eq!(command.commit, CommitFacts::WholeImport);
        assert!(!command.hidden);
        assert_eq!(command.executable, PathBuf::from("greet.sh"));
    }

    /// The closed-vocabulary gate (ADR-0429 §3): every kind outside the
    /// v1 vocabulary — the later strata included — fails the decode
    /// naming the value and the vocabulary, never a future-proofed
    /// install.
    #[test]
    fn an_unknown_kind_fails_the_decode_naming_the_value_and_vocabulary() {
        for kind in [
            "callables",
            "starter",
            "engine-plugin",
            "plugin",
            "COMMAND",
            "",
        ] {
            let text = WELL_FORMED.replacen("\"command\"", &format!("\"{kind}\""), 1);
            let err = serde_json::from_str::<Manifest>(&text)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("unknown variant") && err.contains(kind),
                "kind {kind:?} must fail closed naming the value: {err}"
            );
            assert!(
                err.contains("command"),
                "the error must name the vocabulary it expected: {err}"
            );
        }
    }

    #[test]
    fn an_unknown_field_fails_the_decode_naming_the_field() {
        let text = WELL_FORMED.replacen(
            "\"version\": \"1.2.3\",",
            "\"version\": \"1.2.3\", \"surprise\": 1,",
            1,
        );
        let err = serde_json::from_str::<Manifest>(&text)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("unknown field") && err.contains("surprise"),
            "the unknown field must be named: {err}"
        );
    }

    #[test]
    fn an_unknown_flag_entry_field_fails_the_decode() {
        // The strictness reaches inside the flag table: an entry the
        // flag kinds do not know is a refusal, not a skipped key.
        let text = WELL_FORMED.replacen(
            "{\"kind\": \"string\", \"name\": \"message\", \"default\": \"hello\"}",
            "{\"kind\": \"string\", \"name\": \"message\", \"default\": \"hello\", \"alias\": \"m\"}",
            1,
        );
        let err = serde_json::from_str::<Manifest>(&text)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("unknown field") && err.contains("alias"),
            "the unknown flag-entry field must be named: {err}"
        );
    }

    #[test]
    fn malformed_json_fails_the_decode() {
        for text in ["{ nope", "{\"kind\":", "[]", "null"] {
            let err = serde_json::from_str::<Manifest>(text)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("line 1"),
                "malformed JSON must fail naming where: {text:?}: {err}"
            );
        }
    }

    /// The flag-kind gate: the closed four-value vocabulary, an unknown
    /// kind named in the refusal.
    #[test]
    fn an_unknown_flag_kind_fails_the_decode_naming_the_vocabulary() {
        let text = WELL_FORMED.replacen(
            "{\"kind\": \"integer\", \"name\": \"count\", \"default\": 3}",
            "{\"kind\": \"float\", \"name\": \"count\", \"default\": 3.5}",
            1,
        );
        let err = serde_json::from_str::<Manifest>(&text)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("unknown variant") && err.contains("float"),
            "the unknown flag kind must be named: {err}"
        );
    }

    /// The typed-default rule: a default that does not parse as its
    /// flag's kind fails the decode — the default rides the tag, no
    /// post-decode shape check.
    #[test]
    fn a_default_must_parse_as_its_flag_kind() {
        for (entry, note) in [
            (
                "{\"kind\": \"integer\", \"name\": \"count\", \"default\": \"three\"}",
                "a string default on an integer flag",
            ),
            (
                "{\"kind\": \"boolean\", \"name\": \"shout\", \"default\": \"yes\"}",
                "a string default on a boolean flag",
            ),
            (
                "{\"kind\": \"string\", \"name\": \"message\", \"default\": 5}",
                "an integer default on a string flag",
            ),
        ] {
            let text = WELL_FORMED.replacen(
                "{\"kind\": \"string\", \"name\": \"message\", \"default\": \"hello\"}",
                entry,
                1,
            );
            let err = serde_json::from_str::<Manifest>(&text)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("invalid type"),
                "{note} must fail the decode: {err}"
            );
        }
    }

    #[test]
    fn missing_required_fields_fail_the_decode() {
        for field in [
            "\"name\": \"greet\",",
            "\"commit\": \"whole-import\",",
            "\"executable\": \"greet.sh\",",
        ] {
            let text = WELL_FORMED.replacen(field, "", 1);
            let err = serde_json::from_str::<Manifest>(&text)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("missing field"),
                "dropping {field} must fail the decode: {err}"
            );
        }
    }

    #[test]
    fn optional_fields_carry_their_defaults() {
        let text = WELL_FORMED
            .replacen("\"positional\": {\"name\": \"DIR\"},", "", 1)
            .replacen("\"requires-fresh-store\": true,", "", 1)
            .replacen("\"hidden\": false,", "", 1);
        let manifest: Manifest = serde_json::from_str(&text).unwrap();
        let Manifest::Command(command) = &manifest;
        assert_eq!(command.positional, None);
        assert!(!command.requires_fresh_store);
        assert!(!command.hidden);
    }

    /// The retired dual-wire selector (ADR-0442 §1, retired by ADR-0447):
    /// a package that still declares `wire` is refused by the strict
    /// decode naming the field — the mount speaks one wire, so the field
    /// has no meaning left to default.
    #[test]
    fn the_retired_wire_field_fails_the_decode() {
        for value in ["v1", "mcp"] {
            let text = WELL_FORMED.replacen(
                "\"commit\": \"whole-import\",",
                &format!("\"commit\": \"whole-import\", \"wire\": \"{value}\","),
                1,
            );
            let err = serde_json::from_str::<Manifest>(&text)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("unknown field") && err.contains("wire"),
                "the retired field must be named: {err}"
            );
        }
    }

    /// The commit-policy vocabulary: the two values decode, anything
    /// else fails naming the value.
    #[test]
    fn the_commit_policy_vocabulary_is_closed() {
        for (policy, expected) in [
            ("whole-import", CommitFacts::WholeImport),
            ("per-batch", CommitFacts::PerBatch),
        ] {
            let text = WELL_FORMED.replacen("\"whole-import\"", &format!("\"{policy}\""), 1);
            let manifest: Manifest = serde_json::from_str(&text).unwrap();
            let Manifest::Command(command) = &manifest;
            assert_eq!(command.commit, expected);
        }
        let text = WELL_FORMED.replacen("\"whole-import\"", "\"partial\"", 1);
        let err = serde_json::from_str::<Manifest>(&text)
            .unwrap_err()
            .to_string();
        // The kit's own decode names the value and the vocabulary.
        assert!(
            err.contains("unknown commit policy") && err.contains("partial"),
            "the unknown commit policy must be named: {err}"
        );
    }
}
