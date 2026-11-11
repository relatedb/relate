# Security

This document describes how to report a security vulnerability in Relate,
which surfaces the report can cover, and the security allowances the
project accepts today.

## Reporting a vulnerability

Email **security@neo4j.com**. Do not open a public GitHub issue for a
security report — vulnerabilities reach us first, before anything is
public. PGP-encrypted email is preferred; the current key is published on
the policy page.

Reporting and disclosure follow the Neo4j responsible-disclosure policy:
<https://neo4j.com/responsible-disclosure/>

Please include what the policy asks for: a detailed description of the
vulnerability and the target system, how to reproduce it, any supporting
material that helps resolve it faster, and contact information you can be
reached at (with your PGP key, if you use one).

Per that policy: receipt of a report is confirmed as soon as reasonably
possible, reporters are kept informed of validation and mitigation, and
once a vulnerability is remediated the reporter is notified and invited
to confirm the fix. Neo4j runs no bug bounty, so compensation is not
offered; credit on the policy's public acknowledgements list is offered
on request. Neo4j is a CVE Numbering Authority (CNA), and a finding in
its CNA scope that warrants a CVE record receives one, curated by Neo4j.

The policy also states what it expects of reporters: do not break
applicable laws, do not exploit a vulnerability to access restricted
information, do not modify or remove information, no high-intensity or
destructive scanning, no denial-of-service attacks, no social engineering
or phishing, and report to us first — allowing time to evaluate and
mitigate before going public.

## Supported versions

Only the latest release line receives security fixes (`CHANGELOG.md`);
please reproduce the issue on the latest release before reporting.

## Scope

Relate is an embedded, in-memory property graph engine with local
WAL-and-checkpoint durability, served through a CLI, network surfaces,
and C-ABI clients. A report can cover any of the shipped surfaces:

- **The engine** — every crate under `relate/` (the public Rust API is
  `relate::Database`).
- **The `relate` CLI** — the one product binary (`surfaces/relate-cli`;
  `docs/api/cli/`), including **the extension host**: the `relate run`
  and `relate extension` commands that mount installed extension
  packages (`docs/api/extensions/`, first-party packages under
  `extensions/`).
- **The Bolt server** — the optional remote transport
  (`surfaces/relate-bolt`; `docs/api/bolt/`).
- **The HTTP Query API v2 server** (`surfaces/relate-http`;
  `docs/api/http/`).
- **The MCP server** (`surfaces/relate-mcp`; `docs/api/mcp/`).
- **`relate-auth`**, the generated-token basic auth shared by the network
  surfaces (`surfaces/relate-auth`), and **`relate-serve`**, those
  surfaces' shared accept loop (`surfaces/relate-serve`).
- **The C ABI** (`surfaces/relate-ffi`; `docs/api/c/`) and the language
  clients built on it: **the Python wheel** (`surfaces/relate-python`;
  `docs/api/python/`), the JavaScript, Java, Go, and Swift clients
  (`clients/`; `docs/api/javascript/`, `docs/api/java/`,
  `docs/api/go/`, `docs/api/swift/`), and the browser WASM bindings
  (`surfaces/relate-wasm`; `docs/api/wasm/`).

## Out of scope

The trivial-issues list of the linked policy applies. Adapted to this
project, the following are out of scope:

- Vulnerabilities in applications embedding Relate: the embedding
  application owns its handling of Relate data and surfaces.
- Vulnerabilities in the other repositories in this project's orbit:
  the website, the cookbook, and the spec harness.
- Issues requiring physical access to an unlocked device, pre-existing
  root or system privileges, or an on-path position on the victim's
  machine.
- Issues that only manifest in a configuration deliberately weakened
  from the documented defaults — a surface served with `--auth none`, a
  non-loopback plaintext bind, a third-party `--url` extension install —
  where the report is the documented behavior of that opt-in itself.
- Issues that only reproduce on outdated releases.
- Best-practice deviations without a working exploit: password
  complexity, expiration, or re-use rules; missing security headers;
  verbose errors or stack traces without a substantive proof of concept.
- Known vulnerable dependencies without a novel, working proof of
  concept against Relate itself.
- Self-XSS or issues exploitable only through self-XSS; clickjacking on
  pre-authenticated pages; CSRF with no security impact; content
  spoofing or text injection without the ability to modify HTML or CSS;
  homograph links.
- Weak SSL/TLS algorithms, protocols, or cipher suites as a class,
  without a demonstrated exploit.
- Attacks targeting outdated or obscure browsers (relevant to the WASM
  bindings).
- Enabled HTTP `TRACE` or `OPTIONS` methods, and missing rate limits
  where there is no platform impact — do not run denial-of-service to
  prove impact.
- Spam, phishing, and social engineering.
- Bugs that pose no security risk, and opinions on design or
  architecture — including the accepted allowances below, which are
  recorded decisions, not vulnerabilities.
- Any other submission judged low-risk on the policy's terms: unlikely
  hypothetical attack vectors, significant user interaction required, or
  minimal impact.

## Known gaps and accepted allowances

These are accepted design decisions, not findings waiting on a fix; each
rests on the record cited.

- **No engine-level access control.** The engine has no users, roles, or
  permissions: every connection can read and write the entire graph. The
  per-connection principal a Bolt server forwards is display-only — it
  changes what `dbms.showCurrentUser` reports and confers no
  authorization (ADR-0273).
- **Authentication is surface-level, with an opt-out and auth-less
  transports.** Auth belongs to the network surfaces, not the engine: one
  generated-token basic-auth credential — or `--auth USER:PASSWORD` /
  `RELATE_AUTH` — is shared by every served surface, `--auth none`
  disables it, and the engine never sees any of it (ADR-0232;
  `docs/api/cli/spec.md`). The MCP stdio transport skips the challenge
  entirely: the host process spawned that server (ADR-0232;
  `docs/api/cli/spec.md`).
- **The served surfaces are plaintext TCP.** `relate serve` terminates
  no TLS; the default bind is loopback, and a non-loopback `--host`
  prints a loud startup warning (`docs/api/cli/spec.md`; ADR-0248). The
  HTTP Query API crate terminates no TLS by design (ADR-0428), and the
  Bolt crate's TLS and mTLS support (ADR-0109) is library-level for
  embedders, with no `serve` spelling — front `relate serve` with a
  TLS-terminating proxy (`docs/api/bolt/spec.md`).
- **Extension installs are explicit trust acts.** First-party packages
  install from a signed channel — an SSHSIG-verified index and
  SHA256-checked tarballs. `--url URL` installs unsigned third-party
  code: the https URL you name is the whole trust act. `--from DIR`
  trusts a local directory. Every install records its provenance
  (ADR-0429, ADR-0431; `docs/api/extensions/install.md`). Discovery
  itself never fetches and never reads the working directory
  (ADR-0429 §6–§7).
- **Store files are not encrypted.** The durable form is local
  write-ahead-log segments and checkpoint files written beside the
  database (ADR-0012); nothing encrypts them. Anyone who can read the
  store directory can read the entire graph — protecting it is the
  host's job.
- **Labs preview.** Relate is an experimental Neo4j Labs project: not an
  official Neo4j product, provided as-is with no warranties,
  service-level commitments, maintenance guarantees, security
  assurances, or official technical support, and not intended for
  production use (`LABS_DISCLAIMER.txt`).
