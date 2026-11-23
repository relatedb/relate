package ai.relatedb;

import com.fasterxml.jackson.annotation.JsonProperty;
import java.util.List;

/**
 * The expression-memo counters of one operator (ADR-0654): the memos'
 * probes, hits, inserts and refused inserts summed, their table bytes (never
 * part of {@code memory_bytes}) and how many gave up at their hit-rate
 * review. Present on {@link PlanActual#memo} only when a memo was probed.
 */
public class PlanMemo {
    public long probes;
    public long hits;
    public long inserts;
    public long refused;
    public long bytes;
    @JsonProperty("disabled_count")
    public long disabledCount;
    /** The memo types behind the sums; present on the plan root only, else null. */
    public List<String> kinds;
}
