//! The authored CE error texts that more than one crate's tests pin.
//!
//! A pin must be able to be *wrong* to be worth having, so these carry the
//! literal rather than calling the engine's own builder — but only once. A
//! re-wording updates this module and every pin follows; before it existed,
//! the same sentence was spelled out in five places across three crates,
//! and a re-wording left the CLI surface's copy behind (it failed the CLI
//! gate on the merged tree, not on the branch).

/// ADR-0584's unsupported-Cypher-version rejection, as the engine authors
/// it: `requested` is the integer the statement asked for. Raised by the
/// statement preamble at parse and by `ALTER DATABASE … SET DEFAULT
/// LANGUAGE` at command execution, which is why the wording carries the
/// valid options rather than preamble-specific advice.
pub fn unsupported_cypher_version(requested: impl std::fmt::Display) -> String {
    format!(
        "UnsupportedCypherVersion: {requested} is not a valid option for cypher \
         version. Valid options are: 25"
    )
}

/// ADR-0116 §2: `ALTER DATABASE` of a name that is not a fixed namespace,
/// as the engine authors it (`relate-core`'s
/// `ce_messages::alter_unknown_database`): `name` is the database the
/// statement asked to alter. Pinned by the CE admin suite's unknown-name
/// loop and the ontology preview gate's hidden-namespace pin — the same
/// sentence, so the same single home.
pub fn alter_unknown_database(name: &str) -> String {
    format!(
        "DatabaseNotFound: Failed to alter the specified database '{name}': \
         Database does not exist."
    )
}
