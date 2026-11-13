package ai.relatedb;

/**
 * The closed vector coordinate dtype vocabulary of the C ABI v2
 * (ADR-0502 §1 and §4): the engine's canonical name, the family — which
 * side of the int/float accessor and putter pairs serves it — and the
 * Java box type every coordinate carries. The one table both directions
 * consult: the value walker reading a cell and the params builder
 * binding one, never a re-spelled copy of the eight names.
 */
enum VectorDtype {

    INTEGER8(Family.INT),
    INTEGER16(Family.INT),
    INTEGER32(Family.INT),
    INTEGER(Family.INT),
    FLOAT16(Family.FLOAT),
    BFLOAT16(Family.FLOAT),
    FLOAT32(Family.FLOAT),
    FLOAT(Family.FLOAT);

    /** Which of the ABI's int/float accessor and putter pairs serves the dtype. */
    enum Family {
        INT, FLOAT
    }

    private final Family family;

    VectorDtype(Family family) {
        this.family = family;
    }

    Family family() {
        return family;
    }

    /**
     * The Java box type every coordinate of the dtype carries — derived
     * from the family, never a second stored field: the two cannot
     * disagree by a typo'd table row.
     */
    Class<?> boxType() {
        return family == Family.INT ? Long.class : Double.class;
    }

    /**
     * The dtype for an engine name, or null when the name is outside the
     * vocabulary — each caller refuses that loudly with its own typed
     * error (the walker an ABI contract violation, the builder a bind
     * refusal).
     */
    static VectorDtype fromName(String name) {
        for (VectorDtype dtype : values()) {
            if (dtype.name().equals(name)) {
                return dtype;
            }
        }
        return null;
    }
}
