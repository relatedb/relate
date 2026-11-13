package ai.relatedb;

/**
 * A Cypher duration in its four lossless components (the typed value
 * walker, ADR-0502 §1): {@code duration('P1Y2M')} reads
 * {@code months = 14}. {@link java.time.Duration} cannot hold months, so
 * it binds as a zero-months / zero-days duration; this record is the full
 * form.
 */
public record DurationValue(long months, long days, long seconds, long nanos) {
}
