using System.Globalization;

namespace Relate.Net;

/// <summary>
/// A Cypher <c>POINT</c>. Use the factory methods for the four supported coordinate reference
/// systems.
/// </summary>
/// <param name="Srid">The spatial reference id: 7203, 9157, 4326 or 4979.</param>
/// <param name="X">X, or longitude for a geographic point.</param>
/// <param name="Y">Y, or latitude for a geographic point.</param>
/// <param name="Z">Z, or height for a geographic point; <see langword="null"/> for 2D points.</param>
public readonly record struct CypherPoint(int Srid, double X, double Y, double? Z = null)
{
    /// <summary>SRID of a 2D cartesian point.</summary>
    public const int CartesianSrid = 7203;

    /// <summary>SRID of a 3D cartesian point.</summary>
    public const int Cartesian3DSrid = 9157;

    /// <summary>SRID of a 2D WGS-84 geographic point.</summary>
    public const int Wgs84Srid = 4326;

    /// <summary>SRID of a 3D WGS-84 geographic point.</summary>
    public const int Wgs843DSrid = 4979;

    /// <summary>The number of dimensions, 2 or 3.</summary>
    public int Dimension => Z.HasValue ? 3 : 2;

    /// <summary>Creates a 2D cartesian point (SRID 7203).</summary>
    public static CypherPoint Cartesian(double x, double y) => new(CartesianSrid, x, y);

    /// <summary>Creates a 3D cartesian point (SRID 9157).</summary>
    public static CypherPoint Cartesian(double x, double y, double z) => new(Cartesian3DSrid, x, y, z);

    /// <summary>Creates a 2D WGS-84 point (SRID 4326).</summary>
    public static CypherPoint Wgs84(double longitude, double latitude) => new(Wgs84Srid, longitude, latitude);

    /// <summary>Creates a 3D WGS-84 point (SRID 4979).</summary>
    public static CypherPoint Wgs84(double longitude, double latitude, double height) => new(Wgs843DSrid, longitude, latitude, height);

    /// <inheritdoc />
    public override string ToString() => Z is double z
        ? string.Create(CultureInfo.InvariantCulture, $"point({{srid: {Srid}, x: {X}, y: {Y}, z: {z}}})")
        : string.Create(CultureInfo.InvariantCulture, $"point({{srid: {Srid}, x: {X}, y: {Y}}})");
}

/// <summary>
/// A Cypher <c>DURATION</c>, stored losslessly as months, days, seconds and nanoseconds (Cypher
/// durations cannot be represented by <see cref="TimeSpan"/> because months and days have no
/// fixed length).
/// </summary>
/// <param name="Months">Whole months (a year is 12 months).</param>
/// <param name="Days">Whole days.</param>
/// <param name="Seconds">Whole seconds.</param>
/// <param name="Nanoseconds">Sub-second nanoseconds.</param>
public readonly record struct CypherDuration(long Months, long Days, long Seconds, long Nanoseconds)
{
    /// <summary>Creates a duration from a <see cref="TimeSpan"/> (seconds and nanoseconds only).</summary>
    public static CypherDuration FromTimeSpan(TimeSpan value) =>
        new(0, 0, WholeSeconds(value.Ticks), SubSecondNanos(value.Ticks));

    /// <summary>
    /// Converts to a <see cref="TimeSpan"/>, counting a day as 24 hours. Sub-100ns precision is
    /// truncated.
    /// </summary>
    /// <exception cref="InvalidOperationException">The duration has a non-zero month component.</exception>
    public TimeSpan ToTimeSpan()
    {
        if (Months != 0)
        {
            throw new InvalidOperationException("A duration with months has no fixed length and cannot be converted to a TimeSpan.");
        }

        return TimeSpan.FromTicks(TicksFromSecondsAndNanos(Days * 86_400 + Seconds, Nanoseconds));
    }

    // The one tick<->nanoseconds home; internal marshalling plumbing used by the reader and writer.
    internal static long WholeSeconds(long ticks) => ticks / TimeSpan.TicksPerSecond;

    internal static long SubSecondNanos(long ticks) => (ticks % TimeSpan.TicksPerSecond) * 100;

    internal static long TicksFromSecondsAndNanos(long seconds, long nanoseconds) =>
        checked(seconds * TimeSpan.TicksPerSecond + nanoseconds / 100);

    /// <inheritdoc />
    public override string ToString() =>
        string.Create(CultureInfo.InvariantCulture, $"P{Months}M{Days}DT{Seconds}.{Math.Abs(Nanoseconds):D9}S");
}

/// <summary>
/// A Cypher <c>TIME</c>: a time of day with a UTC offset (there is no equivalent BCL type).
/// </summary>
/// <param name="Time">The local time of day (100 ns precision).</param>
/// <param name="Offset">The offset from UTC.</param>
public readonly record struct CypherOffsetTime(TimeOnly Time, TimeSpan Offset)
{
    /// <inheritdoc />
    public override string ToString()
    {
        string sign = Offset < TimeSpan.Zero ? "-" : "+";
        return string.Create(CultureInfo.InvariantCulture, $"{Time:HH:mm:ss.FFFFFFF}{sign}{Offset.Duration():hh\\:mm}");
    }
}

