//! The `.cypherlite/schema/` text mirror (ADR-0481): the store's graph
//! type, ontology document and index/constraint catalog persisted as
//! canonical, human-diffable text files inside the store directory,
//! rewritten from the same in-memory state a checkpoint persists. The
//! manifest and WAL stay authoritative at open (ADR-0481 §4); the text
//! set is the mirror, never a storage source.
//!
//! One derivation of the expected mirror serves both sides of the
//! contract, so what a checkpoint writes and what an open verifies can
//! never drift apart: every codec's checkpoint (ADR-0481 §5) writes
//! [`expected_mirror`]'s files, and the open-time drift guard
//! ([`verify_open_schema_files`]) verifies the same files against the
//! same renders:
//!
//! - the graph type body: `Graph::graph_type_spec`'s canonical
//!   `canonicalize_graph_type_body` text (the exact text the JSON codec
//!   persists as `graph_type` and the Parquet theater's model
//!   re-renders);
//! - the ontology document: the verbatim validated bytes the codec
//!   already publishes (`publish_ontology`'s `ontology/<hex>.json`
//!   object for the native codec, the inline `ontology` field for JSON)
//!   — never re-serialized here;
//! - the catalog DDL: `catalog_create_statements`, the SHOW
//!   `createStatement` renderers' whole-catalog form.

use relate_core::{Error, Result};
use relate_ontology::{detect_format, DocumentFormat};

use crate::StoreTarget;

/// The schema mirror's directory (ADR-0481 §5):
/// `<store>/.cypherlite/schema/` — under the `.cypherlite` engine
/// directory whose name the markdown codec's sidecar family owns
/// (`codec::markdown::ENGINE_DIR`); the unit tests pin the two
/// spellings together.
pub(crate) const SCHEMA_DIR: &str = ".cypherlite/schema";

/// The store's graph type as its canonical DDL body (ADR-0481 §1):
/// exactly the `SnapshotContents.graph_type` text, absent when the store
/// has no graph type. `pub(crate)` for the unit tests that pin the
/// mirror's paths.
pub(crate) const GRAPH_TYPE_FILE: &str = ".cypherlite/schema/graph-type.cypher";

/// The ontology document's two possible mirrors (ADR-0481 §2): the
/// verbatim document bytes under the extension naming their format —
/// `.json` for a strict-JSON document, `.yaml` otherwise — at most one
/// of the two, both absent when the store holds no ontology.
/// `pub(crate)` for the unit tests that pin the mirror's paths.
pub(crate) const ONTOLOGY_JSON_FILE: &str = ".cypherlite/schema/ontology.json";
pub(crate) const ONTOLOGY_YAML_FILE: &str = ".cypherlite/schema/ontology.yaml";

/// The catalog DDL mirror (ADR-0481 §3): one `CREATE INDEX` /
/// `CREATE CONSTRAINT` statement per renderable catalog entry,
/// semicolon-terminated one per line, always present (possibly empty)
/// for a checkpointed store. `pub(crate)` for the unit tests that pin
/// the mirror's paths.
pub(crate) const SCHEMA_DDL_FILE: &str = ".cypherlite/schema/schema.cypher";

/// The one registry (the `preview_feature_registry` pattern,
/// relate-core/src/preview.rs): each contract file's variant and
/// path constant are declared from this single list, and the macro emits
/// every derivation — the enum, [`MirrorFile::ALL`] from the same rows
/// (a variant cannot exist without its row), and the exhaustive `path`
/// match, whose coverage is a compile guarantee. A fifth contract file
/// lands as one path constant plus one row, and every decision that
/// walks `ALL` or matches the enum names it or fails to compile.
macro_rules! mirror_file_registry {
    ($($variant:ident => $path:ident),* $(,)?) => {
        /// The mirror contract's file vocabulary, typed: every consumer
        /// — the store's object-name grammar (`snapshot::object_name`,
        /// which gives these files their bounded-read class), the
        /// extra-entry check, and [`ExpectedMirror::files`] — walks
        /// [`MirrorFile::ALL`], and the expected-contents dispatch
        /// matches the enum exhaustively, so a new contract file fails
        /// to compile until every decision names it.
        #[derive(Clone, Copy, PartialEq, Eq)]
        pub(crate) enum MirrorFile {
            $($variant,)*
        }

        impl MirrorFile {
            /// The registry width, derived from the rows — the bound
            /// [`MirrorFile::ALL`]'s length is proven against, never
            /// re-spelled.
            const ALL_LEN: usize = [$(stringify!($variant)),*].len();

            /// Every contract file, registry order — the enumeration
            /// every walker iterates instead of re-spelling the
            /// vocabulary. Emitted from the registry's own rows, so a
            /// variant cannot exist without its entry here.
            pub(crate) const ALL: [MirrorFile; Self::ALL_LEN] =
                [$(MirrorFile::$variant),*];

            /// The file's store path — the one spelling, the path
            /// constants above. The exhaustive match is emitted from
            /// the registry: coverage cannot drift from the rows.
            pub(crate) fn path(self) -> &'static str {
                match self {
                    $(MirrorFile::$variant => $path,)*
                }
            }
        }
    };
}

