// MARK: - Vector conveniences
//
// A `Vector` stores `Double` or `Int64` coordinates whatever its dtype, so
// the `Float` conveniences below convert at the edge: widening a `Float` is
// exact, and narrowing back is exact for the dtypes that fit in one.

extension CypherValue.Vector {
    /// The most coordinates a vector built by the conveniences here may
    /// carry: 4,096, the engine's upper bound on a `vector()` dimension and
    /// on a vector index. A longer array is refused, not truncated. An
    /// empty one is accepted, as it is when bound as a parameter.
    public static let maximumCount = 4_096

    /// Refuses a coordinate count above ``maximumCount``. Callers check the
    /// count before they copy or convert the coordinates, so an oversized
    /// array costs nothing.
    private static func check(count: Int) throws(RelateError) {
        guard count <= maximumCount else {
            throw RelateError.invalidParameter(
                "a vector of \(count) coordinates is over the maximum of \(maximumCount)")
        }
    }

    /// `values` when its count is allowed and every coordinate is finite: a
    /// NaN or an infinity is not a vector coordinate to the engine.
    private static func checked<Coordinate: BinaryFloatingPoint>(
        _ values: [Coordinate]
    ) throws(RelateError) -> [Coordinate] {
        try check(count: values.count)
        if let index = values.firstIndex(where: { !$0.isFinite }) {
            throw RelateError.invalidParameter(
                "vector coordinate \(index) is \(values[index]), not a finite number")
        }
        return values
    }

    /// A `FLOAT32` vector — the dtype embedding models produce.
    ///
    /// Throws ``RelateError/invalidParameter(_:)`` for more than
    /// ``maximumCount`` coordinates and for a NaN or infinite one.
    public static func float32(_ values: [Float]) throws(RelateError) -> CypherValue.Vector {
        .floats(dtype: .float32, values: try checked(values).map(Double.init))
    }

    /// A `FLOAT` (64-bit) vector.
    ///
    /// Throws as ``float32(_:)`` does.
    public static func float64(_ values: [Double]) throws(RelateError) -> CypherValue.Vector {
        .floats(dtype: .float, values: try checked(values))
    }

    /// The coordinate type.
    public var dtype: DType {
        switch self {
        case .floats(let dtype, _), .ints(let dtype, _): dtype
        }
    }

    /// The number of coordinates.
    public var count: Int {
        switch self {
        case .floats(_, let values): values.count
        case .ints(_, let values): values.count
        }
    }

    /// The coordinates of a float-typed vector as `Float`s; `nil` for an
    /// integer-typed vector. Exact for `FLOAT32`, `FLOAT16` and `BFLOAT16`;
    /// a `FLOAT` (64-bit) vector is narrowed to the nearest `Float`, and is
    /// `nil` when a finite coordinate lies outside `Float`'s range (it
    /// would read back as an infinity the vector never held).
    public var float32Values: [Float]? {
        let largest = Double(Float.greatestFiniteMagnitude)
        guard let values = doubleValues,
            values.allSatisfy({ !$0.isFinite || $0.magnitude <= largest })
        else { return nil }
        return values.map { Float($0) }
    }

    /// The coordinates of a float-typed vector; `nil` for an integer-typed
    /// vector.
    public var doubleValues: [Double]? {
        guard case .floats(_, let values) = self else { return nil }
        return values
    }
}

extension CypherValue {
    /// A `FLOAT32` vector value.
    ///
    /// Throws as ``Vector/float32(_:)`` does.
    public init(vector values: [Float]) throws(RelateError) {
        self = .vector(try .float32(values))
    }

    /// A `FLOAT` (64-bit) vector value.
    ///
    /// Throws as ``Vector/float64(_:)`` does.
    public init(vector values: [Double]) throws(RelateError) {
        self = .vector(try .float64(values))
    }
}

#if canImport(CoreML)
import CoreML

extension CypherValue.Vector {
    /// The scalars of a Core ML array, in its row-major order, whatever its
    /// shape: a `[1, 384]` embedding output becomes a 384-coordinate vector.
    ///
    /// Throws as ``float32(_:)`` does.
    public init(_ array: MLShapedArray<Float>) throws(RelateError) {
        try Self.check(count: array.scalarCount)
        self = try .float32(array.scalars)
    }

    /// The scalars of a Core ML multi-array, in its row-major order,
    /// whatever its shape. `float16` and `float32` arrays become `FLOAT32`
    /// vectors, `double` arrays `FLOAT`, and `int32` arrays `INTEGER32`; any
    /// other element type is refused with
    /// ``RelateError/invalidParameter(_:)``, as are more than
    /// ``maximumCount`` scalars and a NaN or infinite one.
    public init(_ array: MLMultiArray) throws(RelateError) {
        try Self.check(count: array.count)
        switch array.dataType {
        case .float32, .float16:
            self = try .float32(MLShapedArray<Float>(converting: array).scalars)
        case .double:
            self = try .float64(MLShapedArray<Double>(converting: array).scalars)
        case .int32:
            self = .ints(
                dtype: .integer32,
                values: MLShapedArray<Int32>(converting: array).scalars.map(Int64.init))
        default:
            throw RelateError.invalidParameter(
                "an MLMultiArray of data type \(array.dataType.rawValue) has no vector dtype")
        }
    }
}
#endif
