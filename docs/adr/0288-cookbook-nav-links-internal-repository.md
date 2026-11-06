# 0288. The cookbook navigation links the repository while it is internal

- **Status**: Accepted
- **Renumbered**: from 0177 (shared-number cleanup)
- **Date**: 2026-10-07
- **Deciders**: CypherLite maintainers (decided in conversation)

## Context

ADR-0133 makes `lychee.toml`'s `exclude` list the one switch for links to
URLs that are not public yet: the cookbook build writes no link to such a
URL, and fails on any that slips through. The repository,
`github.com/neo4j-labs/cypherlite`, is on the list, so the cookbook's
navigation has no GitHub item.

The rest of the site's navigation does. The landing page
(`neo4j-labs.github.io/cypherlite/`, kept on the `gh-pages` branch, outside
the cookbook build) lists Examples, Reference, Quickstart, Cookbook, GitHub
and Playground. Opening Cookbook from it drops GitHub from the bar, which
reads as a broken site rather than as a policy. The repository stays
internal for now: organization members can open it, and every other reader
gets a 404.

## Decision

1. The cookbook navigation always links the repository (`--repo-url`) as
   `GitHub`, as the landing page does, whether or not `lychee.toml` excludes
   it. The navigation then matches across the site.
2. This is the only exception to ADR-0133. The footer's GitHub link, the
   "built from <ref>" and "Write a recipe" links, the index page's README
   and source links, prose links into the repository, and `companion` links
   still follow the list.
3. The build's link check (`checkLinks` in `scripts/cookbook/lib/links.mjs`)
   takes the URLs that may be linked anyway (`exempt`); the build passes
   exactly the repository URL. A link to any other not-yet-public URL,
   including any path under the repository, still fails the build.

## Alternatives considered

- **Keep ADR-0133 as it is; the cookbook has no GitHub item until
  publication.** The policy is consistent, but the navigation changes
  between pages of one site. Rejected: the maintainers prefer one
  navigation bar.
- **Drop the GitHub item from the landing page until publication.** Makes
  the navigation match by removing the link for organization members too,
  and the landing page is maintained outside this repository's build.
  Rejected.
- **Make the repository public and delete the `lychee.toml` entries.** The
  `docs/PUBLISHING.md` path, which turns every repository link on. Not
  available while the repository stays internal. Rejected for now.
- **Exempt every repository link from the check.** Would bring back the
  footer, source and ADR links, the 73 dead links ADR-0133 removed.
  Rejected: one navigation item is the inconsistency to fix.
- **Mark the exempt anchor in the HTML (a class or `data-` attribute) for
  the check to skip.** Any template could then mark any link, and the check
  could no longer say which URLs are exempt. Rejected for an explicit URL
  list passed by the build.

## Consequences

- Every cookbook page's navigation shows GitHub. Readers outside the
  organization who follow it get GitHub's 404 until the repository is
  public. This is the one dead link ADR-0133's Alternatives rejected keeping,
  accepted here for the navigation only.
- The Pages deploy builds the cookbook with the latest release's generator
  (ADR-0130), so the item appears at the first deploy after a release that
  includes this change, not at the next deploy from `main`.
- Publication (`docs/PUBLISHING.md`) is unchanged: deleting the `lychee.toml`
  entries turns the remaining repository links on. The exemption then has no
  effect and can stay.
- ADR-0133 Decision item 1 ("no GitHub navigation or footer links") and
  ADR-0130's "the nav's GitHub link render[s] once those URLs are public" no
  longer hold for the navigation.

## Later references

- ADR-0528 moves the cookbook to the relate-web repository: the navigation
  this ADR covered is now the site's own header.