mirror_file_registry! {
    GraphType => GRAPH_TYPE_FILE,
    OntologyJson => ONTOLOGY_JSON_FILE,
    OntologyYaml => ONTOLOGY_YAML_FILE,
    SchemaDdl => SCHEMA_DDL_FILE,
}

impl MirrorFile {
    /// Whether `name` is one of the contract's store paths.
    pub(crate) fn is_contract_path(name: &str) -> bool {
        MirrorFile::ALL.iter().any(|file| file.path() == name)
    }
}

/// The expected mirror for one store state (ADR-0481 §1–§3), derived
/// once for both the checkpoint writer and the open-time guard. Each
/// fact renders through its one canonical renderer — never a second
/// copy of a rendering decision on either side.
struct ExpectedMirror<'a> {
    /// The graph type's canonical body, `None` when the store has none.
    graph_type: Option<String>,
    /// The ontology document's spelling and verbatim bytes, `None` when
    /// the store holds no ontology.
    ontology: Option<OntologyMirror<'a>>,
    /// The catalog DDL: one semicolon-terminated statement per line,
    /// always present (possibly empty).
    ddl: String,
}

/// The ontology document's mirror spelling (ADR-0481 §2): the extension
/// naming its format carries the document verbatim; every other
/// ontology spelling is stale wherever it appears.
struct OntologyMirror<'a> {
    file: MirrorFile,
    document: &'a str,
}

impl<'a> ExpectedMirror<'a> {
    /// The expected contents of every contract file (ADR-0481 §1–§3),
    /// keyed by store path — the one expected-contents table the
    /// checkpoint writer and the open-time verifier both read. `None`
    /// is the expected-absent verdict throughout.
    fn files(&self) -> impl Iterator<Item = (&'static str, Option<&[u8]>)> + '_ {
        MirrorFile::ALL
            .into_iter()
            .map(|file| (file.path(), self.expected(file)))
    }

    /// One contract file's expected contents for this store state,
    /// matched exhaustively. The ontology pair: the document's own
    /// spelling carries its verbatim bytes; the other spelling (and
    /// both, with no ontology) is expected absent.
    fn expected(&self, file: MirrorFile) -> Option<&[u8]> {
        match file {
            MirrorFile::GraphType => self.graph_type.as_deref().map(str::as_bytes),
            MirrorFile::SchemaDdl => Some(self.ddl.as_bytes()),
            MirrorFile::OntologyJson | MirrorFile::OntologyYaml => self
                .ontology
                .as_ref()
                .filter(|spelling| spelling.file == file)
                .map(|spelling| spelling.document.as_bytes()),
        }
    }
}

/// Derive the expected mirror from one store state: the graph type's
/// canonical body, the ontology document verbatim under the extension
/// its one format detector names (ADR-0227 §3), and the catalog DDL —
/// the SHOW createStatement renderings, one semicolon-terminated
/// statement per line.
fn expected_mirror<'a>(
    graph: &'a dyn relate_engine_api::GraphImport,
    ontology: Option<&'a str>,
) -> Result<ExpectedMirror<'a>> {
    let ontology = ontology.map(|document| {
        let file = match detect_format(document.as_bytes()) {
            DocumentFormat::Json => MirrorFile::OntologyJson,
            DocumentFormat::Yaml => MirrorFile::OntologyYaml,
        };
        OntologyMirror { file, document }
    });
    let mut ddl = String::new();
    for statement in graph.catalog_ddl() {
        ddl.push_str(&statement);
        ddl.push_str(";\n");
    }
    Ok(ExpectedMirror {
        graph_type: graph.graph_type_spec()?,
        ontology,
        ddl,
    })
}

