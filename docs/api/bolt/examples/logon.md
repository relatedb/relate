# Example: LOGON and element ids at Bolt 5.4

The server speaks Bolt 4.4 and Bolt 5.4 (ADR-0484). The handshake reads
the client's four proposals in client order and answers the first one
covering a spoken version, so official drivers — whose proposal set
carries a 5.x range beside 4.4 — negotiate 5.4. On a 5.x connection the
HELLO no longer authenticates: it identifies the client (`user_agent`
plus the mandatory `bolt_agent` map), and the driver follows with LOGON
carrying the auth token (`basic` principal + credentials, or `none`).
LOGOFF returns the session to the AUTHENTICATION state, where a fresh
LOGON reopens it. With a credential configured (`--auth user:password`)
the same LOGON checks it, and a mismatch fails
`Neo.ClientError.Security.Unauthorized` and closes the connection. At
5.4 the graph structures also carry element ids — Node and Relationship
append the engine `elementId()` helper's strings (`4:relate-neo4j:n0`,
`5:relate-neo4j:r0`) — and the DateTime structures are the fixed UTC forms
(`0x49`/`0x69`).

In one terminal, run from the repository root:

```bash
cargo run -p relate-cli -- serve --host 127.0.0.1 --bolt 7687 --database :memory: --auth none
```

In another terminal with the official Neo4j Python driver installed
(`python3 -m pip install neo4j`), run the [driver example](logon.py)
from the repository root:

```bash
python3 docs/api/bolt/examples/logon.py
```

The example asserts the negotiated version is 5.4, runs a query through
the LOGON-opened session, and checks that a node and a relationship
result carry the same element ids `elementId()` answers. CI runs this
same script in the `relate-bolt` job.
