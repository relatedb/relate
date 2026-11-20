### Fixed

- `string.regexReplace` now expands a `$n` reference to an existing
  capture group that did not participate in the match as an empty
  string, matching Neo4j: `string.regexReplace('abc', '(a)|(x)', '$2')`
  returns `"bc"` where it used to return `"nullbc"`. A group number past
  the pattern's group count is still rejected.
