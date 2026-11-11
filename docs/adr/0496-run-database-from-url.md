# ADR-0496: Run from URL — `--database` accepts a remote `.cl` bundle

Status: Proposed

Claim: 0b48269065bae51d75ccb7269b7e1901e66764dc

## Context

A CypherLite database that lives in one `.cl` file (ADR-0074) is a
publishable artifact: a demo dataset, a test fixture, a bug report's store.
Sharing one today means "download this file, then point `--database` at it"
— an out-of-band step the CLI cannot express, and the step most likely to
be skipped or mis-spelled when a URL is what the user actually has (a
GitHub blob link is the usual shape).

The engine deliberately refuses remote URIs: `reject_remote_scheme`
(ADR-0044) answers any `scheme://` other than `file://` with "`s3://` is not
an engine URI (ADR-0044); object storage belongs to clients implementing
embedded Relate", and the
`.cl` bundle route opens a **host file** it locks (ADR-0074). So a URL store
cannot be an engine URI without dragging network access, credentials, and a
retry policy into the storage layer — and it would still need a local file
for the bundle's own host-file locking.

The CLI already has the precedent for the other direction of this trade:
`--fetch-params` (ADR-0267) fetches a URL in the CLI process and hands the
engine a value, explicitly *not* an engine capability. LABS-872 (2026-11-03)
asked for the same shape for a database, with the user's direction: the
`--database` flag takes the URL, auto-detected; a remote `.cl` file first;
and no network fetch under safe mode.

## Decision

1. **`--database` accepts an `http(s)` URL** naming a `.cl` single-file
   bundle. Detection is the scheme prefix, ungated (ADR-0104, ADR-0270), so
   every build parses a URL `--database` and every policy refusal below
   applies in every build; only the fetch is feature-gated. Every value that
   is not an `http(s)` URL keeps today's plain local-path behavior exactly.
2. **A GitHub file URL is rewritten** to its
   `raw.githubusercontent.com/<owner>/<repo>/<ref>/<path>` form — both the
   shareable **blob** page link people paste and GitHub's own **raw** verb
   (which would otherwise 302 there), so the bytes arrive in one request.
   Every other host (a raw URL, a loopback test server, anyone else's file
   host) is fetched unchanged, and **redirects are followed** (ureq's
   default chain), so a release-asset URL or any other `30x` that lands on
   the bundle bytes works.
3. **The URL must name a `.cl` file, and folder URLs are refused before any
   fetch**, with the working form named in the error: a repository URL, a
   `tree` URL, a non-`blob` verb, a blob URL with no file path, and a URL
   whose last path segment does not end in `.cl` (query and fragment
   stripped). A remote **native directory** is not supported: it is a
   multi-file layout with its own WAL protocol, and a `.cl` bundle is the
   one self-contained artifact. **`https://` is required for any host;
   plain `http://` is accepted only for a loopback host** (`localhost`,
   `127.0.0.0/8`, `::1`) — the e2e fixtures and a local dev server — and
   refused otherwise with the `https://` form named. There is no
   `--allow-plaintext`-style opt-in here (unlike `pull`, ADR-0122): a
   database fetched over plaintext arrives unverified on a transport anyone
   on the path can rewrite, and this fetch has no signature story either.
   The policy is enforced across **redirects** too: every `https://` fetch
   pins `https_only` on its client (the LABS-470 pin the release channel
   carries), so a `302` cannot downgrade the transport past the scheme the
   URL already passed. The loopback plain-`http` carve-out is the one
   release — there the pin would refuse the first hop — and a local server's
   own redirects are the local server's trust.
4. **The CLI fetches and stages.** One capped GET (1 GiB, 300 s global)
   through the shared `url_fetch` helper the `--fetch-params` fetch also
   rides — its **streaming** form (`get_into`), so the body goes straight to
   the staged file through a fixed-size buffer and a bundle at the cap never
   lives in RAM (`--fetch-params` keeps the buffered form, since it parses
   the document in memory); the bytes are written to a private temp path named by the shared
   `atomic_file::staging_name` recipe (pid + counter + nanosecond clock) with
   the `.cl` suffix, created `create_new` and `0o600` from birth.
   A **scratch** copy (a read-only open) is registered for cleanup before its
   first byte lands and removed on every post-parse exit (`exit_flushed`;
   `process::exit` skips destructors); a **kept** copy (a writable open)
   never registers, so nothing removes it. A failed fetch removes its own
   partial file directly — the rollback belongs to the fetch that owns the
   failed file, whatever the disposition of a download that never
   completed. The engine then opens the staged path through the ordinary
   `.cl` bundle route — codec, superblock, and embedded WAL are the store's
   own, not a remote variant.
