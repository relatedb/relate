# Changelog fragments

One file per pull request, one changelog entry per file. The release cut
collects every fragment into the new version section of `CHANGELOG.md`
and deletes the consumed files; a pull request never edits `CHANGELOG.md`
(ADR-0627 — the `changelog-repair` label is the sanctioned escape for
correcting the published record).

This README is the directory's own documentation; the collector never
consumes it.

## File name

`<PR number>-<short-kebab-slug>.md` — the pull request that owns the
entry, then a slug naming the change:

    512-backtick-and-whitespace-parameter-names.md

The number keeps files from colliding between pull requests (the reason
this directory exists: every pull request editing the same
`## [Unreleased]` lines conflicted with every other) and orders the
collected entries — ascending PR number within each section.

## Content

One or more section blocks. The heading names a Keep a Changelog section
— `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, `Security` — or
`Notes`, the maintainer record for notable-but-not-user-facing changes
(the release-notes generator treats `Notes` as Internal-rollup source
only, never release-notes material). The body is the entry exactly as it
lands in `CHANGELOG.md`: one `- ` bullet per entry, wrapped continuation
lines indented two spaces, no other content.

    ### Changed

    - Two parameter-name spellings Neo4j accepts now plan and run: …
      (the entry text, wrapped as it appears in the changelog)

CI validates the pool on every pull request
(`python3 scripts/changes_collect.py --check`); the release cut runs the
collection itself through `scripts/release-bump.sh`.