/// <summary>
/// A Cypher <c>DATETIME</c> that carries an IANA time-zone id, such as
/// <c>datetime({year: 2024, month: 6, day: 1, timezone: 'Europe/Copenhagen'})</c>. A
/// <c>DATETIME</c> with only an offset is returned as a plain <see cref="DateTimeOffset"/>.
/// </summary>
/// <param name="DateTimeOffset">The local wall-clock time with the offset that applies in <paramref name="ZoneId"/>.</param>
/// <param name="ZoneId">The IANA zone id, e.g. <c>Europe/Copenhagen</c>.</param>
public readonly record struct CypherZonedDateTime(DateTimeOffset DateTimeOffset, string ZoneId)
{
    /// <summary>Converts to a <see cref="System.DateTimeOffset"/>, dropping the zone id.</summary>
    public static implicit operator DateTimeOffset(CypherZonedDateTime value) => value.DateTimeOffset;

    /// <summary>Returns the <see cref="DateTimeOffset"/> (for languages without implicit conversions).</summary>
    public DateTimeOffset ToDateTimeOffset() => DateTimeOffset;

    /// <inheritdoc />
    public override string ToString() =>
        $"{DateTimeOffset.ToString("yyyy-MM-ddTHH:mm:ss.FFFFFFFzzz", CultureInfo.InvariantCulture)}[{ZoneId}]";
}

/// <summary>The coordinate type of a <see cref="CypherVector"/>.</summary>
public enum VectorElementType
{
    /// <summary>8-bit signed integer (<c>INTEGER8</c>).</summary>
    Integer8,

    /// <summary>16-bit signed integer (<c>INTEGER16</c>).</summary>
    Integer16,

    /// <summary>32-bit signed integer (<c>INTEGER32</c>).</summary>
    Integer32,

    /// <summary>64-bit signed integer (<c>INTEGER</c>).</summary>
    Integer64,

    /// <summary>IEEE half precision (<c>FLOAT16</c>).</summary>
    Float16,

    /// <summary>bfloat16 (<c>BFLOAT16</c>).</summary>
    BFloat16,

    /// <summary>IEEE single precision (<c>FLOAT32</c>).</summary>
    Float32,

    /// <summary>IEEE double precision (<c>FLOAT</c>).</summary>
    Float64,
}

/// <summary>
/// A Cypher <c>VECTOR</c> value (for example an embedding). Integer vectors keep exact
/// <see cref="long"/> coordinates; floating-point vectors keep <see cref="double"/> coordinates.
/// </summary>
public sealed class CypherVector : IEquatable<CypherVector>
{
    // The coordinate union as one non-nullable field: a long[] for the integer element types,
    // a double[] for the float types, selected and validated by the constructors and FromNative.
    private readonly Array _coordinates;

    private CypherVector(VectorElementType elementType, Array coordinates)
    {
        ElementType = elementType;
        _coordinates = coordinates;
    }

    /// <summary>The coordinate type.</summary>
    public VectorElementType ElementType { get; }

    /// <summary><see langword="true"/> for the four integer element types.</summary>
    public bool IsInteger => IsIntegerType(ElementType);

    /// <summary>The number of coordinates.</summary>
    public int Dimension => _coordinates.Length;

    /// <summary>The engine's name for the coordinate type, e.g. <c>FLOAT32</c>.</summary>
    public string ElementTypeName => ToDType(ElementType);

    private long[] IntegerCoordinates => _coordinates as long[]
        ?? throw new InvalidOperationException($"A {ElementTypeName} vector has floating-point coordinates.");

    private double[] FloatCoordinates => _coordinates as double[]
        ?? throw new InvalidOperationException($"A {ElementTypeName} vector has integer coordinates.");

    /// <summary>Creates a <c>FLOAT32</c> vector.</summary>
    public static CypherVector FromSingles(IEnumerable<float> values)
    {
        ArgumentNullException.ThrowIfNull(values);
        return new CypherVector(VectorElementType.Float32, values.Select(v => (double)v).ToArray());
    }

    /// <summary>Creates a floating-point vector (<c>FLOAT</c> unless another float type is given).</summary>
    public static CypherVector FromDoubles(IEnumerable<double> values, VectorElementType elementType = VectorElementType.Float64)
    {
        ArgumentNullException.ThrowIfNull(values);
        if (IsIntegerType(elementType))
        {
            throw new ArgumentException($"{elementType} is not a floating-point element type.", nameof(elementType));
        }

        return new CypherVector(elementType, values.ToArray());
    }

    /// <summary>Creates an integer vector (<c>INTEGER</c> unless another integer type is given).</summary>
    public static CypherVector FromInt64s(IEnumerable<long> values, VectorElementType elementType = VectorElementType.Integer64)
    {
        ArgumentNullException.ThrowIfNull(values);
        if (!IsIntegerType(elementType))
        {
            throw new ArgumentException($"{elementType} is not an integer element type.", nameof(elementType));
        }

        return new CypherVector(elementType, values.ToArray());
    }

