# xtask

Workspace maintenance tasks for Relate, run through the
`cargo xtask` alias defined in [`.cargo/config.toml`](../../.cargo/config.toml)
(`xtask = "run -p xtask --"`). Depends only on `std` and `serde_json`.

## Usage

```sh
cargo xtask check-boundaries
```

```text
Usage: cargo xtask <COMMAND> [OPTIONS]
```

| Command | Description |
| --- | --- |
| `check-boundaries` | Check workspace dependency edges against ADR-0103 rules R1-R5 |

| Flag | Default | Description |
| --- | --- | --- |
| `-h`, `--help` | off | Print help (top level or after `check-boundaries`) and exit 0 |

No positionals beyond the command. Missing or unknown commands and unexpected
arguments print an error plus the help text and exit 2.

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

## Environment

| Variable | Description |
| --- | --- |
| `CARGO` | Cargo binary used to run `cargo metadata`; set automatically by `cargo run`. Falls back to `cargo` |

## CI

The `lint` job in [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml)
runs `cargo test -p xtask` and `cargo xtask check-boundaries`.
