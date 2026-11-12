# xtask

Workspace maintenance tasks for Relate, run through the
`cargo xtask` alias defined in [`.cargo/config.toml`](../../.cargo/config.toml)
(`xtask = "run -p xtask --"`). Depends only on `std`, `serde`,
`serde_json` and `tempfile`.

## Usage

```sh
cargo xtask check-boundaries
cargo xtask check-package
```

```text
Usage: cargo xtask <COMMAND> [OPTIONS]
```

| Command | Description |
| --- | --- |
| `check-boundaries` | Check workspace dependency edges against ADR-0103 rules R1-R5 |
| `check-package` | Package the `relate` facade and build the extracted crate against the workspace |

| Flag | Default | Description |
| --- | --- | --- |
| `-h`, `--help` | off | Print help (top level or after either command) and exit 0 |

No positionals beyond the command. Missing or unknown commands and unexpected
arguments print an error plus the help text and exit 2. Neither command takes
any other option.

## `check-boundaries`

```text
Usage: cargo xtask check-boundaries [OPTIONS]
```

Reads `cargo metadata --format-version 1 --no-deps`, classifies every
workspace member by its directory (`relate/<pkg>`, `surfaces/<pkg>`,
`testing/<pkg>`) and checks every workspace-internal dependency edge by kind
(`normal`, `dev`, `build`) against
[ADR-0103](../../docs/adr/0103-engine-surfaces-testing-layout.md):

| Rule | Check |
| --- | --- |
| R1 | Normal and build deps of `relate/*` target only `relate/*` |
| R2 | Dev deps of `relate/*` target `relate/*` or `testing/relate-test-support` |
| R3 | Normal and build deps of `surfaces/*` target `relate/*` or `surfaces/*`; dev deps may also target `testing/relate-test-support` |
| R4 | `testing/*` may depend on any workspace package |
| R5 | Every workspace member lives directly under `relate/`, `surfaces/` or `testing/` |

On success it prints one summary line to stdout:

```text
check-boundaries: 27 packages, 76 edges checked, 0 violations
```

On failure it prints each violation to stderr, followed by the summary line:

```text
relate-storage -> relate-formatter (normal): R1
relate-old (legacy/relate-old): R5
check-boundaries: 28 packages, 77 edges checked, 2 violations
```

| Exit status | Meaning |
| --- | --- |
| 0 | No violations |
| 1 | One or more violations, each printed as `<from-pkg> -> <to-pkg> (<dep-kind>): <rule id>` or `<pkg> (<dir>): R5` |
| 2 | Usage error or `cargo metadata` failure |

### Executor `Vec<Row>` guard (ADR-0555)

`check-boundaries` also fails when a non-test fn under
`relate/relate-executor/src` returns `Vec<Row>` or `Result<Vec<Row>>` and is
not listed in [`row_vec_allowlist.txt`](row_vec_allowlist.txt) (one
`<file>::<fn> # <reason>` per line), or when a listed fn no longer exists, so
the list only shrinks. Violations print as `<file>::<fn>: <reason>`, exit 1.

## `check-package`

```text
Usage: cargo xtask check-package [OPTIONS]
```

Checks that the `relate` facade's packaged crate builds against the *current*
workspace — the package drift window check. `cargo package` verifies against
the crates.io registry and fails for as long as workspace members carry
unpublished changes (the published versions lag the workspace); this check
runs the same verification against the workspace:

1. `cargo package --no-verify` the facade into a scratch directory under the
   system temp dir (nothing is written inside the repository);
2. extract the `.crate` tarball with `tar`;
3. append a `[patch.crates-io]` table to the extracted manifest mapping every
   member of the facade's transitive workspace closure — normal and build
   edges over the workspace manifests, never a hardcoded list — to its
   workspace path;
4. `cargo build --lib --examples --message-format=json` the extracted crate
   under the patch, and read the verdict from the typed build records:
   every closure member — and the extracted facade itself — must have
   produced a compiler artifact whose manifest is the workspace one. A
   `[patch]` entry cargo did not apply compiles its crate from the
   registry cache instead (or not at all), so a registry fallback fails
   the check instead of greening it; the summary's example count comes
   from the same records.

Dev-dependency edges are outside the closure: packaging drops path-only
dev-deps (`relate-test-support`), and patching a crate the build never
references makes cargo warn on every run. For the same reason the extracted
tests are not built — the packaged tests need the dev-deps packaging drops.
The scratch directory is a `tempfile` `TempDir`, removed on every exit path —
failure and panic included.

On success it prints one summary line to stdout:

```text
check-package: relate 1.0.0 packaged, 16 workspace crates patched, lib and 24 examples built against the workspace
```

| Exit status | Meaning |
| --- | --- |
| 0 | The packaged lib and every packaged example built under the patch |
| 1 | Packaging, extraction or the patched build failed (error on stderr) |
| 2 | Usage error, or the check could not run: a `cargo metadata` failure or a workspace without the `relate` facade, an unresolvable workspace root, or an unusable scratch directory (error on stderr) |

## Environment

| Variable | Description |
| --- | --- |
| `CARGO` | Cargo binary used for `cargo metadata`, `cargo package` and the patched build; set automatically by `cargo run`. Falls back to `cargo` |

`check-package` also needs `tar` on `PATH` to extract the `.crate` tarball
(present on the CI runners and both dev platforms).

## CI

The `lint` job in [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml)
runs `cargo test -p xtask` and `cargo xtask check-boundaries`.
