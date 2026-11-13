# 0273. Plumb the Bolt principal to the engine for dbms.showCurrentUser (revisits ADR-0143)

- **Status**: Accepted
- **Renumbered**: from 0144 (shared-number cleanup)
- **Date**: 2026-09-30
- **Deciders**: CypherLite maintainers (explicit maintainer request
  revisiting ADR-0143; Linear LABS-148)

## Context

ADR-0143 pinned the record shapes for the Browser/Desktop-compat `dbms.*`
procedures and decided **session context is not plumbed**:
`dbms.showCurrentUser` returns the documented-placeholder row
`{username: 'cypherlite', roles: ['admin'], flags: []}` because auth in
CypherLite is Bolt-layer only (ADR-0126 —
`surfaces/cypherlite-bolt/src/auth.rs`, `AuthConfig::{None, Basic}`) and
the engine carries no user identity — the procedure arm sees only the
graph (`cypherlite/cypherlite-executor/src/exec/procedures.rs`,
`builtin_procedure(graph, name)`). ADR-0143 rejected the plumbing
alternative as disproportionate: a new public engine API with the
AGENTS.md §2 docs burden to serve a sidebar display field. ADR-0142
established both the documented-placeholder precedent and the rejection of
Bolt-layer record rewriting (the shape is an engine-level contract; the
CLI and the Rust API must see the same rows as Bolt clients).

The maintainer has now **explicitly requested the plumbing**: the
ADR-0126 Bolt principal is carried engine-ward so `dbms.showCurrentUser`
reports the `--auth user:pw` user instead of the placeholder. The WHETHER
is settled by that request; this ADR decides the HOW and revisits exactly
the ADR-0143 session-context decision (LABS-148). ADR-0143 gains a
**Later references** note naming this ADR; its other three procedures are
unaffected.

The query path today is `Database::connect()` →
`Connection { inner, txn }` (`cypherlite/cypherlite/src/lib.rs`,
`connection.rs`) → `Connection::query` → `run_query` → `dispatch` →
`execute` / `execute_read_at` / `commit_optimistic` →
`exec_call(ctx, …)` → `builtin_procedure(graph, name)`. No session-context
struct exists; the statement-level `QueryCtx`/`ExecCtx`
(`cypherlite/cypherlite-executor/src/ctx.rs`) carry the graph view,
params, and the Cypher version only. On the Bolt side the engine
`Connection` is created **before** HELLO arrives
(`surfaces/cypherlite-bolt/src/protocol.rs`, `db.connect()?` ahead of the
message loop), and the validated HELLO principal is currently discarded
after `AuthConfig::check` passes.

## Decision

**The ADR-0126 Bolt principal is plumbed engine-ward as an optional
per-connection identity, and `dbms.showCurrentUser` reads it.** The
ADR-0143 placeholders-only decision is superseded for
`dbms.showCurrentUser` only.

1. **Public engine API shape.** `cypherlite::Connection` gains:

   ```rust
   impl Connection {
       /// Set (or clear, with `None`) the authenticated principal for
       /// this connection. Surfaces with no notion of identity never
       /// call it.
       pub fn set_principal(&mut self, principal: Option<&str>) -> ();

       /// The principal set on this connection, if any.
       pub fn principal(&self) -> Option<&str>;
   }
   ```

   A setter on `Connection` — not a `Database::connect_with(...)`
   constructor and not a new session-context struct — is the least
   invasive honest design: the Bolt server creates the engine
   `Connection` before the HELLO message arrives, so the principal cannot
   be known at `connect()` time, and no session-context struct exists
   today to extend. The principal rides the existing internal context
   channel: `Connection` stores it, `run_query`/`dispatch` pass it into
   the executor's `ExecCtx` (new `principal: Option<&str>` field), and
   `builtin_procedure` gains a principal parameter. `execute`,
   `execute_read_at` and `commit_optimistic` are internal-crate
   signatures (cypherlite-executor is not a public surface), so their
   change is not a public-API break; the only public additions are the
   two `Connection` methods above. `Database::connect()` is unchanged and
   yields a principal-less connection.

2. **Per-auth-mode behavior** (the Bolt server sets the principal once,
   after the HELLO validates):

   | mode | principal set on the `Connection` | `username` reported |
   |---|---|---|
   | `--auth user:pw` (flag or `CYPHERLITE_AUTH`) | `Some(user)` — the validated HELLO principal, which `AuthConfig::Basic::check` has just proven equal to the preset user | `user` |
   | generated-token default (ADR-0126 stage 3) | `Some("cypherlite")` — the same code path; the generated user is `cypherlite` | `'cypherlite'` |
   | `--auth none` | never set — the HELLO principal is **unverified** under `AuthConfig::None` and is **not** forwarded | `'cypherlite'` (documented placeholder, unchanged from ADR-0143) |

   Under `--auth none` the HELLO principal is an unauthenticated claim:
   reporting it as `username` would present an unverified string as an
   identity — dishonest in exactly the way this ADR exists to fix. The
   constant `'cypherlite'` is a documented placeholder, not a claim, and
   it keeps the zero-config default's behavior byte-identical to
   ADR-0143. Non-Bolt surfaces (CLI, Python, WASM, FFI, and the embedded
   Rust API itself) have no principal and never call `set_principal`;
   they see the same documented-placeholder row.

   `dbms.showCurrentUser` still returns **exactly one row** with the
   ADR-0143 columns: `username` is the connection principal when set,
   else `'cypherlite'`; `roles: ['admin']` and `flags: []` are unchanged.
   `roles: ['admin']` stays because it honestly conveys the engine's
   actual authorization model — there is none, so every connected
   principal can do everything — and it matches the Browser's own
   missing-`roles` fallback; per-user roles would be catalog theater
   beyond need. `flags: []` stays honest (CypherLite has no user flags).

