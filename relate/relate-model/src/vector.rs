//! Vector coordinate support: the reduced-precision floating-point
//! encodings behind `VectorCoordKind`'s narrowing, the `vector()`
//! coordinate-string text layer, and the packed runtime buffer
//! (`coords`, ADR-0706) — one entry point for the coordinate vocabulary.

pub(crate) mod coordinates;
pub(crate) mod coords;
pub(crate) mod float16;
