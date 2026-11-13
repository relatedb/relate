package ai.relatedb;

import com.fasterxml.jackson.annotation.JsonProperty;

/**
 * One operator's PROFILE metrics; {@code null} on an EXPLAIN tree. Models
 * the plan/v1 {@code actual} object's keys — every counter the engine
 * emits — while the lenient plan mapper still tolerates keys added later.
 * {@code loops} and {@code kernel} are null when the wire carries null
 * (the seed row has no input child; no rung served the walk).
 */
public class PlanActual {
    public long rows;
    @JsonProperty("time_ms")
    public double timeMs;
    /** The operator's own time plus every descendant's. */
    @JsonProperty("total_time_ms")
    public double totalTimeMs;
    @JsonProperty("db_hits")
    public long dbHits;
    /** Candidate-resolution phase time, zero when the clause does no pattern work. */
    @JsonProperty("seek_ms")
    public double seekMs;
    /** Relationship-traversal phase time, zero when the clause does no pattern work. */
    @JsonProperty("walk_ms")
    public double walkMs;
    /** The input clause's rows (the operator ran once per input row); null at the root. */
    public Long loops;
    @JsonProperty("memory_bytes")
    public long memoryBytes;
    @JsonProperty("expression_built_bytes")
    public long expressionBuiltBytes;
    public long hops;
    @JsonProperty("neighbor_walks")
    public long neighborWalks;
    @JsonProperty("node_reads")
    public long nodeReads;
    /** Whether the operator ran the batch scan/filter/project chain. */
    public boolean batched;
    @JsonProperty("zone_skips")
    public long zoneSkips;
    /** The traversal frontier's counters; all zeros when the clause walks no frontier. */
    public PlanFrontier frontier;
    /** The frontier rung's run-time decline name, or null when it never declined. */
    @JsonProperty("frontier_decline")
    public String frontierDecline;
    public PlanStart start;
    /** Which rung served the clause's MATCH walk, or null when none did. */
    public PlanKernel kernel;
}
