# 0079. Anonymous usage metrics in the CLI

- **Status**: Proposed
- **Date**: 2026-09-21
- **Deciders**: CypherLite maintainers (Linear LABS-12, LABS-55, LABS-42)

## Context

CypherLite ships as a raw `cypherlite` CLI binary on GitHub Releases
(ADR-0067, installed via the ADR-0068 installer or the ADR-0069 `upgrade`
command), and we have no visibility into how that binary is used: which
invocation modes are common, which output modes and storage knobs are
exercised, whether queries succeed, or whether the lifecycle commands are
discovered at all. Linear issue **LABS-12** ("Usage metrics",
<https://linear.app/neo4j/issue/LABS-12/usage-metrics>) asks for anonymous
usage telemetry, with **LABS-55** ("Collect metrics + dimensions") defining
the event/dimension scope and **LABS-42** ("Metrics opt-out") the consent
surface.

The reference implementation is neo4j-cli’s `common/analytics` (Mixpanel
HTTP, background worker, machine ID), `common/clievents` (event taxonomy),
and `common/clicfg` (telemetry config key). This decision adopts that shape
for CypherLite, adapted to this repository’s constraints: a hand-rolled flag
parser in `crates/cypherlite-cli`, a workspace `ureq = "3.4"` dependency
already used by the `upgrade` command, and the ADR-0069 grammar rule that
lifecycle verbs are positional commands, never flags.

Privacy is the binding constraint. CypherLite is an embedded engine whose
users pipe real query text and real database paths through the CLI; a
telemetry system that could ever capture either is a non-starter. The design
below makes that capture structurally impossible (event builders have no
fields for query text, paths, or parameter values) rather than merely
policy-prohibited.

The decisions below are already agreed; this ADR records them before
implementation, per the repository’s decision-log rule.

## Decision

Anonymous usage telemetry is collected by the `cypherlite` CLI binary only,
sent to Mixpanel, identified by a random install ID, delivered off the hot
path, and governed by a layered opt-out that defaults to on.

1. **Scope: CLI binary only.** Telemetry lives in `crates/cypherlite-cli`.
   The embedded library (`cypherlite::Database`), the C ABI
   (`crates/cypherlite-ffi`), and `cypherlite-bolt` never phone home. Bolt
   may adopt metrics later via a new ADR; nothing here precludes it.
2. **Sink and transport: Mixpanel over the existing `ureq`.** Events are
   sent as `POST https://api.mixpanel.com/track` (default endpoint constant
   `DEFAULT_ENDPOINT`; the EU-residency host is
   `https://api-eu.mixpanel.com/track`) with a JSON array body of
   `{"event": ..., "properties": {"token": ..., "time": ...,
   "distinct_id": ..., "$insert_id": ..., ...}}` objects. The HTTP client is
   hand-rolled over the existing workspace `ureq = "3.4"` dependency
   (already used by `upgrade`, ADR-0069); install-ID and `$insert_id`
   randomness come from the existing workspace `rand` dependency (version
   pinned by ADR-0360). **Zero new dependencies.**
3. **Identity: random install ID, not a machine ID.** A random UUID is
   generated once and persisted as the install ID in state under the XDG
   config directory — `$XDG_CONFIG_HOME/cypherlite/` when
   `$XDG_CONFIG_HOME` is set, otherwise `~/.config/cypherlite/`. It is the
   `distinct_id` on every event. No hardware or OS machine identifier is
   ever read.
4. **Events v1 (minimal taxonomy).** Event names carry the
   `CYPHERLITE-CLI_` app prefix so the tracked surface is explicit in
   Mixpanel; a future Bolt adoption gets its own prefix (e.g.
   `CYPHERLITE-BOLT_`) rather than sharing the CLI's. Four events:
   - `CYPHERLITE-CLI_STARTUP` — `kind`: `oneshot` | `stdin` | `repl` |
     `version` | `help`.
   - `CYPHERLITE-CLI_QUERY` — `success` (bool), `source`: `oneshot` | `stdin` |
     `repl`, `storage`: `memory` | `persistent`, `output_mode`.
   - `CYPHERLITE-CLI_COMMAND` — lifecycle `command` name, `action`, `success`.
   - `CYPHERLITE-CLI_HELP` — fired when help is shown.

   Base dimensions on every event: `distinct_id` (install ID), `$os`,
   `arch`, CLI `version`, `uptime`, and `invoker` (`human` | `agent` |
   `ci`, derived from TTY detection, `CI` env, and known agent env vars).
   **Hard privacy rule:** never query text, never database paths, never
   parameter values — the event builder types have no fields for them, so
   capturing them is a compile error, not a policy violation.
5. **Delivery: non-blocking, best-effort, invisible.** Events go onto a
   bounded channel that drops on overflow; a single background thread drains
   it with a 2-second HTTP timeout; a bounded `flush()` runs on exit so the
   CLI is never perceptibly slowed. All telemetry errors are swallowed:
   nothing on stdout or stderr, exit codes unaffected.
6. **Opt-out: layered, default ON.** First match wins:
   `DO_NOT_TRACK=1` (the consoledonottrack convention) →
   `CYPHERLITE_TELEMETRY` env var (`0`/`false`/`off` disables) → persisted
   `telemetry: false` in `<config>/cypherlite/config.json` → enabled.
   Persistent control is a new positional lifecycle command `cypherlite
   telemetry on|off|status` following the ADR-0069 grammar (recognized only
   as the first argument; `./telemetry` and `-- telemetry` remain reachable
   as database paths). There is deliberately **no `--no-telemetry` flag**:
   per ADR-0069, flags configure query-engine runs and lifecycle actions are
   commands. A one-time stderr notice on the first interactive run points at
   the opt-out; a `notice_shown` flag in state suppresses repeats.
7. **Dev/test overrides.** `CYPHERLITE_MIXPANEL_ENDPOINT` and
   `CYPHERLITE_MIXPANEL_TOKEN` environment variables override the
   compiled-in defaults, so development and CI can point at a mock sink.
   The binary ships with the shared Neo4j Labs CLI Mixpanel project token —
   the same project neo4j-cli reports into; events are namespaced by their
   `CYPHERLITE-CLI_` prefix, so both tools coexist in one project.
8. **Documentation in the same change.** `docs/api/cli/spec.md` gains the
   `telemetry` lifecycle command in its command table, the new environment
   variables in its Environment table, and a **Usage metrics** section
   listing every event and every property — that listing is the privacy
   disclosure. A runnable `docs/api/cli/examples/telemetry.md` is added, per
   the repository’s client-facing documentation rule.

## Alternatives considered

- **Hardware/machine ID as `distinct_id` (neo4j-cli does this)** — rejected:
  a random install ID gives the same session correlation without touching OS
  hardware identifiers.
- **A separate `cypherlite-telemetry` crate** — rejected: the CLI is the
  only consumer today; a module keeps it simple and can be promoted to a
  crate when Bolt adopts metrics.
- **An off-the-shelf Mixpanel Rust SDK** — rejected: the `/track` API is a
  single JSON POST; the existing `ureq` dependency covers it and avoids an
  unmaintained dependency.
- **Opt-in-only collection** — rejected: default-on with prominent, layered
  opt-out is what makes the data useful; the one-time notice plus the
  `DO_NOT_TRACK`/env/command layers keep it respectful.

## Consequences

- The CLI gains a telemetry module whose event-builder types structurally
  exclude query text, database paths, and parameter values; any future
  attempt to add such a field is a visible API change requiring an ADR
  referencing this one.
- A fourth positional lifecycle command, `telemetry on|off|status`, joins
  the ADR-0069 grammar; database paths literally named `telemetry` stay
  reachable via `./` or `--`.
- New documented environment variables of the CLI: `DO_NOT_TRACK`,
  `CYPHERLITE_TELEMETRY`, `CYPHERLITE_MIXPANEL_ENDPOINT`, and
  `CYPHERLITE_MIXPANEL_TOKEN`; `docs/api/cli/spec.md` and
  `docs/api/cli/examples/telemetry.md` must land in the same change as the
  code, per the repository’s CLI documentation gate.
- State under `$XDG_CONFIG_HOME/cypherlite/` (or `~/.config/cypherlite/`)
  now holds the install ID, `config.json` (`telemetry` key), and the
  `notice_shown` flag; the layout of that directory is now load-bearing and
  changes to it require a new ADR.
- The binary makes outbound HTTPS calls by default; users in offline or
  high-sensitivity environments rely on the `DO_NOT_TRACK` /
  `CYPHERLITE_TELEMETRY` layers, and the all-errors-swallowed rule means a
  blocked network is indistinguishable from success from the user’s point
  of view.
- Distribution under the Apache-2.0 license (ADR-0055) is unaffected: the
  telemetry source and the full event/property listing ship in the repo, so
  what the binary sends is fully auditable.
- The `MIXPANEL_TOKEN` constant ships with the shared Neo4j Labs CLI project
  token in released binaries. It is a client-side ingestion token — public by
  design, same posture as neo4j-cli — and the full event/property listing in
  `docs/api/cli/spec.md` keeps what the binary sends auditable.

## Later references

- ADR-0263 restricts the §7 `CYPHERLITE_MIXPANEL_ENDPOINT` override: the value
  is no longer used verbatim "as-is". It must be `https://` for any host, or
  `http://` only for loopback (`localhost`, `127.0.0.1`, `::1`); a rejected
  override disables telemetry for the run and prints one stderr warning.
- ADR-0588 moves §2's `/track` client, §6's layered opt-out resolution, and
  §3's state files into `surfaces/relate-feedback`, shared with the feedback
  surface; the telemetry verbs, event taxonomy, envelope builder, and spool
  stay in the CLI.
- ADR-0545 replaces §5's delivery shape: the exit flush no longer waits on
  the network — pending events spool locally and the next enabled run
  delivers them. It also extends §3's config-directory resolution to
  Windows, which never resolved there.
