package ai.relatedb;

import com.fasterxml.jackson.annotation.JsonProperty;

import java.util.List;

/** One operator of a plan document. */
public class PlanNode {
    public String operator;
    public String details;
    public List<String> identifiers;
    @JsonProperty("estimated_rows")
    public Long estimatedRows;
    public PlanActual actual;
    public List<PlanNode> children;
    public List<Notification> warnings;
}
