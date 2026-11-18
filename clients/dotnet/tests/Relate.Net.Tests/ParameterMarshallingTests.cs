using System.Collections;

namespace Relate.Net.Tests;

/// <summary>.NET values sent as parameters and read back: covers both directions of the marshalling layer.</summary>
public class ParameterMarshallingTests
{
    public sealed class Address
    {
        public string City { get; set; } = "Copenhagen";

        public int Zip { get; set; } = 2100;
    }

    public sealed record Person(string Name, int Age, Address Home);

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public void Bool_RoundTrips(bool value)
    {
        using var t = new TestDb();
        Assert.Equal(value, t.RoundTrip(value));
    }

    [Fact]
    public void AllIntegerTypes_BecomeInt64()
    {
        using var t = new TestDb();

        Assert.Equal(-5L, t.RoundTrip((sbyte)-5));
        Assert.Equal(200L, t.RoundTrip((byte)200));
        Assert.Equal(-300L, t.RoundTrip((short)-300));
        Assert.Equal(60000L, t.RoundTrip((ushort)60000));
        Assert.Equal(int.MinValue + 0L, t.RoundTrip(int.MinValue));
        Assert.Equal(uint.MaxValue + 0L, t.RoundTrip(uint.MaxValue));
        Assert.Equal(long.MaxValue, t.RoundTrip(long.MaxValue));
        Assert.Equal(long.MinValue, t.RoundTrip(long.MinValue));
        Assert.Equal(123L, t.RoundTrip(123UL));
    }

    [Fact]
    public void UInt64AboveInt64Max_IsRejected()
    {
        using var t = new TestDb();
        Assert.Throws<ArgumentOutOfRangeException>(() => t.RoundTrip(ulong.MaxValue));
    }

    [Fact]
    public void FloatingPoint_BecomesDouble_IncludingSpecialValues()
    {
        using var t = new TestDb();

        Assert.Equal(1.5, t.RoundTrip(1.5f));
        Assert.Equal(Math.PI, t.RoundTrip(Math.PI));
        Assert.Equal(12.25, t.RoundTrip(12.25m));
        Assert.True(double.IsNaN((double)t.RoundTrip(double.NaN)!));
        Assert.Equal(double.PositiveInfinity, t.RoundTrip(double.PositiveInfinity));
        Assert.Equal(double.NegativeInfinity, t.RoundTrip(double.NegativeInfinity));
    }

    [Theory]
    [InlineData("")]
    [InlineData("hello")]
    [InlineData("Æblegrød på dansk – ünïcødé ✓ 漢字 🚀")]
    [InlineData("embedded\0nul")]
    public void Strings_RoundTripExactly(string value)
    {
        using var t = new TestDb();
        Assert.Equal(value, t.RoundTrip(value));
    }

    [Fact]
    public void LongString_RoundTrips()
    {
        using var t = new TestDb();
        string value = new('x', 100_000);
        Assert.Equal(value, t.RoundTrip(value));
    }

    [Fact]
    public void CharGuidAndEnum_AreSentAsStrings()
    {
        using var t = new TestDb();
        var guid = Guid.NewGuid();

        Assert.Equal("q", t.RoundTrip('q'));
        Assert.Equal(guid.ToString(), t.RoundTrip(guid));
        Assert.Equal("Green", t.RoundTrip(Color.Green));
    }

    [Fact]
    public void Null_And_DBNull_AreCypherNull()
    {
        using var t = new TestDb();

        Assert.Null(t.RoundTrip(null));
        Assert.Null(t.RoundTrip(DBNull.Value));
        Assert.True(t.Connection.Query("RETURN $v IS NULL AS isNull", new { v = (string?)null }).Scalar<bool>());
    }

    [Fact]
    public void Arrays_AndEnumerables_BecomeLists()
    {
        using var t = new TestDb();

        Assert.Equal(new object[] { 1L, 2L, 3L }, (IEnumerable<object?>)t.RoundTrip(new[] { 1, 2, 3 })!);
        Assert.Equal(new object[] { "a", "b" }, (IEnumerable<object?>)t.RoundTrip(new List<string> { "a", "b" })!);
        Assert.Equal(new object?[] { 1L, "two", null, true }, (IEnumerable<object?>)t.RoundTrip(new object?[] { 1, "two", null, true })!);
        Assert.Equal(new object[] { 0L, 1L, 4L }, (IEnumerable<object?>)t.RoundTrip(Enumerable.Range(0, 3).Select(i => i * i))!);
        Assert.Empty((IEnumerable)t.RoundTrip(Array.Empty<int>())!);
    }

    [Fact]
    public void ByteArray_BecomesListOfIntegers()
    {
        using var t = new TestDb();
        Assert.Equal(new object[] { 0L, 127L, 255L }, (IEnumerable<object?>)t.RoundTrip(new byte[] { 0, 127, 255 })!);
    }

