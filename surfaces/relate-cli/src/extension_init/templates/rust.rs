//! The Rust template (`--lang rust`): a crate whose `main` is the wire kit's
//! `serve` call and whose `walk()` is the TODO-marked edit point. The glue
//! is the wire kit ([`relate_wire::mcp`]) — the same kit both sides of
//! the mount already share — so the generated source *shows the kit's
//! usage* rather than vendoring a second client; `Cargo.toml` and
//! `src/main.rs` are generated as source, and the package's executable is
//! the binary the crate builds.

use super::{render, PackageFile, Role};

/// The generated `Cargo.toml`: a workspace-shaped crate, the first-party
/// child layout (ADR-0431 §11).
const CARGO_TOML: &str = r#"[package]
name = "__NAME__"
version = "0.1.0"
edition = "2021"

# A first-party extension crate lives at `extensions/<name>/` in this
# workspace, so its dependencies are the workspace's. From
# outside a checkout, point these at a checkout by path or at a released
# version instead — nothing else in the crate changes.
[dependencies]
relate-wire = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
"#;

/// The generated `src/main.rs`: the wire kit's `serve` and the walk.
const MAIN: &str = r#"//! __NAME__ — a Relate extension (MCP stdio client).
//!
//! Scaffolded by `relate extension init __NAME__ --lang rust`. The
//! package is `extension.json` (the manifest the host parses — mount name,
//! MCP revision, flag table, commit policy) plus the built binary it names;
//! `README.md` is the quickstart.
//!
//! Glue: the transport contract lives in __GLUE_SOURCE__. A Rust child
//! vendors no client: `relate_wire::mcp` is the kit both sides of the
//! mount share, so this source shows its usage — `serve` owns the envelope
//! read, the handshake, the caps and the failure report, and `McpChild`'s
//! methods are the five tools. Re-scaffold or copy that section when the
//! contract changes.
//!
//! Edit `walk()`; everything around it is the generated glue.

use relate_wire::mcp::{serve, StdioChild};
use serde::Deserialize;
use serde_json::json;

/// The mount name the handshake reports — the manifest's `name`.
const NAME: &str = "__NAME__";

/// The host's request envelope: one JSON line carrying the manifest's
/// resolved flag values — one field per flag, spelled as the manifest spells
/// it. `name` is the sample's one flag (default `there`); a field without a
/// manifest flag never arrives.
#[derive(Debug, Deserialize)]
struct Envelope {
    /// `--name`: the sample's greeting.
    name: String,
}

fn main() {
    serve(NAME, env!("CARGO_PKG_VERSION"), walk);
}

/// The one statement the sample walk writes: one node per row, keyed by
/// `id`.
const WRITE: &str = "UNWIND $rows AS row
MERGE (n:Scaffold {id: row.id})
SET n.note = row.note
RETURN count(n) AS written";

/// The sample's greeting: `$name` is a Cypher parameter, bound from the
/// arguments object — never spliced into the statement text.
const GREETING: &str = "RETURN 'Hello ' + $name + ' world!' AS msg";

/// Walk one corpus into the store — the part you write.
///
/// TODO: replace the sample row with your own source. Declare your flags in
/// `extension.json`'s `flags` table and read them off `Envelope` first, then
/// emit one `json!` row per record and shape the graph in `WRITE`.
/// `docs/api/extensions/authoring.md` walks the loop.
fn walk(child: &mut StdioChild, envelope: Envelope) -> Result<(), String> {
    let rows = vec![json!({"id": "example", "note": &envelope.name})];
    let written = rows.len();
    child.write_rows(WRITE, rows.into_iter())?;

    // The flag as a Cypher parameter, and its result back: the read's `msg`
    // is the greeting.
    let greeted = child.call(
        "read_cypher",
        json!({"statement": GREETING, "parameters": {"name": &envelope.name}}),
    )?;
    let msg = greeted["rows"][0]["msg"].as_str().unwrap_or_default();
    child.report_output(
        &format!("{NAME}: {msg} — wrote {written} sample node(s)"),
        &["MATCH (n:Scaffold) RETURN n".to_string()],
    )?;
    Ok(())
}
"#;

/// The Rust package: the crate, the README, and the manifest's `executable`
/// value. The executable is the binary the crate builds, named after the
/// package — the one name fact `Cargo.toml` and the manifest share — so no
/// generated file carries the executable role.
pub(super) fn package(name: &str) -> (String, Vec<PackageFile>) {
    let files = vec![
        PackageFile::new("Cargo.toml", render(CARGO_TOML, name), Role::BuildFile),
        PackageFile::new("src/main.rs", render(MAIN, name), Role::GlueSource),
        PackageFile::new("README.md", render(README, name), Role::Readme),
    ];
    (name.to_string(), files)
}

/// The README body: build, install, run, query, then the edit loop.
const README: &str = r#"# __NAME__

A Relate extension, scaffolded by `relate extension init __NAME__
--lang rust`. The package is `extension.json` (the manifest) plus the binary
it names; `src/main.rs` is the child and `Cargo.toml` builds it. The glue is
the wire kit (`relate-wire`), the same kit both sides of the mount share
— there is no second client to vendor.

## Build and run it

The crate is laid out as a first-party child of this workspace
(`extensions/<name>/`), whose dependencies are the workspace's;
from outside a checkout, point `relate-wire`'s dependency at one by path
or version.

```bash
cargo build --release
cp target/release/__NAME__ .
relate extension install __NAME__ --from .
relate run __NAME__ --name Alex -d ./store
relate query --database ./store "MATCH (n:Scaffold) RETURN n.id AS id, n.note AS note"
```

`relate run` starts the binary, hands it the manifest's flag values as
one JSON envelope line on stdin, and from then on the two pipes carry
JSON-RPC 2.0: the child is the MCP client, the host is the MCP server that
owns the store. `serve` runs the session and reports failures on stderr,
which the host renders.

## Edit it

`walk()` is the only part you write. `relate_wire::mcp` is the glue —
`serve` is the whole `main`, `write_rows` the chunked `write_cypher`,
`update_schema` the DDL call, `report_output` the summary — and its source
of truth is __GLUE_SOURCE__.

1. The `flags` table already declares the sample's one flag — `--name`
   (default `there`), which `walk()` reads off `Envelope` and binds as the
   `$name` Cypher parameter. Add your own flags there (`string`, `integer`,
   `boolean`, `repeatable`) and the matching fields to `Envelope`.
2. Replace the sample row with your own records and shape the graph in
   `WRITE` (one statement, `$rows` is the row list).
3. `report_output` is the run's summary; its starters are offered to the
   interactive shell.

The author guide — the `init → edit → check → install --from → run` loop,
the language tiers and the transport contract — is
`docs/api/extensions/authoring.md`.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// The crate is `serve` plus a walk: the wire kit is the glue, the crate
    /// names the binary the manifest declares, and no generated file claims
    /// the executable role.
    #[test]
    fn the_rust_glue_is_serve_and_a_walk() {
        let (executable, files) = package("my-ext");
        assert_eq!(executable, "my-ext", "the binary the crate builds");
        let roles: Vec<Role> = files.iter().map(|file| file.role).collect();
        assert_eq!(roles, vec![Role::BuildFile, Role::GlueSource, Role::Readme]);

        let cargo = &files[0].contents;
        assert!(cargo.contains("name = \"my-ext\""));
        assert!(cargo.contains("relate-wire"));

        let main = &files[1].contents;
        assert!(main.contains("serve(NAME, env!(\"CARGO_PKG_VERSION\"), walk)"));
        assert!(main.contains("fn walk(child: &mut StdioChild"));
    }
}
