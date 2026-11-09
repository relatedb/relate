package com.relate;

/** The three fixed namespaces of every store (ADR-0117), a closed enum. */
public enum Namespace implements WireEnum {
    /** The user database ({@code "neo4j"}). */
    NEO4J("neo4j"),
    /** The system database ({@code "system"}). */
    SYSTEM("system"),
    /** The ontologies database ({@code "system_ontologies"}). */
    SYSTEM_ONTOLOGIES("system_ontologies");

    private final String wire;

    Namespace(String wire) {
        this.wire = wire;
    }

    @Override
    public String wire() { return wire; }
}