    [Fact]
    public void Dictionaries_BecomeMaps()
    {
        using var t = new TestDb();
        var value = new Dictionary<string, object?> { ["name"] = "Ada", ["age"] = 36, ["tags"] = new[] { "x" }, ["none"] = null };

        var map = Assert.IsAssignableFrom<IReadOnlyDictionary<string, object?>>(t.RoundTrip(value));

        Assert.Equal("Ada", map["name"]);
        Assert.Equal(36L, map["age"]);
        Assert.Equal(new object[] { "x" }, (IEnumerable<object?>)map["tags"]!);
        Assert.Null(map["none"]);
    }

    [Fact]
    public void StronglyTypedAndNonGenericDictionaries_AreAccepted()
    {
        using var t = new TestDb();

        var typed = Assert.IsAssignableFrom<IReadOnlyDictionary<string, object?>>(t.RoundTrip(new SortedDictionary<string, int> { ["b"] = 2, ["a"] = 1 }));
        var legacy = Assert.IsAssignableFrom<IReadOnlyDictionary<string, object?>>(t.RoundTrip(new Hashtable { ["k"] = "v" }));

        Assert.Equal(1L, typed["a"]);
        Assert.Equal("v", legacy["k"]);
    }

    [Fact]
    public void DictionaryWithNonStringKeys_IsRejected()
    {
        using var t = new TestDb();
        Assert.Throws<ArgumentException>(() => t.RoundTrip(new Hashtable { [1] = "x" }));
    }

    [Fact]
    public void TopLevelParameters_AcceptDictionaryAnonymousObjectAndPoco()
    {
        using var t = new TestDb();

        Assert.Equal(3L, t.Connection.Query("RETURN $a + $b", new Dictionary<string, object?> { ["a"] = 1, ["b"] = 2 }).Scalar<long>());
        Assert.Equal(3L, t.Connection.Query("RETURN $a + $b", new { a = 1, b = 2 }).Scalar<long>());
        Assert.Equal("Copenhagen 2100", t.Connection.Query("RETURN $City + ' ' + toString($Zip)", new Address()).Scalar<string>());
    }

    [Fact]
    public void NestedObjectsAndRecords_BecomeNestedMaps()
    {
        using var t = new TestDb();
        var person = new Person("Ada", 36, new Address { City = "London", Zip = 1 });

        var map = Assert.IsAssignableFrom<IReadOnlyDictionary<string, object?>>(t.RoundTrip(person));
        var home = Assert.IsAssignableFrom<IReadOnlyDictionary<string, object?>>(map["Home"]);

        Assert.Equal("Ada", map["Name"]);
        Assert.Equal(36L, map["Age"]);
        Assert.Equal("London", home["City"]);
    }

    [Fact]
    public void CyclicObjectGraph_IsRejectedInsteadOfOverflowingTheStack()
    {
        using var t = new TestDb();
        var list = new List<object>();
        list.Add(list);

        var ex = Assert.Throws<ArgumentException>(() => t.RoundTrip(list));
        Assert.Contains("nested deeper", ex.Message);
    }

    [Fact]
    public void GraphEntities_CannotBeSentAsParameters()
    {
        using var t = new TestDb();
        var node = t.Connection.Query("CREATE (n:X) RETURN n")[0].Get<Node>("n");

        Assert.Throws<ArgumentException>(() => t.RoundTrip(node));
    }

    [Fact]
    public void UnsupportedTopLevelParameterObject_IsRejected()
    {
        using var t = new TestDb();
        Assert.Throws<ArgumentException>(() => t.Connection.Query("RETURN 1", 42));
    }

    [Fact]
    public void Temporals_RoundTripToTheirDotNetTypes()
    {
        using var t = new TestDb();
        var date = new DateOnly(2024, 2, 29);
        var time = new TimeOnly(13, 14, 15).Add(TimeSpan.FromTicks(1234567));
        var local = new DateTime(2024, 1, 2, 3, 4, 5, DateTimeKind.Unspecified).AddTicks(9876543);
        var offset = new DateTimeOffset(2024, 1, 2, 3, 4, 5, TimeSpan.FromHours(-7));

        Assert.Equal(date, t.RoundTrip(date));
        Assert.Equal(time, t.RoundTrip(time));
        Assert.Equal(local, t.RoundTrip(local));
        Assert.Equal(offset, t.RoundTrip(offset));
    }

    [Fact]
    public void UtcDateTime_BecomesDateTimeWithZeroOffset_LocalKindBecomesLocalDateTime()
    {
        using var t = new TestDb();
        var utc = new DateTime(2024, 6, 1, 12, 0, 0, DateTimeKind.Utc);

        var asOffset = Assert.IsType<DateTimeOffset>(t.RoundTrip(utc));
        Assert.Equal(TimeSpan.Zero, asOffset.Offset);
        Assert.Equal(utc, asOffset.UtcDateTime);

        Assert.IsType<DateTime>(t.RoundTrip(new DateTime(2024, 6, 1, 12, 0, 0, DateTimeKind.Local)));
    }

