package ai.relatedb;

/**
 * The rung that served an operator's MATCH walk, as the plan reports it.
 * {@code hops} is reported by the chain rung only: null for any other
 * kernel entry, which carries no {@code hops} key.
 */
public class PlanKernel {
    public String rung;
    public Long hops;
}
