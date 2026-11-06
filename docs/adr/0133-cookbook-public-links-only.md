# 0133. Cookbook pages link only to public targets

- **Status**: Accepted
- **Date**: 2026-09-28
- **Deciders**: CypherLite maintainers

## Context

ADR-0130 publishes the cookbook as static pages built by
`scripts/cookbook/build.mjs`. As built, every page linked to GitHub: the
navigation and footer to the repository, a "Recipe source" link to the
recipe's markdown, the footer's "built from <ref>" and "Write a recipe"
links, prose links to ADRs (rewritten to `blob/<ref>/…`), and, through the
`companion` front-matter field (ADR-0130 Decision item 1), a "Full program"
link into the companion examples repository.

The repository is internal and the examples repository is private until
publication (`docs/PUBLISHING.md`). A review of the rendered site found 73
link instances that return 404 to every reader outside the organization,
six on every page. The existing `link-check` CI job (lychee over the recipe
markdown) cannot catch this: it excludes the not-yet-public URLs on purpose,
and it does not see the links the build adds.

## Decision

1. A URL is not public yet while `lychee.toml` excludes it: that `exclude`
   list is the one switch. The build writes no link to such a URL: no GitHub
   navigation or footer links, no "Write a recipe", the build ref as plain
   text, and prose links to repository files or to other not-yet-public URLs
   rendered as their text. The page offers its own markdown (`index.md`) and
   data files (`data/*.json`) instead; the "Recipe source" link is removed.
   Once the list no longer excludes the repository, the repository links
   come back and `companion` renders as a link in the page's "About the
   data" section, with no other change.
2. Before writing the site, the build checks every `<a href>` it renders
   (`scripts/cookbook/lib/links.mjs`) and fails on a link that resolves
   neither to a file it writes (with the `#fragment` present on that page)
   nor to a known page of the site (its home page and playground, under the
   `--site-url` path), and on any link to a not-yet-public URL. External
   liveness stays with lychee over the markdown. Because the check is part
   of the build, it runs in the cookbook CI jobs and in the Pages deploy.
3. The build reads the list from the `lychee.toml` it is given
   (`--lychee`; the repository's by default), parsed with `smol-toml`, and
   fails when the file is not TOML or its top-level `exclude` is not an
   array of strings, rather than treat every URL as public. The cookbook is built from the latest release tag (ADR-0130), but
   the Pages deploy passes `main`'s `lychee.toml`: whether a URL is public is
   a fact about the world, not about the release. Publication edits one list
   on `main` (a `docs/PUBLISHING.md` step) and takes effect at the next
   deploy.

## Alternatives considered

- **Keep the links; they start working at publication.** Until then every
  shared page carries dead links in its navigation, header, and footer, and
  a preview shared outside the organization looks broken. Rejected.
- **Link to the markdown and data served by the site instead of GitHub.**
  Adopted where the site has a copy (the page's `index.md` and its data
  files); the repository's ADRs and the examples have no copy on the site,
  so those links still need the switch. Rejected as the whole answer.
- **A `--github-links` build flag beside the list.** The first version of
  this decision. Two switches for one fact, flipped separately at
  publication, and the flag's branches would outlive it. Rejected.
- **Publish the ADRs and the cookbook README as site pages.** A larger site
  to build and keep in sync, for pages that GitHub renders once the
  repository is public. Rejected for now.
- **Check the built site's links with lychee in CI.** Needs a network and a
  second exclusion list, and would only run in CI, not in local builds or the
  deploy. An offline check inside the build covers the failure seen here.
  Rejected.
- **Read the `exclude` array with a hand-written lexer.** No dependency, but
  a character-level state machine for a TOML subset has to be kept in step
  with TOML for a list that gates publication. Rejected.

## Consequences

- Pages built today, locally, in CI, and by the Pages deploy, contain no
  dead repository links; a new one fails the build.
- Readers lose the path from a page to its source on GitHub and to the
  companion program until publication. The page still offers its own
  markdown and data.
- Publication removes the not-yet-public entries from `lychee.toml` on
  `main`; the next deploy links the repository and the examples, with no
  release needed.
- `scripts/cookbook` gains `smol-toml` (no dependencies, BSD-3-Clause), a
  build-time dependency only.
- The deploy's cookbook build takes a flag the release's `build.mjs` must
  know: every release that ships the cookbook does, since `--lychee` lands
  with it.
- ADR-0130 Decision item 1 ("each links its full program through the
  `companion` front-matter field") and its Consequences ("The nav gains a
  Cookbook link before GitHub") hold once those URLs are public.

## Later references

- ADR-0288 exempts the navigation's GitHub link from Decision item 1: the
  cookbook navigation links the repository while it is internal, matching
  the landing page, and the link check (Decision item 2) accepts exactly the
  repository URL. Every other not-yet-public link is unchanged.
- ADR-0528 moves the cookbook to the relate-web repository: the link policy
  now lives in relate-web, which keeps its own list of URLs that are not
  public yet.
