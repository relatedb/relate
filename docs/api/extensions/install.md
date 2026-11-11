# Extensions — install guide

`relate extension install` puts a package on the machine; `list` shows
what is there and where each package came from; `remove` takes one off. This
guide is the end-user view of that lifecycle — the trust tiers, what the
install prints, and what the provenance marker records. The authoring side
is [authoring.md](authoring.md); the precise contract is [spec.md](spec.md).

```text
relate extension install <name> [--url URL | --from DIR | --github OWNER/REPO@REF] [--version V] [--force]
relate extension list [--available]
relate extension remove <name>
```

## The trust tiers

Where a package comes from is a trust decision, and the flag you use is that
decision. Four tiers, strongest first:

| Tier | Flag | What you are trusting |
| --- | --- | --- |
| **First-party** | *(none)* | The signed release channel. The index (`extensions.json`) is SSHSIG-verified against the same release key `upgrade` verifies, and the tarball's SHA256 is checked against the signed row. `--version V` pins a channel version; the default is the newest listed |
| **GitHub pin** | `--github OWNER/REPO@REF` | One commit of one repository. `REF` is a tag or a full commit SHA; a ref must be spelled (an untagged `owner/repo` is a moving target and is a usage error). The tag's commit SHA and the platform asset's SHA256 are resolved through the release route and recorded; a later install of the same pin is silent, a tag that moved to another commit is refused before any fetch, and moving a pin takes an explicit re-approval flag |
| **Unsigned** | `--url URL` | Whoever serves that URL — no signature exists to check, so the https transport is the act's one leg of integrity. `https` only (plain `http` is accepted for a loopback address, the dev case); any other URL is a usage error *before* any fetch |
| **Local** | `--from DIR` | The directory you point at. Offline, CI and air-gapped path (`pull --from`'s posture): the directory is the trust act, no network, no signature. The executable must resolve inside it (an intermediate-directory symlink is a refusal) |

Two invariants across all four: every install runs the same manifest,
protocol and collision checks, and a failed install leaves no residue — the
package is staged in a sibling directory, verified, and only then renamed
into place.

## What the install prints

```text
$ relate extension install git-to-graph --from ./git-to-graph
installed local extension git-to-graph 0.2.0 from ./git-to-graph
```

One loud line per install, naming the tier, the package's own name and
version, and the source. It is the moment the machine's grammar changed —
read it. The same fact is recorded in the package's provenance marker
(`.relate-provenance.json`, inside the package directory) and shown by
`extension list`:

```text
$ relate extension list
git-to-graph 0.2.0 local ./git-to-graph
fs-to-graph 0.2.0 first-party https://relatedb.ai
```

`list` is offline and never fetches; a package copied into place by hand
(no marker) lists as `unmarked` — honest about its origin. `list
--available` asks the other question, what the channel offers, and needs the
network (it fails closed naming the index URL when offline).

`remove` requires the marker: a directory relate did not install is a
refusal naming what it found, and there is no `--force`. Removing a package
removes its directory but never its recorded pin — a GitHub pin lives in the
per-user trust store (`extensions-trust.json` under the config directory) and
stays until it is inspected or cleared deliberately, so remove-then-reinstall
cannot bypass a moving tag. A reinstall goes through the tier's checks again,
never around them.

## Where packages live

One directory per package under `$XDG_CONFIG_HOME/relate/extensions/`
(falling back to `~/.config/relate/extensions/` when `$XDG_CONFIG_HOME`
is unset, empty or relative). That is the only discovery root — never the
working directory — so cloning a repository can never add commands to your
`relate`. A store named like a command stays reachable as
`relate --database run`.

## The walkthrough

[quickstart.md](quickstart.md) walks the whole thing end to end: scaffold a
package, install it with `--from`, run it into a store, and read the graph
back — then the same loop against the shipped examples and the first-party
channel.

## Upgrading

There is no `extension upgrade` verb: reinstall with a newer `--version V`
(channel) or a newer pin, which runs the tier's checks again. The recorded
provenance is what makes the change auditable after the fact.
