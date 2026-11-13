package ai.relatedb;

/** The statement's plan-time classification (ADR-0162, ADR-0332), a closed enum. */
public enum QueryType implements WireEnum {
    /** Read-only ({@code "r"}). */
    READ("r"),
    /** Write-only ({@code "w"}). */
    WRITE("w"),
    /** Read and write ({@code "rw"}). */
    READ_WRITE("rw"),
    /** Schema ({@code "s"}). */
    SCHEMA("s");

    private final String wire;

    QueryType(String wire) {
        this.wire = wire;
    }

    @Override
    public String wire() { return wire; }
}