/// Render and write the schema text mirror for one checkpoint
/// (ADR-0481 §5/§6): unconditional, no flag. `graph` and `ontology` are
/// the same in-memory state the checkpoint is persisting — the callers
/// hold the checkpoint's graph and the store's current ontology
/// document. Every expected-absent entry in [`ExpectedMirror::files`]
/// is cleared, so the directory holds exactly the three-file contract
/// after every checkpoint.
fn write_checkpoint_schema_files(
    target: &StoreTarget,
    graph: &dyn relate_engine_api::GraphImport,
    ontology: Option<&str>,
) -> Result<()> {
    let mirror = expected_mirror(graph, ontology)?;
    for (file, expected) in mirror.files() {
        match expected {
            Some(bytes) => write_object(target, file, bytes)?,
            None => remove_object(target, file)?,
        }
    }
    Ok(())
}

/// The checkpoint tail every codec shares (ADR-0481 §5): write the
/// schema text mirror from the same in-memory state this checkpoint
/// just persisted, then clear the graph's checkpoint-dirty tracking —
/// the checkpoint now holds everything it tracked. Each codec calls
/// this exactly where it wrote the mirror and cleared the flag, so the
/// write/clear pairing is spelled once; the mirror still publishes
/// after the manifest the codec already wrote (the manifest and WAL
/// stay authoritative, the mirror is the reflection). The markdown
/// codec's journal removal follows this call: the mirror lands before
/// the journal is cleared, the ordering its crash window records.
pub(crate) fn finish_checkpoint(
    target: &StoreTarget,
    graph: &dyn relate_engine_api::GraphImport,
    ontology: Option<&str>,
) -> Result<()> {
    write_checkpoint_schema_files(target, graph, ontology)?;
    graph.clear_checkpoint_dirty();
    Ok(())
}

/// The open-time drift guard (ADR-0481 §4): when the store dir has the
/// `.cypherlite/schema/` mirror, every file must byte-equal the
/// rendering of the state this open's snapshot load restored. The
/// comparison basis is the checkpoint the manifest names — the same
/// state the mirror was written from — so WAL records past the
/// checkpoint LSN, which legitimately move the live schema past what
/// the mirror reflects, are replayed only after the guard and the next
/// checkpoint rewrites the mirror from the state they produce. Any
/// drift — a changed, missing, or extra file — fails the open closed
/// with an authored error naming the file and the repair. An absent
/// `.cypherlite/schema/` folder is a store written before ADR-0481 (or
/// one repaired by deleting the mirror) and never refuses the open.
pub(crate) fn verify_open_schema_files(
    target: &StoreTarget,
    graph: &dyn relate_engine_api::GraphImport,
    ontology: Option<&str>,
) -> Result<()> {
    if !target
        .vfs
        .exists(SCHEMA_DIR)
        .map_err(|e| dir_error(e, "checking for"))?
    {
        return Ok(());
    }
    let mirror = expected_mirror(graph, ontology)?;
    for (file, expected) in mirror.files() {
        verify_mirror_file(target, file, expected)?;
    }
    verify_mirror_holds_only_contract_files(target)
}

/// One contract file's drift verdict: byte-equal when the state expects
/// it present, absent when the state expects none. Any other
/// combination is drift (ADR-0481 §4) and fails the open closed with
/// the one drift error naming the file — never a diff dump, never a
/// panic. The read rides the store's bounded-read schema-mirror class
/// (the same metadata-then-handle length proof the manifest and
/// ontology objects take), so a planted oversized file refuses the open
/// with the class's limit error naming the file instead of an unbounded
/// allocation.
fn verify_mirror_file(
    target: &StoreTarget,
    file: &'static str,
    expected: Option<&[u8]>,
) -> Result<()> {
    let actual = crate::snapshot::get_object_opt(target, file).map_err(|e| named(e, file))?;
    match (actual, expected) {
        (Some(actual), Some(expected)) if actual == expected => Ok(()),
        (Some(_), Some(_)) => Err(drift(
            file,
            "differs from the schema this store's checkpoint persisted; it was edited by hand \
             or copied from another store",
        )),
        (None, Some(_)) => Err(drift(
            file,
            "is missing from the mirror this store's checkpoint wrote",
        )),
        (Some(_), None) => Err(drift(file, NOT_PART_OF_MIRROR)),
        (None, None) => Ok(()),
    }
}

