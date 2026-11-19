---
type: adr
area: surface
claim: a28d1af8bec5ac03bec34d65f305f28641ef8faf
name: 0455-state-changing-method-origin-guard
status: proposed
tags:
  - http
  - security
title: The browser-origin guard covers every state-changing method
REFERENCES:
  - "[[adr/0237-query-api-tx-routes]]"
  - "[[adr/0248-serve-host-flag]]"
  - "[[adr/0266-bolt-websocket-transport]]"
  - "[[adr/0425-typed-request-wire-alignment]]"
---

# ADR-0455: The browser-origin guard covers every state-changing method

## Context

The HTTP surfaces' browser-attack guard (`browser_guard`, [ADR-0266](./0266-bolt-websocket-transport.md)'s origin
classification applied by `cypherlite-http/src/origin.rs`) exists to close
CSRF and DNS rebinding against a loopback server. Its module documentation
claimed it "applies to every state-changing request", but the code began
with `if head.method != "POST" { return Ok(()) }`: only `POST` was
origin-checked.

The HTTP Query API surface answers more than `POST`. The managed-transaction
routes ([ADR-0237](./0237-query-api-tx-routes.md)) close a transaction with a body-less `DELETE
/db/{name}/query/v2/tx/{id}`, which is state-changing: it rolls the
transaction back and releases its snapshot pin. A foreign-origin `DELETE`
therefore passed the guard. The public spec (`docs/api/http/spec.md`) had
drifted to document the narrow behavior ("the guard applies to `POST`
only"), so the policy and its two statements no longer agreed.

The 27 Oct 2026 security audit (finding 7, Low) asked for the guard to be
extended beyond `POST` to `DELETE` and other unsafe methods, keeping the
JSON-content-type requirement specific to `POST`, and asked whether
unauthenticated deployments should validate `Host` (the DNS-rebinding
scenario), with the foreign-origin rollback rejection validated under
`--auth none`.

## Decision

1. **The guard's scope is the complement of RFC 9110 §9.2.1's safe
   methods.** `GET`, `HEAD`, `OPTIONS`, and `TRACE` are exempt; every other
   method is origin-checked. This covers the surface's `POST` statement
   routes and its `DELETE` rollback today, and any future state-changing
   route or extension-host method by construction instead of by extending a
   list. The method match is exact (RFC 9110 §9.1: names are
   case-sensitive); a lowercase or unknown spelling is treated as unsafe, so
   the verdict fails closed.
2. **A present `Origin` on a state-changing method must be allowlisted** by
   the surface's policy (the Query API's `--origin` allowlist; the MCP
   transport's fixed loopback set). A missing `Origin` remains a
   non-browser client and is allowed. A rejected origin is `403`.
3. **The browser-"simple" content-type rule stays `POST`-specific**
 ([ADR-0425](./0425-typed-request-wire-alignment.md)): a cross-origin form can only submit as `POST`, and the
   body-less `DELETE` carries no body. Non-`POST` unsafe methods are not
   held to a `Content-Type` requirement.
4. **Host-header validation for unauthenticated deployments is
   deliberately deferred.** The DNS-rebinding shape is a browser addressing
   an attacker hostname that resolves to the loopback server: the request
   carries `Host: <attacker>` and `Origin: http://<attacker>`. The Origin
   verdict above refuses that request (`403`) under every policy that does
   not explicitly allowlist the attacker origin; the `--origin *` case is
   the wildcard's already-documented risk acceptance. Enforcing a
   loopback-only `Host` under `--auth none` would instead refuse the
   sanctioned non-loopback deployment postures (`--host 0.0.0.0`, a
 reverse proxy; [ADR-0248](./0248-serve-host-flag.md) warns loudly about them), because a request
   reaching a `0.0.0.0` bind through the machine's own IP is
   indistinguishable at the HTTP layer from a rebound request. A correct
   Host policy therefore needs its own configurable trusted-host allowlist
   — a separate public-surface decision, not a completion of this guard.
   The test suite pins the rebinding-shaped rejection under `--auth none`
   so a future change to either verdict is caught.

## Alternatives considered

- **Keep the guard `POST`-only.** Rejected: the `DELETE` rollback is a
  state change, and the audit's whole point is that a cross-origin page
  could terminate a transaction when the origin policy would refuse the
  same page's `POST`.
- **Enumerate state-changing methods** (`POST`, `PUT`, `PATCH`, `DELETE`,
  `CONNECT`) in an explicit list. Rejected: a second copy of a
  classification RFC 9110 already owns, and a new extension method would
  have to remember to join the list. The safe-set complement is the same
  fact stated once and fails closed.
- **Gate safe methods too.** Rejected: `GET /` is the deliberately public
  discovery document that browser tooling probes, and `HEAD`/`OPTIONS`/
  `TRACE` carry no state; refusing them buys nothing and breaks tooling.
- **Loopback-only `Host` under `--auth none`.** Rejected now for the
  breakage above; deferred to a future trusted-host decision rather than
  shipping a rule that silently forbids the documented non-loopback
  postures.
- **Apply the simple-content-type rule to every unsafe method.** Rejected:
  forms cannot submit `DELETE`/`PUT`/`PATCH` cross-origin without a
  preflight, the rollback carries no body, and the audit asked for the
  requirement to stay `POST`-specific.

## Consequences

- A foreign-origin `DELETE` rollback is `403` under `--auth none` (and
  before auth under any mode), exactly like a foreign-origin `POST`; the
  transaction stays open.
- The MCP transport inherits the widened guard: it answers only `POST`, but
  a foreign-origin `DELETE` now answers `403` instead of the transport's
  `405`.
- `browser_guard`'s public signature is unchanged; only its method scope
  widened, so embedders need no change.
- `docs/api/http/spec.md`, `docs/api/mcp/spec.md`, and the CLI `--origin`
  row now state the state-changing-method scope and the Host deferral.
- The residual DNS-rebinding exposure under `--origin *` remains the
  wildcard's documented risk; a configurable trusted-host rule is a
  possible follow-up.
