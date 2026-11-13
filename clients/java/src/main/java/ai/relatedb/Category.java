package ai.relatedb;

/** A notification's category (ADR-0363), a closed enum; the wire name is the constant's name. */
public enum Category implements WireEnum {
    /** A performance hazard, e.g. a Cartesian product. */
    PERFORMANCE,
    /** A name the catalogue does not know. */
    UNRECOGNIZED,
    /** A deprecated construct. */
    DEPRECATION,
    /** A schema-related notice. */
    SCHEMA,
    /** A hint that could not be honored. */
    HINT;

    @Override
    public String wire() { return name(); }
}
