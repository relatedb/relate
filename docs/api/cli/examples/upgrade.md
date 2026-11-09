# Example: check for a newer release

```bash
relate upgrade --check
```

Expected stdout reports the running binary's current version against the
latest release listed by the website's `releases.json` (ADR-0250) — e.g. an
"already up to date" line when they match, followed by the two-line rename
notice ([Lifecycle commands](../spec.md#lifecycle-commands)). The run is
read-only: it changes nothing on disk and exits 0.

Point the metadata URL at a local fixture with `RELATE_RELEASES_JSON_URL`
(the documented test seam; `RELATE_RELEASES_JSON_FALLBACK_URL` overrides the
fallback host consulted when the primary read fails — ADR-0550, temporary
until launch):

```bash
RELATE_RELEASES_JSON_URL=http://127.0.0.1:8000/releases.json \
  relate upgrade --check
```

A real upgrade probes the direct release-download route first. While the
repository is internal that route answers 404, so the run needs
`GITHUB_TOKEN` to fall back to the authenticated GitHub API asset route
(ADR-0358); without a token that 404 aborts with one line naming
`GITHUB_TOKEN` and the reinstall one-liner. Both route bases are test seams
too — `RELATE_RELEASE_DOWNLOAD_URL` (default
`https://github.com/relatedb/relate/releases/download`) and
`RELATE_GITHUB_API_URL` (default `https://api.github.com`) — so a fixture
server can serve the three assets without touching GitHub (let the direct
probe answer 404 to exercise the fallback):

```bash
RELATE_RELEASES_JSON_URL=http://127.0.0.1:8000/releases.json \
RELATE_RELEASE_DOWNLOAD_URL=http://127.0.0.1:8000/download \
RELATE_GITHUB_API_URL=http://127.0.0.1:8000/api \
GITHUB_TOKEN=test-token \
  relate upgrade
```