3. **Scope.** Only `dbms.showCurrentUser` becomes honest. The
   `dbms.clientConfig`/`dbms.listConfig` row
   `dbms.security.auth_enabled: 'true'` **stays the ADR-0143 documented
   placeholder**: the channel decided here carries an identity, not an
   auth-mode fact, and plumbing `AuthConfig::None` vs `Basic` engine-ward
   would be a second transport fact crossing the ADR-0142 layering line
   to fix a row that is accurate for both the default and
   `--auth user:pw` deployments and wrong only under the explicit opt-out
   `--auth none` — where the inaccuracy is cosmetic and documented in
   `docs/api/bolt/`. No other procedure, config row, or surface changes
   behavior.

4. **Docs burden.** The two new public methods ship with the full
   AGENTS.md §2 treatment on the `rust` surface: `docs/api/rust/spec.md`
   (contract: `set_principal`/`principal`, lifecycle, that the value is
   display-only and confers no authorization), `docs/api/rust/description.md`,
   and a runnable doctest example; `docs/api/bolt/` documents the
   per-mode table above, including that `--auth none` never forwards the
   unverified HELLO principal. The implementing node writes these; this
   ADR mandates them.

## Alternatives considered

- **Keep the ADR-0143 placeholder** (session context stays unplumbed;
  `username: 'cypherlite'` under every auth mode). Rejected: explicit
  maintainer requirement — the maintainer has asked for the plumbing, so
  the proportionality call ADR-0143 made is revisited by the authority
  that made it.
- **Bolt-layer record rewriting** (intercept the `dbms.showCurrentUser`
  result in the transport and patch in the principal). Rejected per the
  ADR-0142 precedent: the shape is an engine-level contract, the CLI and
  the Rust API must see the same rows as Bolt clients, and intercepting
  and patching result records in the transport is too fragile for a
  display field.
- **A `Database::connect_with(principal)` constructor** (principal known
  at connection-creation time). Rejected: the Bolt server creates the
  engine `Connection` before HELLO arrives
  (`protocol.rs`: `db.connect()?` precedes the message loop), so the
  principal does not exist yet; a setter matches the actual lifecycle.
  A constructor variant also multiplies the public connect API for one
  optional field.
- **A public `SessionContext` struct threaded through the query path**
  (room for future session facts: auth mode, client address, …).
  Rejected: speculative generality — no session-context struct exists
  today, the only consumer is one display field, and every added public
  type carries the AGENTS.md §2 docs burden; if a later ADR needs more
  session facts it can introduce the struct and deprecate the setter.
- **Forward the unverified HELLO principal under `--auth none`.**
  Rejected: dishonest — under `AuthConfig::None` the principal is never
  checked, so reporting it as `username` presents an unauthenticated
  claim as an identity; a documented constant placeholder is the honest
  answer for a mode with no authentication.
- **Report `username: null` (or `''`) when no principal is set.**
  Rejected: a behavior change under the zero-config default for no
  honesty gain — there *is* no authenticated user to name, and the
  Browser renders missing/`'-'` usernames worse than a clearly
  documented constant; the placeholder constant is already established
  by ADR-0143.
- **Plumb the auth mode too and make `dbms.security.auth_enabled`
  honest.** Rejected: a second transport fact crossing the ADR-0142
  layering line for a config row that is accurate in every deployment
  except the explicit `--auth none` opt-out, where the placeholder is
  documented; scope stays at the one procedure the maintainer asked for.

## Consequences

- Under `--auth user:pw` the Browser sidebar shows the real user;
  under the generated-token default it shows `cypherlite` (now an honest
  read, not a placeholder); under `--auth none` and on every non-Bolt
  surface it shows the documented placeholder `cypherlite`.
- The engine gains a per-connection identity concept for the first time.
  It is **display-only**: it confers no authorization, and
  `docs/api/rust/` must say so. A future authorization ADR can build on
  the channel; nothing here commits to one.
- The ADR-0143 placeholder row is superseded for `dbms.showCurrentUser`
  only; `dbms.clientConfig`, `dbms.listConfig` and
  `dbms.licenseAgreementDetails` are unchanged, and
  `dbms.security.auth_enabled: 'true'` remains a documented placeholder.
- Implementing nodes: add `Connection::set_principal`/`principal`
  (with doctests), thread the principal through `ExecCtx` to
  `builtin_procedure`, set it in `surfaces/cypherlite-bolt` after HELLO
  validation (Basic mode only), extend the tests in
  `cypherlite/cypherlite/tests/dbms_procedures.rs` to pin both the
  principal-set and principal-less rows, add a Bolt-side test that a
  `--auth user:pw` session reports `user`, and write the
  `docs/api/rust/` + `docs/api/bolt/` documentation mandated above.
