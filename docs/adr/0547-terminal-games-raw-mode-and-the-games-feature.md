# ADR-0547: Terminal games for the eggs: raw mode, a restoring guard, and the `games` feature

Status: Proposed

Claim: cf7a4317a9e04f149b5a2e37542682f00cbc86b2

## Context

The hidden dot commands (ADR-0543) include playable toys: Conway's Life,
Snake and Tetris. They need what a line-oriented REPL does not give: single
key presses without echo or line buffering, a full-screen canvas that leaves
the shell's scrollback untouched, and a clock tick that is not blocked on
input. They also hold the user's terminal, so a game that exits badly leaves
the shell unusable. The workspace already names a terminal library
(`crossterm`) for such work; it is small, and nothing links it today.

## Decision

1. **Raw mode on the alternate screen.** A game switches the terminal to raw
   mode, enters the alternate screen and hides the cursor; on exit all three
   are undone. One shared module owns this and the game loop (a tick timer,
   key polling, a full redraw of the frame the game renders); each game is
   pure rules plus a small renderer, so the rules are testable without a
   terminal.
2. **The terminal is always given back.** Restoration lives in a guard's
   drop, so it runs on normal exit, on an I/O error and on unwinding. Ctrl-C
   arrives as a key press in raw mode and ends the game through the normal
   path. A panic inside a game is caught at the loop's edge: the default
   panic hook is silenced meanwhile (its message would print into the screen
   that is about to vanish), the guard restores the terminal, the hook is put
   back, and the failure is reported as an ordinary error on the restored
   terminal. Key presses still queued at exit are discarded so they do not
   reach the shell.
3. **Refuse rather than garble.** A game refuses, with a typed error on the
   restored terminal and before touching the screen, when stdin or stdout is
   not a terminal or the window is smaller than the game's fixed layout.
   While running, a window shrunk below the layout shows a one-line notice
   and the game pauses. Colour is used only under the capability rules of
   ADR-0543; otherwise cells fall back to ASCII. Life needs a UTF-8 locale
   for braille and refuses without one.
4. **A `games` feature of the eggs crate** is the only thing that links
   `crossterm`. The CLI's `eggs` feature enables it, so the dependency
   exists in the `full` bundle alone and never in `agent`, `minimal` or
   bare `serve`.
5. **Bounded work.** The frame size, the Life grid and the Snake board are
   capped, so per-tick work and output are bounded by constants, not input.

## Alternatives considered

- **Cooked mode with line input.** Rejected: no single-key play and no
  non-blocking tick.
- **Hand-written termios and escape handling.** Rejected: unsafe, platform
  specific code and a resize and key decoder to maintain, for a toy.
- **A full TUI framework.** Rejected: a much larger dependency tree for
  three fixed-size screens that a full redraw serves.
- **Always linking the terminal library in the eggs crate.** Rejected: the
  eggs crate also builds without games, and the dependency is only needed
  when they are on.
- **Restoring from a signal handler.** Rejected: raw mode turns Ctrl-C into
  a key, so the ordinary exit path covers it; a handler adds nothing.

## Consequences

- One small dependency enters the `full` bundle, about 70 KB measured.
- A forcibly killed process (SIGKILL) cannot restore the terminal; the user
  runs `reset`. Accepted for a hidden toy.
- Later full-screen eggs reuse the loop and guard instead of owning a
  terminal.
- The library pulls a small event-loop and signal stack on Unix; it sits in
  a surface crate, outside the engine's dependency rules.