    [Fact]
    public void OffsetTime_And_ZonedDateTime_RoundTrip()
    {
        using var t = new TestDb();
        var offsetTime = new CypherOffsetTime(new TimeOnly(10, 30), TimeSpan.FromHours(5.5));
        var zoned = new CypherZonedDateTime(new DateTimeOffset(2024, 6, 1, 12, 0, 0, TimeSpan.FromHours(2)), "Europe/Copenhagen");

        Assert.Equal(offsetTime, t.RoundTrip(offsetTime));
        Assert.Equal(zoned, t.RoundTrip(zoned));
    }

    [Fact]
    public void ZonedDateTime_ResolvesOffsetFromZone()
    {
        using var t = new TestDb();
        // Offset deliberately wrong: relate resolves it from the zone (Copenhagen is UTC+1 in January).
        var zoned = new CypherZonedDateTime(new DateTimeOffset(2024, 1, 15, 9, 0, 0, TimeSpan.Zero), "Europe/Copenhagen");

        var back = Assert.IsType<CypherZonedDateTime>(t.RoundTrip(zoned));

        Assert.Equal(TimeSpan.FromHours(1), back.DateTimeOffset.Offset);
        Assert.Equal(new DateTime(2024, 1, 15, 9, 0, 0), back.DateTimeOffset.DateTime);
    }

    [Fact]
    public void UnknownTimeZone_IsRejectedAsArgumentException()
    {
        using var t = new TestDb();
        var zoned = new CypherZonedDateTime(DateTimeOffset.UnixEpoch, "Not/AZone");

        var ex = Assert.Throws<ArgumentException>(() => t.RoundTrip(zoned));

        Assert.Contains("'v'", ex.Message);
        Assert.IsType<RelateException>(ex.InnerException);
    }

    [Fact]
    public void Durations_RoundTrip()
    {
        using var t = new TestDb();
        var duration = new CypherDuration(14, 3, 3_600, 500);

        Assert.Equal(duration, t.RoundTrip(duration));
        Assert.Equal(new CypherDuration(0, 0, 90, 500_000_000), t.RoundTrip(TimeSpan.FromSeconds(90.5)));
    }

    [Fact]
    public void Points_RoundTrip_In2DAnd3D()
    {
        using var t = new TestDb();

        foreach (var p in new[] { CypherPoint.Cartesian(1, 2), CypherPoint.Cartesian(1, 2, 3), CypherPoint.Wgs84(12.57, 55.68), CypherPoint.Wgs84(12.57, 55.68, 10) })
        {
            Assert.Equal(p, t.RoundTrip(p));
        }
    }

    [Fact]
    public void InvalidPoint_IsRejected()
    {
        using var t = new TestDb();
        // SRID 7203 is 2D: a z coordinate is a srid/dimension mismatch.
        Assert.Throws<ArgumentException>(() => t.RoundTrip(new CypherPoint(CypherPoint.CartesianSrid, 1, 2, 3)));
    }

    [Fact]
    public void Vectors_RoundTrip_ForFloatAndIntegerTypes()
    {
        using var t = new TestDb();
        var vectors = new[]
        {
            CypherVector.FromSingles(new[] { 1.5f, -2.25f, 0f }),
            CypherVector.FromDoubles(new[] { 0.1, 0.2 }),
            CypherVector.FromDoubles(new[] { 0.5, 1.0 }, VectorElementType.Float16),
            CypherVector.FromInt32s(new[] { 1, -2, 3 }),
            CypherVector.FromInt64s(new[] { long.MaxValue }),
            CypherVector.FromInt64s(new long[] { -128, 127 }, VectorElementType.Integer8),
        };

        foreach (var v in vectors)
        {
            Assert.Equal(v, t.RoundTrip(v));
        }
    }

    [Fact]
    public void VectorCoordinateOutOfRange_IsRejected()
    {
        using var t = new TestDb();

        Assert.Throws<ArgumentException>(() => t.RoundTrip(CypherVector.FromInt64s(new long[] { 1000 }, VectorElementType.Integer8)));
        Assert.Throws<ArgumentException>(() => t.RoundTrip(CypherVector.FromDoubles(new[] { 1.1 }, VectorElementType.Float16)));
    }

    [Fact]
    public void ParametersCanBeUsedInWrites()
    {
        using var t = new TestDb();
        t.Connection.Query(
            "CREATE (p:Person {name: $name, born: $born, tags: $tags, location: $where})",
            new { name = "Ada", born = new DateOnly(1815, 12, 10), tags = new[] { "math" }, where = CypherPoint.Wgs84(-0.12, 51.5) });

        var p = t.Connection.Query("MATCH (p:Person) RETURN p")[0].Get<Node>("p");
        Assert.NotNull(p);

        Assert.Equal("Ada", p["name"]);
        Assert.Equal(new DateOnly(1815, 12, 10), p["born"]);
        Assert.Equal(new[] { "math" }, p.Get<string[]>("tags"));
        Assert.Equal(CypherPoint.Wgs84(-0.12, 51.5), p["location"]);
    }
}
