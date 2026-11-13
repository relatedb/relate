package ai.relatedb;

/**
 * A point value hydrated from a result cell (the typed value walker,
 * ADR-0502 §1). {@code z} is null on a two-dimensional point (srid 7203
 * cartesian / 4326 WGS-84); a three-dimensional point carries srid 9157 /
 * 4979 and a non-null {@code z}.
 */
public record Point(int srid, double x, double y, Double z) {
}