5. **A downloaded copy is a local store like any other; only the remote
   origin is never written back.** The staged `.cl` opens with the access
   mode the flags chose — writable by default, `--read-only` honored
   exactly as for a local path (a write on a read-only copy refuses with
   the engine's `WriteOnReadOnlyAccessDatabase`, exit 6). A writable copy
   is the user's working database, so it is **kept** after the run (its
   path is in the announcement); a read-only copy is scratch and is removed
   on exit. (The first cut of this ADR forced read-only on every download —
   the user's direction changed it: a copy this process downloaded is
   local, and deleting a modified copy would silently discard writes.)
   `--read-only=live` is a usage error (exit 2): live freshness means
   tailing a WAL beside a writer, and there is no writer beside a
   downloaded file.
6. **Safe mode refuses a remote `--database` URL** (exit 2, before any
   network I/O; ADR-0477): a downloaded store is untrusted input, so safe
   mode introduces none. The refusal precedes the capability gate, so it
   reads the same in every build, and it is scoped to the store — a
   `--fetch-params` URL source binds a parameter, not a store, and is not
   governed by it.
7. **Own Cargo feature** `remote-database` (`full` + `agent`, not
   `minimal`), per AGENTS.md's "new CLI capability → own feature". Without
   it the URL still parses and the fetch fails closed (exit 1) with
   `<url>: the remote bundle fetch is not compiled into this build (enable
   the "remote-database" feature)`.
8. **Exit-code classes**: an unsupported URL shape, safe mode, and live
   freshness are usage errors (exit 2); a fetch or staging failure is a
   runtime error (exit 1); a write on an explicitly `--read-only` copy
   keeps the engine's semantic class (exit 6).
9. **CLI-only, not an engine capability.** The engine's URI surface keeps
   refusing remote schemes (ADR-0044); there is no `https://` store URI, no
   engine-side credential handling, and no change to `parse_uri`. A remote
   database is fetched with the same trust as a caller-typed path: no
   signature check and no host allowlist (the ADR-0267 posture for
   `--fetch-params`).
10. **Scope: the statement session.** `serve`, `info`, and `clone` keep
    local paths; giving them URL support is a separate decision per verb.
    `--wal-dir` with a remote URL stays refused by the engine (a `.cl`
    bundle embeds its WAL, ADR-0074).

## Alternatives considered

- **An engine URI scheme (`https://…` in `parse_uri`)** — rejected: it puts
  network access, credentials, and retry semantics inside storage, and the
  `.cl` bundle route needs a host file to lock, so a remote URI would still
  have to materialize a local file first. ADR-0044's boundary stands.
- **A download cache under the config directory, keyed by URL** — rejected
  for this change: no invalidation story (a URL is not a version), and a
  stale database is a worse failure than a re-fetch. A cache with an
  explicit `--refresh`/TTL contract is a possible follow-up, not a silent
  default.
- **A remote folder / native-directory store (tar, zip, or per-file
  fetches)** — rejected: it adds an archive format and an extraction surface
  for a case a `.cl` bundle already covers, and the WAL protocol is defined
  against a local directory.
- **Polling the URL to fake live freshness for `--read-only=live`** —
  rejected: "live" is defined as tailing a WAL beside a writer (ADR-0480);
  a poll of an immutable download is a different feature (a refresh), and
  naming it `live` would make the flag mean two things.
- **Fetching inside the engine's open path behind a flag** — rejected: the
  engine would need the network stack, and the CLI already owns the
  process-level fetch posture (`--fetch-params`, `pull`).

## Consequences

- A database is now shareable as one line: `cypherlite -d
  https://github.com/<owner>/<repo>/blob/<ref>/store.cl "<query>"`.
- The download is announced on stderr — `--database <url>: downloaded,
  opening read-only` for a scratch copy, `--database <url>: downloaded to
  <path> (read-write; the local copy is kept there)` for a working copy —
  and `.info`'s cold store read reports the **staged**
  path — the local file is the store; the URL is the provenance.
- Telemetry keeps classifying the run as a persistent store
  (`StorageKind::for_uri` on the staged `.cl` path), unchanged from a local
  `.cl`.
- The fetched database is untrusted input of the same class as a downloaded
  file: the bundle's own open path validates it (superblock, codec,
  embedded WAL), and a corrupt or non-bundle body surfaces as the engine's
  open error, not a remote-specific one.
- The e2e coverage serves a real cloned bundle from a loopback stub, so the
  fetch path, the writable and read-only opens, the kept-copy survival,
  the read-only write refusal, both policy refusals,
  and the HTTP-error class are pinned without any external network.

## Later references

ADR-0500 withdraws this ADR's §1 and §5: `--database` is the store
**target** only (an `http(s)` URL in it is a usage error naming
`--load`), and the kept-copy disposition is gone — every fetched copy is
scratch. The withdrawal predates any release, so no shipped spelling
changes. Everything else here (§2–§4, §6–§9: the GitHub rewrite, the
redirect and scheme policy, the `.cl` requirement, the capped streaming
stage, the safe-mode refusal, the feature gate, the CLI-process posture)
carries over unchanged to `--load`'s remote sources.
