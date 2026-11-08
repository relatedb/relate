# ADR-0543: Hidden REPL dot commands, the `eggs` feature, and the Archie banner and prompt

Status: Proposed

Claim: 11e36494171d3981afd3840f075f4b459c3392cd

## Context

The maintainers want light client-side delight in the interactive shell:
the mascot, Archie, on first start and as an opt-in prompt glyph, plus a
family of small terminal toys behind dot commands. These are neither engine
behaviour nor extensions: they own the terminal and need nothing from the
query path. The CLI's feature bundles (ADR-0104) already
say which build carries which capability, and agent mode (ADR-0407) says a
machine consumer must see no per-run decoration.

## Decision

1. **Hidden dot commands in the `relate` CLI.** Eggs are REPL dot commands
   resolved after the built-in dot commands, in the CLI surface only. They
   are not part of the engine, the C ABI, or the extension system.
2. **One registry, one hook, one crate.** The seam lives in a new surface
   crate, `relate-eggs`: a single table mapping each dot-command name to its
   handler, the shared terminal-capability and refusal types, and the sprite
   kit. Each egg is its own module there. The crate depends on the engine
   only, never on the CLI. The CLI's session carries one dispatch call plus
   the banner and prompt calls, through a small adapter that also supplies
   the CLI's own first-party rows (Archie), which need CLI facilities. The
   dispatch searches both tables through one lookup. Handlers receive the
   open session (database and connection), the output stream and the
   terminal's capabilities. The extra eggs that follow land in `relate-eggs`.
3. **The `eggs` Cargo feature** pulls in `relate-eggs` as an optional
   dependency, in the `full` bundle only — never `agent`, `minimal`, or bare
   `serve`. Without it the adapter compiles to inert stubs and every egg name
   is an ordinary unknown command.
4. **A closed gate.** An egg runs only on the interactive REPL with a human
   at it (never in agent mode, whether detected or explicit) and only when
   stdout is a terminal. Where the gate is closed the name is an unknown
   command, so a hidden command is indistinguishable from a missing one. With
   the gate open, an egg degrades: plain output without colour, or a typed
   refusal on stderr (exit 1) where it cannot draw at all, such as a
   non-UTF-8 locale. Never garbled escape sequences. One-shot and piped runs
   never print banners (ADR-0407).
5. **Documentation stance.** Hidden means hidden: eggs are not listed in
   `.help` or `--help` and the CLI spec does not enumerate them. The spec
   states that hidden commands exist in `eggs` builds and are unlisted,
   documents the `eggs` feature and the environment variables that
   configure them, and nothing more. The hand-written `--help` therefore
   matches the spec, and a registry test pins that no egg name appears in
   `.help`.
6. **Archie assets are editable text files**, one character per pixel and one
   line per row, embedded at build time. One legend table maps each symbol to
   its colour and its plain shade glyph, so later variants add rows. Two
   renderers exist: 24-bit half-block colour, and plain shade characters
   (doubled width). Colour is used only when the terminal advertises 24-bit
   colour and neither `NO_COLOR` nor `TERM=dumb` is set; a non-UTF-8 locale
   gets no art. The three tone colours are the brand's logomark values,
   kept in one palette constant.
7. **First-run banner.** On the first interactive human start the large
   logomark prints beside the unchanged banner line, once per install,
   tracked by its own marker file in the config directory, claimed by
   exclusive create. It does not reuse the telemetry first-run claim, since
   opting out of telemetry must not disable it. The marker is claimed last,
   so a start that could not draw does not burn it; where no marker can be
   created the banner is not shown unforced. `RELATE_BANNER=1` forces it,
   `RELATE_BANNER=0` suppresses it, other values are reported and ignored.
8. **Prompt glyph, opt-in.** `RELATE_PROMPT=archie` makes the primary prompt
   the single glyph U+2229; the continuation prompt is unchanged. Colour is
   applied through the line editor's prompt-highlighter hook, because the
   editor measures the raw prompt string for cursor placement and escape
   sequences inside it would miscount width. It falls back to the classic
   prompts on a non-UTF-8 locale or under `NO_COLOR`.

## Alternatives considered

- **Extensions** (ADR-0429). Rejected: an extension child has no terminal
  and no session, and the egg family needs the REPL's own.
- **A visible `.eggs` or help listing.** Rejected: discovery defeats the
  point, and listing in `--help` would force the spec to enumerate them.
- **Always compiled in, runtime-gated.** Rejected: it ships dead weight to
  the `agent` and `minimal` bundles that ADR-0104 keeps lean.
- **Eggs as a module inside the CLI.** Rejected: the family is expected to
  grow, and a separate crate keeps it out of the CLI's file budget and lets
  it be built and tested alone.
- **Reusing the telemetry first-run claim for the banner.** Rejected: an
  opt-out would silently disable the banner.
- **Colour escapes inside the prompt string.** Rejected: miscounts width in
  the line editor.

## Consequences

- A later egg is one registry row and one module; the session does not change.
- `full` builds gain a hidden surface that the spec deliberately does not
  list; the set is discoverable from the source, not from the CLI.
- `agent`, `minimal` and bare `serve` builds, and every help output, are
  unchanged.
