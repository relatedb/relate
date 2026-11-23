### Changed

- A `tools/call` whose `arguments` is not an object is refused with the one
  wording `"arguments" must be an object` on both the product MCP server and
  the extension mount (including `report_output`), and `null` is now absent
  for every optional member: `{"parameters": {"rows": null}}` is no longer a
  shape error.