/// The mirror directory holds only the contract's files (ADR-0481
/// §1–§3): any other entry — a stray file, an unexpected subdirectory,
/// a crash-window temp — is drift too. Membership is judged on the raw
/// entry name against the contract's paths ([`MirrorFile::is_contract_path`]); the untrusted name is escaped
/// through [`escape_entry_name`] only where it enters the drift error's
/// text — presentation never decides identity.
fn verify_mirror_holds_only_contract_files(target: &StoreTarget) -> Result<()> {
    let names = target
        .vfs
        .list_dir(SCHEMA_DIR)
        .map_err(|e| dir_error(e, "listing"))?;
    for name in names {
        let path = format!("{SCHEMA_DIR}/{name}");
        if !MirrorFile::is_contract_path(&path) {
            return Err(drift(
                &format!("{SCHEMA_DIR}/{}", escape_entry_name(&name)),
                NOT_PART_OF_MIRROR,
            ));
        }
    }
    Ok(())
}

/// One directory-entry name as it may sit inside an error message: the
/// VFS is untrusted, so a crafted mirror entry carrying newlines or ANSI
/// escapes must not reach CLI stderr, serve logs, or client bodies
/// verbatim (line and terminal spoofing). Control characters render
/// through `char::escape_default` (`\n`, `\u{1b}`, …); every other
/// character passes through unchanged, so the operator still reads the
/// name they would see in a directory listing. (Deliberately narrower
/// than `relate-core`'s private `escape_for_message`, which escapes
/// every character: a file name should stay recognizable.)
fn escape_entry_name(name: &str) -> String {
    let mut escaped = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_control() {
            escaped.extend(c.escape_default());
        } else {
            escaped.push(c);
        }
    }
    escaped
}

/// The extra-entry drift verdict — one spelling for both producers (a
/// stale contract file, an entry outside the contract).
const NOT_PART_OF_MIRROR: &str = "is not part of this store's schema mirror";

/// The one drift error (ADR-0481 §4): names the drifted file and the
/// repair — deleting the mirror directory lets the open proceed and the
/// next checkpoint regenerate the mirror from the store's schema.
fn drift(file: &str, what: &str) -> Error {
    Error::storage(format!(
        "schema mirror {file} {what}; delete the {SCHEMA_DIR}/ directory and reopen: \
         the next checkpoint regenerates it from the store's schema"
    ))
}

/// A mirror-directory-level VFS failure, naming the directory and the
/// step it failed at (the file-level steps name their file through
/// [`named`]).
fn dir_error(error: Error, step: &str) -> Error {
    crate::storage_context(error, format_args!("{step} the schema mirror {SCHEMA_DIR}"))
}

/// Write one mirror object through the store's checkpoint object path
/// (the same atomic write every checkpoint object takes), its errors
/// naming the file.
fn write_object(target: &StoreTarget, file: &str, bytes: &[u8]) -> Result<()> {
    crate::snapshot::put_object(target, file, bytes).map_err(|e| named(e, file))
}

/// Clear one stale mirror object. A missing file is already the wanted
/// state — every VFS backend's `remove_file` treats it as a no-op — and
/// any other failure is named like a write failure.
fn remove_object(target: &StoreTarget, file: &str) -> Result<()> {
    target.vfs.remove_file(file).map_err(|e| named(e, file))
}

/// A mirror-file error naming the file it happened to (the ADR-0481 §4
/// drift errors name the drifted file the same way). The file constants
/// already carry the mirror directory, so the context is the path alone.
fn named(error: Error, file: &str) -> Error {
    crate::storage_context(error, format_args!("{file}"))
}
