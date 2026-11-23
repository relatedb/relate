### Fixed

- Markdown vault full-form wikilinks (`[[Label/<key>]]`) resolve at the
  label they name whenever a node carries that label as primary; a
  label also carried as an extra label by an earlier-sorted file no
  longer silently redirects the link into that file's label namespace —
  hand edits to frontmatter link lists (container rewires) survive a
  load/checkpoint cycle instead of being silently rewritten back
  (REL8-2039)
- Variant key spellings (case, Unicode normalization) in a full-form
  link resolve to the same node instead of stubbing a duplicate, a
  repeated link to a not-yet-existing target resolves to the first
  stub instead of creating a second node under the same key, and two
  bare links to one target are one relationship at load (set
  semantics); unresolved links materialize their stub under the label
  the wikilink names
