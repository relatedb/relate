package ai.relatedb;

/**
 * A statement's structured plan (LABS-418, ADR-0381): the
 * {@code relate.plan/v1} document — the operator tree with per-node
 * estimates, PROFILE metrics and each operator's remedy-bearing warnings
 * (ADR-0363) joined by operator id.
 *
 * <p>The plan family ({@code PlanDocument}, {@link PlanNode},
 * {@link PlanActual}, {@link PlanStart}) is deliberately a public-field
 * struct bound by the lenient plan mapper, not an immutable getter type
 * like {@link Notification}: the engine keeps adding plan fields
 * (LABS-836), so the document stays an open, caller-owned decoded
 * snapshot — unknown keys are tolerated rather than refused, and the
 * fields are the contract.
 */
public class PlanDocument {
    public String schema;
    public boolean profiled;
    public String statement;
    public PlanNode root;
}
