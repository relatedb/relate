### Fixed

- A markdown vault's committed ontology now pins the schema it declares:
  `key: true` installs the enforced key constraint and labelled
  relationship endpoints install their endpoint-label constraints — both
  listed by `SHOW CONSTRAINTS` and enforced — and a write session
  checkpoints cleanly. Before, the first write checkpoint after adding the
  ontology failed with `invalid checkpoint enforced-label constraint`.
