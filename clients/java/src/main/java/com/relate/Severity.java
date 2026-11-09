package com.relate;

/** A notification's severity (ADR-0363), a closed enum; the wire name is the constant's name. */
public enum Severity implements WireEnum {
    /** Informational. */
    INFORMATION,
    /** A warning. */
    WARNING;

    @Override
    public String wire() { return name(); }
}
