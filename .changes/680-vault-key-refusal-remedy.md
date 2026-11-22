### Fixed

- The markdown vault's key-choice refusals — a node missing its label's
  key property, a non-string key value, a key value with no representable
  characters — now name the remedy: declare `key: true` on the label's
  node type in the vault's committed `ontology.yaml` (or `ontology.json`)
  to pin a better key property, instead of stating only the requirement.
