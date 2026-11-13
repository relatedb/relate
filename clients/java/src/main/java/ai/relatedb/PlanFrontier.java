package ai.relatedb;

import com.fasterxml.jackson.annotation.JsonProperty;

/** The traversal frontier's run counters of one operator (ADR-0478, ADR-0516). */
public class PlanFrontier {
    @JsonProperty("edge_visits")
    public long edgeVisits;
    public long levels;
    public long endpoints;
    public long cells;
    @JsonProperty("memo_hits")
    public long memoHits;
    @JsonProperty("memo_misses")
    public long memoMisses;
    @JsonProperty("representative_proofs")
    public long representativeProofs;
    @JsonProperty("search_proven")
    public long searchProven;
    @JsonProperty("search_disproven")
    public long searchDisproven;
    @JsonProperty("declined_exhausted")
    public long declinedExhausted;
    /** The re-arrival rule's spent-work fraction over its budget. */
    @JsonProperty("budget_fraction")
    public double budgetFraction;
}
