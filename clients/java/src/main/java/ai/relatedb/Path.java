package ai.relatedb;

import java.util.List;

/**
 * A path hydrated from a result cell (the typed value walker, ADR-0502
 * §1): its elements alternate {@link Node}, {@link Relationship}, ...,
 * {@link Node} — {@code 2n+1} elements for {@code n} hops, in walk order.
 */
public record Path(List<Object> elements) {
}
