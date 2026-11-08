# Extensions — description

The extension system is how userland commands reach the CLI: a package
directory under `$XDG_CONFIG_HOME/relate/extensions/` (falling back to
`~/.config/relate/extensions/`) mounts as `relate run <name> [flags]`
(ADR-0429) and speaks MCP stdio to a host that alone owns the store
(ADR-0442, ADR-0447): the child writes JSON-RPC 2.0 `tools/call` requests
on stdout and the host answers on stdin, so any MCP client — an agent, a
framework, a script — can be an extension and can read the graph it is
filling. The handshake is MCP's own — `initialize` with `protocolVersion`
and `capabilities`, answered by `notifications/initialized` — and the tool
vocabulary is the one the CLI's MCP server surface speaks, from one table
both surfaces serve ([MCP](../mcp/spec.md)). The store, the caps, the
commit policy, the summary and the handoff stay host property.

The shape is deliberate: the kinds of commands that used to require a
compiled-in CLI change — importers, converters, scrapers, anything that
walks an external source and writes parameterized batches into a store —
are packages a user (or an agent, or a CI job) authors in any language
that can speak JSON-RPC — the shipped examples are bash, Python and
TypeScript. The host keeps everything a package author should
never have to re-implement: the flag grammar (parsed from the package's
own manifest table, so help, did-you-mean and usage errors reach
extensions for free), the store (`-d`/`--database` is host property; the
child never even sees the path), the commit policies, the resource caps,
the exit-code mapping, the summary and the interactive handoff. A package
is a source walker; its only power is the calls it makes.

The authoring story is one command and one file to edit: `relate
extension init <name>` scaffolds a package — the manifest, an executable
that is a minimal MCP client generated inline (envelope read, JSON-RPC
transport, the tool calls — no published client dependency to install or
manage), a TODO-marked `walk()`, and a README quickstart — into a directory
that `extension install --from` and `relate run` take as it is. The four
first-class languages (`sh`, `py`, `ts`, `rust`) plus the tier that any MCP
client language reaches by construction are in the [author guide](authoring.md);
the install side (the trust tiers first-party / GitHub pin / unsigned URL /
local directory, what the loud install line and the provenance marker mean)
is the [install guide](install.md).

Where it fits the product: the CLI (`docs/api/cli/`) owns the verb; this
surface owns the contract between the CLI and package authors — the
manifest schema, the discovery walk and its refusals, the mount's wire and
caps, the host execution lifecycle, and the error classes. The same spine
(manifest, registry, discovery, trust, telemetry, help integration) later
carries the other extension strata — callables, starters, engine plugins
(ADR-0429 §3) — and the distribution half (the signed first-party channel
and the `extension init | install | list | remove | check` verbs, ADR-0431)
has landed beside it, with a fourth, commit-pinned source for GitHub releases
(`--github OWNER/REPO@TAG|@SHA`, ADR-0449): the first install records the
tag's commit and the asset's SHA256 in a per-user trust store, and a
moved tag refuses without an explicit re-approval.

The host ships with zero installed extensions: every unknown name fails
closed with the pinned discovery error naming the first-party install
path, and nothing is ever fetched (ADR-0429 §7). The first real content has
landed with the importer extraction (ADR-0431 §11): `fs-to-graph` and
`git-to-graph` are first-party extension crates — the directory-tree
walker and the checkout-history reader as MCP stdio children, their sources
in the workspace's `extensions/` group with their committed manifests
beside them — and the `lab` surface they rode retired outright (ADR-0431
§12). A third first-party child is the game: `dungeon` (LABS-884), a
MUD-style keep where every verb — move, look, take, give, ask — is one
Cypher statement against the store the host owns, the map an explored
window the child renders from the rooms the player has entered.

A fourth, `ideas`, is a small linked idea store: add ideas with tags, link
them with a closed vocabulary of relationship types, walk the links, and
load a bundled seed set of product ideas to start from. It is a `hidden`
package, so it is installed by name and absent from listings.
