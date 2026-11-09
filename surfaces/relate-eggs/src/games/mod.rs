//! The playable terminal games (ADR-0547): `.dash`, `.life`, `.snake`, `.tetris`.
//! One shared terminal and game-loop module ([`term`]); each game is pure
//! logic (rules, collisions, generations — unit-tested, no I/O) plus a
//! small renderer and a `run` handler.

pub(crate) mod dash;
pub(crate) mod life;
pub(crate) mod snake;
pub(crate) mod term;
pub(crate) mod tetris;
