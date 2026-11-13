// MARK: - Vector conveniences
//
// A `Vector` stores `Double` or `Int64` coordinates whatever its dtype, so
// the `Float` conveniences below convert at the edge: widening a `Float` is
// exact, and narrowing back is exact for the dtypes that fit in one.

extension CypherValue.Vector {
    /// A `FLOAT32` vector — the dtype embedding models produce.
    public static func float32(_ values: [Float]) -> CypherValue.Vector {
        .floats(dtype: .float32, values: values.map(Double.init))
    }

    /// A `FLOAT` (64-bit) vector.
    public static func float64(_ values: [Double]) -> CypherValue.Vector {
        .floats(dtype: .float, values: values)
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
        guard let values = doubleValues else { return nil }
        let narrowed = values.map { Float($0) }
        guard zip(values, narrowed).allSatisfy({ $0.isFinite == $1.isFinite }) else { return nil }
        return narrowed
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
    public init(vector values: [Float]) {
        self = .vector(.float32(values))
    }

    /// A `FLOAT` (64-bit) vector value.
    public init(vector values: [Double]) {
        self = .vector(.float64(values))
    }
}

#if canImport(CoreML)
import CoreML

extension CypherValue.Vector {
    /// The scalars of a Core ML array, in its row-major order, whatever its
    /// shape: a `[1, 384]` embedding output becomes a 384-coordinate vector.
    public init(_ array: MLShapedArray<Float>) {
        self = .float32(array.scalars)
    }

    /// The scalars of a Core ML multi-array, in its row-major order,
    /// whatever its shape. `float16` and `float32` arrays become `FLOAT32`
    /// vectors, `double` arrays `FLOAT`, and `int32` arrays `INTEGER32`; any
    /// other element type is refused with
    /// ``RelateError/invalidParameter(_:)``.
    public init(_ array: MLMultiArray) throws(RelateError) {
        switch array.dataType {
        case .float32, .float16:
            self = .float32(MLShapedArray<Float>(converting: array).scalars)
        case .double:
            self = .float64(MLShapedArray<Double>(converting: array).scalars)
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
