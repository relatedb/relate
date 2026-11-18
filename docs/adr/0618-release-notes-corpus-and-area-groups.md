# 0618. Release notes read a harvested PR corpus and fixed area groups

- **Status**: Accepted
Claim: 28c5791dcaa86db8e03bf5558a7862e4d4508761
- **Date**: 2026-11-17
- **Deciders**: the maintainers
- **Amends**: ADR-0124

## Context

ADR-0124 generates release notes with a sandboxed OpenCode run: the agent
reads the released CHANGELOG.md section, corroborates with the git log,
and its section is prepended to GitHub's own auto-generated pull-request
list. A recent minor release exposed three weaknesses: the changelog
section arrived thin (merged pull requests had filed their entries under
an already-released section, where the release-cut rotation never collects
them, and no check caught it); the multi-step git-log corroboration was
prompted but skipped by the pinned router model; and the auto-generated
list — a hundred uncategorized titles carrying internal tracker ids —
dominated the published page.

## Decision

**Feed the generator a deterministically harvested corpus, organize its
output under a fixed area taxonomy, and guard changelog placement at
pull-request time.**

1. **Corpus harvest.** A trusted workflow step (git log over the tag range
   plus one `gh pr list` call) writes, before the agent runs, a corpus
   file of every merged pull request: number, subject, author, labels, and
   the description's Summary, User-facing impact and Example usage
   sections. The sandboxed agent reads the file; its shell is a single
   validating read wrapper — one lens on git history and the notes
   files, whose argument shapes are checked before any command runs and
   whose refusals never echo the argument (an expanded secret dies
   unnamed). A harvest failure with pull requests in range fails the run
   — no silent thin input. The trust boundary the harvest enforces:
   markup that renders as nothing on the pull-request page is stripped
   (the agent sees what a reviewer saw), and the prompt frames the
   corpus as data, never instructions. (Amended by §5 below.)
2. **Fixed area groups.** The generated "Changes by area" section
   organizes the changelog's entries under a fixed ten-area table —
   Performance & memory; Cypher language & conformance; CLI; Bindings
   & clients; Servers & protocols; Storage & durability;
   Agent-friendliness; Packaging & compliance; Docs & guides; Internal
   — canonical in the prompt. Every changelog entry lands in exactly
   one group or a highlight, empty groups are checked but omitted, and
   GitHub's auto-generated list stays on the page below as
   the full audit trail.
3. **Model tier.** Generation — judgment-heavy, public-facing prose — runs
   on a pinned strong model, overridable through a `RELEASE_NOTES_MODEL`
   repository variable or secret, the same override shape the review
   workflows use.
4. **Placement guard.** CI refuses a pull request whose CHANGELOG.md diff
   touches anything but the `[Unreleased]` section (its body and its link
   ref); a `changelog-repair` label is the sanctioned escape for
   correcting the historical record. `[Unreleased]` may carry a `### Notes`
   subsection — one line per notable-but-not-user-facing change — which
   rides the rotation into the released section as maintainer record,
   never as release-notes material (Internal-rollup source only). When a
   pull request describes user-facing impact but changes no changelog
   entry, CI posts a warning annotation — reviewer judgment stays in
   charge of what counts as user-facing; the warning closes the gap
   humanly.

5. **Input trust model (amendment).** The released changelog section —
   reviewed at pull-request time, frozen at merge — is the complete
   what-changed source, and the coverage set the generator walks. The
   corpus contributes ONLY each merged pull request's Example usage
   section, quoted runnable material for highlights: pull-request
   descriptions are untrusted, post-merge-editable text, so the channel
   that reaches the agent is narrowed to quoted examples, and the output
   guard defends that one channel — built on real parsers rather than
   pattern matching. Every literal URL is parsed as a URL (userinfo,
   ports and off-allowlist hosts refused; code fences included, a fenced
   command is a copy-paste target), encodable markup outside code fences
   is refused by presence rather than decoded, and the payload is
   decoded by a JSON parser and judged the same way. The original design
   fed the generator full pull-request bodies as a completeness
   backstop for thin changelog sections; entry discipline at
   pull-request time (the placement guard, the warning above, the
   pull-request template's entry duty) addresses the same failure
   earlier, with a human already in the loop.

## Alternatives considered

- **GitHub release-notes configuration with exclusion labels** — rejected:
  it needs pull-request labeling discipline the flow does not have, and it
  filters without curating or grouping.
- **Stripping the auto-generated list from the release body** — rejected:
  per-pull-request links and the new-contributor section stay valuable;
  the full list remains the audit trail below the curated section.
- **Stronger dig instructions, same router model** — rejected: the dig was
  already instructed and skipped; deterministic harvesting removes the
  multi-step shell work instead of hoping it happens.
- **Requiring a changelog entry in every pull request** — rejected: the
  changelog is a curated user-facing document, not an audit log; most
  merged work belongs in no section.
- **A human reading the notes before publish** — rejected: the pipeline's
  contract (ADR-0124) is automatic publish; the output checks bound the
  dangerous payload classes, and the prose-level residual equals the
  trust the changelog always carried — contributor-written summaries.

## Consequences

One extra `gh pr list` call per release run. The prompt is the canonical
home of the area table and of the end-user voice — internal ids and
vocabulary never reach the page, and the output guard refuses the id
classes — a group change edits one file. Misplaced entries fail at
pull-request time instead of thinning a future release's notes, and a
declared user-facing impact without an entry warns there too; releases
cut before the guard may need the repair label. Coverage now rests on
entry discipline at pull-request time — a user-facing change without
its changelog entry ships unreported, and the warning plus the
pull-request template's duty are what catch it. Example usage sections
become load-bearing for highlights, so they are worth writing well.
