package ai.relatedb;

import java.util.List;
import java.util.Map;

/**
 * A graph node hydrated from a result cell (the typed value walker,
 * ADR-0502 §1). The element id is spelled exactly as Cypher's
 * {@code elementId(n)} returns it, so
 * {@code WHERE elementId(n) = $id} round-trips; it stays the primary
 * identity. The {@code id} is the deprecated legacy numeric entity id
 * (ADR-0506) — the {@code long} Cypher's {@code id(n)} returns for the
 * same node.
 */
public record Node(String elementId, long id, List<String> labels, Map<String, Object> properties) {
}
