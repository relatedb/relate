# Example: multi-line REPL statement

End a statement with `;`. Lines before that stay in the buffer under the
`   ...> ` continuation prompt.

```text
cypher> MATCH (p:Person)-[r:FRIENDS_WITH]->(friend)
   ...> RETURN p.name, friend.name, r.since;
+--------+-------------+---------+
| p.name | friend.name | r.since |
+--------+-------------+---------+
| "Ada"  | "Grace"     | 2020    |
+--------+-------------+---------+
```

Only a terminating top-level `;` submits: a blank line does **not** submit an
unfinished statement, and a `;` inside a string or comment does not
terminate one (it keeps buffering). On a TTY, `Ctrl+C` cancels the current
buffer and returns to `cypher>` (ADR-0073). `Ctrl+D` or `.quit` exits; a
non-empty pending buffer at EOF is executed first.

The same statement splitting applies to piped stdin, so the snippet above
runs unchanged as a script:

```bash
printf 'MATCH (p:Person)-[r:FRIENDS_WITH]->(friend)\nRETURN p.name, friend.name, r.since;\n' | \
  cargo run -p relate-cli -- -d ./graph-json
```

On `./graph-json` as the JSON snapshot example leaves it (one `Person`
node, no `FRIENDS_WITH` relationships) this matches zero rows and prints
the header with an empty body; create the depicted data first —
`CREATE (:Person {name: 'Ada'})-[:FRIENDS_WITH {since: 2020}]->(:Person {name: 'Grace'});`
— to reproduce the table above.