    /// <summary>Creates an <c>INTEGER32</c> vector.</summary>
    public static CypherVector FromInt32s(IEnumerable<int> values)
    {
        ArgumentNullException.ThrowIfNull(values);
        return new CypherVector(VectorElementType.Integer32, values.Select(v => (long)v).ToArray());
    }

    /// <summary>Returns the coordinates as doubles (integer coordinates are converted).</summary>
    public double[] ToDoubleArray() => _coordinates is double[] floats ? (double[])floats.Clone() : IntegerCoordinates.Select(v => (double)v).ToArray();

    /// <summary>Returns the coordinates as singles (with rounding for wider types).</summary>
    public float[] ToSingleArray() => ToDoubleArray().Select(v => (float)v).ToArray();

    /// <summary>Returns the coordinates of an integer vector.</summary>
    /// <exception cref="InvalidOperationException">The vector is a floating-point vector.</exception>
    public long[] ToInt64Array() => (long[])IntegerCoordinates.Clone();

    /// <inheritdoc />
    public bool Equals(CypherVector? other) =>
        other is not null
        && ElementType == other.ElementType
        && (IsInteger
            ? other.IntegerCoordinates.AsSpan().SequenceEqual(IntegerCoordinates)
            : other.FloatCoordinates.AsSpan().SequenceEqual(FloatCoordinates));

    /// <inheritdoc />
    public override bool Equals(object? obj) => Equals(obj as CypherVector);

    /// <inheritdoc />
    public override int GetHashCode() => HashCode.Combine(ElementType, Dimension);

    /// <inheritdoc />
    public override string ToString() =>
        $"vector([{string.Join(", ", IsInteger ? IntegerCoordinates.Select(v => v.ToString(CultureInfo.InvariantCulture)) : FloatCoordinates.Select(v => v.ToString(CultureInfo.InvariantCulture)))}], {Dimension}, {ElementTypeName})";

    /// <summary>Wraps natively read integer coordinates; the element type must be an integer type.</summary>
    internal static CypherVector FromNative(VectorElementType type, long[] integers) =>
        IsIntegerType(type)
            ? new(type, integers)
            : throw new RelateException($"An {ToDType(type)} vector cannot hold integer coordinates.");

    /// <summary>Wraps natively read floating-point coordinates; the element type must be a float type.</summary>
    internal static CypherVector FromNative(VectorElementType type, double[] floats) =>
        IsIntegerType(type)
            ? throw new RelateException($"A {ToDType(type)} vector cannot hold floating-point coordinates.")
            : new(type, floats);

    /// <summary>The stored coordinates without the defensive copy — interop marshalling only.</summary>
    internal long[] IntegerCoordinateArray => IntegerCoordinates;

    /// <summary>The stored coordinates without the defensive copy — interop marshalling only.</summary>
    internal double[] FloatCoordinateArray => FloatCoordinates;

    internal static bool IsIntegerType(VectorElementType type) =>
        type is VectorElementType.Integer8 or VectorElementType.Integer16
            or VectorElementType.Integer32 or VectorElementType.Integer64;

    // The one dtype vocabulary table: element type -> the engine's canonical
    // coordinate-type name. The reverse lookup derives from it, plus the
    // inbound alias spellings the engine accepts alongside the canonical ones.
    private static readonly IReadOnlyDictionary<VectorElementType, string> DTypeNames =
        new Dictionary<VectorElementType, string>
        {
            [VectorElementType.Integer8] = "INTEGER8",
            [VectorElementType.Integer16] = "INTEGER16",
            [VectorElementType.Integer32] = "INTEGER32",
            [VectorElementType.Integer64] = "INTEGER",
            [VectorElementType.Float16] = "FLOAT16",
            [VectorElementType.BFloat16] = "BFLOAT16",
            [VectorElementType.Float32] = "FLOAT32",
            [VectorElementType.Float64] = "FLOAT",
        };

    private static readonly IReadOnlyDictionary<string, VectorElementType> DTypeByEngineName =
        DTypeNames.ToDictionary(pair => pair.Value, pair => pair.Key)
            .Concat(new Dictionary<string, VectorElementType>
            {
                ["INTEGER64"] = VectorElementType.Integer64,
                ["FLOAT64"] = VectorElementType.Float64,
            })
            .ToDictionary(pair => pair.Key, pair => pair.Value);

    internal static string ToDType(VectorElementType type) =>
        DTypeNames.TryGetValue(type, out string? name)
            ? name
            : throw new ArgumentOutOfRangeException(nameof(type));

    internal static VectorElementType? FromDType(string? dtype) =>
        dtype is not null && DTypeByEngineName.TryGetValue(dtype, out VectorElementType type) ? type : null;
}

/// <summary>
/// A value of a kind this version of Relate.Net does not know (a later, additive relate ABI).
/// </summary>
/// <param name="KindCode">The raw <c>relate_value_kind</c> enumerator.</param>
public sealed record UnsupportedValue(int KindCode);
