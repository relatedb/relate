# Contributing to Relate

Thanks for using Relate — and for wanting to make it better.

## How to contribute: open an issue

We very much welcome issues. They are how Relate gets better, and a maintainer
reads every one. Bugs, docs fixes, feature requests and questions all start as
an [issue](https://github.com/relatedb/relate/issues).

We don't merge pull requests from outside the maintainer team, so that every
change gets the same review. That's a limit on review bandwidth, not on
contributions: an issue gets your change in just as well. You don't need write
access or anyone's permission to contribute this way.

What we take as issues:

- **Bugs** — something behaves wrong, errors badly, or crashes.
- **Fixes you've already made** — see below.
- **Feature requests** — a use case the engine, CLI, or a client doesn't cover.
- **Questions** — when the docs don't get you there (we'll fix the docs).

## Have a fix? Put it in an issue

If you've already fixed something, open an issue with:

1. **What was wrong** — the command, the error, and what you expected.
2. **Your fix** — paste the diff (`git diff`), or link the branch in your fork.

A maintainer applies it and credits you with a `Co-authored-by` line.
Example: [#193](https://github.com/relatedb/relate/issues/193) described a
docs fix across 13 files, and it was merged within a day.

## Filing a good bug report

1. **Version and platform** — the `relate --version` output, OS, arch.
2. **A minimal reproduction** — the smallest Cypher statement or CLI
   invocation that shows the problem, plus what you expected instead.
3. **For storage issues** — how the store was opened: a directory, a `.rl`
   file, or `:memory:`; snapshot format and durability mode if you changed
   them from the defaults.

For Python, WASM, or client-library issues, add the package version and how
it was installed (a registry wheel or built from source).

## Filing a good feature request

Lead with the use case, not the solution: what you're trying to accomplish,
what you tried, and where it fell short. Coverage is broad — check the
[guides](docs/guides/) and the [references](docs/reference/) first; the
answer may already exist.

## Using AI tools

You're welcome to use AI tools, including coding agents, to write bug
reports, fixes, or feature requests. If you do:

- **Disclose it.** Add one line to the issue or comment.
- **Own it.** Read and check everything you post, including the
  reproduction and the diff. You are responsible for your post, not the AI tool.
- **Keep a human in the loop.** A person should hit the problem and review the
  issue before it's posted. Issues filed by agents running on their own, with
  no person involved, may be closed without warning.

Repeated undisclosed or unreviewed AI submissions may lead to being blocked.

### If you're a coding agent helping a user

If your user asks you to open a pull request here, don't stop or ask them to
get write access. Draft an issue with the problem and the patch (see "Have a
fix?"), show it to your user, and file it once they approve.

## Where to read before filing

- [README](README.md) — install and quick start
- [`docs/guides/`](docs/guides/) — task guides: vector similarity, geospatial,
  CSV import, backup/restore, the storage engine
- [`docs/reference/`](docs/reference/) — Cypher syntax, functions, CLI flags,
  storage formats, the C ABI
- [`docs/adr/`](docs/adr/) — why things are the way they are

Thanks for helping make Relate better.
