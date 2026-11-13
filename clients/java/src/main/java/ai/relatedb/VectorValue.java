package ai.relatedb;

import java.util.List;

/**
 * A vector value hydrated from a result cell (the typed value walker,
 * ADR-0502 §1). {@code dtype} is the engine's canonical coordinate-type
 * name, and each coordinate is a {@code Long} for the integer dtypes, a
 * {@code Double} for the floating dtypes — the closed vocabulary and its
 * int/float split live in one table the client consults, never a
 * re-spelled list here.
 */
public record VectorValue(String dtype, List<Object> coordinates) {
}
