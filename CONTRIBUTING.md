# Contributing to Relate

Thanks for using Relate — and for wanting to make it better.

## Issues only — no pull requests

Relate does **not** accept pull requests from outside the maintainer team;
this repository is the public home of the source, the docs, and the releases. 
Pull requests are turned off for accounts without write access — not because 
the work is unwelcome, but because the team cannot give external code the review 
bandwidth it deserves.

What we **do** accept, and read carefully, are
[issues](https://github.com/relatedb/relate/issues):

- **Bugs** — something behaves wrong, errors badly, or crashes.
- **Feature requests** — a use case the engine, CLI, or a client doesn't cover.
- **Questions** — when the docs don't get you there (we'll fix the docs).

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

You're welcome to use AI tools for bug reports or feature requests.
If you do:

- **Disclose it.** Add one line to the issue or comment.
- **Own it.** Read and check everything you post, including the
  reproduction. You are responsible for your post and not the AI tool.
- **Keep a human in the loop.** Issues or comments created by an agent
  without a person who hit the problem and read the output may be closed
  without warning. Please don't point autonomous agents at this repository.

Repeated undisclosed or unreviewed AI submissions may lead to being blocked.

## Where to read before filing

- [README](README.md) — install and quick start
- [`docs/guides/`](docs/guides/) — task guides: vector similarity, geospatial,
  CSV import, backup/restore, the storage engine
- [`docs/reference/`](docs/reference/) — Cypher syntax, functions, CLI flags,
  storage formats, the C ABI
- [`docs/adr/`](docs/adr/) — why things are the way they are

Thanks for helping make Relate better.
