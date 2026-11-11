//! The syntax tag of an ontology document, shared by the parser and the
//! error vocabulary.

/// The syntax of an ontology document, detected by a strict JSON parse
/// (ADR-0227 §3: a document that parses as JSON counts as JSON, anything
/// else as YAML).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentFormat {
    /// The document parses as strict JSON.
    Json,
    /// Anything else (parsed as YAML 1.2).
    Yaml,
}

impl DocumentFormat {
    /// The `db.ontology.get` format argument naming this format
    /// (CIP-287: `"json"` or `"yaml"`).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            DocumentFormat::Json => "json",
            DocumentFormat::Yaml => "yaml",
        }
    }
}

/// The uppercase syntax name (`JSON` or `YAML`) error messages carry.
impl std::fmt::Display for DocumentFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_str().to_ascii_uppercase())
    }
}
