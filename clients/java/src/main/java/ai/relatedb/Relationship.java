package ai.relatedb;

import java.util.Map;

/**
 * A relationship hydrated from a result cell (the typed value walker,
 * ADR-0502 §1). Element ids are spelled exactly as Cypher's
 * {@code elementId(r)} returns them and stay the primary identity; the
 * {@code id} / {@code startId} / {@code endId} longs are the deprecated
 * legacy numeric ids (ADR-0506) — the values Cypher's {@code id(r)},
 * {@code id(startNode(r))} and {@code id(endNode(r))} return.
 */
public record Relationship(String elementId, long id, String startElementId, long startId,
                           String endElementId, long endId, String type, Map<String, Object> properties) {
}
