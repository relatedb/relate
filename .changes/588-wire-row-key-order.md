### Fixed

- Wire envelope row objects now serialize with keys in the query's column
  order instead of alphabetically sorted (ADR-0634): every JSON surface —
  the WASM `execute` envelope, the C ABI envelope, the CLI
  `--mode json`/`jsonl`/`envelope` modes, MCP tool results — keys rows as
  the statement's `columns` array lists them, and every wire object
  follows its documented member order. `columns` remains the
  authoritative column order.

### Notes

- Breaking (pre-1.0, ADR-0634): installed bash-kit extensions parse the
  MCP tool-result member order and must be regenerated after upgrading
  (`relate extension init` again, or reinstall from the first-party
  channel) — the mount now serializes `{"type":"text","text":…}` as
  documented. Recordings are unchanged: map literals and entity
  properties keep their code-point key order, now sorted explicitly at
  the decode boundary. HTTP notification entries and the MCP tool result
  serialize in the Neo4j member order their specs document. The markdown
  vault's parse boundary now sorts frontmatter object keys explicitly —
  the canonical value form ADR-0406 always specified, previously
  inherited from serde_json's sorted map — so the link sort/dedup key
  and rendered files stay spelling-independent.
