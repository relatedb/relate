package ai.relatedb;

/** The walk's chosen start element (LABS-369, ADR-0375). */
public class PlanStart {
    public long element;
    public boolean backward;
    public String reason;
}
